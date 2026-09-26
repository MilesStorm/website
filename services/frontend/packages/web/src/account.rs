//! The profile page's account settings: display name (stored by auth with the
//! account) and profile picture (SurrealDB, `profile_picture` table).
//!
//! The picture has plain HTTP routes rather than server functions so the browser
//! can upload the raw file and show it with an ordinary `<img>`:
//! - `GET /api/profile/picture`: the session's own picture (404 when none);
//! - `POST /api/profile/picture`: raw image bytes, at most `PICTURE_MAX_UPLOAD`.
//!
//! Every upload is decoded and re-encoded to a 256x256 JPEG, so only pixels are
//! stored: no hidden data such as location, and nothing but a plain JPEG is served.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountInfo {
    /// Login name; can't be changed.
    pub username: String,
    pub display_name: Option<String>,
    /// Version of the profile picture (for the `?v=` cache key), `None` when there
    /// is none.
    pub picture_version: Option<i64>,
    /// False when the picture store (SurrealDB) isn't reachable/configured.
    pub pictures_available: bool,
    /// `None` for accounts without one (GitHub logins).
    pub email: Option<String>,
    pub email_verified: bool,
}

impl AccountInfo {
    /// Display name, or the username when none is set.
    pub fn shown_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }

    /// URL of the profile picture, when there is one.
    pub fn picture_url(&self) -> Option<String> {
        self.picture_version.map(|v| format!("/api/profile/picture?v={v}"))
    }
}

/// Largest upload accepted, in bytes (phone photos are typically 2-5 MB).
pub const PICTURE_MAX_UPLOAD: usize = 5 * 1024 * 1024;
/// Side of the stored square picture, in pixels.
#[cfg(not(target_arch = "wasm32"))]
const PICTURE_SIDE: u32 = 256;

#[cfg(not(target_arch = "wasm32"))]
mod server {
    use super::*;
    use axum::{
        body::Bytes,
        http::{header, HeaderMap, HeaderValue, StatusCode},
        response::{IntoResponse, Response},
        Json,
    };
    use serde_json::json;

    /// The session's account, looked up with auth.
    pub(super) async fn session_account(
        session: &tower_sessions::Session,
    ) -> Result<(String, api::AccountProfile), StatusCode> {
        let token: Option<String> = session.get("opaque_token").await.ok().flatten();
        let Some(token) = token else { return Err(StatusCode::UNAUTHORIZED) };
        match api::account_profile(&token).await {
            Ok(p) => Ok((token, p)),
            Err(api::ProfileError::LoggedOut) => Err(StatusCode::UNAUTHORIZED),
            Err(e) => {
                tracing::error!(error = ?e, "reading the account from auth failed");
                Err(StatusCode::BAD_GATEWAY)
            }
        }
    }

    pub(super) async fn info(profile: api::AccountProfile) -> AccountInfo {
        let (picture_version, pictures_available) = match crate::dataset::dataset() {
            Some(ds) => match ds.profile_picture_version(profile.user_id).await {
                Ok(v) => (v, true),
                Err(e) => {
                    tracing::error!(error = %e, "reading the profile picture version failed");
                    (None, false)
                }
            },
            None => (None, false),
        };
        AccountInfo {
            username: profile.username,
            display_name: profile.display_name,
            picture_version,
            pictures_available,
            email: profile.email,
            email_verified: profile.email_verified,
        }
    }

    fn error(status: StatusCode, code: &str) -> Response {
        (status, Json(json!({ "error": code }))).into_response()
    }

