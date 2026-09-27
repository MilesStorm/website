use dioxus::prelude::*;

use ui::{data_dir::Theme, default_profile_picture, get_mode, set_mode};

use crate::account::{get_account, remove_profile_picture, set_account_display_name, AccountInfo, PICTURE_MAX_UPLOAD};
use crate::emails::{delete_account_without_email, request_account_deletion, send_confirmation_email, server_message};
use crate::sharing::{delete_my_dataset, get_dataset_sharing, set_dataset_sharing, SharingState};
use crate::{ACCOUNT, LOGIN_STATUS, PERMISSIONS};
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

/// Card used by every section of the page: title, one-line description, body.
const CARD: &str = "rounded-box border border-base-300 bg-base-100 overflow-hidden";

#[component]
fn ProfileForm() -> Element {
    let mut load_error = use_signal(|| false);
    // Fresh copy on every visit (the navbar's may be from login time).
    use_effect(move || {
        spawn(async move {
            match get_account().await {
                Ok(a) => *ACCOUNT.write() = Some(a),
                Err(_) => load_error.set(true),
            }
        });
    });

    rsx! {
        div { class: "container mx-auto mt-10 px-4 pb-16",
            div { class: "bg-base-200 p-6 sm:p-10 rounded-lg shadow-lg max-w-4xl mx-auto flex flex-col gap-8",
                h1 { class: "text-2xl font-bold", "Profile" }
                match ACCOUNT() {
                    Some(account) => rsx! {
                        AccountCard { account: account.clone() }
                        EmailCard { account }
                    },
                    None if load_error() => rsx! {
                        p { class: "text-error", "Your account can't be loaded right now. Try again later." }
                    },
                    None => rsx! { div { class: "{CARD} h-48 animate-pulse" } },
                }
                AppearanceCard {}
                if PERMISSIONS.read().contains_key("arcane") {
                    DiceSharing {}
                }
                if let Some(account) = ACCOUNT() {
                    DeleteCard { account }
                }
            }
        }
    }
}

