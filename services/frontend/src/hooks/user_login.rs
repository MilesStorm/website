use std::fmt::Display;

use dioxus::signals::{GlobalSignal, Signal};
use serde::{Deserialize, Serialize};
use serde_json::value::Value as Json;

pub static ROOT_DOMAIN: GlobalSignal<String> = Signal::global(|| {
    web_sys::js_sys::eval("location.protocol + '//' + location.host")
        .expect("cannot get domain")
        .as_string()
        .expect("cannot convert to string")
});

use crate::LOGIN_STATUS;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LogInStatus {
    LoggedOut,
    LoggedIn(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RestartRequestResponse {
    restart_result: String,
    exit_code: Option<i32>,
}

impl LogInStatus {
    pub async fn set_logged_in() {
        let resp = reqwest::get(format!("{}/api/login", ROOT_DOMAIN())).await;

        if let Ok(res) = resp {
            let json_value: Json = res.json().await.expect("cannot convert to json");
            if json_value["user"]["username"].is_null() {
                *LOGIN_STATUS.write() = LogInStatus::LoggedOut;
            } else {
                *LOGIN_STATUS.write() = LogInStatus::LoggedIn(
                    json_value["user"]["username"].as_str().unwrap().to_string(),
                );
            }
        } else {
            *LOGIN_STATUS.write() = LogInStatus::LoggedOut;
        }
    }

    pub async fn is_logged_in() -> LogInStatus {
        let resp = reqwest::get(format!("{}/api/login/status", ROOT_DOMAIN()).as_str()).await;
        match resp {
            Ok(res) => {
                let json_value: Json = res.json().await.expect("cannot convert to json");
                if let Some(username) = json_value["user"]["username"].as_str() {
                    LogInStatus::LoggedIn(username.to_string())
                } else {
                    LogInStatus::LoggedOut
                }
            }
            Err(_) => {
                tracing::warn!("Could not log in, server resonse warn");
                LogInStatus::LoggedOut
            }
        }
    }

    pub fn username(&self) -> Option<String> {
        match self {
            LogInStatus::LoggedOut => None,
            LogInStatus::LoggedIn(name) => Some(name.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientUser {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
}

impl Display for LogInStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogInStatus::LoggedOut => write!(f, "LoggedOut"),
            LogInStatus::LoggedIn(name) => write!(f, "LoggedIn({})", name),
        }
    }
}

static REQWEST_CLIENT: dioxus::signals::GlobalSignal<reqwest::Client> =
    Signal::global(|| reqwest::Client::new());

/// Function to send a POST request to the server to register a user.
pub async fn register(
    username: &str,
    email: &str,
    password: &str,
) -> Result<(String, Option<String>), reqwest::Error> {
    let params = [
        ("username", username),
        ("email", email),
        ("password", password),
    ];

    let response = post_reqwest(
        format!("{}/api/register/password", ROOT_DOMAIN()).as_str(),
        &params,
    )
    .await;

    match response {
        Ok(res) => {
            let json_value = res.json::<Json>().await?;
            Ok((
                json_value["message"]
                    .as_str()
                    .expect("cannot convert to strign")
                    .to_string(),
                if json_value["user"]["username"].is_null() {
                    None
                } else {
                    Some(json_value["user"]["username"].as_str().unwrap().to_string())
                },
            ))
        }
        Err(e) => Err(e),
    }
}

pub async fn logout() -> Result<(), reqwest::Error> {
    let resp = reqwest::get(format!("{}/api/logout", ROOT_DOMAIN()).as_str()).await?;
    tracing::info!("logout response: {:?}", resp);
    *LOGIN_STATUS.write() = LogInStatus::LoggedOut;

    Ok(())
}

pub async fn restart_valheim() -> Result<RestartRequestResponse, reqwest::Error> {
    let resp =
        reqwest::get(format!("{}/api/permission/valheim_player/restart", ROOT_DOMAIN()).as_str())
            .await?;
    tracing::info!("restart_valheim response: {:?}", resp);
    let result = resp.json::<RestartRequestResponse>().await?;

    Ok(result)
}

pub async fn has_permission(permission: &str) -> bool {
    let resp =
        reqwest::get(format!("{}/api/permission/{}", ROOT_DOMAIN(), permission).as_str()).await;

    match resp {
        Ok(res) => {
            let json_value: Json = res.json().await.expect("Could not get result") else {
                return false;
            };

            tracing::warn!("has_permission: {:?}", json_value);

            match json_value["has_permission"].as_bool() {
                Some(b) => b,
                None => {
                    tracing::warn!("has_permission: {:?}", json_value);
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!("Could not log in, error: {:?}", e);
            false
        }
    }
}

pub async fn login(
    username: &str,
    password: &str,
) -> Result<(String, Option<String>), reqwest::Error> {
    let params = [("username", username), ("password", password)];
    tracing::debug!("params: {:?}", params);

    let _ = post_reqwest(
        format!("{}/api/login/password", ROOT_DOMAIN()).as_str(),
        &params,
    )
    .await;

    let response = reqwest::get(format!("{}/api/login", ROOT_DOMAIN())).await;
    match response {
        Ok(res) => {
            let json_value: Json = res.json().await?;
            Ok((
                "tes".into(),
                json_value["user"]["username"]
                    .as_str()
                    .map(|s| s.to_string()),
            ))
        }
        Err(e) => Err(e),
    }
}

pub async fn google_oauth() -> Result<String, reqwest::Error> {
    let response = post_reqwest(format!("{}/api/login/google", ROOT_DOMAIN()).as_str(), &[]).await;

    // if the request succeeds, get the url from the json response.
    match response {
        Ok(res) => {
            tracing::info!("response: {:?}", res);
            let json_value: Json = res.json().await?;
            Ok(json_value["next"]
                .as_str()
                .expect("cannot convert to string")
                .to_string())
        }
        Err(e) => Err(e),
    }
}

pub async fn github_oauth() -> Result<String, reqwest::Error> {
    let response = post_reqwest(format!("{}/api/login/github", ROOT_DOMAIN()).as_str(), &[]).await;

    // if the request succeeds, get the url from the json response.
    match response {
        Ok(res) => {
            let json_value: Json = res.json().await?;
            Ok(json_value["next"]
                .as_str()
                .expect("cannot convert to string")
                .to_string())
        }
        Err(e) => Err(e),
    }
}

/// Function to send a POST request to the server on a given url
pub async fn post_reqwest(
    url: &str,
    params: &[(&str, &str)],
) -> Result<reqwest::Response, reqwest::Error> {
    tracing::info!("ROOT_DOMAIN: {:?}", ROOT_DOMAIN());
    let res = REQWEST_CLIENT().post(url).form(params).send().await?;

    Ok(res)
}
