#![allow(non_snake_case)]

mod components;
mod pages;
mod utils;

use dioxus::prelude::*;
use dioxus::signals::GlobalSignal;
use log::LevelFilter;
use utils::LogInStatus;

use crate::{cv::CvPage, pages::*};

// Urls are relative to your Cargo.toml file
const _TAILWIND_URL: &str = manganis::mg!(file("assets/main.css"));
pub static LOGIN_STATUS: GlobalSignal<LogInStatus> = Signal::global(|| LogInStatus::LoggedOut);

#[derive(Clone, Routable, Debug, PartialEq)]
enum Route {
    #[route("/")]
    Home {},
    #[route("/blog/:id")]
    Blog { id: i32 },
    #[route("/cv")]
    CvPage {},
    #[route("/profile")]
    Profile {},
    #[route("/login")]
    Login {},
    #[route("/:..segments")]
    NotFound { segments: Vec<String> },
}

fn main() {
    // Init debug
    dioxus_logger::init(LevelFilter::Info).expect("failed to init logger");
    console_error_panic_hook::set_once();

    launch(App);
}

fn App() -> Element {
    hook::setup_mode();
    rsx! {
        Router::<Route> {}
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
