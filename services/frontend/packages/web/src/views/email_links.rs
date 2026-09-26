//! Pages for account emails: asking for a password reset, and the pages the emailed
//! links open (confirm email, choose a new password, delete the account).
//! Server side: `crate::emails`.

use std::collections::HashMap;

use dioxus::prelude::*;
use ui::data_dir::LoginStatus;

use crate::emails::{
    account_to_delete, confirm_email, delete_account, forgot_password, reset_password, server_message, Deletion,
};
use crate::{ACCOUNT, LOGIN_STATUS, PERMISSIONS};

/// Centered card, like the login page.
#[component]
fn Panel(title: String, children: Element) -> Element {
    rsx! {
        div { class: "min-h-[calc(100vh-5rem)] flex items-center justify-center px-4 py-10",
            div { class: "bg-base-200 p-8 rounded-lg shadow-lg max-w-md w-full flex flex-col gap-4",
                h2 { class: "text-2xl font-bold text-center", "{title}" }
                {children}
            }
        }
    }
}

/// This browser was logged out on the server (password reset, account deleted).
fn forget_login() {
    *LOGIN_STATUS.write() = LoginStatus::LoggedOut;
    *PERMISSIONS.write() = HashMap::new();
    *ACCOUNT.write() = None;
}

const MISSING_CODE: &str = "This link is incomplete. Open it straight from the email, or copy the whole link.";

// ---- Forgot password ----

