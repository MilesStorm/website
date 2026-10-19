//! The JSON API under `/api/*` (types in crates/api-types). Plain axum handlers registered
//! through aide, so the OpenAPI document (`web openapi`, committed as web/src/api/openapi.json)
//! is read off their signatures: handlers return `Result<Json<T>, ApiError>`, never `impl
//! IntoApiResponse`, which aide can't see through. The Dioxus server functions call the same
//! functions, so both keep the `bff.<fn>` spans.
//!
//! Access is per route group, as layers: `/api/account/*` needs a login; the session routes
//! are public. Every non-GET request must come from the site itself ([`same_origin`]).

use aide::axum::routing::{get, get_with, post, put};
use aide::axum::ApiRouter;
use aide::openapi::{Info, OpenApi};
use aide::NoApi;
use aide::{OperationInput, OperationOutput};
use api_types::{AccountInfo, Bootstrap, DisplayName, ErrorCode, LoggedIn, Login, Registration};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::{IntoResponse, NoContent, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use tower_sessions::Session;

use crate::account;

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

/// A rejected request body as an `invalid` error, with the rejection's status (400, 413,
/// 415, 422) and text.
fn rejected(status: StatusCode, text: String) -> ApiError {
    api_types::ApiError::new(ErrorCode::Invalid, text).with_status(status.as_u16()).into()
}

/// `axum::Json` as a request body, rejected with an [`ApiError`].
pub struct Body<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(body)) => Ok(Self(body)),
            Err(e) => Err(rejected(e.status(), e.body_text())),
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
            Err(e) => Err(rejected(e.status(), e.body_text())),
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

/// Who the request comes from: resolved once per request (one call to auth) and kept in its
/// extensions, so an access layer and the handler share it.
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

/// Access layer: only a logged-in viewer gets through (401 `logged_out`; 503 while auth can't
/// say who is logged in).
async fn logged_in(req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    match Viewer::from_request_parts(&mut parts, &()).await.and_then(|v| v.token().map(|_| ())) {
        Ok(()) => next.run(Request::from_parts(parts, body)).await,
        Err(e) => e.into_response(),
    }
}

/// Every request that changes something must come from the site's own pages (the session
/// cookie is sent from any site's forms): `rolls::origin_allowed`, so requests without an
/// `Origin` (not a browser) pass.
async fn same_origin(req: Request, next: Next) -> Response {
    if req.method() == Method::GET || req.method() == Method::HEAD || crate::rolls::origin_allowed(req.headers()) {
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
        .route_layer(from_fn(logged_in));
    ApiRouter::new()
        .api_route("/api/session", get(session).delete(logout))
        .api_route("/api/session/login", post(login))
        .api_route("/api/session/register", post(register))
        .merge(account)
        .route_layer(from_fn(same_origin))
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

pub fn router() -> axum::Router {
    finish().0
}

/// The OpenAPI document as `web openapi` prints it (and web/src/api/openapi.json holds it).
pub fn openapi_json() -> String {
    let mut json = serde_json::to_string_pretty(&finish().1).expect("the OpenAPI document serializes");
    json.push('\n');
    json
}

#[cfg(test)]
pub(crate) mod tests;
