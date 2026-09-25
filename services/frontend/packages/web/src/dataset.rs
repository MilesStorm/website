//! Training-data capture (server only): opted-in users' rolls are sampled, and any
//! user can flag a wrong roll, into SurrealDB (schema: `surreal/dataset.surql`).
//! Storage split: the opt-in choice lives with the account in auth's PostgreSQL
//! (`api::dataset_consent`); the picture held for flagging and rate counters live in
//! Redis (`capture.rs`); saved rolls, pictures and the deletion log live here.
//!
//! Talks to SurrealDB's HTTP `/rpc` endpoint with JSON and Basic auth as the
//! database-level `dice` user. Pictures travel as base64 and are stored as bytes.
//! Every failure is logged and swallowed by callers: training data must never break
//! the camera stream.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

/// Bump when the consent wording on the profile page changes (stored by auth).
pub const CONSENT_VERSION: &str = "1";
/// Below this confidence a die counts as "unsure" and the roll is always kept.
pub const UNSURE_CONF: f64 = 0.9;
const DEFAULT_SAMPLE_EVERY: u32 = 20;
/// Per request; a slow store only delays the capture worker, never live rolls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Set once at startup; None when the store isn't configured (e.g. local dev).
pub static DATASET: OnceLock<Option<Dataset>> = OnceLock::new();

pub fn dataset() -> Option<&'static Dataset> {
    DATASET.get().and_then(Option::as_ref)
}

#[derive(Clone)]
pub struct Dataset {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    rpc_url: String,
    user: String,
    pass: String,
    ns: String,
    db: String,
    sample_every: u32,
}

/// A roll plus the camera frame that showed it, as held for flagging.
pub struct Capture {
    pub roll_id: String,
    /// The roll event JSON from ai_pipeline.
    pub roll: Value,
    /// The last per-frame result JSON (all detections), if any.
    pub frame: Option<Value>,
    pub jpeg: Vec<u8>,
}

