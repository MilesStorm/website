//! This crate contains all shared UI for the workspace.
pub mod data;
mod hero;
use dioxus::prelude::*;
pub use hero::*;

mod navbar;
pub use navbar::*;

mod echo;
pub use echo::*;

pub const TAILWIND: Asset = asset!("/assets/styling/tailwind.css");
