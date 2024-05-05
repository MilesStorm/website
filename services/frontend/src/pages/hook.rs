use dioxus::signals::{GlobalSignal, Readable, Writable};
use dioxus_sdk::storage::*;
use js_sys;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum Theme {
    Dark,
    Light,
    Preffered,
}

impl Theme {
    pub fn from(input: String) -> Theme {
        let input = input.to_lowercase();
        match input.as_str() {
            "dark" => Theme::Dark,
            "light" => Theme::Light,
            "preffered" | "system" => Theme::Preffered,
            _ => Theme::Preffered,
        }
    }

    pub fn to_css_class(&self) -> String {
        let pref = dioxus_sdk::color_scheme::use_preferred_color_scheme();
        match self {
            Theme::Dark => "dark".to_string(),
            Theme::Light => "light".to_string(),
            Theme::Preffered => if let Ok(prefference) = pref {
                tracing::info!("Preffered theme found: {:?}", prefference);

                match prefference {
                    dioxus_sdk::color_scheme::PreferredColorScheme::Light => "light",
                    dioxus_sdk::color_scheme::PreferredColorScheme::Dark => "dark",
                }
            } else {
                gloo::console::log!("No preffered theme found, using default");
                "dark"
            }
            .to_owned(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Preffered
    }
}

pub fn get_mode() -> Theme {
    use_persistent("theme-mode", || Theme::Preffered)()
}

pub fn setup_mode() {
    let mode = use_persistent("theme-mode", || Theme::default());

    js_sys::eval(
        format!(
            "document.documentElement.setAttribute('data-theme', '{}');",
            mode().to_css_class()
        )
        .as_str(),
    )
    .expect("Failed to set theme");
}

pub fn set_mode(theme_mode: Theme) {
    let mut storage = use_persistent("theme-mode", || Theme::default());

    storage.set(theme_mode);
    let pref = dioxus_sdk::color_scheme::use_preferred_color_scheme();
    match theme_mode {
        Theme::Dark => {
            js_sys::eval("document.documentElement.setAttribute('data-theme', 'dark');")
                .expect("Failed to set theme");
            // storage.set(Theme::Dark);
        }
        Theme::Light => {
            js_sys::eval("document.documentElement.setAttribute('data-theme', 'light');")
                .expect("Failed to set theme");
        }

        Theme::Preffered => {
            if let Ok(prefference) = pref {
                tracing::info!("Preffered theme found: {:?}", prefference);

                match prefference {
                    dioxus_sdk::color_scheme::PreferredColorScheme::Light => {
                        js_sys::eval(
                            "document.documentElement.setAttribute('data-theme', 'light');",
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
            } else {
                gloo::console::log!("No preffered theme found, using default");
                js_sys::eval("document.documentElement.setAttribute('data-theme', 'dark');")
                    .expect("Failed to set theme");
            };
        }
    };
}