impl Dataset {
    /// From `SURREAL_URL` (server base, e.g. the in-cluster service), `SURREAL_USER`,
    /// `SURREAL_PASS`, optional `SURREAL_NS` / `SURREAL_DB` (default milesstorm /
    /// arcane) and `DATASET_SAMPLE_EVERY` (default 20). None if any required one is unset.
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let url = var("SURREAL_URL")?;
        let sample_every = var("DATASET_SAMPLE_EVERY")
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_SAMPLE_EVERY);
        Some(Self {
            inner: Arc::new(Inner {
                http: reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().ok()?,
                rpc_url: format!("{}/rpc", url.trim_end_matches('/')),
                user: var("SURREAL_USER")?,
                pass: var("SURREAL_PASS")?,
                ns: var("SURREAL_NS").unwrap_or_else(|| "milesstorm".into()),
                db: var("SURREAL_DB").unwrap_or_else(|| "arcane".into()),
                sample_every,
            }),
        })
    }

    pub fn sample_every(&self) -> u32 {
        self.inner.sample_every
    }

    /// Run SurrealQL; returns each statement's result, or the first error.
    async fn query(&self, sql: &str, vars: Value) -> anyhow::Result<Vec<Value>> {
        let i = &self.inner;
        let resp = i
            .http
            .post(&i.rpc_url)
            .basic_auth(&i.user, Some(&i.pass))
            .header("Accept", "application/json")
            .header("surreal-ns", &i.ns)
            .header("surreal-db", &i.db)
            .header("surreal-auth-ns", &i.ns)
            .header("surreal-auth-db", &i.db)
            .json(&json!({"id": 1, "method": "query", "params": [sql, vars]}))
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        if let Some(err) = body.get("error") {
            anyhow::bail!("surrealdb {status}: {err}");
        }
        let results = body
            .get("result")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("surrealdb {status}: no result"))?;
        results
            .iter()
            .map(|r| match r.get("status").and_then(Value::as_str) {
                Some("OK") => Ok(r.get("result").cloned().unwrap_or(Value::Null)),
                _ => Err(anyhow::anyhow!("surrealdb: {}", r.get("result").unwrap_or(r))),
            })
            .collect()
    }

    /// Delete every sample and picture the user shared or flagged; returns how many
    /// rolls. The deletion is logged (username and time only) so copies already
    /// pulled for training are removed too.
    pub async fn delete_user_data(&self, user: &str) -> anyhow::Result<usize> {
        let r = self
            .query(
                "DELETE roll_sample WHERE username = $user RETURN BEFORE; \
                 DELETE roll_image WHERE username = $user RETURN NONE; \
                 CREATE dataset_deletion CONTENT { username: $user } RETURN NONE;",
                json!({"user": user}),
            )
            .await?;
        Ok(r.first().and_then(Value::as_array).map_or(0, Vec::len))
    }

    /// Store (or refresh) the roll and its picture. `auto_reason` marks an automatic
    /// sample; `flag` marks an explicit "wrong roll" with the user's corrections.
    ///
    /// Rules, enforced in the query: an automatic re-send of a roll never replaces a
    /// flagged or already reviewed sample, and nothing replaces the picture of a
    /// reviewed one (the owner's labels belong to that picture).
    pub async fn save(
        &self,
        user: &str,
        c: &Capture,
        auto_reason: Option<&str>,
        flag: Option<&[Option<String>]>,
    ) -> anyhow::Result<()> {
        const SAVE: &str = "
            LET $rec = type::record('roll_sample', [$user, $roll_id]);
            LET $reason_ = $reason ?? NONE;
            LET $values_ = $values ?? NONE;
            LET $existing = (SELECT flagged, review_status FROM ONLY $rec);
            LET $locked = $existing != NONE
                AND ($existing.review_status != 'new' OR ($reason_ != NONE AND $existing.flagged));
            IF !$locked {
                UPSERT type::record('roll_image', [$user, $roll_id])
                    MERGE { username: $user, jpeg: encoding::base64::decode($jpeg) } RETURN NONE;
                UPSERT $rec MERGE {
                    username: $user, roll_id: $roll_id, roll: $roll, model: $model,
                    image: type::record('roll_image', [$user, $roll_id]) } RETURN NONE;
                IF ($frame ?? NONE) != NONE { UPDATE $rec SET frame = $frame RETURN NONE; };
                IF $reason_ != NONE { UPDATE $rec SET auto_reason = $reason_ RETURN NONE; };
            };
            IF $values_ != NONE {
                UPDATE $rec SET flagged = true, flagged_at = time::now(), user_values = $values_ RETURN NONE;
            };";
        let model = c.roll.get("model").and_then(Value::as_str).unwrap_or("unknown");
        // Absent keys arrive as NONE; JSON null would be NULL, which the typed
        // fields reject, so only present values are sent.
        let mut vars = json!({
            "user": user,
            "roll_id": c.roll_id,
            "jpeg": base64::engine::general_purpose::STANDARD.encode(&c.jpeg),
            "roll": c.roll,
            "model": model,
        });
        if let Some(frame) = &c.frame {
            vars["frame"] = frame.clone();
        }
        if let Some(reason) = auto_reason {
            vars["reason"] = reason.into();
        }
        if let Some(values) = flag {
            vars["values"] = json!(values);
        }
        self.query(SAVE, vars).await?;
        Ok(())
    }
}

/// Whether the session's user shares roll pictures (asked fresh each time, so
/// switching it off applies on every replica at once). Bounded like store calls.
pub async fn shares(token: &str) -> anyhow::Result<bool> {
    match tokio::time::timeout(REQUEST_TIMEOUT, api::dataset_consent(token)).await {
        Ok(Ok(share)) => Ok(share),
        Ok(Err(e)) => anyhow::bail!("consent lookup: {e}"),
        Err(_) => anyhow::bail!("consent lookup timed out"),
    }
}

/// Why an opted-in user's roll should be kept automatically, if at all:
/// "unsure" when any die is unreadable or below `UNSURE_CONF`; otherwise
/// "sampled" for about one roll in `every`, picked from the roll id so every
/// re-emission of the same roll gets the same answer.
pub fn auto_reason(roll: &Value, roll_id: &str, every: u32) -> Option<&'static str> {
    let dice = roll.get("dice").and_then(Value::as_array)?;
    let unsure = roll.get("complete").and_then(Value::as_bool) != Some(true)
        || dice.iter().any(|d| {
            d.get("value").is_none_or(Value::is_null)
                || d.get("conf").and_then(Value::as_f64).unwrap_or(0.0) < UNSURE_CONF
        });
    if unsure {
        return Some("unsure");
    }
    (stable_hash(roll_id) % u64::from(every.max(1)) == 0).then_some("sampled")
}

