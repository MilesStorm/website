use dioxus::prelude::*;

use crate::{ACCOUNT, LOGIN_STATUS, PERMISSIONS};
use ui::data_dir::LoginStatus;
use ui::default_profile_picture;

/// One part of the site, as a card on the landing page.
struct Feature {
    title: &'static str,
    text: &'static str,
    to: &'static str,
    /// Permission that unlocks it; `None` = every logged-in user.
    permission: Option<&'static str>,
    /// Shown (as locked) to people without the permission. Unadvertised
    /// features only appear for those who have them.
    advertised: bool,
    /// SVG path (24x24, stroked) for the icon.
    icon: &'static str,
}

const FEATURES: &[Feature] = &[
    Feature {
        title: "Arcane dice reader",
        text: "Roll real dice in front of your camera and the site reads them for you, live on any device and in the browser extension.",
        to: "/arcane",
        permission: Some("arcane"),
        advertised: true,
        icon: "M4 6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2zM8.5 8.5h.01M15.5 8.5h.01M12 12h.01M8.5 15.5h.01M15.5 15.5h.01",
    },
    Feature {
        title: "Ark server",
        text: "Start, stop and restart the ARK game server, and see who's playing.",
        to: "/ark",
        permission: Some("llama"),
        advertised: false,
        icon: "M5 12h14M5 12a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2M5 12a2 2 0 0 0-2 2v4a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-4a2 2 0 0 0-2-2M7 8h.01M7 16h.01",
    },
    Feature {
        title: "Your profile",
        text: "Your display name, profile picture and site theme.",
        to: "/profile",
        permission: None,
        advertised: true,
        icon: "M16 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0zM12 14a7 7 0 0 0-7 7h14a7 7 0 0 0-7-7z",
    },
];

const ADMIN: Feature = Feature {
    title: "Admin",
    text: "Roles and permissions for every account.",
    to: "/admin",
    permission: Some("manage_permissions"),
    advertised: false,
    icon: "M12 3l7 4v5c0 4.5-3 8-7 9-4-1-7-4.5-7-9V7z",
};

/// Stands in for the unadvertised features on the logged-out page.
const MORE: Feature = Feature {
    title: "More for invited accounts",
    text: "A few other tools for the people who play with us.",
    to: "/login",
    permission: None,
    advertised: true,
    icon: "M12 3l1.9 5.8H20l-4.9 3.6 1.9 5.8L12 14.6l-4.9 3.6 1.9-5.8L4 8.8h6.1z",
};

#[component]
pub fn Landing() -> Element {
    rsx! {
        div { class: "container mx-auto max-w-5xl px-4 pt-8 pb-16 flex flex-col gap-10",
            match LOGIN_STATUS() {
                LoginStatus::LoggedOut => rsx! { Welcome {} },
                LoginStatus::LoggedIn(username) => rsx! { Home { username } },
            }
        }
    }
}

/// Logged out: what the site is, and how to get in.
#[component]
fn Welcome() -> Element {
    rsx! {
        section { class: "relative overflow-hidden rounded-box border border-base-300 bg-base-200 px-6 py-16 sm:px-12 sm:py-20",
            Glow {}
            div { class: "relative max-w-2xl",
                p { class: "text-sm font-semibold uppercase tracking-widest text-primary", "MilesStorm" }
                h1 { class: "mt-3 text-4xl font-extrabold leading-tight sm:text-5xl",
                    "Tools for game nights with friends."
                }
                p { class: "mt-4 text-lg opacity-70",
                    "A dice reader that follows your real rolls, and more for invited accounts."
                }
                div { class: "mt-8 flex flex-wrap gap-3",
                    Link { class: "btn btn-primary", to: "/login", "Log in" }
                    Link { class: "btn btn-ghost", to: "/register", "Create an account" }
                }
                p { class: "mt-6 text-sm opacity-60",
                    "Early access: features are unlocked for invited accounts."
                }
            }
        }
        section { class: "grid gap-4 sm:grid-cols-2",
            for f in FEATURES.iter().filter(|f| f.permission.is_some() && f.advertised) {
                FeatureCard { title: f.title, text: f.text, to: f.to, icon: f.icon, state: CardState::Preview }
            }
            FeatureCard { title: MORE.title, text: MORE.text, to: MORE.to, icon: MORE.icon, state: CardState::Preview }
        }
    }
}

