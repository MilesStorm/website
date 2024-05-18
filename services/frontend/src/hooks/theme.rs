use core::slice::Iter;
use dioxus::signals::Writable;
use dioxus_sdk::storage::*;
use serde::{Deserialize, Serialize};
use web_sys;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum Theme {
    Dark,
    Light,
    Dracula,
    Synthwave,
    Retro,
    Dim,
    Corporate,
    Preffered,
}

impl Theme {
    pub fn from(input: String) -> Theme {
        let input = input.to_lowercase();
        match input.as_str() {
            "dark" => Theme::Dark,
            "light" => Theme::Light,
            "dracula" => Theme::Dracula,
            "synthwave" => Theme::Synthwave,
            "retro" => Theme::Retro,
            "dim" => Theme::Dim,
            "corporate" => Theme::Corporate,
            "preffered" | "system" => Theme::Preffered,
            _ => Theme::Preffered,
        }
    }

    pub fn iterator() -> Iter<'static, Theme> {
        static THEMES: [Theme; 8] = [
            Theme::Dark,
            Theme::Light,
            Theme::Dracula,
            Theme::Synthwave,
            Theme::Retro,
            Theme::Dim,
            Theme::Corporate,
            Theme::Preffered,
        ];
        THEMES.iter()
    }

    pub fn to_css_class(&self) -> String {
        let pref = dioxus_sdk::color_scheme::use_preferred_color_scheme();
        match self {
            Theme::Dark => "dark".to_string(),
            Theme::Light => "light".to_string(),
            Theme::Dracula => "dracula".to_string(),
            Theme::Synthwave => "synthwave".to_string(),
            Theme::Retro => "retro".to_string(),
            Theme::Dim => "dim".to_string(),
            Theme::Corporate => "corporate".to_string(),
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

impl ToString for Theme {
    fn to_string(&self) -> String {
        match self {
            Theme::Dark => "Dark".to_string(),
            Theme::Light => "Light".to_string(),
            Theme::Dracula => "Dracula".to_string(),
            Theme::Synthwave => "Synthwave".to_string(),
            Theme::Retro => "Retro".to_string(),
            Theme::Dim => "Dim".to_string(),
            Theme::Corporate => "Corporate".to_string(),
            Theme::Preffered => "System".to_string(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Preffered
    }
}

pub fn get_mode() -> Theme {
    // use_persistent("theme-mode", || Theme::Preffered)()
    use_synced_storage::<LocalStorage, Theme>("theme-mode".to_owned(), || Theme::Preffered)()
}

pub fn setup_mode() {
    // let mode = use_persistent("theme-mode", || Theme::default());
    let mode =
        use_synced_storage::<LocalStorage, Theme>("theme-mode".to_owned(), || Theme::Preffered);

    web_sys::js_sys::eval(
        format!(
            "document.documentElement.setAttribute('data-theme', '{}');",
            mode().to_css_class()
        )
        .as_str(),
    )
    .expect("Failed to set theme");
}

pub fn set_mode(theme_mode: Theme) {
    // let mut storage = use_persistent("theme-mode", || Theme::default());
    let mut storage =
        use_synced_storage::<LocalStorage, Theme>("theme-mode".to_owned(), || Theme::Preffered);

    storage.set(theme_mode);
    let pref = dioxus_sdk::color_scheme::use_preferred_color_scheme();
    match theme_mode {
        Theme::Dark => {
            web_sys::js_sys::eval("document.documentElement.setAttribute('data-theme', 'dark');")
                .expect("Failed to set theme");
            // storage.set(Theme::Dark);
        }
        Theme::Light => {
            web_sys::js_sys::eval("document.documentElement.setAttribute('data-theme', 'light');")
                .expect("Failed to set theme");
        }
        Theme::Dracula => {
            web_sys::js_sys::eval(
                "document.documentElement.setAttribute('data-theme', 'dracula');",
            )
            .expect("Failed to set theme");
        }
        Theme::Synthwave => {
            web_sys::js_sys::eval(
                "document.documentElement.setAttribute('data-theme', 'synthwave');",
            )
            .expect("Failed to set theme");
        }
        Theme::Retro => {
            web_sys::js_sys::eval("document.documentElement.setAttribute('data-theme', 'retro');")
                .expect("Failed to set theme");
        }
        Theme::Dim => {
            web_sys::js_sys::eval("document.documentElement.setAttribute('data-theme', 'dim');")
                .expect("Failed to set theme");
        }
        Theme::Corporate => {
            web_sys::js_sys::eval(
                "document.documentElement.setAttribute('data-theme', 'corporate');",
            )
            .expect("Failed to set theme");
        }
        Theme::Preffered => {
            if let Ok(prefference) = pref {
                tracing::info!("Preffered theme found: {:?}", prefference);

                match prefference {
                    dioxus_sdk::color_scheme::PreferredColorScheme::Light => {
                        web_sys::js_sys::eval(
                            "document.documentElement.setAttribute('data-theme', 'light');",
                        )
                        .expect("Failed to set theme");
                    }
                    dioxus_sdk::color_scheme::PreferredColorScheme::Dark => {
                        web_sys::js_sys::eval(
                            "document.documentElement.setAttribute('data-theme', 'dark');",
                        )
                        .expect("Failed to set theme");
                    }
                }
            } else {
                gloo::console::log!("No preffered theme found, using default");
                web_sys::js_sys::eval(
                    "document.documentElement.setAttribute('data-theme', 'dark');",
                )
                .expect("Failed to set theme");
            };
        }
    };
}
