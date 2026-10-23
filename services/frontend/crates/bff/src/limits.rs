//! Rate limits on password login and registration: per visitor (Cloudflare's `CF-Connecting-IP`,
//! as the Gateway's own limits use) and per username, so neither one address nor many addresses
//! can try passwords for an account without end. Over a limit, the API answers 429
//! `rate_limited` before auth is asked.
//!
//! The counts are kept in each replica's memory, like the Gateway's per-pod limits: a restart
//! forgets them, and with N replicas the real ceiling is up to N times the number here.
//!
//! A key is held only while it has slots counted: a slot given back to 0 removes it.
//!
//! The per-username limit means anyone can lock an account out of password login for its window
//! (15 minutes) by failing ten logins for it; GitHub and Google logins still work then
//! (TRACING.md, "Rate limits").

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use api_types::{ApiError, ErrorCode};
use axum::http::HeaderMap;

/// Fixed windows: at most `max` slots per key in each `window`. A slot is taken before the work
/// it limits starts ([`Limit::try_take`]), so concurrent requests can't all pass the check before
/// any of them is counted, and given back when the work turns out not to count.
pub(crate) struct Limit {
    /// `limit` on `bff_rate_limit_overflow_total`.
    name: &'static str,
    max: u32,
    window: Duration,
    state: Mutex<State>,
}

struct State {
    hits: HashMap<String, (Instant, u32)>,
    last_sweep: Instant,
}

/// The most keys one limit holds: a bound on memory only. Past it, after a sweep, a new key's
/// request is let through uncounted (`bff_rate_limit_overflow_total` and a warning): refusing it
/// would let a flood of addresses or usernames lock everyone out. The other limit of the pair
/// (per username for a flood of addresses, and the other way round) still holds the flood.
const MAX_KEYS: usize = 100_000;

/// A slot taken by [`Limit::try_take`]: the window it counts in, or none when the key wasn't
/// tracked (the map was full).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Slot(Option<Instant>);

impl Limit {
    pub(crate) fn new(name: &'static str, max: u32, window: Duration) -> Self {
        Self { name, max, window, state: Mutex::new(State { hits: HashMap::new(), last_sweep: Instant::now() }) }
    }

    /// Takes one of `key`'s slots in the current window; None when none is left.
    pub(crate) fn try_take(&self, key: &str, now: Instant) -> Option<Slot> {
        let mut state = self.state.lock().expect("limit lock");
        if !state.hits.contains_key(key) && state.hits.len() >= MAX_KEYS {
            // Expired windows go at most once a window: the sweep is O(keys), under the lock.
            if now.duration_since(state.last_sweep) >= self.window {
                let window = self.window;
                state.hits.retain(|_, &mut (start, _)| now.duration_since(start) < window);
                state.last_sweep = now;
            }
            if state.hits.len() >= MAX_KEYS {
                drop(state);
                metrics::counter!("bff_rate_limit_overflow_total", "limit" => self.name).increment(1);
                tracing::warn!(limit = self.name, keys = MAX_KEYS, "rate limit full: a request passed uncounted");
                return Some(Slot(None));
            }
        }
        let entry = state.hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.max {
            return None;
        }
        entry.1 += 1;
        Some(Slot(Some(entry.0)))
    }

    /// Gives back `slot`, in the window it was taken in (one that has ended since is left alone).
    /// A key whose count reaches 0 is removed, so work that didn't count holds no key.
    pub(crate) fn release(&self, key: &str, slot: Slot) {
        let Slot(Some(window)) = slot else { return };
        let mut state = self.state.lock().expect("limit lock");
        if let Some(entry) = state.hits.get_mut(key) {
            if entry.0 == window {
                entry.1 = entry.1.saturating_sub(1);
                if entry.1 == 0 {
                    state.hits.remove(key);
                }
            }
        }
    }

    /// Forgets `key` (a login that worked clears its username's failures).
    pub(crate) fn clear(&self, key: &str) {
        self.state.lock().expect("limit lock").hits.remove(key);
    }
}

/// The limits of the login and registration endpoints.
pub(crate) struct Limits {
    /// Failed password logins per visitor.
    pub(crate) login_ip: Limit,
    /// Failed password logins per username.
    pub(crate) login_user: Limit,
    /// Registrations per visitor.
    pub(crate) register_ip: Limit,
    /// Registrations per username asked for.
    pub(crate) register_user: Limit,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            login_ip: Limit::new("login_visitor", 30, Duration::from_secs(10 * 60)),
            login_user: Limit::new("login_username", 10, Duration::from_secs(15 * 60)),
            register_ip: Limit::new("register_visitor", 10, Duration::from_secs(60 * 60)),
            register_user: Limit::new("register_username", 5, Duration::from_secs(60 * 60)),
        }
    }
}

/// Takes a slot of each of `limits` (limit, key), or of none: None, with nothing held, when one
/// is used up. The slots are in the order of `limits`.
pub(crate) fn take_all(limits: &[(&Limit, &str)], now: Instant) -> Option<Vec<Slot>> {
    let mut taken = Vec::with_capacity(limits.len());
    for (limit, key) in limits {
        match limit.try_take(key, now) {
            Some(slot) => taken.push(slot),
            None => {
                for ((limit, key), slot) in limits.iter().zip(taken) {
                    limit.release(key, slot);
                }
                return None;
            }
        }
    }
    Some(taken)
}

/// Requests without a usable `CF-Connecting-IP` (that didn't come through Cloudflare) share one
/// visitor: they are limited together, never skipped.
pub(crate) const UNKNOWN_VISITOR: &str = "unknown";

