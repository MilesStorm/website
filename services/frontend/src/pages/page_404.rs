use dioxus::prelude::*;

#[component]
pub fn NotFound(segments: Vec<String>) -> Element {
    rsx! {
        div { class: "bg-size-200 h-screen flex items-center justify-center text-center content",
            div {
                h1 { class: "text-6xl font-extrabold bg-clip-text text-transparent
        bg-[linear-gradient(to_right,theme(colors.indigo.400),theme(colors.indigo.700),theme(colors.sky.400),theme(colors.fuchsia.600),theme(colors.sky.400),theme(colors.accent),theme(colors.indigo.400))]
        bg-[length:200%_auto] animate-gradient", "404 Not Found" }
                p { class: "text-2xl", "Oops! The page you're looking for isn't here." }
                Link { to: "/", class: "btn btn-primary mt-4", "Go Home" }
            }
        }
    }
}
