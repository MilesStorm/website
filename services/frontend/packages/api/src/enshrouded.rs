//! The Enshrouded server panel's calls to auth (`services/auth/src/auth/enshrouded.rs`, which
//! checks the token's `enshrouded` permission), shared by the JSON API and the server functions.

use api_types::{ApiError, EnshroudedCommand, EnshroudedStatus, ErrorCode};
use serde_json::{json, Value};

use crate::{auth_json, trace};

fn offline() -> ApiError {
    ApiError::new(ErrorCode::Upstream, "The website can't reach the server right now.")
}

/// What auth's `{"error": code}` means for the user, with that code's status.
fn refused(status: u16, reply: &Value) -> ApiError {
    let code = reply.get("error").and_then(Value::as_str).unwrap_or("");
    let (error, message) = match code {
        _ if status == 401 => return ApiError::logged_out(),
        "forbidden" => (ErrorCode::Forbidden, "You don't have access to the Enshrouded server."),
        "too_soon" => (ErrorCode::RateLimited, "Someone just sent a command. Wait a moment and try again."),
        "busy" => (ErrorCode::Conflict, "A restart or update is already under way."),
        "not_configured" => (ErrorCode::Unavailable, "The website can't control the server yet (no control password)."),
        "unreachable" => (ErrorCode::Upstream, "The server's control port doesn't answer."),
        _ if (400..500).contains(&status) => {
            tracing::warn!(status, code, "auth refused an Enshrouded request");
            (ErrorCode::Invalid, "Something went wrong. Try again later.")
        }
        _ => {
            tracing::error!(status, code, "auth failed an Enshrouded request");
            (ErrorCode::Upstream, "Something went wrong. Try again later.")
        }
    };
    ApiError::new(error, message)
}

/// The server's state, players, and the latest commands.
#[tracing::instrument(name = "bff.enshrouded_status", skip_all)]
pub async fn status(token: &str) -> Result<EnshroudedStatus, ApiError> {
    trace::rejectable(async {
        let (status, reply) = auth_json("/internal/enshrouded/status", &json!({ "token": token })).await.map_err(|e| {
            tracing::error!(error = %e, "reaching auth failed");
            offline()
        })?;
        if status != 200 {
            return Err(refused(status, &reply));
        }
        serde_json::from_value(reply).map_err(|e| {
            tracing::error!(error = %e, "auth's Enshrouded status is unreadable");
            offline()
        })
    })
    .await
}

/// Restarts or updates the server.
#[tracing::instrument(name = "bff.enshrouded_command", skip_all, fields(cmd = cmd.as_str()))]
pub async fn command(token: &str, cmd: EnshroudedCommand) -> Result<(), ApiError> {
    trace::rejectable(async {
        let body = json!({ "token": token, "cmd": cmd.as_str() });
        let (status, reply) = auth_json("/internal/enshrouded/command", &body).await.map_err(|e| {
            tracing::error!(error = %e, "reaching auth failed");
            offline()
        })?;
        let outcome = if status == 200 { "success" } else { "error" };
        metrics::counter!("bff_enshrouded_commands_total", "cmd" => cmd.as_str(), "status" => outcome).increment(1);
        if status != 200 {
            return Err(refused(status, &reply));
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_have_their_codes_status_and_failures_are_gateway_errors() {
        let error = |status, code: &str| {
            let e = refused(status, &json!({ "error": code }));
            (e.status(), e.error)
        };
        assert_eq!(error(403, "forbidden"), (403, ErrorCode::Forbidden));
        assert_eq!(error(429, "too_soon"), (429, ErrorCode::RateLimited));
        assert_eq!(error(409, "busy"), (409, ErrorCode::Conflict));
        assert_eq!(error(503, "not_configured"), (503, ErrorCode::Unavailable));
        assert_eq!(error(502, "unreachable"), (502, ErrorCode::Upstream));
        assert_eq!(error(400, "invalid_command"), (400, ErrorCode::Invalid));
        // A refusal this site doesn't know: its status is its code's.
        assert_eq!(error(422, "something_new"), (400, ErrorCode::Invalid));
        assert_eq!(error(401, ""), (401, ErrorCode::LoggedOut));
        assert_eq!(error(500, "boom"), (502, ErrorCode::Upstream));
        assert_eq!(refused(500, &Value::Null).status(), 502);
    }
}