/// The visitor, as Cloudflare saw it: an IPv4 address (also when written IPv4-mapped, `::ffff:a.b.c.d`),
/// or an IPv6 address's /56. A subscriber is usually delegated a /56 (RIPE-690's recommendation
/// for homes; some get a /48, a few one /64) and can use any /64 in it, so keying by /64 would
/// give one household 256 visitors' worth of tries; a /56 rarely holds more than one household.
/// The header is trusted because only Cloudflare reaches the origin (homelab #98).
pub(crate) fn visitor(headers: &HeaderMap) -> String {
    let ip = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<IpAddr>().ok());
    let ip = match ip {
        Some(IpAddr::V6(v6)) => Some(v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4)),
        other => other,
    };
    match ip {
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(IpAddr::V6(v6)) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/56", s[0], s[1], s[2], s[3] & 0xff00)
        }
        None => UNKNOWN_VISITOR.to_owned(),
    }
}

/// The longest username an account can have (auth's `users.username varchar(255)`).
pub(crate) const MAX_USERNAME: usize = 255;

/// A username as a limit's key, counted without case (`Ann` and `ann` share one count); None
/// when it is longer than any account's, which is refused before any limit is touched.
pub(crate) fn username_key(username: &str) -> Option<String> {
    let name = username.trim();
    (name.chars().count() <= MAX_USERNAME).then(|| name.to_lowercase())
}

/// The answer to a username no account can have.
pub(crate) fn username_too_long() -> ApiError {
    ApiError::new(ErrorCode::Invalid, "That username is too long.")
}

pub(crate) fn rate_limited() -> ApiError {
    ApiError::new(ErrorCode::RateLimited, "Too many tries. Wait a few minutes and try again.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(max: u32) -> Limit {
        Limit::new("test", max, Duration::from_secs(60))
    }

    #[test]
    fn a_window_has_max_slots_then_refuses_until_it_ends() {
        let limit = limit(2);
        let t0 = Instant::now();
        let first = limit.try_take("a", t0).unwrap();
        limit.try_take("a", t0).unwrap();
        assert!(limit.try_take("a", t0 + Duration::from_secs(59)).is_none());
        assert!(limit.try_take("b", t0).is_some(), "keys are counted apart");
        limit.release("a", first);
        assert!(limit.try_take("a", t0).is_some(), "a slot given back can be taken again");
        assert!(limit.try_take("a", t0 + Duration::from_secs(60)).is_some(), "a new window");
        limit.clear("a");
        assert!(limit.try_take("a", t0).is_some());
    }

    #[test]
    fn a_slot_goes_back_to_its_own_window_and_an_empty_key_is_removed() {
        let limit = limit(1);
        let t0 = Instant::now();
        let old = limit.try_take("a", t0).unwrap();
        let new = limit.try_take("a", t0 + Duration::from_secs(60)).unwrap();
        limit.release("a", old);
        assert!(limit.try_take("a", t0 + Duration::from_secs(61)).is_none(), "the old window's slot isn't the new one's");
        limit.release("a", new);
        assert_eq!(limit.state.lock().unwrap().hits.len(), 0, "count 0: the key is gone");
    }

    #[test]
    fn taking_from_several_limits_holds_all_or_none() {
        let (a, b) = (limit(1), limit(1));
        let t0 = Instant::now();
        b.try_take("k", t0).unwrap();
        assert!(take_all(&[(&a, "k"), (&b, "k")], t0).is_none());
        assert!(a.try_take("k", t0).is_some(), "a's slot was given back");
    }

    #[test]
    fn a_full_map_lets_a_new_key_through_uncounted() {
        let limit = limit(1);
        let t0 = Instant::now();
        for i in 0..MAX_KEYS {
            limit.try_take(&i.to_string(), t0).unwrap();
        }
        // Full and nothing expired: a new key passes, and isn't added.
        for _ in 0..3 {
            assert!(limit.try_take("new", t0).is_some());
        }
        assert_eq!(limit.state.lock().unwrap().hits.len(), MAX_KEYS);
        assert!(limit.try_take("0", t0).is_none(), "keys already held are still limited");
        // A sweep frees the expired ones: the new key is counted again.
        let later = t0 + Duration::from_secs(60);
        limit.try_take("new", later).unwrap();
        assert!(limit.try_take("new", later).is_none());
    }

    #[test]
    fn visitors_are_cloudflares_address_or_an_ipv6_slash_56_and_usernames_ignore_case() {
        let mut headers = HeaderMap::new();
        assert_eq!(visitor(&headers), UNKNOWN_VISITOR);
        headers.insert("cf-connecting-ip", " 203.0.113.7 ".parse().unwrap());
        assert_eq!(visitor(&headers), "203.0.113.7");
        headers.insert("cf-connecting-ip", "::ffff:203.0.113.7".parse().unwrap());
        assert_eq!(visitor(&headers), "203.0.113.7", "IPv4-mapped is the IPv4 address");
        headers.insert("cf-connecting-ip", "2001:db8:1:2aa:aaaa::1".parse().unwrap());
        assert_eq!(visitor(&headers), "2001:db8:1:200::/56");
        headers.insert("cf-connecting-ip", "2001:db8:1:2ff::9".parse().unwrap());
        assert_eq!(visitor(&headers), "2001:db8:1:200::/56", "same /56");
        headers.insert("cf-connecting-ip", "not an address".parse().unwrap());
        assert_eq!(visitor(&headers), UNKNOWN_VISITOR);
        assert_eq!(username_key(" Ann "), username_key("ann"));
        assert!(username_key(&"a".repeat(MAX_USERNAME)).is_some());
        assert_eq!(username_key(&"a".repeat(MAX_USERNAME + 1)), None);
    }
}
