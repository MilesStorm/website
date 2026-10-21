//! Profile-page actions for sharing roll pictures (`/api/account/sharing*`). The choice is
//! stored with the account by auth (PostgreSQL); the pictures in SurrealDB.
//!
//! Reading and changing the choice requires a logged-in user with the `arcane` permission;
//! deleting what was shared only the session's username. Every call needs, as a second line
//! of defence after the SameSite=Lax session cookie, a same-site Origin. Store errors are
//! logged and returned as a generic message.

pub use api_types::SharingState;
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
    let share = crate::auth::dataset_consent(token).await.map_err(store_error)?;
    Ok(SharingState { available: true, share })
}

#[tracing::instrument(name = "bff.set_dataset_sharing", skip_all, err, fields(share = share))]
pub async fn set_sharing(token: &str, share: bool) -> Result<SharingState, ApiError> {
    if crate::dataset::dataset().is_none() {
        return Err(unavailable());
    }
    crate::auth::set_dataset_consent(token, share, crate::dataset::CONSENT_VERSION).await.map_err(store_error)?;
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
        if let Err(e) = crate::auth::set_dataset_consent(token, false, crate::dataset::CONSENT_VERSION).await {
            tracing::warn!(error = %e, "turning sharing off failed during delete; deleting anyway");
        }
    }
    if let Some(hub) = &hub {
        hub.clear_held(&user).await;
    }
    let n = ds.delete_user_data(&user).await.map_err(store_error)?;
    tracing::info!(rolls = n, "user deleted shared roll pictures");
    let ds = ds.clone();
    crate::trace::spawn_in_trace("dataset.delete_again", async move {
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