    /// `GET /api/profile/picture`: the session's own picture.
    pub async fn get_picture(session: tower_sessions::Session) -> Response {
        let (_, profile) = match session_account(&session).await {
            Ok(a) => a,
            Err(s) => return s.into_response(),
        };
        let Some(ds) = crate::dataset::dataset() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match ds.profile_picture(profile.user_id).await {
            Ok(Some((jpeg, _))) => (
                [
                    (header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg")),
                    // The page asks for it with ?v=<version>, so a new picture is a new
                    // URL; private because it depends on who is logged in.
                    (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable")),
                    (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
                ],
                jpeg,
            )
                .into_response(),
            Ok(None) => StatusCode::NOT_FOUND.into_response(),
            Err(e) => {
                tracing::error!(error = %e, "reading the profile picture failed");
                StatusCode::BAD_GATEWAY.into_response()
            }
        }
    }

    /// `POST /api/profile/picture`: replace the session's picture with the uploaded
    /// image. Replies `{"version": n}`, or `{"error": code}` with 400 `not_image`,
    /// 401, 403 `origin`, 413 `too_large`, 502 `store` or 503 `unavailable`.
    pub async fn upload_picture(headers: HeaderMap, session: tower_sessions::Session, body: Bytes) -> Response {
        if !crate::rolls::origin_allowed(&headers) {
            return error(StatusCode::FORBIDDEN, "origin");
        }
        let (_, profile) = match session_account(&session).await {
            Ok(a) => a,
            Err(s) => return error(s, "login"),
        };
        let Some(ds) = crate::dataset::dataset() else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        };
        if body.len() > PICTURE_MAX_UPLOAD {
            return error(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
        }
        let jpeg = match tokio::task::spawn_blocking(move || to_profile_jpeg(&body)).await {
            Ok(Ok(jpeg)) => jpeg,
            Ok(Err(e)) => {
                tracing::info!(error = %e, "profile picture upload is not a usable image");
                return error(StatusCode::BAD_REQUEST, "not_image");
            }
            Err(e) => {
                tracing::error!(error = %e, "profile picture conversion panicked");
                return error(StatusCode::BAD_REQUEST, "not_image");
            }
        };
        match ds.set_profile_picture(profile.user_id, &jpeg).await {
            Ok(version) => {
                tracing::info!(user_id = profile.user_id, bytes = jpeg.len(), "profile picture changed");
                Json(json!({ "version": version })).into_response()
            }
            Err(e) => {
                tracing::error!(error = %e, "saving the profile picture failed");
                error(StatusCode::BAD_GATEWAY, "store")
            }
        }
    }

    /// Decode any JPEG/PNG/WebP/GIF (upright, per its orientation tag), crop the
    /// centre square and re-encode it as a `PICTURE_SIDE`² JPEG. Transparent areas
    /// become white. Decoding is bounded so a small file can't claim huge memory.
    pub(crate) fn to_profile_jpeg(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage, ImageDecoder, ImageReader, Limits};

        let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(10_000);
        limits.max_image_height = Some(10_000);
        limits.max_alloc = Some(512 * 1024 * 1024);
        reader.limits(limits);
        let mut decoder = reader.into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut img = DynamicImage::from_decoder(decoder)?;
        img.apply_orientation(orientation);

        let side = img.width().min(img.height());
        if side == 0 {
            anyhow::bail!("empty image");
        }
        let square = img.crop_imm((img.width() - side) / 2, (img.height() - side) / 2, side, side);
        let small = square.resize_exact(PICTURE_SIDE, PICTURE_SIDE, FilterType::Lanczos3).to_rgba8();
        let rgb = image::RgbImage::from_fn(PICTURE_SIDE, PICTURE_SIDE, |x, y| {
            let [r, g, b, a] = small.get_pixel(x, y).0;
            let over_white = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
            image::Rgb([over_white(r), over_white(g), over_white(b)])
        });
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 85).encode_image(&rgb)?;
        Ok(out)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use server::{get_picture, upload_picture};

#[cfg(not(target_arch = "wasm32"))]
async fn session() -> Result<tower_sessions::Session, ServerFnError> {
    use dioxus::fullstack::FullstackContext;

    let ctx = FullstackContext::current().ok_or_else(|| ServerFnError::new("no request context"))?;
    let parts = ctx.parts_mut();
    if !crate::rolls::origin_allowed(&parts.headers) {
        return Err(ServerFnError::new("forbidden"));
    }
    parts
        .extensions
        .get::<tower_sessions::Session>()
        .cloned()
        .ok_or_else(|| ServerFnError::new("not logged in"))
}

#[cfg(not(target_arch = "wasm32"))]
fn account_error(status: axum::http::StatusCode) -> ServerFnError {
    if status == axum::http::StatusCode::UNAUTHORIZED {
        ServerFnError::new("not logged in")
    } else {
        ServerFnError::new("Your account can't be loaded right now. Try again later.")
    }
}

/// The logged-in user's account settings.
#[server(prefix = "/bff")]
pub async fn get_account() -> Result<AccountInfo, ServerFnError> {
    let session = session().await?;
    let (_, profile) = server::session_account(&session).await.map_err(account_error)?;
    Ok(server::info(profile).await)
}

/// Set the display name; blank clears it. Errors carry a message for the page.
#[server(prefix = "/bff")]
pub async fn set_account_display_name(name: String) -> Result<AccountInfo, ServerFnError> {
    let session = session().await?;
    let (token, _) = server::session_account(&session).await.map_err(account_error)?;
    match api::set_display_name(&token, Some(&name)).await {
        Ok(profile) => Ok(server::info(profile).await),
        Err(api::ProfileError::Invalid) => Err(ServerFnError::new(
            "Use at most 40 characters, without line breaks or hidden characters.",
        )),
        Err(api::ProfileError::LoggedOut) => Err(ServerFnError::new("not logged in")),
        Err(api::ProfileError::Other(e)) => {
            tracing::error!(error = %e, "saving the display name failed");
            Err(ServerFnError::new("Couldn't save your name. Try again."))
        }
    }
}

/// Remove the profile picture (back to the default one).
#[server(prefix = "/bff")]
pub async fn remove_profile_picture() -> Result<AccountInfo, ServerFnError> {
    let session = session().await?;
    let (_, profile) = server::session_account(&session).await.map_err(account_error)?;
    let ds = crate::dataset::dataset()
        .ok_or_else(|| ServerFnError::new("Pictures aren't available right now."))?;
    ds.delete_profile_picture(profile.user_id).await.map_err(|e| {
        tracing::error!(error = %e, "removing the profile picture failed");
        ServerFnError::new("Couldn't remove your picture. Try again.")
    })?;
    tracing::info!(user_id = profile.user_id, "profile picture removed");
    Ok(server::info(profile).await)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::server::to_profile_jpeg;
    use image::{GenericImageView, ImageFormat, Rgba, RgbaImage};

    fn encode(img: &RgbaImage, format: ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img.clone()).write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn any_image_becomes_a_256_square_jpeg() {
        // Wide PNG with a transparent left half: the centre square is kept and
        // transparency turns white.
        let img = RgbaImage::from_fn(600, 300, |x, _| if x < 300 { Rgba([0, 0, 0, 0]) } else { Rgba([200, 0, 0, 255]) });
        let jpeg = to_profile_jpeg(&encode(&img, ImageFormat::Png)).unwrap();
        let out = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg).unwrap();
        assert_eq!(out.dimensions(), (256, 256));
        let left = out.get_pixel(10, 128).0;
        let right = out.get_pixel(245, 128).0;
        assert!(left[0] > 240 && left[1] > 240 && left[2] > 240, "transparent -> white, got {left:?}");
        assert!(right[0] > 150 && right[1] < 60, "red kept, got {right:?}");
    }

    #[test]
    fn non_images_are_refused() {
        assert!(to_profile_jpeg(b"hello, not an image").is_err());
        assert!(to_profile_jpeg(b"").is_err());
        // A truncated PNG.
        let png = encode(&RgbaImage::new(50, 50), ImageFormat::Png);
        assert!(to_profile_jpeg(&png[..png.len() / 2]).is_err());
    }

    #[test]
    fn oversized_dimensions_are_refused() {
        // A tiny GIF header claiming 60000x60000 pixels.
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&60000u16.to_le_bytes());
        gif.extend_from_slice(&60000u16.to_le_bytes());
        gif.extend_from_slice(&[0, 0, 0, 0x2C, 0, 0, 0, 0]);
        assert!(to_profile_jpeg(&gif).is_err());
    }
}
