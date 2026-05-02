//! Shared UI components, types, and hooks for all platforms.

pub mod components;
pub mod data_dir;
pub mod hooks;

mod hero;
mod navbar;

pub use components::{default_profile_picture, logo_c, CookieConsent};
pub use hero::Hero;
pub use hooks::theme::{get_mode, set_mode, setup_mode};
pub use navbar::{Navbar, Navbarr};

pub use dioxus::prelude::*;

pub const TAILWIND: Asset = asset!("/assets/styling/tailwind.css");
