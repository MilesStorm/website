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
    let mut message = use_signal(|| Option::<String>::None);
    let mut busy = use_signal(|| false);
    let mut confirm_delete = use_signal(|| false);

    use_effect(move || {
        spawn(async move {
            match get_dataset_sharing().await {
                Ok(s) => state.set(Some(s)),
                Err(_) => message.set(Some("Couldn't load your sharing setting.".into())),
            }
        });
    });

    let Some(current) = state() else {
        return rsx! {
            if let Some(m) = message() { p { class: "mt-10 text-error", "{m}" } }
        };
    };
    if !current.available {
        return rsx! {};
    }

    rsx! {
        div { class: "mt-10",
            h2 { class: "text-xl font-bold mb-2", "Help improve dice recognition" }
            p { class: "mb-2 max-w-2xl",
                "Share pictures of your rolls so the dice reader can learn from them. When this is on, "
                "the camera picture and what the model read are saved for rolls it was unsure about, "
                "and for about 1 in 20 other rolls. Pictures are only used to train the dice model and "
                "are only looked at by the site owner when checking the right numbers. You can switch "
                "this off at any time; nothing new is saved after that."
            }
            p { class: "mb-4 max-w-2xl text-sm opacity-70",
                "Rolls you send with \"Flag as wrong roll\" in the browser extension are saved even when this is off."
            }
            label { class: "label cursor-pointer justify-start gap-3",
                input {
                    r#type: "checkbox",
                    class: "toggle toggle-primary",
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
                                Err(_) => message.set(Some("Couldn't save your choice. Try again.".into())),
                            }
                            busy.set(false);
                        });
                    },
                }
                span { "Share pictures of my rolls" }
            }
            div { class: "mt-4",
                if confirm_delete() {
                    p { class: "mb-2",
                        "This deletes every roll picture you've shared or flagged, and turns sharing off."
                    }
                    button {
                        class: "btn bg-red-500 hover:bg-red-700 text-white mr-2",
                        disabled: busy(),
                        onclick: move |_| {
                            spawn(async move {
                                busy.set(true);
                                match delete_my_dataset().await {
                                    Ok(n) => {
                                        state.set(Some(SharingState { available: true, share: false }));
                                        message.set(Some(match n {
                                            0 => "Done. There was nothing to delete.".to_string(),
                                            1 => "Deleted 1 roll.".to_string(),
                                            n => format!("Deleted {n} rolls."),
                                        }));
                                    }
                                    Err(_) => message.set(Some("Couldn't delete right now. Try again.".into())),
                                }
                                confirm_delete.set(false);
                                busy.set(false);
                            });
                        },
                        "Yes, delete everything"
                    }
                    button { class: "btn", onclick: move |_| confirm_delete.set(false), "Cancel" }
                } else {
                    button {
                        class: "btn btn-outline",
                        onclick: move |_| confirm_delete.set(true),
                        "Delete all pictures I've shared"
                    }
                }
            }
            if let Some(m) = message() {
                p { class: "mt-2", "{m}" }
            }
        }
    }
}