#[component]
pub fn ForgotPassword() -> Element {
    let mut login = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut sent = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);

    let submit = move |evt: FormEvent| {
        evt.prevent_default();
        spawn(async move {
            busy.set(true);
            error.set(None);
            match forgot_password(login()).await {
                Ok(()) => sent.set(true),
                Err(e) => error.set(Some(server_message(e))),
            }
            busy.set(false);
        });
    };

    rsx! {
        Panel { title: "Forgot your password?",
            if sent() {
                div { class: "alert alert-success",
                    span {
                        "If that account has an email address, we've sent it a link to choose a new password. "
                        "It works for 1 hour. Check your spam folder too."
                    }
                }
                Link { class: "btn btn-ghost", to: "/login", "Back to log in" }
            } else {
                p { class: "text-sm opacity-80",
                    "Enter your username or email, and we'll email you a link to choose a new password."
                }
                form { class: "flex flex-col gap-4", onsubmit: submit,
                    label { class: "sr-only", r#for: "login", "Username or email" }
                    input {
                        id: "login",
                        r#type: "text",
                        name: "login",
                        autocomplete: "username",
                        placeholder: "Username or email",
                        class: "input input-bordered w-full",
                        required: true,
                        value: "{login}",
                        oninput: move |evt| login.set(evt.value()),
                    }
                    button { r#type: "submit", class: "btn btn-primary w-full", disabled: busy(), "Email me a link" }
                }
                if let Some(e) = error() {
                    div { class: "alert alert-error", span { "{e}" } }
                }
                p { class: "text-xs opacity-60",
                    "Log in with GitHub or Google? Then you have no password here: use that button on the log in page."
                }
                Link { class: "link text-sm text-center", to: "/login", "Back to log in" }
            }
        }
    }
}

// ---- Reset password (emailed link) ----

#[component]
pub fn ResetPassword(code: String) -> Element {
    let mut password = use_signal(String::new);
    let mut again = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut done = use_signal(|| Option::<String>::None);
    let mut error = use_signal(|| Option::<String>::None);

    if code.is_empty() {
        return rsx! { Panel { title: "Choose a new password", p { "{MISSING_CODE}" } } };
    }

    let submit = move |evt: FormEvent| {
        evt.prevent_default();
        let code = code.clone();
        spawn(async move {
            if password().chars().count() < 8 {
                error.set(Some("Use at least 8 characters.".into()));
                return;
            }
            if password() != again() {
                error.set(Some("The two passwords are different.".into()));
                return;
            }
            busy.set(true);
            error.set(None);
            match reset_password(code, password()).await {
                Ok(d) => {
                    if d.logged_out {
                        forget_login();
                    }
                    done.set(Some(d.username));
                }
                Err(e) => error.set(Some(server_message(e))),
            }
            busy.set(false);
        });
    };

    rsx! {
        Panel { title: "Choose a new password",
            if let Some(username) = done() {
                div { class: "alert alert-success",
                    span { "Your password has been changed, and you've been logged out everywhere. Log in as "
                        span { class: "font-semibold", "{username}" }
                        " with your new password."
                    }
                }
                Link { class: "btn btn-primary", to: "/login", "Log in" }
            } else {
                form { class: "flex flex-col gap-4", onsubmit: submit,
                    div {
                        label { class: "block mb-2 text-sm font-medium", r#for: "new-password", "New password" }
                        input {
                            id: "new-password",
                            r#type: "password",
                            autocomplete: "new-password",
                            class: "input input-bordered w-full",
                            minlength: 8,
                            required: true,
                            value: "{password}",
                            oninput: move |evt| password.set(evt.value()),
                        }
                        p { class: "mt-1 text-xs opacity-60", "At least 8 characters." }
                    }
                    div {
                        label { class: "block mb-2 text-sm font-medium", r#for: "new-password-again", "Type it again" }
                        input {
                            id: "new-password-again",
                            r#type: "password",
                            autocomplete: "new-password",
                            class: "input input-bordered w-full",
                            required: true,
                            value: "{again}",
                            oninput: move |evt| again.set(evt.value()),
                        }
                    }
                    button { r#type: "submit", class: "btn btn-primary w-full", disabled: busy(), "Change password" }
                }
                if let Some(e) = error() {
                    div { class: "alert alert-error", span { "{e}" } }
                    if e.contains("expired") {
                        Link { class: "link text-sm text-center", to: "/forgot-password", "Get a new link" }
                    }
                }
            }
        }
    }
}

// ---- Confirm email (emailed link) ----

#[derive(Clone, PartialEq)]
enum Confirming {
    Working,
    Done(String),
    Failed(String),
}

#[component]
pub fn VerifyEmail(code: String) -> Element {
    let mut state = use_signal(|| Confirming::Working);

    // In the browser only: opening the link confirms (no button needed, since a link
    // checker opening it too would do no harm).
    use_effect(move || {
        let code = code.clone();
        spawn(async move {
            if code.is_empty() {
                state.set(Confirming::Failed(MISSING_CODE.into()));
                return;
            }
            match confirm_email(code).await {
                Ok(username) => {
                    if let Some(a) = ACCOUNT.write().as_mut().filter(|a| a.username == username) {
                        a.email_verified = true;
                    }
                    state.set(Confirming::Done(username));
                }
                Err(e) => state.set(Confirming::Failed(server_message(e))),
            }
        });
    });

    rsx! {
        Panel { title: "Confirm your email",
            match state() {
                Confirming::Working => rsx! {
                    div { class: "flex justify-center", span { class: "loading loading-spinner loading-md" } }
                },
                Confirming::Done(username) => rsx! {
                    div { class: "alert alert-success",
                        span { "Thanks! The email for "
                            span { class: "font-semibold", "{username}" }
                            " is confirmed."
                        }
                    }
                    Link { class: "btn btn-primary", to: "/", "Go to the site" }
                },
                Confirming::Failed(message) => rsx! {
                    div { class: "alert alert-warning", span { "{message}" } }
                    p { class: "text-sm opacity-80",
                        "Your profile page shows whether your email is confirmed, and can send a new link."
                    }
                    Link { class: "btn btn-ghost", to: "/profile", "Open my profile" }
                },
            }
        }
    }
}

// ---- Delete account (emailed link) ----

#[component]
pub fn DeleteAccount(code: String) -> Element {
    let mut account = use_signal(|| Option::<Result<Deletion, String>>::None);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);

    let check_code = code.clone();
    use_effect(move || {
        let code = check_code.clone();
        spawn(async move {
            if code.is_empty() {
                account.set(Some(Err(MISSING_CODE.into())));
                return;
            }
            account.set(Some(account_to_delete(code).await.map_err(server_message)));
        });
    });

    let delete = move |_| {
        let code = code.clone();
        spawn(async move {
            busy.set(true);
            error.set(None);
            match delete_account(code).await {
                Ok(d) => {
                    if d.logged_out {
                        forget_login();
                    }
                    navigator().replace("/account-deleted");
                }
                Err(e) => error.set(Some(server_message(e))),
            }
            busy.set(false);
        });
    };

    rsx! {
        Panel { title: "Delete your account",
            match account() {
                None => rsx! {
                    div { class: "flex justify-center", span { class: "loading loading-spinner loading-md" } }
                },
                Some(Err(message)) => rsx! {
                    div { class: "alert alert-warning", span { "{message}" } }
                    p { class: "text-sm opacity-80", "You can ask for a new link on your profile page." }
                    Link { class: "btn btn-ghost", to: "/profile", "Open my profile" }
                },
                Some(Ok(d)) => rsx! {
                    p {
                        "This permanently deletes the account "
                        span { class: "font-semibold", "{d.username}" }
                        ": your profile, your profile picture and any dice pictures you shared. "
                        "It can't be undone."
                    }
                    div { class: "flex flex-col gap-2 sm:flex-row-reverse",
                        button { class: "btn btn-error sm:flex-1", disabled: busy(), onclick: delete,
                            if busy() { span { class: "loading loading-spinner loading-sm" } }
                            "Delete my account"
                        }
                        Link { class: "btn btn-ghost sm:flex-1", to: "/", "Keep my account" }
                    }
                    if let Some(e) = error() {
                        div { class: "alert alert-error", span { "{e}" } }
                    }
                },
            }
        }
    }
}

#[component]
pub fn AccountDeleted() -> Element {
    rsx! {
        Panel { title: "Account deleted",
            p { class: "text-center", "Your account and its data are gone. Thanks for stopping by." }
            Link { class: "btn btn-ghost", to: "/", "Back to the start page" }
        }
    }
}
