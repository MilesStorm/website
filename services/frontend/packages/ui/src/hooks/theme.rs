use dioxus::prelude::*;
use dioxus_sdk::storage::*;

use crate::data_dir::Theme;

pub fn get_mode() -> Theme {
    use_synced_storage::<LocalStorage, Theme>("theme".to_owned(), || Theme::Preferred)()
}

pub fn setup_mode() {
    let mode = use_synced_storage::<LocalStorage, Theme>("theme".to_owned(), || Theme::Preferred);
    let css_val = theme_to_css(mode());
    let js = format!(
        "document.documentElement.setAttribute('data-theme', '{}');",
        css_val
    );
    let _ = document::eval(&js);
}

pub fn set_mode(theme: Theme) {
    let mut storage =
        use_synced_storage::<LocalStorage, Theme>("theme".to_owned(), || Theme::Preferred);
    *storage.write() = theme;
    let js = format!(
        "document.documentElement.setAttribute('data-theme', '{}');",
        theme_to_css(theme)
    );
    let _ = document::eval(&js);
}

fn theme_to_css(theme: Theme) -> &'static str {
    match theme {
        Theme::Dark => "dark",
        Theme::Light => "light",
        Theme::Dracula => "dracula",
        Theme::Synthwave => "synthwave",
        Theme::Retro => "retro",
        Theme::Dim => "dim",
        Theme::Corporate => "corporate",
        Theme::Preferred => "dark",
    }
}
