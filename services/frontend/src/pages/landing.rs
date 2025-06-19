use dioxus::prelude::*;

use crate::components::Navbar::Navbar;

pub fn Landing() -> Element {
    rsx! {
        div {
            Navbar {}
            div {
                class: "container mx-auto mt-10",
                p { class: "text", "Login to see your profile"}
                p { class: "text", "if you're page is blank, contact Miles to get features added"}
                p { class: "text", "As this is hosted on my own server i have to limit the people who can access the different features for now"}
                p { class: "text", "but when a feature can take many users i will make it publically available"}
            }
        }
    }
}
