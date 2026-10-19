//! The JSON API under `/api/*` (types in crates/api-types). Plain axum handlers registered
//! through aide, so the OpenAPI document (`web openapi`, committed as web/src/api/openapi.json)
//! is read off their signatures: handlers return `Result<Json<T>, ApiError>`, never `impl
//! IntoApiResponse`, which aide can't see through. The Dioxus server functions call the same
//! functions, so both keep the `bff.<fn>` spans.
//!
//! Access is per route group, as layers ([`access`]): `/api/account/*` needs a login, and its
//! sharing settings the `arcane` permission; the session, password, email, deletion and invite
//! routes are public (a code in the body authorizes them). The one exception under
//! `/api/account/` is deleting shared pictures, which needs only the session's username. Every
//! non-GET request must come from the site itself ([`same_origin`]). Unknown `/api/*` paths and
//! methods get the error envelope too.

use aide::axum::routing::{delete, get, get_with, post, put};
use aide::axum::ApiRouter;
use aide::openapi::{Info, OpenApi};
use aide::NoApi;
use aide::{OperationInput, OperationOutput};
use api_types::{
    Access, AccountInfo, Bootstrap, Code, Confirmed, Deleted, Deletion, DirectDeletion, DisplayName, Done, EmailSent,
    ErrorCode, ForgotPassword, LoggedIn, Login, Opened, PasswordReset, Registration, Sharing, SharingState,
};
use axum::Extension;
use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, NoContent, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use tower_sessions::Session;

use crate::rolls::RollHub;
use crate::{account, emails, invites, sharing};

/// An [`api_types::ApiError`] as a response: its status and the `{error, message}` body.
#[derive(Debug)]
pub struct ApiError(pub api_types::ApiError);

impl From<api_types::ApiError> for ApiError {
    fn from(e: api_types::ApiError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    /// As `api::trace::rejectable`: a 4xx is an answer, anything else fails the current span.
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        if !status.is_client_error() {
            api::trace::failed("api");
        }
        (status, Json(self.0)).into_response()
    }
}

impl OperationOutput for ApiError {
    type Inner = api_types::ApiError;

    fn operation_response(
        ctx: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Option<aide::openapi::Response> {
        Json::<api_types::ApiError>::operation_response(ctx, operation)
    }

    /// Every error, whatever its status, is the same envelope: the default response.
    fn inferred_responses(
        ctx: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Vec<(Option<aide::openapi::StatusCode>, aide::openapi::Response)> {
        Self::operation_response(ctx, operation).map(|r| (None, r)).into_iter().collect()
    }
}

/// The documented responses of an extractor rejected with an [`ApiError`].
fn rejections(
    ctx: &mut aide::generate::GenContext,
    operation: &mut aide::openapi::Operation,
) -> Vec<(Option<aide::openapi::StatusCode>, aide::openapi::Response)> {
    ApiError::inferred_responses(ctx, operation)
}

/// A rejected request body as an `invalid` error with the rejection's status (400, 413, 415,
/// 422). Only the status is logged: the rejection's text can quote what was sent.
fn rejected(status: StatusCode) -> ApiError {
    tracing::info!(status = status.as_u16(), "API request body rejected");
    api_types::ApiError::new(ErrorCode::Invalid, "The request body isn't valid.").with_status(status.as_u16()).into()
}

/// `axum::Json` as a request body, rejected with an [`ApiError`].
pub struct Body<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(body)) => Ok(Self(body)),
            Err(e) => Err(rejected(e.status())),
        }
    }
}

impl<T: schemars::JsonSchema> OperationInput for Body<T> {
    fn operation_input(ctx: &mut aide::generate::GenContext, operation: &mut aide::openapi::Operation) {
        Json::<T>::operation_input(ctx, operation);
    }

