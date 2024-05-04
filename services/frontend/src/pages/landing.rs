use dioxus::prelude::*;

use crate::components::Navbar::Navbar;

pub fn Landing() -> Element {
    rsx! {
        Navbar {}
    }
}
