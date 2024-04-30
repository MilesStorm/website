use dioxus::signals::Writable;
use dioxus_sdk::storage::*;
use js_sys;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum Theme {
    Dark,
    Light,
    Preffered,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Preffered
    }
}

pub fn mode(theme_mode: Theme) {
    let mut storage = use_persistent("theme-mode", || Theme::Preffered);

    match theme_mode {
        Theme::Dark => {
            js_sys::eval("document.documentElement.setAttribute('data-theme', 'dark');")
                .expect("Failed to set theme");
            if theme_mode != Theme::Dark {
                storage.set(Theme::Dark);
            }
        }
        Theme::Light => {
            js_sys::eval("document.documentElement.setAttribute('data-theme', 'light');")
                .expect("Failed to set theme");
            if theme_mode != Theme::Light {
                storage.set(Theme::Light);
            }
        }
        Theme::Preffered => {
            let pref = dioxus_sdk::color_scheme::use_preferred_color_scheme();
            if let Ok(prefference) = pref {
                tracing::info!("Preffered theme found: {:?}", prefference);

                match prefference {
                    dioxus_sdk::color_scheme::PreferredColorScheme::Light => {
                        js_sys::eval(
                            "document.documentElement.setAttribute('data-theme', 'dark');",
                        )
                        .expect("Failed to set theme");
                    }
                    dioxus_sdk::color_scheme::PreferredColorScheme::Dark => {
                        js_sys::eval(
                            "document.documentElement.setAttribute('data-theme', 'dark');",
                        )
                        .expect("Failed to set theme");
                    }
                }

                if theme_mode != Theme::Preffered {
                    storage.set(theme_mode);
                }
            } else {
                gloo::console::log!("No preffered theme found, using default");
                js_sys::eval("document.documentElement.setAttribute('data-theme', 'dark');")
                    .expect("Failed to set theme");
                storage.set(Theme::Dark);
            };
        }
    };
}
