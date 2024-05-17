use std::fmt::Display;

use dioxus::{
    prelude::navigator,
    signals::{Readable, Signal, Writable},
};
use serde::{Deserialize, Serialize};
use serde_json::value::Value as Json;

use crate::LOGIN_STATUS;

#[derive(Debug, Eq, PartialEq)]
pub enum LogInStatus {
    LoggedOut,
    LoggedIn(String),
}

impl LogInStatus {
    pub async fn is_logged_in() {
        let resp = reqwest::get("http://localhost:8080/api/login").await;

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
            // LOGIN_STATUS.set(LogInStatus::LoggedOut);
            *LOGIN_STATUS.write() = LogInStatus::LoggedOut;
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

    let response = post_reqwest("http://localhost:8080/api/register/password", &params).await;

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

pub async fn login(
    username: &str,
    password: &str,
) -> Result<(String, Option<String>), reqwest::Error> {
    let params = [("username", username), ("password", password)];
    tracing::debug!("params: {:?}", params);

    let response = post_reqwest("http://localhost:8080/api/login/password", &params).await;

    reqwest::get("http://localhost:8080/api/login").await;
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

/// Function to send a POST request to the server on a given url
pub async fn post_reqwest(
    url: &str,
    params: &[(&str, &str)],
) -> Result<reqwest::Response, reqwest::Error> {
    let res = REQWEST_CLIENT().post(url).form(params).send().await?;

    Ok(res)
}
