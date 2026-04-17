use dioxus::prelude::*;

use crate::hooks::{has_permission, restart_ark, start_ark, stop_ark};
use crate::LOGIN_STATUS;

use crate::{components::Navbar::Navbar, LogInStatus};

#[component]
pub fn Ark() -> Element {
    rsx! {
        Navbar {}
        match LOGIN_STATUS() {
            LogInStatus::LoggedIn(_) => {
                Restart_Ark_Page()
            }
            LogInStatus::LoggedOut => {
                rsx! {}
            }
        }
    }
}

pub async fn has_ark_permission() -> bool {
    let valheim_permission = has_permission("ark").await;
    let llama_permission = has_permission("llama").await;

    return valheim_permission || llama_permission;
}

#[component]
pub fn Restart_Ark_Page() -> Element {
    let is_permitted = use_resource(|| async move { has_ark_permission().await });

    match (is_permitted.value())() {
        Some(is_permitted) => {
            rsx! {
                div{
                class: "",
                // restart_button {is_permitted}
                stop_button { is_permitted }
                // start_button {is_permitted}
                }
            }
        }
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

#[component]
fn restart_button(is_permitted: bool) -> Element {
    let mut restart_action = use_action(move || async move { restart_ark().await });
    let mut has_clicked = use_signal(|| false);

    if is_permitted {
        if has_clicked() {
            rsx! {
            match &restart_action.value() {
                Some(Err(_)) => rsx! {
                    div {
                        class: "tooltip tooltip-open tooltip-error",
                        "data-tip": "error",
                        style: "margin: auto",
                        button {
                            class: "btn btn-error btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                            onclick: move |_| {
                                restart_action.call();
                            },
                            "Error!"
                        }
                    }
                },
                Some(_) => rsx! {
                    div {
                        class: "tooltip tooltip-open tooltip-success",
                        "data-tip": "success",
                        style: "margin: auto",
                            button {
                            class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                            onclick: move |_| {
                                restart_action.call();
                            },
                            "Restart"
                    }
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
        } else {
            rsx! {
                button {
                    class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                    style: "margin: auto",
                    onclick: move |_| {
                        restart_action.call();
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

#[component]
fn stop_button(is_permitted: bool) -> Element {
    let mut stop_action = use_action(move || async move { stop_ark().await });
    let mut has_clicked = use_signal(|| false);

    if is_permitted {
        if has_clicked() {
            rsx! {
                match &stop_action.value() {
                    Some(Err(_)) => rsx! {
                        div {
                            class: "tooltip tooltip-open tooltip-error",
                            "data-tip": "error",
                            style: "margin: auto",
                            button {
                                class: "btn btn-error btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                onclick: move |_| {
                                    stop_action.call();
                                },
                                "Error!"
                            }
                        }
                    },
                    Some(_) => rsx! {
                        div {
                            class: "tooltip tooltip-open tooltip-success",
                            "data-tip": "success",
                            style: "margin: auto",
                                button {
                                class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                onclick: move |_| {
                                    stop_action.call();
                                },
                                "Stop"
                        }
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
        } else {
            rsx! {
                button {
                    class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                    style: "margin: auto",
                    onclick: move |_| {
                        stop_action.call();
                        has_clicked.set(true);
                    },
                    "Stop"
                }
            }
        }
    } else {
        rsx! {
            div {
                class: "flex justify-center items-center h-full",
                "You do not have permission to stop the Valheim server."
            }
        }
    }
}

#[component]
fn start_button(is_permitted: bool) -> Element {
    let mut start_action = use_action(move || async move { start_ark().await });
    let mut has_clicked = use_signal(|| false);

    if is_permitted {
        if has_clicked() {
            rsx! {
            match &start_action.value() {
                Some(Err(_)) => rsx! {
                    div {
                        class: "tooltip tooltip-open tooltip-error",
                        "data-tip": "error",
                        style: "margin: auto",
                        button {
                            class: "btn btn-error btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                            onclick: move |_| {
                                start_action.call();
                            },
                            "Error!"
                        }
                    }
                },
                Some(_) => rsx! {
                    div {
                        class: "tooltip tooltip-open tooltip-success",
                        "data-tip": "success",
                        style: "margin: auto",
                            button {
                            class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                            onclick: move |_| {
                                start_action.call();
                            },
                            "Start"
                    }
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
        } else {
            rsx! {
                button {
                    class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                    style: "margin: auto",
                    onclick: move |_| {
                        start_action.call();
                        has_clicked.set(true);
                    },
                    "Start"
                }
            }
        }
    } else {
        rsx! {
            div {
                class: "flex justify-center items-center h-full",
                "You do not have permission to start the Valheim server."
            }
        }
    }
}
