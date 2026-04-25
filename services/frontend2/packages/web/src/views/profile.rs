use dioxus::prelude::*;

use ui::{data_dir::Theme, default_profile_picture, get_mode, set_mode};

use crate::LOGIN_STATUS;
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
