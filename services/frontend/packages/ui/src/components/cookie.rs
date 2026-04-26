use dioxus::prelude::*;
use dioxus_sdk::storage::*;

#[component]
pub fn CookieConsent() -> Element {
    let mut accepted =
        use_synced_storage::<LocalStorage, bool>("accepted_cookies".to_owned(), || false);
    let mut showing =
        use_synced_storage::<LocalStorage, bool>("showing_cookies".to_owned(), || true);

    if accepted() || !showing() {
        return rsx! {};
    }

    rsx! {
        div { class: "alert alert-info fixed bottom-4 left-4 right-4 z-50 max-w-xl shadow-lg",
            svg {
                xmlns: "http://www.w3.org/2000/svg",
                class: "stroke-current shrink-0 h-6 w-6",
                fill: "none",
                view_box: "0 0 24 24",
                path {
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    stroke_width: "2",
                    d: "M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"
                }
            }
            div {
                span { class: "font-bold", "Cookies" }
                p { class: "text-sm",
                    "This site uses functional cookies required for login and session management."
                }
            }
            div { class: "flex gap-2",
                button {
                    class: "btn btn-sm btn-ghost",
                    onclick: move |_| {
                        showing.set(false);
                    },
                    "Dismiss"
                }
                button {
                    class: "btn btn-sm btn-primary",
                    onclick: move |_| {
                        accepted.set(true);
                        showing.set(false);
                    },
                    "Accept"
                }
            }
        }
    }
}
