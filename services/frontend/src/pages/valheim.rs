use dioxus::prelude::*;

use crate::{
    components::Navbar::Navbar,
    hooks::{
        has_permission,
        theme::{get_mode, set_mode, Theme},
    },
};

#[component]
pub fn Valheim() -> Element {
    rsx! {
        Navbar {}
        Restart {}
    }
}

#[component]
pub fn Restart() -> Element {
    let is_permitted = use_resource(move || async move { has_permission("restart_valheim").await });

    match (is_permitted.value())() {
        Some(is_permitted) => {
            if is_permitted {
                rsx! {
                    button {
                        class: "btn btn-error btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                        style: "margin: auto",
                        "Restart"
                    }
                }
            } else {
                rsx! {
                    div {
                        class: "flex justify-center items-center h-full",
                        "You do not have permission to restart the Valheim server."
                    }
                }
            }
        }
        None => {
            rsx! {
                div {
                    class: "flex justify-center items-center h-full",
                    "Loading..."
                }
            }
        }
    }
}