    fn inferred_early_responses(
        ctx: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Vec<(Option<aide::openapi::StatusCode>, aide::openapi::Response)> {
        rejections(ctx, operation)
    }
}

/// Raw bytes as a request body (`application/octet-stream`), rejected with an [`ApiError`].
pub struct Bytes(pub axum::body::Bytes);

impl<S: Send + Sync> FromRequest<S> for Bytes {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match axum::body::Bytes::from_request(req, state).await {
            Ok(bytes) => Ok(Self(bytes)),
            Err(e) => Err(rejected(e.status())),
        }
    }
}

impl OperationInput for Bytes {
    fn operation_input(ctx: &mut aide::generate::GenContext, operation: &mut aide::openapi::Operation) {
        axum::body::Bytes::operation_input(ctx, operation);
    }

    fn inferred_early_responses(
        ctx: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Vec<(Option<aide::openapi::StatusCode>, aide::openapi::Response)> {
        rejections(ctx, operation)
    }
}

/// Who the request comes from: resolved once per request (one introspect call to auth) and
/// kept in its extensions, so the access layer and the handler share it.
#[derive(Debug, Clone)]
pub struct Viewer(pub api::Viewer);

impl Viewer {
    /// The logged-in user's token; `logged_out`, or `unavailable` while auth can't be asked.
    pub fn token(&self) -> Result<&str, ApiError> {
        match &self.0 {
            api::Viewer::User { token, .. } => Ok(token),
            api::Viewer::Anonymous => Err(api_types::ApiError::logged_out().into()),
            api::Viewer::Degraded { .. } => Err(api_types::ApiError::login_unavailable().into()),
        }
    }

