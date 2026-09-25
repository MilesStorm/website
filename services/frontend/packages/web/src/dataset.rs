//! Training-data capture (server only): opted-in users' rolls are sampled, and any
//! user can flag a wrong roll, into SurrealDB (schema: `surreal/database/schema/`, managed with surrealkit).
//! Storage split: the opt-in choice lives with the account in auth's PostgreSQL
//! (`api::dataset_consent`); the picture held for flagging and rate counters live in
//! Redis (`capture.rs`); saved rolls, pictures and the deletion log live here.
//!
//! Talks to SurrealDB's HTTP `/rpc` endpoint with JSON and Basic auth as the
//! database-level `dice` user. Pictures travel as base64 and are stored as bytes.
//!
//! The tables are defined in `surreal/database/schema/` and applied by the website
//! itself at startup with surrealkit (like auth's sqlx migrations). The store only
//! becomes available once they are, so nothing is written before the tables exist.
//! Every failure is logged and swallowed by callers: training data must never break
//! the camera stream.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use tower_sessions_redis_store::fred::prelude::*;

/// Bump when the consent wording on the profile page changes (stored by auth).
pub const CONSENT_VERSION: &str = "1";
/// Below this confidence a die counts as "unsure" and the roll is always kept.
pub const UNSURE_CONF: f64 = 0.9;
const DEFAULT_SAMPLE_EVERY: u32 = 20;
/// Per request; a slow store only delays the capture worker, never live rolls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

// The table definitions, compiled in (`build.rs` rebuilds when files are added).
surrealkit::embed_schema!("../../surreal/database/schema");

/// The configured store, set at startup ([`start`]); unset in local dev without
/// SurrealDB settings.
static STORE: OnceLock<Dataset> = OnceLock::new();
/// Whether the schema has been applied by this process ([`start`]).
static SCHEMA_READY: AtomicBool = AtomicBool::new(false);

/// The store, for saving: only once the schema is applied, so nothing is written
/// before the tables exist. None means sharing and flagging are off.
pub fn dataset() -> Option<&'static Dataset> {
    SCHEMA_READY.load(Ordering::Acquire).then(|| STORE.get()).flatten()
}

/// The store whenever it's configured, schema applied or not. Only for deleting what
/// users shared: that must keep working even while the schema step can't finish.
pub fn configured() -> Option<&'static Dataset> {
    STORE.get()
}

/// One attempt to apply the schema may take this long before it's given up.
const SCHEMA_ATTEMPT_LIMIT: Duration = Duration::from_secs(600);
const SCHEMA_RETRY_MAX: Duration = Duration::from_secs(60);
/// While one replica is waiting for the other, it logs this often.
const SCHEMA_WAIT_LOG: Duration = Duration::from_secs(60);

/// Applies the schema once per process, one replica at a time (see [`SchemaLock`]),
/// retrying until SurrealDB accepts it, then turns sharing on (like sqlx migrations
/// at startup). Runs in the background: the website serves pages meanwhile.
pub async fn start(store: Dataset, redis: Pool) {
    let _ = STORE.set(store.clone());
    let lock = SchemaLock::new(redis);
    let mut delay = Duration::from_secs(1);
    let mut waiting_since: Option<tokio::time::Instant> = None;
    loop {
        match lock.acquire().await {
            Ok(true) => waiting_since = None,
            Ok(false) => {
                let since = *waiting_since.get_or_insert_with(tokio::time::Instant::now);
                let waited = since.elapsed();
                if waited.as_secs() % SCHEMA_WAIT_LOG.as_secs() < SchemaLock::POLL.as_secs() {
                    tracing::info!(waited_s = waited.as_secs(), "roll-sharing schema: another replica is applying it; waiting");
                }
                tokio::time::sleep(SchemaLock::POLL).await;
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, retry_in = ?delay, "roll-sharing schema: Redis lock failed");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(SCHEMA_RETRY_MAX);
                continue;
            }
        }
        match lock.hold_while(tokio::time::timeout(SCHEMA_ATTEMPT_LIMIT, store.apply_schema())).await {
            Ok(Ok(())) => {
                lock.release().await;
                break;
            }
            Ok(Err(e)) => {
                lock.release().await;
                tracing::warn!(error = %format!("{e:#}"), retry_in = ?delay, "applying the roll-sharing schema failed");
            }
            // Not released: SurrealDB may still be running the abandoned statements,
            // and another replica must not start alongside them. The lock expires
            // SchemaLock::TTL after its last renewal.
            Err(_) => tracing::warn!(retry_in = ?delay, "applying the roll-sharing schema timed out"),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(SCHEMA_RETRY_MAX);
    }
    SCHEMA_READY.store(true, Ordering::Release);
    tracing::info!("roll-sharing schema applied; sharing and flagging are on");
}

