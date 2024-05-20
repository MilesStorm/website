use dioxus::prelude::*;

#[derive(Clone, Debug, Props, PartialEq)]
pub struct CookieProps {
    is_showing: Signal<bool>,
    is_eating: Signal<bool>,
}

#[component]
pub fn CookieAlert(mut props: CookieProps) -> Element {
    rsx! {
        div { role: "alert", class: "alert",
            svg {
                "fill": "none",
                "viewBox": "0 0 24 24",
                "xmlns": "http://www.w3.org/2000/svg",
                class: "stroke-info shrink-0 w-6 h-6",
                path {
                    "stroke-linejoin": "round",
                    "stroke-linecap": "round",
                    "d": "M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z",
                    "stroke-width": "2"
                }
            }
            span { "This site only uses functionality cookies, without them it will not work. You can still choose to not use them." }
            div {
                button {onclick: move |_| {
                    *props.is_showing.write() = false;
                    *props.is_eating.write() = false;
                },
                class: "btn btn-sm",
                "Deny"
                }
                button {onclick: move |_| {
                    *props.is_showing.write() = false;
                    *props.is_eating.write() = true;
                },
                class: "btn btn-sm btn-primary",
                "Accept"
                }
            }
        }
    }
}
