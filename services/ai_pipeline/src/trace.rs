//! The crate's doors to background tasks and in-process queues (TRACING.md).
//!
//! `clippy.toml` bans the raw spawn functions and channel constructors, so a task or a
//! queue can't be added without deciding which trace its work belongs to. The few raw
//! uses, here and elsewhere, carry an `#[allow]` of that lint with a reason and are
//! listed in `allowed_raw_io.toml`; `every_raw_use_is_registered` fails on any that is not.

use std::future::Future;

use opentelemetry::{Context, ContextGuard};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// A message together with the trace context of whoever sent it, so the receiver's work
/// (and its log lines) stay in the sender's trace across a channel or a thread.
#[derive(Clone)]
pub struct Carried<T> {
    pub cx: Context,
    pub msg: T,
}

impl<T> Carried<T> {
    /// Wraps `msg` with the sender's current context.
    pub fn new(msg: T) -> Self {
        Self { cx: Context::current(), msg }
    }

    /// The same context around another message, for passing work on to the next queue.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Carried<U> {
        Carried { cx: self.cx, msg: f(self.msg) }
    }

    /// Makes the sender's context current on this thread until the guard is dropped.
    /// The guard can't be held across an `.await`; async receivers use `cx` directly.
    pub fn enter(self) -> (ContextGuard, T) {
        (self.cx.attach(), self.msg)
    }
}

/// A bounded queue whose messages carry their sender's context.
#[allow(clippy::disallowed_methods, reason = "trace::channel is the Carried queue constructor")]
pub fn channel<T>(bound: usize) -> (mpsc::Sender<Carried<T>>, mpsc::Receiver<Carried<T>>) {
    mpsc::channel(bound)
}

/// What a `watch_channel` holds: nothing until the first send.
pub type Slot<T> = Option<Carried<T>>;

/// A newest-wins slot whose value carries its sender's context.
#[allow(clippy::disallowed_methods, reason = "trace::watch_channel is the Carried slot constructor")]
pub fn watch_channel<T>() -> (watch::Sender<Slot<T>>, watch::Receiver<Slot<T>>) {
    watch::channel(None)
}

