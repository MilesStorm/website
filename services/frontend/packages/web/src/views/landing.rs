use dioxus::prelude::*;

use crate::LOGIN_STATUS;
use ui::data_dir::LoginStatus;

#[component]
pub fn Landing() -> Element {
    rsx! {
        div { class: "container mx-auto mt-10 px-4",
            match LOGIN_STATUS() {
                LoginStatus::LoggedOut => rsx! {
                    p { class: "text-lg", "Login to see your profile." }
                    p { class: "text-sm text-base-content/60 mt-2",
                        "Features are limited to invited users while the site is in early access."
                    }
                },
                LoginStatus::LoggedIn(username) => rsx! {
                    h1 { class: "text-3xl font-bold mb-4", "Welcome, {username}!" }
                    p { class: "text-base-content/70",
                        "Use the navbar to navigate to your available features."
                    }
                }
            }
        }
    }
}
