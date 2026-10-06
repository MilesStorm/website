//! The page an invite link opens (`/invite#…`). Logged in: joins right away.
//! Logged out: offers signing up or logging in, after which the site comes back
//! here and the user is in. Server side: `crate::invites`.

use dioxus::prelude::*;

use api::get_my_permissions;

use crate::emails::server_message;
use crate::invites::{after_join, group_name, open_invite, Opened};
use crate::PERMISSIONS;

#[component]
pub fn Invite(code: String) -> Element {
    let code = super::email_links::use_link_code(code);
    let mut state = use_signal(|| Option::<Result<Opened, String>>::None);

    // In the browser only, like the email links; again if the address's code changes.
    use_effect(move || {
        let Some(code) = code() else { return };
        state.set(None);
        spawn(async move {
            let opened = open_invite(code).await.map_err(server_message);
            if let Ok(Opened::Joined { .. }) = opened {
                // Show the new pages (e.g. Arcane in the menu) straight away.
                if let Ok(perms) = get_my_permissions().await {
                    *PERMISSIONS.write() = perms.into_iter().map(|n| (n, true)).collect();
                }
            }
            state.set(Some(opened));
        });
    });

    rsx! {
        // The address holds the invite code: don't send it along to other sites.
        document::Meta { name: "referrer", content: "no-referrer" }
        div { class: "min-h-[calc(100vh-5rem)] flex items-center justify-center px-4 py-10",
            div { class: "bg-base-200 p-8 rounded-lg shadow-lg max-w-md w-full flex flex-col gap-4",
                h2 { class: "text-2xl font-bold text-center",
                    match state() {
                        Some(Ok(Opened::Joined { .. })) => "Welcome!",
                        Some(Err(_)) => "Invite link",
                        _ => "You're invited",
                    }
                }
                match state() {
                    None => rsx! {
                        div { class: "flex justify-center", span { class: "loading loading-spinner loading-md" } }
                    },
                    Some(Err(message)) => rsx! {
                        div { class: "alert alert-warning", span { "{message}" } }
                        Link { class: "btn btn-ghost", to: "/", "Go to the site" }
                    },
                    Some(Ok(Opened::Joined { role })) => {
                        let (page, button) = after_join(&role);
                        rsx! {
                            div { class: "alert alert-success",
                                span { "You're in! You've joined {group_name(&role)}." }
                            }
                            Link { class: "btn btn-primary", to: page, "{button}" }
                        }
                    },
                    Some(Ok(Opened::NeedsAccount { role })) => rsx! {
                        p { class: "text-center",
                            "You've been invited to join "
                            span { class: "font-semibold", "{group_name(&role)}" }
                            "."
                        }
                        p { class: "text-sm opacity-80 text-center",
                            "Make an account, or log in if you have one. You'll join as soon as you're in."
                        }
                        Link { class: "btn btn-primary", to: "/register", "Sign up" }
                        Link { class: "btn btn-ghost", to: "/login", "I have an account: log in" }
                    },
                }
            }
        }
    }
}
