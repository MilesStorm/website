use std::collections::HashMap;

use dioxus::prelude::*;

use api::{check_login_status, get_my_permissions, logout};
use ui::{data_dir::LoginStatus, setup_mode, CookieConsent, Navbar, TAILWIND};
use views::{Ark, Landing, Login, NotFound, Profile, Register};

mod views;

pub static LOGIN_STATUS: GlobalSignal<LoginStatus> = Signal::global(|| LoginStatus::LoggedOut);
pub static PERMISSIONS: GlobalSignal<HashMap<String, bool>> = Signal::global(HashMap::new);

const FAVICON: Asset = asset!("/assets/favicon.ico");

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        match dotenvy::dotenv() {
            Ok(_) => {}
            Err(_) if !cfg!(debug_assertions) => {}
            Err(e) => panic!("could not load .env: {e}"),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    server_launch();

    #[cfg(target_arch = "wasm32")]
    dioxus::launch(App);
}

// ---- Server launch ----

#[cfg(not(target_arch = "wasm32"))]
fn server_launch() -> ! {
    use axum::{routing::get, Router};
    use tower_sessions::cookie::time::Duration;
    use tower_sessions::cookie::SameSite;
    use tower_sessions::{Expiry, SessionManagerLayer};
    use tower_sessions_redis_store::fred::prelude::*;
    use tower_sessions_redis_store::RedisStore;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    // try_init() is a no-op if something already registered a subscriber, so this is safe.
    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "info,dioxus=warn,tower_sessions=warn".into(),
        )))
        .with(tracing_subscriber::fmt::layer().json())
        .try_init()
        .ok();

    let redis_host = std::env::var("REDIS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let redis_port = std::env::var("REDIS_PORT").unwrap_or_else(|_| "6379".to_string());
    let redis_password = std::env::var("REDIS_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty());
    let redis_url = match &redis_password {
        Some(p) => format!("redis://:{p}@{redis_host}:{redis_port}"),
        None => format!("redis://{redis_host}:{redis_port}"),
    };

    dioxus::serve(move || {
        let redis_url = redis_url.clone();
        async move {
            let config = Config::from_url(&redis_url).expect("invalid Redis URL");
            let pool = Pool::new(config, None, None, None, 6).expect("failed to build Redis pool");
            pool.connect();
            pool.wait_for_connect()
                .await
                .expect("failed to connect to Redis");
            let session_store = RedisStore::new(pool);

            let layer = SessionManagerLayer::new(session_store)
                .with_secure(false)
                .with_same_site(SameSite::Lax)
                .with_name("milesstorm.bff")
                .with_expiry(Expiry::OnInactivity(Duration::days(7)));

            let router = Router::new()
                .serve_dioxus_application(ServeConfig::default(), App)
                .route("/oauth/start/{provider}", get(oauth_start))
                .route("/oauth/callback/{provider}", get(oauth_callback))
                .layer(layer);
            Ok(router)
        }
    })
}

// ---- OAuth Axum handlers ----

const OAUTH_CSRF_KEY: &str = "oauth_csrf_state";
const OAUTH_PROVIDER_KEY: &str = "oauth_provider";

/// Begin the OAuth flow for `provider`. Asks auth (cluster-internal) for the provider's
/// authorization URL, stashes the CSRF state in the BFF session, and redirects the browser.
#[cfg(not(target_arch = "wasm32"))]
async fn oauth_start(
    axum::extract::Path(provider): axum::extract::Path<String>,
    session: tower_sessions::Session,
) -> axum::response::Response {
    use axum::response::{IntoResponse, Redirect};

    if provider != "github" && provider != "google" {
        return Redirect::to("/login?error=unknown_provider").into_response();
    }

    match api::start_oauth(&provider).await {
        Ok((auth_url, state)) => {
            let _ = session.insert(OAUTH_CSRF_KEY, &state).await;
            let _ = session.insert(OAUTH_PROVIDER_KEY, &provider).await;
            Redirect::to(&auth_url).into_response()
        }
        Err(_) => Redirect::to("/login?error=start_failed").into_response(),
    }
}

/// Provider callback. Validates CSRF state, asks auth to exchange the code, and
/// stores the resulting opaque token + username on the BFF session.
#[cfg(not(target_arch = "wasm32"))]
async fn oauth_callback(
    axum::extract::Path(provider): axum::extract::Path<String>,
    session: tower_sessions::Session,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::{IntoResponse, Redirect};

    let Some(code) = params.get("code").cloned() else {
        return Redirect::to("/login?error=missing_code").into_response();
    };
    let Some(state) = params.get("state").cloned() else {
        return Redirect::to("/login?error=missing_state").into_response();
    };

    let expected_state: Option<String> = session.get(OAUTH_CSRF_KEY).await.ok().flatten();
    let expected_provider: Option<String> = session.get(OAUTH_PROVIDER_KEY).await.ok().flatten();
    let _ = session.remove::<String>(OAUTH_CSRF_KEY).await;
    let _ = session.remove::<String>(OAUTH_PROVIDER_KEY).await;

    if expected_state.as_deref() != Some(&state) || expected_provider.as_deref() != Some(&provider)
    {
        return Redirect::to("/login?error=csrf_mismatch").into_response();
    }

    match api::exchange_oauth_code(&provider, &code).await {
        Ok((token, username)) => {
            let _ = session.insert("opaque_token", token).await;
            let _ = session.insert("username", username).await;
            Redirect::to("/").into_response()
        }
        Err(e) if e.contains("email_exists") || e.contains("Email already in use") => {
            Redirect::to("/login?error=email_exists").into_response()
        }
        Err(_) => Redirect::to("/login?error=exchange_failed").into_response(),
    }
}

// ---- Dioxus app ----

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
enum Route {
    #[layout(WebNavbar)]
        #[route("/")]
        Landing {},
        #[route("/login?:error")]
        Login { error: String },
        #[route("/register")]
        Register {},
        #[route("/profile")]
        Profile {},
        #[route("/ark")]
        Ark {},
        #[route("/:..segments")]
        NotFound { segments: Vec<String> },
}

#[component]
fn App() -> Element {
    let status = use_server_future(check_login_status)?;
    let perms = use_server_future(get_my_permissions)?;

    use_effect(move || {
        if let Some(Ok(s)) = status.value()() {
            *LOGIN_STATUS.write() = s;
        }
        if let Some(Ok(p)) = perms.value()() {
            let map: HashMap<String, bool> = p.into_iter().map(|n| (n, true)).collect();
            *PERMISSIONS.write() = map;
        }
    });

    setup_mode();

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: TAILWIND }

        Router::<Route> {}

        CookieConsent {}
    }
}

#[component]
fn WebNavbar() -> Element {
    let logout_handler = move |_: ()| {
        spawn(async move {
            let _ = logout().await;
            *LOGIN_STATUS.write() = LoginStatus::LoggedOut;
            *PERMISSIONS.write() = HashMap::new();
        });
    };

    rsx! {
        Navbar {
            user: LOGIN_STATUS(),
            on_logout: logout_handler
        }
        Outlet::<Route> {}
    }
}
