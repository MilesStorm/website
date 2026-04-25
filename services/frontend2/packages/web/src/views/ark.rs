use dioxus::prelude::*;

use api::{ark_command, ark_player_count};
use ui::data_dir::CommandResult;

use crate::{LOGIN_STATUS, PERMISSIONS};
use ui::data_dir::LoginStatus;

#[component]
pub fn Ark() -> Element {
    let has_perm = PERMISSIONS.read().contains_key("llama");

    match LOGIN_STATUS() {
        LoginStatus::LoggedOut => rsx! {
            div { class: "flex h-screen items-center justify-center",
                p { "Please log in to access the Ark panel." }
            }
        },
        LoginStatus::LoggedIn(_) if !has_perm => rsx! {
            div { class: "flex h-screen items-center justify-center",
                p { "You do not have permission to access the Ark panel." }
            }
        },
        LoginStatus::LoggedIn(_) => rsx! { ArkPanel {} },
    }
}

#[component]
fn ArkPanel() -> Element {
    rsx! {
        div { class: "flex flex-col items-center justify-center min-h-[calc(100vh-5rem)] gap-12",
            PlayerCount {}
            div { class: "flex items-center gap-10",
                ArkButton { cmd: "restart".to_string(), label: "Restart" }
                ArkButton { cmd: "stop".to_string(), label: "Stop" }
                ArkButton { cmd: "start".to_string(), label: "Start" }
            }
        }
    }
}

#[component]
fn PlayerCount() -> Element {
    let mut count = use_signal(|| None::<i32>);

    let refresh = use_resource(move || async move { ark_player_count().await });

    use_effect(move || {
        if let Some(Ok(n)) = refresh.value()() {
            count.set(Some(n));
        }
    });

    rsx! {
        div { class: "stats shadow-xl bg-base-300",
            div { class: "stat",
                div { class: "stat-title", "Players Online" }
                div { class: "stat-value",
                    match count() {
                        None => rsx! { span { class: "loading loading-spinner loading-sm" } },
                        Some(n) if n < 0 => rsx! { span { class: "text-error text-2xl", "Offline" } },
                        Some(0) => rsx! { span { class: "text-warning", "0" } },
                        Some(n) => rsx! { span { class: "text-success", "{n}" } },
                    }
                }
                div { class: "stat-desc",
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
fn ArkButton(cmd: String, label: String) -> Element {
    let mut result: Signal<Option<Result<CommandResult, ServerFnError>>> = use_signal(|| None);
    let mut loading = use_signal(|| false);

    let cmd_clone = cmd.clone();
    let run = move |_| {
        let c = cmd_clone.clone();
        spawn(async move {
            loading.set(true);
            result.set(Some(ark_command(c).await));
            loading.set(false);
        });
    };

    let tooltip = result().as_ref().map(|r| match r {
        Ok(cr) => cr.to_string(),
        Err(_) => "Error".to_string(),
    });

    let btn_class = match result().as_ref() {
        Some(Ok(_)) => "btn btn-success",
        Some(Err(_)) => "btn btn-error",
        None => "btn btn-primary",
    };

    rsx! {
        div {
            class: "tooltip",
            "data-tip": tooltip.unwrap_or_default(),
            button {
                class: "{btn_class} btn-xs sm:btn-sm md:btn-md lg:btn-lg",
                disabled: loading(),
                onclick: run,
                if loading() {
                    span { class: "loading loading-spinner loading-sm" }
                } else {
                    "{label}"
                }
            }
        }
    }
}