    /// The session's username, also while auth can't be asked; `logged_out` without one.
    pub fn username(&self) -> Result<&str, ApiError> {
        match &self.0 {
            api::Viewer::User { username, .. } | api::Viewer::Degraded { username } => Ok(username),
            api::Viewer::Anonymous => Err(api_types::ApiError::logged_out().into()),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Viewer {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        if let Some(viewer) = parts.extensions.get::<api::Viewer>() {
            return Ok(Self(viewer.clone()));
        }
        let Some(session) = parts.extensions.get::<Session>() else {
            tracing::error!("no session on an API request: the session layer is missing");
            return Err(api_types::ApiError::internal().into());
        };
        let viewer = api::viewer(session).await.map_err(|e| {
            tracing::error!(error = %e, "reading the session failed");
            ApiError::from(api_types::ApiError::login_unavailable())
        })?;
        parts.extensions.insert(viewer.clone());
        Ok(Self(viewer))
    }
}

impl OperationInput for Viewer {
    fn inferred_early_responses(
        ctx: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Vec<(Option<aide::openapi::StatusCode>, aide::openapi::Response)> {
        rejections(ctx, operation)
    }
}

/// [`same_origin`]'s rule: `dev` origins count only in a debug build.
fn origin_allowed(headers: &axum::http::HeaderMap, dev: &str, debug: bool) -> bool {
    crate::rolls::origin_allowed_with(headers, if debug { dev } else { "" }, debug)
}

/// Whether `viewer` may use a route of `access`: 401 `logged_out`, 403 `forbidden`, or 503
/// while auth can't say who is logged in.
fn allowed(access: Access, viewer: &Viewer) -> Result<(), ApiError> {
    let permissions = match &viewer.0 {
        api::Viewer::User { permissions, .. } => permissions,
        _ => return viewer.token().map(|_| ()),
    };
    match access {
        Access::Public | Access::LoggedIn => Ok(()),
        Access::Perm(p) if permissions.iter().any(|q| q.known() == Some(p)) => Ok(()),
        Access::Perm(_) => Err(api_types::ApiError::new(ErrorCode::Forbidden, "You don't have access to this.").into()),
    }
}

/// The access layer of a group of routes: `.route_layer(from_fn_with_state(Access::LoggedIn,
/// access))`. Public routes don't resolve the viewer.
async fn access(State(access): State<Access>, req: Request, next: Next) -> Response {
    if access == Access::Public {
        return next.run(req).await;
    }
    let (mut parts, body) = req.into_parts();
    let checked = match Viewer::from_request_parts(&mut parts, &()).await {
        Ok(viewer) => allowed(access, &viewer),
        Err(e) => Err(e),
    };
    match checked {
        Ok(()) => next.run(Request::from_parts(parts, body)).await,
        Err(e) => e.into_response(),
    }
}

/// Every request that changes something must come from the site's own pages (the session
/// cookie is sent from any site's forms): the `Origin` must be this host, over HTTPS (or
/// HTTP in a debug build). Requests without an `Origin` aren't from a browser and pass.
/// `ARCANE_ALLOWED_ORIGINS` (the extension's) doesn't apply here; a debug build also takes the
/// local dev proxies' origins from `DEV_ALLOWED_ORIGINS` (ENV.md).
async fn same_origin(req: Request, next: Next) -> Response {
    let read = req.method() == Method::GET || req.method() == Method::HEAD;
    let dev = std::env::var("DEV_ALLOWED_ORIGINS").unwrap_or_default();
    if read || origin_allowed(req.headers(), &dev, cfg!(debug_assertions)) {
        return next.run(req).await;
    }
    tracing::warn!(origin = ?req.headers().get(axum::http::header::ORIGIN), "API request rejected: foreign origin");
    ApiError::from(api_types::ApiError::new(ErrorCode::Forbidden, "This request came from another site.")).into_response()
}

// ---- Session ----

/// `GET /api/session`: who is logged in, with their name, picture and permissions.
async fn session(viewer: Viewer) -> Json<Bootstrap> {
    let viewer = match viewer.0 {
        api::Viewer::Anonymous => None,
        api::Viewer::Degraded { username } => {
            Some(api_types::Viewer { shown_name: username.clone(), username, picture_url: None, permissions: vec![] })
        }
        api::Viewer::User { username, token, permissions } => {
            // Without the account (auth or the picture store is down) the name is shown.
            let (shown_name, picture_url) = match account::account(&token).await {
                Ok(a) => (a.shown_name, a.picture_url),
                Err(_) => (username.clone(), None),
            };
            let permissions = permissions.iter().filter_map(api_types::Permission::known).collect();
            Some(api_types::Viewer { username, shown_name, picture_url, permissions })
        }
    };
    Json(Bootstrap { viewer })
}

/// `POST /api/session/login`.
async fn login(NoApi(session): NoApi<Session>, Body(login): Body<Login>) -> Result<Json<LoggedIn>, ApiError> {
    let username = api::log_in(&session, login.username, login.password).await?;
    Ok(Json(LoggedIn { username }))
}

/// `POST /api/session/register`.
async fn register(NoApi(session): NoApi<Session>, Body(r): Body<Registration>) -> Result<Json<LoggedIn>, ApiError> {
    let username = api::register(&session, r.username, r.email, r.password).await?;
    Ok(Json(LoggedIn { username }))
}

/// `DELETE /api/session`: log out.
async fn logout(NoApi(session): NoApi<Session>) -> Result<NoContent, ApiError> {
    api::log_out(&session).await?;
    Ok(NoContent)
}

// ---- Account ----

/// `GET /api/account`.
async fn get_account(viewer: Viewer) -> Result<Json<AccountInfo>, ApiError> {
    Ok(Json(account::account(viewer.token()?).await?))
}

/// `PUT /api/account/display-name`.
async fn set_display_name(viewer: Viewer, Body(name): Body<DisplayName>) -> Result<Json<AccountInfo>, ApiError> {
    Ok(Json(account::set_name(viewer.token()?, &name.name).await?))
}

/// `DELETE /api/account/picture`.
async fn remove_picture(viewer: Viewer) -> Result<Json<AccountInfo>, ApiError> {
    Ok(Json(account::remove_picture(viewer.token()?).await?))
}

// ---- Account emails ----
//
// The codes from emailed links come in the body, never the URL. The calls a code alone
// authorizes are public, so a link works on a logged-out device too.

/// The roll hub, for forgetting a deleted account's held rolls (absent in tests).
type Hub = NoApi<Option<Extension<RollHub>>>;

/// `POST /api/account/email/send-confirmation`.
async fn send_confirmation(viewer: Viewer) -> Result<Json<EmailSent>, ApiError> {
    Ok(Json(EmailSent { email: emails::server::send_confirmation(viewer.token()?).await? }))
}

/// `POST /api/email/confirm`.
async fn confirm_email(Body(code): Body<Code>) -> Result<Json<Confirmed>, ApiError> {
    Ok(Json(Confirmed { username: emails::server::confirm(&code.code).await? }))
}

/// `POST /api/password/forgot`: answered the same whether or not the account exists.
async fn forgot_password(Body(f): Body<ForgotPassword>) -> Result<NoContent, ApiError> {
    emails::server::forgot(&f.login).await?;
    Ok(NoContent)
}

/// `POST /api/password/reset`.
async fn reset_password(NoApi(session): NoApi<Session>, Body(r): Body<PasswordReset>) -> Result<Json<Done>, ApiError> {
    Ok(Json(emails::server::reset(Some(&session), &r.code, &r.password).await?))
}

/// `POST /api/account/deletion/request`.
async fn request_deletion(viewer: Viewer) -> Result<Json<EmailSent>, ApiError> {
    Ok(Json(EmailSent { email: emails::server::request_deletion(viewer.token()?).await? }))
}

/// `POST /api/deletion/check`.
async fn check_deletion(Body(code): Body<Code>) -> Result<Json<Deletion>, ApiError> {
    Ok(Json(emails::server::deletion(&code.code).await?))
}

/// `POST /api/deletion/confirm`.
async fn confirm_deletion(NoApi(session): NoApi<Session>, NoApi(hub): Hub, Body(code): Body<Code>) -> Result<Json<Done>, ApiError> {
    let hub = hub.map(|Extension(h)| h);
    Ok(Json(emails::server::delete(Some(&session), hub.as_ref(), &code.code).await?))
}

/// `POST /api/account/deletion/direct`.
async fn delete_direct(
    viewer: Viewer,
    NoApi(session): NoApi<Session>,
    NoApi(hub): Hub,
    Body(d): Body<DirectDeletion>,
) -> Result<Json<Done>, ApiError> {
    let hub = hub.map(|Extension(h)| h);
    Ok(Json(emails::server::delete_direct(Some(&session), hub.as_ref(), viewer.token()?, &d.confirm).await?))
}

// ---- Invites ----

/// `POST /api/invites/open`: joins when logged in, else keeps the invite for after logging in.
async fn open_invite(NoApi(session): NoApi<Session>, Body(code): Body<Code>) -> Result<Json<Opened>, ApiError> {
    Ok(Json(invites::server::open(&session, &code.code).await?))
}

// ---- Sharing roll pictures ----

/// `GET /api/account/sharing`.
async fn get_sharing(viewer: Viewer) -> Result<Json<SharingState>, ApiError> {
    Ok(Json(sharing::server::sharing(viewer.token()?).await?))
}

/// `PUT /api/account/sharing`.
async fn set_sharing(viewer: Viewer, Body(s): Body<Sharing>) -> Result<Json<SharingState>, ApiError> {
    Ok(Json(sharing::server::set_sharing(viewer.token()?, s.share).await?))
}

/// `DELETE /api/account/sharing/data`: needs only the session's username, so it works without
/// the `arcane` permission and while auth is down.
async fn delete_shared(viewer: Viewer, NoApi(hub): Hub) -> Result<Json<Deleted>, ApiError> {
    let user = viewer.username()?.to_string();
    let token = viewer.token().ok().map(str::to_string);
    let hub = hub.map(|Extension(h)| h);
    let n = sharing::server::delete_shared(user, token.as_deref(), hub).await?;
    Ok(Json(Deleted { deleted: n as u64 }))
}

/// The routes, with their documentation. Builds without Redis, env or network.
fn api_router() -> ApiRouter {
    let picture_limit = || axum::extract::DefaultBodyLimit::max(account::PICTURE_MAX_UPLOAD);
    let account = ApiRouter::new()
        .api_route("/api/account", get(get_account))
        .api_route("/api/account/display-name", put(set_display_name))
        .api_route(
            "/api/account/picture",
            get_with(account::get_picture, |op| {
                op.response_with::<200, axum::body::Bytes, _>(|r| r.description("The picture: a 256x256 JPEG."))
            })
            .post(account::upload_picture)
            .delete(remove_picture)
            // Room for the largest accepted upload; bigger bodies get 413.
            .layer(picture_limit()),
        )
        // The picture's old path, for the Dioxus pages and cached `<img>` URLs: the same
        // handlers, left out of the document.
        .route(
            "/api/profile/picture",
            axum::routing::get(account::get_picture).post(account::upload_picture).layer(picture_limit()),
        )
        .api_route("/api/account/email/send-confirmation", post(send_confirmation))
        .api_route("/api/account/deletion/request", post(request_deletion))
        .api_route("/api/account/deletion/direct", post(delete_direct))
        .route_layer(from_fn_with_state(Access::LoggedIn, access));
    let arcane = ApiRouter::new()
        .api_route("/api/account/sharing", get(get_sharing).put(set_sharing))
        .route_layer(from_fn_with_state(Access::Perm(api_types::KnownPermission::Arcane), access));
    // Public: the handlers take what they need (a code, the session's username).
    let public = ApiRouter::new()
        .api_route("/api/session", get(session).delete(logout))
        .api_route("/api/session/login", post(login))
        .api_route("/api/session/register", post(register))
        .api_route("/api/email/confirm", post(confirm_email))
        .api_route("/api/password/forgot", post(forgot_password))
        .api_route("/api/password/reset", post(reset_password))
        .api_route("/api/deletion/check", post(check_deletion))
        .api_route("/api/deletion/confirm", post(confirm_deletion))
        .api_route("/api/invites/open", post(open_invite))
        // Deleting what one shared must not depend on auth (see the handler).
        .api_route("/api/account/sharing/data", delete(delete_shared))
        .route_layer(from_fn_with_state(Access::Public, access));
    public.merge(account).merge(arcane).route_layer(from_fn(same_origin))
}

/// The API's routes, and its OpenAPI document.
fn finish() -> (axum::Router, OpenApi) {
    let mut doc = OpenApi {
        info: Info {
            title: "milesstorm.com".into(),
            // The document's own version: the spec is diffed in CI, not versioned.
            version: "0".into(),
            ..Info::default()
        },
        ..OpenApi::default()
    };
    // The schemas describe what the server sends: an `Option` field is always there (as
    // `null`), so it is required. aide's settings otherwise.
    aide::generate::in_context(|ctx| {
        let settings = schemars::generate::SchemaSettings::draft07().for_serialize().with(|s| {
            s.inline_subschemas = false;
            s.definitions_path = "#/components/schemas/".into();
        });
        ctx.schema = schemars::SchemaGenerator::new(settings);
    });
    let router = api_router().finish_api(&mut doc);
    (router, doc)
}

/// The API's routes, and the envelope for every other `/api/*` path and for a method a route
/// doesn't have. The plain `/api/arcane/*` routes are explicit routes, so they still match
/// before the catch-all.
pub fn router() -> axum::Router {
    let not_found = || async { ApiError::from(api_types::ApiError::new(ErrorCode::NotFound, "There is no such API.")) };
    let not_allowed = || async {
        ApiError::from(api_types::ApiError::new(ErrorCode::Invalid, "That method isn't allowed here.").with_status(405))
    };
    finish()
        .0
        .method_not_allowed_fallback(not_allowed)
        .route("/api/{*rest}", axum::routing::any(not_found))
}

/// The OpenAPI document as `web openapi` prints it (and web/src/api/openapi.json holds it).
pub fn openapi_json() -> String {
    let mut json = serde_json::to_string_pretty(&finish().1).expect("the OpenAPI document serializes");
    json.push('\n');
    json
}

#[cfg(test)]
pub(crate) mod tests;
