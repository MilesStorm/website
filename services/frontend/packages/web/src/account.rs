//! The profile page's account settings: display name (stored by auth with the
//! account) and profile picture (SurrealDB, `profile_picture` table).
//!
//! The picture has plain HTTP routes rather than JSON so the browser can upload the
//! raw file and show it with an ordinary `<img>` (`/api/account/picture`, and its old
//! path `/api/profile/picture`; json_api.rs):
//! - `GET`: the viewer's own picture (404 when none);
//! - `POST`: raw image bytes, at most `PICTURE_MAX_UPLOAD`.
//!
//! Every upload is decoded and re-encoded to a 256x256 JPEG, so only pixels are
//! stored: no hidden data such as location, and nothing but a plain JPEG is served.

use dioxus::prelude::*;

pub use api_types::AccountInfo;

/// Largest upload accepted, in bytes (phone photos are typically 2-5 MB).
pub const PICTURE_MAX_UPLOAD: usize = 5 * 1024 * 1024;
/// Side of the stored square picture, in pixels.
#[cfg(not(target_arch = "wasm32"))]
const PICTURE_SIDE: u32 = 256;

#[cfg(not(target_arch = "wasm32"))]
mod server {
    use super::*;
    use api_types::{ApiError, ErrorCode, PictureVersion};
    use axum::{
        http::{header, HeaderValue},
        response::{IntoResponse, Response},
        Json,
    };

    use crate::json_api::{self, Viewer};

    /// The token's account, looked up with auth.
    async fn profile(token: &str) -> Result<api::AccountProfile, ApiError> {
        match api::account_profile(token).await {
            Ok(p) => Ok(p),
            Err(api::ProfileError::LoggedOut) => Err(ApiError::logged_out()),
            Err(e) => {
                tracing::error!(error = ?e, "reading the account from auth failed");
                Err(ApiError::new(ErrorCode::Upstream, "Your account can't be loaded right now. Try again later."))
            }
        }
    }