/// Spawns a task that lives as long as a connection or the process: `reason` says why.
/// It gets no span of its own, which would stay open and unexported for the task's whole
/// life; each unit of work inside it makes its own trace (`unit_trace.rs`).
#[allow(clippy::disallowed_methods, reason = "trace::spawn_loop is the long-lived task spawner")]
pub fn spawn_loop<F>(reason: &'static str, fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tracing::debug!(reason, "long-lived task started");
    tokio::spawn(fut)
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState};

    use super::*;

    fn some_context() -> Context {
        Context::new().with_remote_span_context(SpanContext::new(
            TraceId::from(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736_u128),
            SpanId::from(0x00f0_67aa_0ba9_02b7_u64),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        ))
    }

    /// The inference thread's case: the context set on the sending side is current on the
    /// receiving thread while the guard lives, and gone after.
    #[test]
    fn carried_takes_the_senders_context_across_a_thread() {
        let (tx, mut rx) = channel::<u32>(1);
        let cx = some_context();
        let sent = cx.span().span_context().clone();
        {
            let _cx = cx.attach();
            tx.try_send(Carried::new(7)).ok().unwrap();
        }

        #[allow(clippy::disallowed_methods, reason = "test: a plain thread stands in for the inference thread")]
        let receiver = std::thread::spawn(move || {
            assert!(!Context::current().has_active_span());
            let (guard, msg) = rx.blocking_recv().unwrap().enter();
            let inside = Context::current().span().span_context().clone();
            drop(guard);
            (msg, inside, Context::current().has_active_span())
        });
        let (msg, inside, active_after) = receiver.join().unwrap();

        assert_eq!(msg, 7);
        assert_eq!(inside, sent);
        assert!(!active_after);
    }

    #[test]
    fn map_keeps_the_context() {
        let cx = some_context();
        let carried = {
            let _cx = cx.clone().attach();
            Carried::new(1u8)
        };
        let mapped = carried.map(|n| u32::from(n) + 1);
        assert_eq!(mapped.msg, 2);
        assert_eq!(mapped.cx.span().span_context(), cx.span().span_context());
    }

    /// Every `#[allow]` of a disallowed-methods/types lint in the crate (or of a group wide
    /// enough to include them) must be listed in
    /// `allowed_raw_io.toml` (file, lint, reason), and every entry must still exist, so a
    /// new raw spawn or channel shows up in review as a change to that file.
    #[test]
    fn every_raw_use_is_registered() {
        #[derive(serde::Deserialize)]
        struct Registry {
            allow: Vec<Entry>,
        }
        #[derive(serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord)]
        struct Entry {
            file: String,
            lint: String,
            reason: String,
        }

        /// `source` without whitespace and with each string literal replaced by its number
        /// (`"0"`, `"1"`, ...), and the literals: attribute syntax, however it is laid out.
        fn syntax_only(source: &str) -> (String, Vec<String>) {
            let (mut out, mut strings, mut current, mut escaped) = (String::new(), Vec::new(), None::<String>, false);
            // A quote as a character literal opens no string.
            for c in source.replace(concat!("'", "\"", "'"), "").chars() {
                match &mut current {
                    Some(_) if escaped => escaped = false,
                    Some(_) if c == '\\' => escaped = true,
                    Some(text) if c == '"' => {
                        out.push_str(&format!("\"{}\"", strings.len()));
                        strings.push(std::mem::take(text));
                        current = None;
                    }
                    Some(text) => text.push(c),
                    None if c == '"' => current = Some(String::new()),
                    None if !c.is_whitespace() => out.push(c),
                    None => {}
                }
            }
            (out, strings)
        }

        fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    rust_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        files.push(root.join("build.rs"));

        // Split so this test's own source doesn't match.
        let marker = concat!("clippy::", "disallowed_");
        let mut found = Vec::new();
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            let file = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            // An allow wide enough to cover the bans without naming them (`disallowed_*` are
            // in `clippy::style`) is an exception too: listed, with its reason.
            let (syntax, strings) = syntax_only(&source);
            for opener in [concat!("al", "low("), concat!("exp", "ect(")] {
                for (at, _) in syntax.match_indices(opener) {
                    let group = syntax[at + opener.len()..].split(')').next().unwrap_or_default();
                    let reason = group
                        .split(',')
                        .find_map(|part| part.strip_prefix("reason=\"")?.strip_suffix('"')?.parse::<usize>().ok())
                        .map(|n| strings[n].clone());
                    for lint in group.split(',') {
                        if [concat!("warn", "ings"), concat!("clippy::", "all"), concat!("clippy::", "style")].contains(&lint) {
                            let reason = reason.clone().unwrap_or_else(|| panic!("{file}: allowing `{lint}` turns the bans off: it needs a reason"));
                            found.push(Entry { file: file.clone(), lint: lint.to_string(), reason });
                        }
                    }
                }
            }
            for (at, _) in source.match_indices(marker) {
                let attr_start = source[..at].rfind("#[").unwrap_or_else(|| panic!("{file}: {marker} outside an attribute"));
                let attr = &source[attr_start..];
                let attr = &attr[..attr.find(")]").unwrap_or_else(|| panic!("{file}: unterminated attribute")) + 2];
                assert!(
                    attr.starts_with("#[allow(") || attr.starts_with("#[expect("),
                    "{file}: {marker} must sit in an #[allow] or #[expect] on the item or statement: {attr}"
                );
                let lint: String = source[at + "clippy::".len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let reason = attr
                    .split_once("reason = \"")
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .unwrap_or_else(|| panic!("{file}: {attr} needs a reason"))
                    .0
                    .to_string();
                found.push(Entry { file: file.clone(), lint, reason });
            }
        }
        found.sort();

        let registry: Registry =
            toml::from_str(&std::fs::read_to_string(root.join("allowed_raw_io.toml")).unwrap()).unwrap();
        let mut registered = registry.allow;
        registered.sort();

        assert_eq!(found, registered, "left: #[allow] sites in the source, right: allowed_raw_io.toml");

        // However the attribute is written.
        let (syntax, strings) = syntax_only("#[cfg_attr(test, allow(clippy :: all, reason = \"a (b)\") ) ]");
        assert_eq!((syntax.as_str(), &strings[..]), ("#[cfg_attr(test,allow(clippy::all,reason=\"0\"))]", &["a (b)".to_string()][..]));
    }
}
