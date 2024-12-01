use dioxus::prelude::*;

use crate::{
    components::Navbar::Navbar,
    hooks::{
        has_permission, restart_valheim,
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
    let mut has_clicked = use_signal(|| false);
    let is_permitted = use_resource(|| async move { has_permission("valheim_player").await });

    match (is_permitted.value())() {
        Some(is_permitted) => {
            if is_permitted {
                if has_clicked() {
                    restart_button()
                } else {
                    rsx! {
                        button {
                            class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                            style: "margin: auto",
                            onclick: move |_| {
                                has_clicked.set(true);
                            },
                            "Restart"
                        }
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

#[component]
fn restart_button() -> Element {
    let mut restart_action = use_resource(|| async move { restart_valheim().await });

    match &*restart_action.read_unchecked() {
        Some(_) => rsx! {
            button {
                class: "btn btn-success btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                style: "margin: auto",
                onclick: move |_| {
                    restart_action.restart();
                },
                "Success!"
            }
        },
        Some(Err(e)) => rsx! {
            button {
                class: "btn btn-error btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                style: "margin: auto",
                onclick: move |_| {
                    restart_action.restart();
                },
                "Error!"
            }
        },
        None => {
            rsx! {
                button {
                    class: "btn",
                    style: "margin: auto",
                    span { class: "loading loading-spinner", "loading" }
                }
            }
        }
    }
}
