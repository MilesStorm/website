use dioxus::{fullstack::reqwest, logger::tracing};
use ui::data_dir::LoginStatus;

use crate::LOGIN_STATUS;

const fn root_domain() -> &'static str {
    if cfg!(debug_assertions) {
        "localhost"
    } else {
        "https://milesstorm.com"
    }
}

pub async fn logout() -> Result<(), reqwest::Error> {
    let resp = reqwest::get(format!("{}/api/logout", root_domain()).as_str()).await?;
    tracing::info!("logout response: {:?}", resp);
    *LOGIN_STATUS.write() = LoginStatus::LoggedOut;

    Ok(())
}
