//! Shared UI components, types, and hooks for all platforms.

pub mod components;
pub mod data_dir;
pub mod hooks;

mod hero;
mod navbar;

pub use hero::Hero;
pub use navbar::{Navbar, Navbarr};
pub use components::{CookieConsent, Logo_c, default_profile_picture};
pub use hooks::theme::{get_mode, set_mode, setup_mode};

pub use dioxus::prelude::*;

pub const TAILWIND: Asset = asset!("/assets/styling/tailwind.css");