/// Logged in: a greeting and the parts of the site, open or locked.
#[component]
fn Home(username: String) -> Element {
    let account = ACCOUNT();
    let name = account.as_ref().map_or(username.clone(), |a| a.shown_name().to_string());
    let picture = account.as_ref().and_then(|a| a.picture_url());
    let perms = PERMISSIONS.read();
    let has = |f: &Feature| f.permission.is_none_or(|p| perms.contains_key(p));

    // Open ones first, then locked advertised ones; unadvertised ones (Ark, admin)
    // only for people who have them.
    let mut cards: Vec<(&Feature, bool)> = FEATURES
        .iter()
        .chain(std::iter::once(&ADMIN))
        .map(|f| (f, has(f)))
        .filter(|(f, open)| *open || f.advertised)
        .collect();
    cards.sort_by_key(|(_, open)| !open);

    rsx! {
        section { class: "relative overflow-hidden rounded-box border border-base-300 bg-base-200 px-6 py-10 sm:px-10",
            Glow {}
            div { class: "relative flex items-center gap-5",
                div { class: "w-16 h-16 shrink-0 rounded-full overflow-hidden ring ring-primary ring-offset-base-200 ring-offset-2",
                    if let Some(src) = picture {
                        img { src: "{src}", alt: "", width: 64, height: 64 }
                    } else {
                        default_profile_picture { width: 64, height: 64 }
                    }
                }
                div {
                    p { class: "text-sm opacity-60", "Welcome back" }
                    h1 { class: "text-3xl font-extrabold", "{name}" }
                }
            }
        }
        section { class: "grid gap-4 sm:grid-cols-2 lg:grid-cols-3",
            for (f, open) in cards {
                FeatureCard {
                    title: f.title,
                    text: f.text,
                    to: f.to,
                    icon: f.icon,
                    state: if open { CardState::Open } else { CardState::Locked },
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum CardState {
    /// Logged out: describes the feature, not clickable.
    Preview,
    Open,
    Locked,
}

#[component]
fn FeatureCard(title: &'static str, text: &'static str, to: &'static str, icon: &'static str, state: CardState) -> Element {
    let body = rsx! {
        div { class: "flex items-start gap-4",
            div { class: "grid h-11 w-11 shrink-0 place-items-center rounded-xl bg-primary/15 text-primary",
                svg {
                    xmlns: "http://www.w3.org/2000/svg",
                    view_box: "0 0 24 24",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.8",
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    class: "h-6 w-6",
                    path { d: icon }
                }
            }
            div { class: "min-w-0",
                h2 { class: "font-bold", "{title}" }
                p { class: "mt-1 text-sm opacity-70", "{text}" }
                match state {
                    CardState::Open => rsx! { p { class: "mt-3 text-sm font-semibold text-primary", "Open →" } },
                    CardState::Locked => rsx! { p { class: "mt-3 text-xs opacity-60", "Ask the site owner for access." } },
                    CardState::Preview => rsx! { p { class: "mt-3 text-xs opacity-60", "For invited accounts." } },
                }
            }
        }
    };
    match state {
        CardState::Open => rsx! {
            Link {
                class: "rounded-box border border-base-300 bg-base-100 p-5 transition hover:-translate-y-0.5 hover:border-primary/60 hover:shadow-lg",
                to: to,
                {body}
            }
        },
        _ => rsx! {
            div { class: if state == CardState::Locked { "rounded-box border border-dashed border-base-300 bg-base-100/60 p-5 opacity-70" } else { "rounded-box border border-base-300 bg-base-100 p-5" },
                {body}
            }
        },
    }
}

/// Soft coloured light behind a hero, in the theme's own colours.
#[component]
fn Glow() -> Element {
    rsx! {
        div { class: "pointer-events-none absolute -top-24 -right-24 h-72 w-72 rounded-full bg-primary/25 blur-3xl" }
        div { class: "pointer-events-none absolute -bottom-32 left-1/3 h-72 w-72 rounded-full bg-secondary/20 blur-3xl" }
    }
}
