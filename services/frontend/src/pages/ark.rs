use std::ops::Deref;

use dioxus::core::use_before_render;
use dioxus::prelude::*;
use dioxus_html::u::rest;
use serde::ser::Impossible;

use crate::hooks::{
    has_permission, num_players_ark, restart_ark, start_ark, stop_ark, CommandResult,
};
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
    let ark_permission = has_permission("ark").await;
    let llama_permission = has_permission("llama").await;

    return ark_permission || llama_permission;
}

#[component]
pub fn Restart_Ark_Page() -> Element {
    let is_permitted = use_resource(|| async move { has_ark_permission().await });

    match (is_permitted.value())() {
        Some(is_permitted) => {
            rsx! {
                div{
                    class: "flex flex-col items-center justify-center h-screen",
                    num_players { is_permitted }
                    div {
                        class: "flex items-center justify-center gap-40",
                        restart_button { is_permitted }
                        stop_button { is_permitted }
                        start_button { is_permitted }

                    }
                }
            }
        }
        None => {
            rsx! {
                button {
                    class: "btn",
                    class: "content-center",
                    style: "margin: auto",
                    span { class: "loading loading-spinner", "loading" }
                }
            }
        }
    }
}

#[component]
fn num_players(is_permitted: bool) -> Element {
    let mut count = use_signal(|| None::<i32>);

    use_future(move || async move {
        loop {
            let n = match num_players_ark().await {
                Ok(v) => match v.command_result {
                    Some(CommandResult::NumPlayers(n)) => n,
                    _ => -1,
                },
                Err(_) => -1,
            };
            count.set(Some(n));

            gloo::timers::future::TimeoutFuture::new(30_000).await;
        }
    });

    rsx! {
        div {
            class: "stats shadow-xl bg-base-300 mb-16",
            div {
                class: "stat",
                div { class: "stat-title", "Players Online" }
                div {
                    class: "stat-value",
                    match count() {
                        None => rsx! { span { class: "loading loading-spinner loading-sm" } },
                        Some(n) if n < 0 => rsx! { span { class: "text-error text-2xl", "Offline" } },
                        Some(0) => rsx! { span { class: "text-warning", "0" } },
                        Some(n) => rsx! { span { class: "text-success", "{n}" } },
                    }
                }
                div {
                    class: "stat-desc",
                    match count() {
                        Some(n) if n > 0 => rsx! { "Server is active" },
                        Some(0) => rsx! { "Server running, no players" },
                        _ => rsx! { "" },
                    }
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
                                class: "btn btn-error btn-xs sm:btn-sm md:btn-md lflex items-center justify-center h-screeng:btn-lg",
                                onclick: move |_| {
                                    restart_action.call();
                                },
                                "Error!"
                            }
                        }
                    },
                    Some(Ok(v)) => rsx! {
                        match &(v.read().command_result) {
                            Some(ice) => rsx! {
                                match &ice {
                                    CommandResult::Restarting => {
                                        dbg!(&ice);
                                        rsx! {
                                            div {
                                                class: "tooltip tooltip-open tooltip-success",
                                                "data-tip": "Success",
                                                style: "margin: auto",
                                                button {
                                                class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                onclick: move |_| {
                                                    restart_action.call();
                                                },
                                                "Restart"
                                            }
                                        }
                                    }
                                    },
                                    CommandResult::AlreadyStopped => {
                                        rsx! {
                                            div {
                                                class: "tooltip tooltip-open tooltip-success",
                                                "data-tip": "Already Stopped",
                                                style: "margin: auto",
                                                button {
                                                class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                onclick: move |_| {
                                                    restart_action.call();
                                                },
                                                "Restart"
                                            }
                                        }
                                    }
                                    },
                                    CommandResult::Restarting | CommandResult::FailedToStop | CommandResult::Stopped | CommandResult::Timeout | CommandResult::Started | CommandResult::AlreadyRunning | CommandResult::FailedToStart | CommandResult::NumPlayers(_) => {
                                        rsx!{
                                            div {
                                                class: "tooltip tooltip-open tooltip-error",
                                                "data-tip": "FailedToStop",
                                                style: "margin: auto",
                                                button {
                                                    class: "btn btn-error btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                    onclick: move |_| {
                                                        restart_action.call();
                                                    },
                                                    "Error!"
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            None =>
                                rsx! {
                                    div {
                                        class: "tooltip tooltip-open tooltip-success",
                                        "data-tip": "Already Stopped",
                                        style: "margin: auto",
                                        button {
                                        class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                        onclick: move |_| {
                                            restart_action.call();
                                        },
                                        "Restart"
                                    }
                                }
                            }
                        }
                    },
                    None => load_btn()
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
                "You do not have permission to restart the Ark server."
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
                    Some(Ok(v)) => rsx! {
                        match &(v.read().command_result) {
                            Some(ice) => rsx! {
                                match &ice {
                                    CommandResult::Stopped => {
                                        dbg!(&ice);
                                        rsx! {
                                            div {
                                                class: "tooltip tooltip-open tooltip-success",
                                                "data-tip": "Success",
                                                style: "margin: auto",
                                                button {
                                                class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                onclick: move |_| {
                                                    stop_action.call();
                                                },
                                                "Stop"
                                            }
                                        }
                                    }
                                    },
                                    CommandResult::AlreadyStopped => {
                                        rsx! {
                                            div {
                                                class: "tooltip tooltip-open tooltip-success",
                                                "data-tip": "Already Stopped",
                                                style: "margin: auto",
                                                button {
                                                class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                onclick: move |_| {
                                                    stop_action.call();
                                                },
                                                "Stop"
                                            }
                                        }
                                    }
                                    },
                                    CommandResult::Restarting | CommandResult::FailedToStop | CommandResult::Restarting | CommandResult::Timeout | CommandResult::Started | CommandResult::AlreadyRunning | CommandResult::FailedToStart | CommandResult::NumPlayers(_)=> {
                                        rsx!{
                                            div {
                                                class: "tooltip tooltip-open tooltip-error",
                                                "data-tip": "FailedToStop",
                                                style: "margin: auto",
                                                button {
                                                    class: "btn btn-error btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                    onclick: move |_| {
                                                        stop_action.call();
                                                    },
                                                    "Error!"
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            None => load_btn()

                        }
                    },
                    None => load_btn()
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
                "You do not have permission to stop the Ark server."
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
                    Some(Ok(v)) => rsx! {
                        match &(v.read().command_result) {
                            Some(ice) => rsx! {
                                match &ice {
                                    CommandResult::Started => {
                                        dbg!(&ice);
                                        rsx! {
                                            div {
                                                class: "tooltip tooltip-open tooltip-success",
                                                "data-tip": "Success",
                                                style: "margin: auto",
                                                button {
                                                class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                onclick: move |_| {
                                                    start_action.call();
                                                },
                                                "Start"
                                            }
                                        }
                                    }
                                    },
                                    CommandResult::AlreadyRunning => {
                                        rsx! {
                                            div {
                                                class: "tooltip tooltip-open tooltip-success",
                                                "data-tip": "Already Running",
                                                style: "margin: auto",
                                                button {
                                                class: "btn btn-primary btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                onclick: move |_| {
                                                    start_action.call();
                                                },
                                                "Start"
                                            }
                                        }
                                    }
                                    },
                                    CommandResult::Restarting | CommandResult::FailedToStop | CommandResult::Restarting | CommandResult::Timeout | CommandResult::Stopped | CommandResult::FailedToStart | CommandResult::AlreadyStopped | CommandResult::NumPlayers(_)=> {
                                        rsx!{
                                            div {
                                                class: "tooltip tooltip-open tooltip-error",
                                                "data-tip": "FailedToStop",
                                                style: "margin: auto",
                                                button {
                                                class: "btn btn-error btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                                                onclick: move |_| {
                                                    start_action.call();
                                                },
                                                "Error!"
                                            }
                                        }
                                    }
                                    }
                                }
                            },
                            None => load_btn()

                        }
                    },
                    None => load_btn()
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
                "You do not have permission to start the Ark server."
            }
        }
    }
}

#[component]
fn load_btn() -> Element {
    rsx! {
        button {
            class: "btn",
            style: "margin: auto",
            span { class: "loading loading-spinner", "loading" }
        }
    }
}
