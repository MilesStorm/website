//! Profile-page actions for sharing roll pictures (server functions). The choice is
//! stored with the account by auth (PostgreSQL); the pictures in SurrealDB.
//!
//! Each call requires a logged-in user with the `arcane` permission and, as a
//! second line of defence after the SameSite=Lax session cookie, a same-site
//! Origin. Store errors are logged and returned as a generic message.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharingState {
    /// False when the training-data store isn't configured on this server.
    pub available: bool,
    pub share: bool,
}

/// The calling user (username, session token, roll hub), checked as described above.
/// `need_arcane` is false only for deleting: anyone may remove what they shared,
/// even after losing access to the dice roller.
#[cfg(not(target_arch = "wasm32"))]
async fn caller(need_arcane: bool) -> Result<(String, String, Option<crate::rolls::RollHub>), ServerFnError> {
    use dioxus::fullstack::FullstackContext;

    let ctx = FullstackContext::current().ok_or_else(|| ServerFnError::new("no request context"))?;
    let (session, hub, origin_ok) = {
        let parts = ctx.parts_mut();
        (
            parts.extensions.get::<tower_sessions::Session>().cloned(),
            parts.extensions.get::<crate::rolls::RollHub>().cloned(),
            crate::rolls::origin_allowed(&parts.headers),
        )
    };
    if !origin_ok {
        return Err(ServerFnError::new("forbidden"));
    }
    let session = session.ok_or_else(|| ServerFnError::new("not logged in"))?;
    let user = if need_arcane {
        crate::rolls::arcane_user(&session)
            .await
            .map_err(|_| ServerFnError::new("not allowed"))?
    } else {
        session
            .get::<String>("username")
            .await
            .ok()
            .flatten()
            .ok_or_else(|| ServerFnError::new("not logged in"))?
    };
    let token: String = session.get("opaque_token").await.ok().flatten().unwrap_or_default();
    Ok((user, token, hub))
}

#[cfg(not(target_arch = "wasm32"))]
fn store_error(e: impl std::fmt::Display) -> ServerFnError {
    tracing::error!(error = %e, "roll sharing store failed");
    ServerFnError::new("The picture store is unavailable. Try again later.")
}

#[server(prefix = "/bff")]
pub async fn get_dataset_sharing() -> Result<SharingState, ServerFnError> {
    let (_, token, _) = caller(true).await?;
    if crate::dataset::dataset().is_none() {
        return Ok(SharingState { available: false, share: false });
    }
    let share = api::dataset_consent(&token).await.map_err(store_error)?;
    Ok(SharingState { available: true, share })
}

#[server(prefix = "/bff")]
pub async fn set_dataset_sharing(share: bool) -> Result<SharingState, ServerFnError> {
    let (_, token, _) = caller(true).await?;
    if crate::dataset::dataset().is_none() {
        return Err(ServerFnError::new("Sharing isn't available."));
    }
    api::set_dataset_consent(&token, share, crate::dataset::CONSENT_VERSION)
        .await
        .map_err(store_error)?;
    tracing::info!(share, "roll sharing changed");
    Ok(SharingState { available: true, share })
}

/// Deletes everything the user shared or flagged, turns sharing off, and drops the
/// roll picture held for flagging. Returns how many rolls were deleted.
///
/// Deleting never depends on auth being reachable. A save that was already under
/// way can land just after the delete, so the delete runs again a few seconds later.
#[server(prefix = "/bff")]
pub async fn delete_my_dataset() -> Result<usize, ServerFnError> {
    let (user, token, hub) = caller(false).await?;
    // Deleting works whenever the store is configured, even while the schema step
    // hasn't finished (then there is at most nothing, or older data, to delete).
    let ds = crate::dataset::configured().ok_or_else(|| ServerFnError::new("Sharing isn't available."))?;
    // Stop new samples first (best effort), then delete what exists.
    if let Err(e) = api::set_dataset_consent(&token, false, crate::dataset::CONSENT_VERSION).await {
        tracing::warn!(error = %e, "turning sharing off failed during delete; deleting anyway");
    }
    if let Some(hub) = &hub {
        hub.clear_held(&user).await;
    }
    let n = ds.delete_user_data(&user).await.map_err(store_error)?;
    tracing::info!(rolls = n, "user deleted shared roll pictures");
    let (ds, hub) = (ds.clone(), hub.clone());
    tokio::spawn(async move {
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
