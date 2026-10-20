//! Profile-page actions for sharing roll pictures (`/api/account/sharing*` and the server
//! functions over the same functions). The choice is
//! stored with the account by auth (PostgreSQL); the pictures in SurrealDB.
//!
//! Reading and changing the choice requires a logged-in user with the `arcane` permission;
//! deleting what was shared only the session's username. Every call needs, as a second line
//! of defence after the SameSite=Lax session cookie, a same-site Origin. Store errors are
//! logged and returned as a generic message.

use dioxus::prelude::*;

pub use api_types::SharingState;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod server {
    use super::*;
    use api_types::{ApiError, ErrorCode};

    use crate::rolls::RollHub;

    fn store_error(e: impl std::fmt::Display) -> ApiError {
        tracing::error!(error = %e, "roll sharing store failed");
        ApiError::new(ErrorCode::Upstream, "The picture store is unavailable. Try again later.")
    }

    fn unavailable() -> ApiError {
        ApiError::new(ErrorCode::Unavailable, "Sharing isn't available.")
    }

    /// Whether the token's user shares roll pictures.
    #[tracing::instrument(name = "bff.get_dataset_sharing", skip_all, err)]
    pub async fn sharing(token: &str) -> Result<SharingState, ApiError> {
        if crate::dataset::dataset().is_none() {
            return Ok(SharingState { available: false, share: false });
        }
        let share = api::dataset_consent(token).await.map_err(store_error)?;
        Ok(SharingState { available: true, share })
    }

    #[tracing::instrument(name = "bff.set_dataset_sharing", skip_all, err, fields(share = share))]
    pub async fn set_sharing(token: &str, share: bool) -> Result<SharingState, ApiError> {
        if crate::dataset::dataset().is_none() {
            return Err(unavailable());
        }
        api::set_dataset_consent(token, share, crate::dataset::CONSENT_VERSION).await.map_err(store_error)?;
        tracing::info!(share, "roll sharing changed");
        Ok(SharingState { available: true, share })
    }

    /// Deletes everything `user` shared or flagged, turns sharing off (with `token`, when
    /// auth can be asked), and drops the roll picture held for flagging. Returns how many
    /// rolls were deleted.
    ///
    /// Deleting never depends on auth being reachable. A save that was already under
    /// way can land just after the delete, so the delete runs again a few seconds later.
    #[tracing::instrument(name = "bff.delete_my_dataset", skip_all, err)]
    pub async fn delete_shared(user: String, token: Option<&str>, hub: Option<RollHub>) -> Result<usize, ApiError> {
        // Deleting works whenever the store is configured, even while the schema step
        // hasn't finished (then there is at most nothing, or older data, to delete).
        let ds = crate::dataset::configured().ok_or_else(unavailable)?;
        // Stop new samples first (best effort), then delete what exists.
        if let Some(token) = token {
            if let Err(e) = api::set_dataset_consent(token, false, crate::dataset::CONSENT_VERSION).await {
                tracing::warn!(error = %e, "turning sharing off failed during delete; deleting anyway");
            }
        }
        if let Some(hub) = &hub {
            hub.clear_held(&user).await;
        }
        let n = ds.delete_user_data(&user).await.map_err(store_error)?;
        tracing::info!(rolls = n, "user deleted shared roll pictures");
        let ds = ds.clone();
        api::trace::spawn_in_trace("dataset.delete_again", async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if let Some(hub) = &hub {
                hub.clear_held(&user).await;
            }
            match ds.delete_user_data(&user).await {
                Ok(0) => {}
                Ok(late) => tracing::info!(rolls = late, "deleted rolls saved during a delete"),
                Err(e) => tracing::error!(error = %e, "second delete pass failed"),
            }
        });
        Ok(n)
    }
}

/// For a server function: the request's session and roll hub, after checking it came from
/// this site.
#[cfg(not(target_arch = "wasm32"))]
fn request() -> Result<(tower_sessions::Session, Option<crate::rolls::RollHub>), ServerFnError> {
    let (session, hub) = crate::emails::server::request()?;
    Ok((session.ok_or_else(|| ServerFnError::new("not logged in"))?, hub))
}

/// For a server function: the session's token, if it holds the `arcane` permission (the
/// viewer and rule of the `/api/arcane/*` routes).
#[cfg(not(target_arch = "wasm32"))]
async fn arcane_token() -> Result<String, ServerFnError> {
    let (session, _) = request()?;
    let viewer = api::viewer(&session).await.map(crate::json_api::Viewer).map_err(|_| ServerFnError::new("not allowed"))?;
    crate::json_api::allowed(api_types::Access::Perm(api_types::KnownPermission::Arcane), &viewer)
        .and_then(|()| viewer.token().map(str::to_string))
        .map_err(|_| ServerFnError::new("not allowed"))
}

#[server(prefix = "/bff")]
pub async fn get_dataset_sharing() -> Result<SharingState, ServerFnError> {
    server::sharing(&arcane_token().await?).await.map_err(api::server_error)
}

#[server(prefix = "/bff")]
pub async fn set_dataset_sharing(share: bool) -> Result<SharingState, ServerFnError> {
    server::set_sharing(&arcane_token().await?, share).await.map_err(api::server_error)
}

/// Deletes everything the user shared or flagged. Returns how many rolls were deleted. Anyone
/// logged in may, even after losing access to the dice roller or while auth is down; a token
/// auth has dropped is logged out.
#[server(prefix = "/bff")]
pub async fn delete_my_dataset() -> Result<usize, ServerFnError> {
    let (session, hub) = request()?;
    let (user, token) = match api::viewer(&session).await.map_err(ServerFnError::new)? {
        api::Viewer::User { username, token, .. } => (username, Some(token)),
        api::Viewer::Degraded { username } => (username, None),
        api::Viewer::Anonymous => return Err(api::server_error(api_types::ApiError::logged_out())),
    };
    server::delete_shared(user, token.as_deref(), hub).await.map_err(api::server_error)
}