    async fn info(profile: api::AccountProfile) -> AccountInfo {
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
            shown_name: profile.display_name.clone().unwrap_or_else(|| profile.username.clone()),
            username: profile.username,
            display_name: profile.display_name,
            picture_version,
            picture_url: picture_version.map(api_types::picture_url),
            pictures_available,
            email: profile.email,
            email_verified: profile.email_verified,
        }
    }

    /// The token's account settings.
    #[tracing::instrument(name = "bff.get_account", skip_all)]
    pub async fn account(token: &str) -> Result<AccountInfo, ApiError> {
        api::trace::rejectable(async { Ok(info(profile(token).await?).await) }).await
    }

    /// Set the display name; blank clears it. Errors carry a message for the page.
    #[tracing::instrument(name = "bff.set_account_display_name", skip_all)]
    pub async fn set_name(token: &str, name: &str) -> Result<AccountInfo, ApiError> {
        api::trace::rejectable(async {
            match api::set_display_name(token, Some(name)).await {
                Ok(profile) => Ok(info(profile).await),
                Err(api::ProfileError::Invalid) => Err(ApiError::new(
                    ErrorCode::Invalid,
                    "Use at most 40 characters, without line breaks or hidden characters.",
                )),
                Err(api::ProfileError::LoggedOut) => Err(ApiError::logged_out()),
                Err(api::ProfileError::Other(e)) => {
                    tracing::error!(error = %e, "saving the display name failed");
                    Err(ApiError::new(ErrorCode::Upstream, "Couldn't save your name. Try again."))
                }
            }
        })
        .await
    }

    /// Remove the profile picture (back to the default one).
    #[tracing::instrument(name = "bff.remove_profile_picture", skip_all)]
    pub async fn remove_picture(token: &str) -> Result<AccountInfo, ApiError> {
        api::trace::rejectable(async {
            let profile = profile(token).await?;
            let ds = crate::dataset::dataset().ok_or_else(pictures_unavailable)?;
            ds.delete_profile_picture(profile.user_id).await.map_err(|e| {
                tracing::error!(error = %e, "removing the profile picture failed");
                ApiError::new(ErrorCode::Upstream, "Couldn't remove your picture. Try again.")
            })?;
            tracing::info!(user_id = profile.user_id, "profile picture removed");
            Ok(info(profile).await)
        })
        .await
    }

    fn pictures_unavailable() -> ApiError {
        ApiError::new(ErrorCode::Unavailable, "Pictures can't be changed right now.")
    }

    /// `GET /api/account/picture`: the viewer's own picture (404 when none).
    pub async fn get_picture(viewer: Viewer) -> Result<Response, json_api::ApiError> {
        let profile = profile(viewer.token()?).await?;
        let no_picture = || ApiError::new(ErrorCode::NotFound, "There is no profile picture.");
        let ds = crate::dataset::dataset().ok_or_else(no_picture)?;
        match ds.profile_picture(profile.user_id).await {
            Ok(Some((jpeg, _))) => Ok((
                [
                    (header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg")),
                    // The page asks for it with ?v=<version>, so a new picture is a new
                    // URL; private because it depends on who is logged in.
                    (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable")),
                    (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
                ],
                jpeg,
            )
                .into_response()),
            Ok(None) => Err(no_picture().into()),
            Err(e) => {
                tracing::error!(error = %e, "reading the profile picture failed");
                Err(ApiError::new(ErrorCode::Upstream, "Your picture can't be loaded right now.").into())
            }
        }
    }

    /// `POST /api/account/picture`: replace the viewer's picture with the uploaded image (raw
    /// bytes). 400 when it isn't a usable image, 413 over `PICTURE_MAX_UPLOAD`.
    pub async fn upload_picture(
        viewer: Viewer,
        json_api::Bytes(body): json_api::Bytes,
    ) -> Result<Json<PictureVersion>, json_api::ApiError> {
        let profile = profile(viewer.token()?).await?;
        let ds = crate::dataset::dataset().ok_or_else(pictures_unavailable)?;
        if body.len() > PICTURE_MAX_UPLOAD {
            return Err(ApiError::new(ErrorCode::Invalid, "That picture is too large (at most 5 MB).").with_status(413).into());
        }
        let not_image = || ApiError::new(ErrorCode::Invalid, "That file isn't a picture that can be read. Use JPEG, PNG, WebP or GIF.");
        // In a span of its own (made inside the closure), so the conversion's CPU time shows
        // under its own name.
        let convert = move || {
            use tracing_opentelemetry::OpenTelemetrySpanExt as _;
            tracing::Span::current().set_attribute("bytes", body.len() as i64);
            to_profile_jpeg(&body)
        };
        let jpeg = match api::trace::spawn_blocking("profile_picture.convert", convert).await {
            Ok(Ok(jpeg)) => jpeg,
            Ok(Err(e)) => {
                tracing::info!(error = %e, "profile picture upload is not a usable image");
                return Err(not_image().into());
            }
            Err(e) => {
                tracing::error!(error = %e, "profile picture conversion panicked");
                return Err(not_image().into());
            }
        };
        match ds.set_profile_picture(profile.user_id, &jpeg).await {
            Ok(version) => {
                tracing::info!(user_id = profile.user_id, bytes = jpeg.len(), "profile picture changed");
                Ok(Json(PictureVersion { version }))
            }
            Err(e) => {
                tracing::error!(error = %e, "saving the profile picture failed");
                Err(ApiError::new(ErrorCode::Upstream, "Couldn't save your picture. Try again.").into())
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
pub use server::{account, get_picture, remove_picture, set_name, upload_picture};

/// The session's token, for a server function: like the API, only from the site's own pages.
#[cfg(not(target_arch = "wasm32"))]
async fn session_token() -> Result<String, ServerFnError> {
    use dioxus::fullstack::FullstackContext;

    let ctx = FullstackContext::current().ok_or_else(|| ServerFnError::new("no request context"))?;
    let session = {
        let parts = ctx.parts_mut();
        if !crate::rolls::origin_allowed(&parts.headers) {
            return Err(ServerFnError::new("forbidden"));
        }
        parts.extensions.get::<tower_sessions::Session>().cloned()
    };
    let token: Option<String> = match session {
        Some(session) => session.get("opaque_token").await.ok().flatten(),
        None => None,
    };
    token.ok_or_else(|| api::server_error(api_types::ApiError::logged_out()))
}

/// The logged-in user's account settings.
#[server(prefix = "/bff")]
pub async fn get_account() -> Result<AccountInfo, ServerFnError> {
    account(&session_token().await?).await.map_err(api::server_error)
}

/// Set the display name; blank clears it. Errors carry a message for the page.
#[server(prefix = "/bff")]
pub async fn set_account_display_name(name: String) -> Result<AccountInfo, ServerFnError> {
    set_name(&session_token().await?, &name).await.map_err(api::server_error)
}

/// Remove the profile picture (back to the default one).
#[server(prefix = "/bff")]
pub async fn remove_profile_picture() -> Result<AccountInfo, ServerFnError> {
    remove_picture(&session_token().await?).await.map_err(api::server_error)
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