/// Profile picture and display name.
#[component]
fn AccountCard(account: AccountInfo) -> Element {
    let mut name = use_signal(|| account.display_name.clone().unwrap_or_default());
    let mut busy = use_signal(|| false);
    // (is_error, text) for the name and the picture separately.
    let mut name_msg = use_signal(|| Option::<(bool, String)>::None);
    let mut picture_msg = use_signal(|| Option::<(bool, String)>::None);
    // Bumped after each upload so choosing the same file again still triggers.
    let mut file_key = use_signal(|| 0u32);

    let saved_name = account.display_name.clone().unwrap_or_default();
    let unchanged = name().trim() == saved_name;
    let username = account.username.clone();

    let save_name = move || {
        spawn(async move {
            busy.set(true);
            match set_account_display_name(name()).await {
                Ok(a) => {
                    name.set(a.display_name.clone().unwrap_or_default());
                    name_msg.set(Some((false, "Saved.".into())));
                    *ACCOUNT.write() = Some(a);
                }
                Err(e) => name_msg.set(Some((true, server_message(e)))),
            }
            busy.set(false);
        });
    };

    rsx! {
        section { class: CARD,
            div { class: "p-6",
                h2 { class: "text-lg font-bold", "Account" }
                p { class: "mt-1 text-sm opacity-70", "How you appear on this site." }
            }
            div { class: "grid gap-8 border-t border-base-300 p-6 sm:grid-cols-[auto_1fr]",
                // ---- Picture ----
                div { class: "flex flex-col items-center gap-3 sm:items-start",
                    div { class: "w-24 h-24 rounded-full overflow-hidden ring ring-primary ring-offset-base-100 ring-offset-2",
                        if let Some(src) = account.picture_url() {
                            img { src: "{src}", alt: "Your profile picture", width: 96, height: 96 }
                        } else {
                            default_profile_picture { width: 96, height: 96 }
                        }
                    }
                    if account.pictures_available {
                        div { class: "flex gap-2",
                            label { class: if busy() { "btn btn-sm btn-primary btn-disabled" } else { "btn btn-sm btn-primary" },
                                "Upload"
                                input {
                                    key: "{file_key}",
                                    r#type: "file",
                                    accept: "image/jpeg,image/png,image/webp,image/gif",
                                    class: "hidden",
                                    disabled: busy(),
                                    onchange: move |evt: Event<FormData>| {
                                        let file = evt.files().into_iter().next();
                                        spawn(async move {
                                            let Some(file) = file else { return };
                                            file_key += 1;
                                            if file.size() > PICTURE_MAX_UPLOAD as u64 {
                                                picture_msg.set(Some((true, "That picture is too large (at most 5 MB).".into())));
                                                return;
                                            }
                                            busy.set(true);
                                            picture_msg.set(None);
                                            let result = match file.read_bytes().await {
                                                Ok(bytes) => upload_picture(&bytes).await,
                                                Err(_) => Err("Couldn't read that file.".into()),
                                            };
                                            match result {
                                                Ok(version) => {
                                                    if let Some(a) = ACCOUNT.write().as_mut() {
                                                        a.picture_version = Some(version);
                                                    }
                                                    picture_msg.set(Some((false, "Picture updated.".into())));
                                                }
                                                Err(m) => picture_msg.set(Some((true, m))),
                                            }
                                            busy.set(false);
                                        });
                                    },
                                }
                            }
                            if account.picture_version.is_some() {
                                button {
                                    class: "btn btn-sm btn-ghost",
                                    disabled: busy(),
                                    onclick: move |_| {
                                        spawn(async move {
                                            busy.set(true);
                                            match remove_profile_picture().await {
                                                Ok(a) => {
                                                    *ACCOUNT.write() = Some(a);
                                                    picture_msg.set(Some((false, "Picture removed.".into())));
                                                }
                                                Err(e) => picture_msg.set(Some((true, server_message(e)))),
                                            }
                                            busy.set(false);
                                        });
                                    },
                                    "Remove"
                                }
                            }
                        }
                        p { class: "text-xs opacity-60", "JPEG, PNG, WebP or GIF, up to 5 MB." }
                    } else {
                        p { class: "text-xs opacity-60 max-w-40 text-center sm:text-left",
                            "Pictures can't be changed right now."
                        }
                    }
                    Message { msg: picture_msg() }
                }
                // ---- Name ----
                div { class: "flex flex-col gap-2",
                    label { class: "text-sm font-semibold", r#for: "display-name", "Display name" }
                    div { class: "flex flex-wrap gap-2",
                        input {
                            id: "display-name",
                            r#type: "text",
                            class: "input input-bordered w-full max-w-xs",
                            placeholder: "{username}",
                            maxlength: 40,
                            value: "{name}",
                            disabled: busy(),
                            oninput: move |evt| {
                                name.set(evt.value());
                                name_msg.set(None);
                            },
                            onkeydown: move |evt: KeyboardEvent| {
                                if evt.key() == Key::Enter && !busy() {
                                    save_name();
                                }
                            },
                        }
                        button {
                            class: "btn btn-primary",
                            disabled: busy() || unchanged,
                            onclick: move |_| save_name(),
                            "Save"
                        }
                    }
                    p { class: "text-xs opacity-60 max-w-md",
                        "Shown on the site instead of your username. Leave it empty to use your username, "
                        span { class: "font-semibold", "{username}" }
                        ", which you log in with and which can't be changed."
                    }
                    Message { msg: name_msg() }
                }
            }
        }
    }
}

/// The account's email address, and whether it's confirmed.
#[component]
fn EmailCard(account: AccountInfo) -> Element {
    let mut busy = use_signal(|| false);
    let mut msg = use_signal(|| Option::<(bool, String)>::None);

    rsx! {
        section { class: CARD,
            div { class: "flex flex-col gap-4 p-6 sm:flex-row sm:items-center sm:justify-between",
                div { class: "min-w-0",
                    h2 { class: "text-lg font-bold", "Email" }
                    match account.email.clone() {
                        Some(email) => rsx! {
                            p { class: "mt-1 flex flex-wrap items-center gap-2 text-sm",
                                span { class: "break-all", "{email}" }
                                if account.email_verified {
                                    span { class: "badge badge-success badge-sm", "Confirmed" }
                                } else {
                                    span { class: "badge badge-warning badge-sm", "Not confirmed" }
                                }
                            }
                            if !account.email_verified {
                                p { class: "mt-1 text-xs opacity-60 max-w-md",
                                    "Confirm it so we can help you if you forget your password."
                                }
                            }
                        },
                        None => rsx! {
                            p { class: "mt-1 text-sm opacity-70", "Your account has no email address: you log in with GitHub." }
                        },
                    }
                }
                if account.email.is_some() && !account.email_verified {
                    button {
                        class: "btn btn-sm btn-primary shrink-0",
                        disabled: busy(),
                        onclick: move |_| {
                            spawn(async move {
                                busy.set(true);
                                msg.set(Some(match send_confirmation_email().await {
                                    Ok(to) => (false, format!("Sent. Open the link in the email to {to}; check your spam folder too.")),
                                    Err(e) => (true, server_message(e)),
                                }));
                                busy.set(false);
                            });
                        },
                        "Send confirmation email"
                    }
                }
            }
            if msg().is_some() {
                div { class: "border-t border-base-300 px-6 py-3", Message { msg: msg() } }
            }
        }
    }
}

/// Deleting the account: by emailed link, or (without an email) by typing the username.
#[component]
fn DeleteCard(account: AccountInfo) -> Element {
    let mut open = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut typed = use_signal(String::new);
    let mut msg = use_signal(|| Option::<(bool, String)>::None);
    // A confirmed address gets a link; otherwise it might never arrive.
    let by_email = account.email.is_some() && account.email_verified;
    let unconfirmed = account.email.is_some() && !account.email_verified;
    let username = account.username.clone();
    let matches = typed().trim() == username;

    rsx! {
        section { class: "rounded-box border border-error/40 bg-base-100 overflow-hidden",
            div { class: "flex flex-col gap-4 p-6 sm:flex-row sm:items-center sm:justify-between",
                div {
                    h2 { class: "text-lg font-bold", "Delete account" }
                    p { class: "mt-1 text-sm opacity-70 max-w-md",
                        "Removes your account, profile picture and any dice pictures you shared. This can't be undone."
                    }
                }
                if !open() {
                    button {
                        class: "btn btn-sm btn-outline btn-error shrink-0",
                        onclick: move |_| open.set(true),
                        "Delete my account…"
                    }
                }
            }
            if open() {
                div { class: "flex flex-col gap-3 border-t border-error/40 bg-error/10 px-6 py-4",
                    if by_email {
                        p { class: "text-sm",
                            "We'll email you a link to confirm. Nothing is deleted until you open it and confirm once more."
                        }
                        div { class: "flex flex-wrap gap-2",
                            button { class: "btn btn-ghost btn-sm", onclick: move |_| { open.set(false); msg.set(None); }, "Cancel" }
                            button {
                                class: "btn btn-error btn-sm",
                                disabled: busy(),
                                onclick: move |_| {
                                    spawn(async move {
                                        busy.set(true);
                                        msg.set(Some(match request_account_deletion().await {
                                            Ok(to) => (false, format!("Sent to {to}. The link works for 1 hour.")),
                                            Err(e) => (true, server_message(e)),
                                        }));
                                        busy.set(false);
                                    });
                                },
                                "Email me the link"
                            }
                        }
                    } else {
                        label { class: "text-sm", r#for: "delete-confirm",
                            if unconfirmed { "Your email isn't confirmed, so there's no link to wait for. " }
                            "Type your username, "
                            span { class: "font-semibold", "{username}" }
                            ", to confirm."
                        }
                        div { class: "flex flex-wrap gap-2",
                            input {
                                id: "delete-confirm",
                                r#type: "text",
                                autocomplete: "off",
                                class: "input input-bordered input-sm w-full max-w-xs",
                                value: "{typed}",
                                oninput: move |evt| typed.set(evt.value()),
                            }
                            button { class: "btn btn-ghost btn-sm", onclick: move |_| { open.set(false); typed.set(String::new()); msg.set(None); }, "Cancel" }
                            button {
                                class: "btn btn-error btn-sm",
                                disabled: busy() || !matches,
                                onclick: move |_| {
                                    spawn(async move {
                                        busy.set(true);
                                        match delete_account_without_email(typed()).await {
                                            Ok(_) => {
                                                *LOGIN_STATUS.write() = LoginStatus::LoggedOut;
                                                *PERMISSIONS.write() = Default::default();
                                                *ACCOUNT.write() = None;
                                                navigator().replace("/account-deleted");
                                            }
                                            Err(e) => msg.set(Some((true, server_message(e)))),
                                        }
                                        busy.set(false);
                                    });
                                },
                                "Delete permanently"
                            }
                        }
                    }
                    Message { msg: msg() }
                }
            }
        }
    }
}

/// Site theme, kept in this browser.
#[component]
fn AppearanceCard() -> Element {
    rsx! {
        section { class: CARD,
            div { class: "flex flex-col gap-4 p-6 sm:flex-row sm:items-center sm:justify-between",
                div {
                    h2 { class: "text-lg font-bold", "Site theme" }
                    p { class: "mt-1 text-sm opacity-70", "Saved in this browser." }
                }
                select {
                    class: "select select-bordered w-full sm:w-56",
                    aria_label: "Site theme",
                    onchange: move |evt: Event<FormData>| {
                        set_mode(Theme::from_str_theme(&evt.value()));
                    },
                    value: get_mode().to_string(),
                    for theme in Theme::all() {
                        option { value: theme.to_string(), "{theme}" }
                    }
                }
            }
        }
    }
}

/// A success (green) or error (red) line under a control.
#[component]
fn Message(msg: Option<(bool, String)>) -> Element {
    match msg {
        Some((true, m)) => rsx! { p { class: "text-sm text-error", "{m}" } },
        Some((false, m)) => rsx! { p { class: "text-sm text-success", "{m}" } },
        None => rsx! {},
    }
}


/// POST the file to `/api/profile/picture`; the new picture's version, or a message.
async fn upload_picture(bytes: &[u8]) -> Result<i64, String> {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;

        const OFFLINE: &str = "Couldn't reach the site. Check your connection and try again.";
        let window = web_sys::window().ok_or(OFFLINE)?;
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&js_sys::Uint8Array::from(bytes));
        let resp: web_sys::Response = JsFuture::from(window.fetch_with_str_and_init("/api/profile/picture", &init))
            .await
            .ok()
            .and_then(|r| r.dyn_into().ok())
            .ok_or(OFFLINE)?;
        let body = match resp.json() {
            Ok(p) => JsFuture::from(p).await.ok(),
            Err(_) => None,
        };
        let field = |k: &str| body.as_ref().and_then(|b| js_sys::Reflect::get(b, &k.into()).ok());
        match resp.status() {
            200 => field("version").and_then(|v| v.as_f64()).map(|v| v as i64).ok_or_else(|| OFFLINE.to_string()),
            400 => Err("That file isn't a picture that can be read. Use JPEG, PNG, WebP or GIF.".into()),
            401 => Err("You've been logged out. Log in again.".into()),
            413 => Err("That picture is too large (at most 5 MB).".into()),
            503 => Err("Pictures can't be changed right now.".into()),
            _ => Err("Couldn't save your picture. Try again.".into()),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = bytes;
        Err("Uploading only works in the browser.".into())
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
            if let Some((_, m)) = message() { p { class: "text-error", "{m}" } }
        };
    };
    if !current.available {
        return rsx! {};
    }

    rsx! {
        section { class: CARD,
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
