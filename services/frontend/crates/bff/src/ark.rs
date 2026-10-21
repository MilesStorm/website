//! The Ark server panel's calls to auth (`/internal/ark/*`, which checks the token's `llama`
//! permission and asks the game host), for the JSON API.

use api_types::{ApiError, ArkCommand, ArkOutcome, ArkPlayers, ErrorCode};
use serde::Deserialize;
use serde_json::json;

use crate::auth::auth_json;
use crate::trace;

/// Auth's reply to an Ark call: the game host's result, in its own names.
#[derive(Deserialize)]
struct DockerRequestResponse {
    command_result: Option<CommandResult>,
}

#[derive(Debug, Deserialize, PartialEq)]
enum CommandResult {
    Stopped,
    AlreadyStopped,
    FailedToStop,
    Started,
    AlreadyRunning,
    FailedToStart,
    Timeout,
    Restarting,
    NumPlayers(i32),
}

fn unreachable() -> ApiError {
    ApiError::new(ErrorCode::Upstream, "The Ark server can't be reached right now. Try again later.")
}

/// Asks auth; the game host's result, if it gave one. `label` names the call in the metrics.
async fn call(path: &str, body: serde_json::Value, label: &str) -> Result<Option<CommandResult>, ApiError> {
    let (status, reply) = auth_json(path, &body).await.map_err(|e| {
        tracing::error!(error = %e, "reaching auth failed");
        unreachable()
    })?;
    let outcome = if (200..300).contains(&status) { "success" } else { "error" };
    metrics::counter!("bff_ark_commands_total", "cmd" => label.to_string(), "status" => outcome).increment(1);
    match status {
        200..=299 => serde_json::from_value::<DockerRequestResponse>(reply).map(|r| r.command_result).map_err(|e| {
            tracing::error!(error = %e, "auth's Ark reply is unreadable");
            unreachable()
        }),
        403 => Err(ApiError::new(ErrorCode::Forbidden, "You don't have access to the Ark server.")),
        _ => {
            tracing::error!(status, "an Ark request failed");
            Err(unreachable())
        }
    }
}

/// How many players are on the server; `None` when it doesn't say.
#[tracing::instrument(name = "bff.ark_player_count", skip_all)]
pub async fn players(token: &str) -> Result<ArkPlayers, ApiError> {
    trace::rejectable(async {
        let result = call("/internal/ark/num_players", json!({ "token": token }), "num_players").await?;
        let count = match result {
            Some(CommandResult::NumPlayers(n)) => u32::try_from(n).ok(),
            _ => None,
        };
        Ok(ArkPlayers { count })
    })
    .await
}

/// Starts, stops or restarts the server, for `username` (logged).
#[tracing::instrument(name = "bff.ark_command", skip_all, fields(cmd = cmd.as_str()))]
pub async fn command(token: &str, username: &str, cmd: ArkCommand) -> Result<ArkOutcome, ApiError> {
    trace::rejectable(async {
        tracing::info!(username, cmd = cmd.as_str(), "ark command requested");
        let result = call("/internal/ark/command", json!({ "token": token, "cmd": cmd.as_str() }), cmd.as_str()).await?;
        outcome(result.as_ref()).ok_or_else(|| {
            tracing::error!(result = ?result, "auth's Ark reply has no command result");
            unreachable()
        })
    })
    .await
}

/// The result of a command, as the browser sees it. A player count isn't one.
fn outcome(result: Option<&CommandResult>) -> Option<ArkOutcome> {
    Some(match result? {
        CommandResult::Stopped => ArkOutcome::Stopped,
        CommandResult::AlreadyStopped => ArkOutcome::AlreadyStopped,
        CommandResult::FailedToStop => ArkOutcome::FailedToStop,
        CommandResult::Started => ArkOutcome::Started,
        CommandResult::AlreadyRunning => ArkOutcome::AlreadyRunning,
        CommandResult::FailedToStart => ArkOutcome::FailedToStart,
        CommandResult::Timeout => ArkOutcome::Timeout,
        CommandResult::Restarting => ArkOutcome::Restarting,
        CommandResult::NumPlayers(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_result_but_a_count_is_an_outcome() {
        let all = [
            (CommandResult::Stopped, "stopped"),
            (CommandResult::AlreadyStopped, "already_stopped"),
            (CommandResult::FailedToStop, "failed_to_stop"),
            (CommandResult::Started, "started"),
            (CommandResult::AlreadyRunning, "already_running"),
            (CommandResult::FailedToStart, "failed_to_start"),
            (CommandResult::Timeout, "timeout"),
            (CommandResult::Restarting, "restarting"),
        ];
        for (result, name) in all {
            assert_eq!(serde_json::to_value(outcome(Some(&result)).unwrap()).unwrap(), name);
        }
        assert_eq!(outcome(Some(&CommandResult::NumPlayers(3))), None);
        assert_eq!(outcome(None), None);
    }
}