/// Makes replicas take turns applying the schema: two surrealkit runs at once leave
/// its bookkeeping (`__entity`) inconsistent, and every later run then fails (seen
/// with two replicas starting together). The lock lives in Redis, which all
/// replicas share, under a token only this process knows; it is renewed while held
/// and expires by itself if the holder dies.
struct SchemaLock {
    redis: Pool,
    token: String,
}

impl SchemaLock {
    const KEY: &'static str = "arcane:schema_lock";
    const TTL: Duration = Duration::from_secs(30);
    const RENEW_EVERY: Duration = Duration::from_secs(10);
    /// How often a waiting replica checks whether the lock is free.
    const POLL: Duration = Duration::from_secs(2);
    /// Extend or delete the lock only while it still holds our token.
    const RENEW: &'static str =
        "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('pexpire', KEYS[1], ARGV[2]) else return 0 end";
    const RELEASE: &'static str =
        "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end";

    fn new(redis: Pool) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let host = std::env::var("HOSTNAME").unwrap_or_default();
        Self { redis, token: format!("{host}:{}:{nanos}", std::process::id()) }
    }

    /// True once this process holds the lock; false while another one does.
    async fn acquire(&self) -> Result<bool, Error> {
        let ttl = Some(Expiration::PX(Self::TTL.as_millis() as i64));
        let set: Option<String> = self.redis.set(Self::KEY, self.token.as_str(), ttl, Some(SetOptions::NX), false).await?;
        if set.is_some() {
            return Ok(true);
        }
        // A retried SET whose first reply was lost also answers "exists": check whose.
        let holder: Option<String> = self.redis.get(Self::KEY).await?;
        Ok(holder.as_deref() == Some(self.token.as_str()))
    }

    /// Runs `work`, renewing the lock meanwhile so it can't expire under a slow run.
    async fn hold_while<T>(&self, work: impl std::future::Future<Output = T>) -> T {
        tokio::pin!(work);
        let mut renew = tokio::time::interval(Self::RENEW_EVERY);
        renew.tick().await; // the first tick is immediate
        loop {
            tokio::select! {
                out = &mut work => return out,
                _ = renew.tick() => {
                    let ttl = Self::TTL.as_millis().to_string();
                    let renewed: Result<i64, _> = self.redis.eval(Self::RENEW, vec![Self::KEY], vec![self.token.as_str(), ttl.as_str()]).await;
                    match renewed {
                        Ok(1) => {}
                        Ok(_) => tracing::error!("roll-sharing schema: lost the Redis lock while applying"),
                        Err(e) => tracing::warn!(error = %e, "roll-sharing schema: renewing the Redis lock failed"),
                    }
                }
            }
        }
    }

    async fn release(&self) {
        let released: Result<i64, _> = self.redis.eval(Self::RELEASE, vec![Self::KEY], vec![self.token.as_str()]).await;
        if let Err(e) = released {
            // It expires by itself; the other replica just waits a little longer.
            tracing::warn!(error = %e, "roll-sharing schema: releasing the Redis lock failed");
        }
    }
}

