use dioxus::prelude::*;

use ui::{data_dir::Theme, default_profile_picture, get_mode, set_mode};

use crate::sharing::{delete_my_dataset, get_dataset_sharing, set_dataset_sharing, SharingState};
use crate::{LOGIN_STATUS, PERMISSIONS};
use ui::data_dir::LoginStatus;

#[component]
pub fn Profile() -> Element {
    match LOGIN_STATUS() {
        LoginStatus::LoggedOut => rsx! {
            div { class: "flex h-screen items-center justify-center",
                div {
                    p { "Please log in to view your profile." }
                    Link { class: "btn btn-primary mt-4", to: "/login", "Log In" }
                }
            }
        },
        LoginStatus::LoggedIn(_) => rsx! { ProfileForm {} },
    }
}

#[component]
fn ProfileForm() -> Element {
    rsx! {
        div { class: "container mx-auto mt-10 px-4",
            div { class: "bg-base-200 p-10 rounded-lg shadow-lg max-w-4xl mx-auto",
                h1 { class: "text-2xl font-bold mb-10", "Profile Account" }
                div { class: "grid grid-cols-1 md:grid-cols-2 gap-x-10 gap-y-6",
                    div { class: "mb-4",
                        label { class: "block text-sm font-bold mb-2", "Profile Photo" }
                        div { class: "w-24 h-24 mb-4",
                            default_profile_picture { width: 96, height: 96 }
                        }
                        input { r#type: "file", class: "file-input file-input-primary w-full max-w-xs" }
                    }
                    div { class: "mb-4",
                        label { class: "block text-sm font-bold mb-2", "Username" }
                        input {
                            r#type: "text",
                            placeholder: "Username",
                            class: "input input-primary w-full max-w-xs"
                        }
                    }
                    div { class: "mb-4",
                        label { class: "block text-sm font-bold mb-2", "Email" }
                        input {
                            r#type: "email",
                            placeholder: "Email",
                            class: "input input-primary w-full max-w-xs"
                        }
                    }
                    div { class: "mb-4",
                        label { class: "block text-sm font-bold mb-2", "Site Theme" }
                        select {
                            class: "select select-primary w-full max-w-xs",
                            onchange: move |evt: Event<FormData>| {
                                set_mode(Theme::from_str_theme(&evt.value()));
                            },
                            value: get_mode().to_string(),
                            for theme in Theme::all() {
                                option { value: theme.to_string(), "{theme}" }
                            }
                        }
                    }
                    div { class: "mb-4",
                        label { class: "block text-sm font-bold mb-2", "Full Name" }
                        input {
                            r#type: "text",
                            placeholder: "Full name",
                            class: "input input-primary w-full max-w-xs"
                        }
                    }
                    div { class: "mb-4",
                        label { class: "block text-sm font-bold mb-2", "Language" }
                        select { class: "select select-primary w-full max-w-xs",
                            option { "English" }
                            option { "Spanish" }
                        }
                    }
                }
                div { class: "flex justify-end mt-10",
                    button { class: "btn bg-purple-500 hover:bg-purple-700 text-white", "Update" }
                }
                if PERMISSIONS.read().contains_key("arcane") {
                    DiceSharing {}
                }
                div { class: "mt-10",
                    h2 { class: "text-xl font-bold mb-2", "Delete Account" }
                    div { class: "mb-4",
                        input {
                            r#type: "email",
                            placeholder: "Confirm your Email",
                            class: "input input-primary w-full max-w-xs"
                        }
                    }
                    button { class: "btn bg-red-500 hover:bg-red-700 text-white", "Delete Account" }
                }
            }
        }
    }
}

/// Opt-in to share roll pictures for training the dice reader, and a way to take
/// everything back. Only shown to users who can use the dice roller.
#[component]
fn DiceSharing() -> Element {
    let mut state = use_signal(|| Option::<SharingState>::None);
    // (is_error, text) under the card.
    let mut message = use_signal(|| Option::<(bool, String)>::None);
    let mut busy = use_signal(|| false);
    let mut confirm_delete = use_signal(|| false);
    // Bumped when saving fails, to rebuild the switch so it shows the real setting.
    let mut switch_key = use_signal(|| 0u32);

    use_effect(move || {
        spawn(async move {
            match get_dataset_sharing().await {
                Ok(s) => state.set(Some(s)),
                Err(_) => message.set(Some((true, "Couldn't load your sharing setting.".into()))),
            }
        });
    });

    let Some(current) = state() else {
        return rsx! {
            if let Some((_, m)) = message() { p { class: "mt-10 text-error", "{m}" } }
        };
    };
    if !current.available {
        return rsx! {};
    }

    rsx! {
        section { class: "mt-10 rounded-box border border-base-300 bg-base-100 overflow-hidden",
            div { class: "flex flex-col gap-4 p-6 sm:flex-row sm:items-start sm:justify-between sm:gap-6",
                div {
                    h2 { class: "text-lg font-bold", "Help improve dice recognition" }
                    p { class: "mt-1 text-sm opacity-70",
                        "Share pictures of your rolls so the dice reader gets better at reading them."
                    }
                }
                label { class: "flex shrink-0 cursor-pointer items-center gap-3",
                    span { class: "text-sm font-medium opacity-70",
                        if current.share { "On" } else { "Off" }
                    }
                    input {
                        key: "{switch_key}",
                        r#type: "checkbox",
                        class: "toggle toggle-primary",
                        aria_label: "Share pictures of my rolls",
                        checked: current.share,
                        disabled: busy(),
                        onchange: move |evt: Event<FormData>| {
                            let share = evt.checked();
                            spawn(async move {
                                busy.set(true);
                                match set_dataset_sharing(share).await {
                                    Ok(s) => {
                                        state.set(Some(s));
                                        message.set(None);
                                    }
                                    Err(_) => {
                                        message.set(Some((true, "Couldn't save your choice. Try again.".into())));
                                        switch_key += 1;
                                    }
                                }
                                busy.set(false);
                            });
                        },
                    }
                }
            }
            dl { class: "grid gap-5 border-t border-base-300 px-6 py-5 sm:grid-cols-3",
                div {
                    dt { class: "text-xs font-semibold uppercase tracking-wide opacity-60", "What's saved" }
                    dd { class: "mt-1 text-sm", "The camera picture and what the model read." }
                }
                div {
                    dt { class: "text-xs font-semibold uppercase tracking-wide opacity-60", "Which rolls" }
                    dd { class: "mt-1 text-sm", "Rolls the model was unsure about, and about 1 in 20 others." }
                }
                div {
                    dt { class: "text-xs font-semibold uppercase tracking-wide opacity-60", "Who sees them" }
                    dd { class: "mt-1 text-sm",
                        "Only the site owner, to check the numbers. Used only to train the dice reader."
                    }
                }
            }
            if confirm_delete() {
                div { class: "flex flex-wrap items-center justify-between gap-3 border-t border-error/40 bg-error/10 px-6 py-4",
                    p { class: "text-sm",
                        "Delete every roll picture you've shared or flagged? This also turns sharing off."
                    }
                    div { class: "flex gap-2",
                        button {
                            class: "btn btn-ghost btn-sm",
                            onclick: move |_| confirm_delete.set(false),
                            "Cancel"
                        }
                        button {
                            class: "btn btn-error btn-sm",
                            disabled: busy(),
                            onclick: move |_| {
                                spawn(async move {
                                    busy.set(true);
                                    match delete_my_dataset().await {
                                        Ok(n) => {
                                            state.set(Some(SharingState { available: true, share: false }));
                                            message.set(Some((false, match n {
                                                0 => "Done. There was nothing to delete.".to_string(),
                                                1 => "Deleted 1 roll.".to_string(),
                                                n => format!("Deleted {n} rolls."),
                                            })));
                                        }
                                        Err(_) => message.set(Some((true, "Couldn't delete right now. Try again.".into()))),
                                    }
                                    confirm_delete.set(false);
                                    busy.set(false);
                                });
                            },
                            "Delete everything"
                        }
                    }
                }
            } else {
                div { class: "flex flex-wrap items-center justify-between gap-3 border-t border-base-300 bg-base-200/50 px-6 py-4",
                    p { class: "max-w-lg text-xs opacity-60",
                        "Turning this off stops new saves. Rolls you flag as wrong in the browser extension "
                        "are saved either way; your latest roll's picture is kept for 10 minutes so it can be flagged."
                    }
                    button {
                        class: "btn btn-ghost btn-sm text-error",
                        onclick: move |_| confirm_delete.set(true),
                        "Delete my shared pictures"
                    }
                }
            }
            if let Some((is_error, m)) = message() {
                p {
                    class: if is_error { "border-t border-base-300 px-6 py-3 text-sm text-error" } else { "border-t border-base-300 px-6 py-3 text-sm text-success" },
                    "{m}"
                }
            }
        }
    }
}