/// FNV-1a: stable across processes and replicas (unlike std's randomized hasher).
fn stable_hash(s: &str) -> u64 {
    s.bytes()
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// Valid face labels: "0" (the d10's zero) through "20".
pub fn is_face(v: &str) -> bool {
    matches!(v.parse::<u8>(), Ok(0..=20)) && !v.starts_with('+') && (v == "0" || !v.starts_with('0'))
}

/// Roll ids are generated by ai_pipeline as `<hex ms>-<8 hex>` (lowercase). Checked
/// before a roll id is used in any Redis key or record id.
pub fn is_roll_id(v: &str) -> bool {
    let hex = |s: &str| s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    match v.split_once('-') {
        Some((ms, r)) => (1..=16).contains(&ms.len()) && hex(ms) && r.len() == 8 && hex(r),
        None => false,
    }
}

/// Normalise the popup's corrections: one per die, empty → null, only valid faces.
pub fn clean_values(values: &[Option<String>], dice: usize) -> Option<Vec<Option<String>>> {
    if values.len() != dice {
        return None;
    }
    values
        .iter()
        .map(|v| match v.as_deref().map(str::trim) {
            None | Some("") => Some(None),
            Some(f) if is_face(f) => Some(Some(f.to_string())),
            Some(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roll(dice: Value, complete: bool) -> Value {
        json!({"type": "roll", "dice": dice, "complete": complete})
    }

    #[test]
    fn unreadable_or_low_confidence_is_unsure() {
        let r = roll(json!([{"value": "5", "conf": 0.99}, {"value": null, "conf": 0.4}]), false);
        assert_eq!(auto_reason(&r, "a-1", 1_000_000), Some("unsure"));
        let r = roll(json!([{"value": "5", "conf": 0.85}]), true);
        assert_eq!(auto_reason(&r, "a-1", 1_000_000), Some("unsure"));
    }

    #[test]
    fn confident_rolls_are_sampled_about_one_in_n_and_stably() {
        let r = roll(json!([{"value": "5", "conf": 0.99}]), true);
        let kept = (0..2000)
            .filter(|i| auto_reason(&r, &format!("18f{i:x}-{:08x}", i * 7919), 20).is_some())
            .count();
        assert!((60..=140).contains(&kept), "kept {kept} of 2000");
        let id = "18f3a-0badf00d";
        assert_eq!(auto_reason(&r, id, 20), auto_reason(&r, id, 20));
        assert_eq!(auto_reason(&r, id, 1), Some("sampled"));
    }

    #[test]
    fn faces_and_ids() {
        for ok in ["0", "1", "9", "10", "20"] {
            assert!(is_face(ok), "{ok}");
        }
        for bad in ["21", "-1", "05", "+5", "a", "", "1.5", "200"] {
            assert!(!is_face(bad), "{bad}");
        }
        assert!(is_roll_id("18f3a2b-0badf00d"));
        for bad in ["x'); DROP", "", "18f3a2b", "18f3a2b-0badf00", "18F3-0badf00d", "1-2-0badf00d", "-0badf00d"] {
            assert!(!is_roll_id(bad), "{bad}");
        }
    }

    #[test]
    fn corrections_are_cleaned() {
        let v = |s: &[Option<&str>]| s.iter().map(|o| o.map(String::from)).collect::<Vec<_>>();
        assert_eq!(
            clean_values(&v(&[Some("5"), Some(" "), None, Some("12")]), 4),
            Some(vec![Some("5".into()), None, None, Some("12".into())])
        );
        assert_eq!(clean_values(&v(&[Some("5")]), 2), None, "wrong count");
        assert_eq!(clean_values(&v(&[Some("99")]), 1), None, "not a face");
    }
}
