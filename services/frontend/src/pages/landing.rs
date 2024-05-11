use dioxus::prelude::*;

use crate::components::Navbar::Navbar;

pub fn Landing() -> Element {
    rsx! {
        Navbar {}
        p { class: "text",
            r#"Login to see your profile\n
            if you're page is blank, contact yousof to get features added\n
            As this is hosted on my own server i have to limit the people who can access the different features for now\n
            but when a feature can take many users i will make it publically available"#
        }
    }
}
