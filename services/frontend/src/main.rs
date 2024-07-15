#![allow(non_snake_case)]

mod components;
mod hooks;
mod pages;

use dioxus::prelude::*;
use dioxus::signals::GlobalSignal;
use dioxus_sdk::storage::{use_synced_storage, LocalStorage};
use hooks::LogInStatus;
use log::LevelFilter;

use crate::{components::cookie::CookieAlert, cv::CvPage, pages::*};

// Urls are relative to your Cargo.toml file
const _TAILWIND_URL: &str = manganis::mg!(file("assets/main.css"));
// pub static LOGIN_STATUS: GlobalSignal<LogInStatus> = Signal::global(|| LogInStatus::LoggedOut);
pub static LOGIN_STATUS: GlobalSignal<LogInStatus> = Signal::global(|| LogInStatus::LoggedOut);

#[derive(Clone, Routable, Debug, PartialEq)]
enum Route {
    #[route("/")]
    Home {},
    #[route("/arcaneeye")]
    ArcaneEye {},
    #[route("/blog/:id")]
    Blog { id: i32 },
    #[route("/resume")]
    CvPage {},
    #[route("/profile")]
    Profile {},
    #[route("/login?:error")]
    Login { error: String },
    #[route("/register")]
    Register {},
    #[route("/:..segments")]
    NotFound { segments: Vec<String> },
}

fn main() {
    // Init debug
    if cfg!(debug_assertions) {
        dioxus_logger::init(LevelFilter::Info).expect("failed to init logger");
    } else {
        dioxus_logger::init(LevelFilter::Error).expect("failed to init logger");
    }
    console_error_panic_hook::set_once();

    launch(App);
}

fn App() -> Element {
    hooks::setup_mode();
    let _ = use_resource(|| async move {
        *LOGIN_STATUS.write() = LogInStatus::is_logged_in().await;
    });

    let is_eating_cookies =
        use_synced_storage::<LocalStorage, bool>("accepted_cookies".to_owned(), || false);
    let is_showing_cookies =
        use_synced_storage::<LocalStorage, bool>("showing_cookies".to_owned(), || true);

    rsx! {
        div {
            class: "flex flex-col justify-between h-screen",
            Router::<Route> {}
            if is_showing_cookies() {
                CookieAlert {is_showing: is_showing_cookies,is_eating: is_eating_cookies}
            }
        }
    }
}

#[component]
fn Blog(id: i32) -> Element {
    rsx! {
        Link { to: Route::Home {}, "Go to counter" }
        "Blog post {id}"
    }
}

#[component]
fn Home() -> Element {
    rsx! {
        Landing {}
    }
}