#[derive(Clone)]
pub struct Dataset {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    /// Server base URL (`SURREAL_URL`).
    url: String,
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
                url,
                user: var("SURREAL_USER")?,
                pass: var("SURREAL_PASS")?,
                ns: var("SURREAL_NS").unwrap_or_else(|| "milesstorm".into()),
                db: var("SURREAL_DB").unwrap_or_else(|| "arcane".into()),
                sample_every,
            }),
        })
    }

    /// Brings SurrealDB's tables in line with `surreal/database/schema/` (surrealkit
    /// sync: only changed files are applied; nothing is dropped). Signs in as the
    /// website's own login.
    async fn apply_schema(&self) -> anyhow::Result<()> {
        let i = &self.inner;
        let cfg = surrealkit::DbCfg::from_env(
            None,
            &surrealkit::DbOverrides {
                host: Some(i.url.clone()),
                ns: Some(i.ns.clone()),
                db: Some(i.db.clone()),
                user: Some(i.user.clone()),
                pass: Some(i.pass.clone()),
                auth_level: Some("database".into()),
                folder: None,
            },
        )?;
        let db = surrealkit::connect(&cfg).await?;
        // No pruning: a table or field that disappears from the files is left in place
        // (and logged by surrealkit) rather than dropped with the users' pictures in it.
        // Removing one is a deliberate surrealkit rollout, not a side effect of a deploy.
        surrealkit::Sync::embedded(embedded_schema::SCHEMA).prune(false).run(&db).await
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

    /// The account's profile picture (JPEG) and when it was last changed (ms since
    /// 1970, used to tell browsers a new one exists), if it has one.
    pub async fn profile_picture(&self, user_id: i64) -> anyhow::Result<Option<(Vec<u8>, i64)>> {
        let r = self
            .query(
                "SELECT encoding::base64::encode(jpeg) AS jpeg, time::millis(updated_at) AS version \
                 FROM ONLY type::record('profile_picture', $id);",
                json!({"id": user_id}),
            )
            .await?;
        let Some(row) = r.first().filter(|v| v.is_object()) else { return Ok(None) };
        let jpeg = row.get("jpeg").and_then(Value::as_str).unwrap_or_default();
        // SurrealDB's `encoding::base64::encode` leaves out the `=` padding.
        const ANY_PADDING: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            base64::engine::GeneralPurposeConfig::new()
                .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
        );
        let jpeg = ANY_PADDING.decode(jpeg)?;
        let version = row.get("version").and_then(Value::as_i64).unwrap_or(0);
        Ok(Some((jpeg, version)))
    }

    /// When the account's profile picture was last changed (see `profile_picture`),
    /// without loading it. `None` when it has none.
    pub async fn profile_picture_version(&self, user_id: i64) -> anyhow::Result<Option<i64>> {
        let r = self
            .query(
                "SELECT VALUE time::millis(updated_at) FROM ONLY type::record('profile_picture', $id);",
                json!({"id": user_id}),
            )
            .await?;
        Ok(r.first().and_then(Value::as_i64))
    }

    /// Replace the account's profile picture; returns its new version.
    pub async fn set_profile_picture(&self, user_id: i64, jpeg: &[u8]) -> anyhow::Result<i64> {
        let r = self
            .query(
                "UPSERT type::record('profile_picture', $id) \
                 CONTENT { jpeg: encoding::base64::decode($jpeg) } \
                 RETURN VALUE time::millis(updated_at);",
                json!({"id": user_id, "jpeg": base64::engine::general_purpose::STANDARD.encode(jpeg)}),
            )
            .await?;
        r.first()
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("surrealdb: no version returned"))
    }

    pub async fn delete_profile_picture(&self, user_id: i64) -> anyhow::Result<()> {
        self.query(
            "DELETE type::record('profile_picture', $id) RETURN NONE;",
            json!({"id": user_id}),
        )
        .await?;
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
