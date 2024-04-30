#![allow(non_snake_case)]

mod pages;

use dioxus::prelude::*;
use log::LevelFilter;

use crate::{cv::CvPage, hook::Theme, pages::*};

// Urls are relative to your Cargo.toml file
const _TAILWIND_URL: &str = manganis::mg!(file("assets/main.css"));

#[derive(Clone, Routable, Debug, PartialEq)]
enum Route {
    #[route("/")]
    Home {},
    #[route("/blog/:id")]
    Blog { id: i32 },
    #[route("/cv")]
    CvPage {},
}

fn main() {
    // Init debug
    dioxus_logger::init(LevelFilter::Info).expect("failed to init logger");
    console_error_panic_hook::set_once();

    launch(App);
}

fn App() -> Element {
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
    hook::mode(Theme::Light);
    rsx! {}
}
