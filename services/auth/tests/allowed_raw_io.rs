//! Every use of a raw I/O or task API that clippy.toml bans is listed in allowed_raw_io.toml
//! (TRACING.md): an `#[allow]` of one of clippy's `disallowed_*` lints that isn't there fails
//! this test.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(serde::Deserialize)]
struct Registry {
    allow: Vec<Allowed>,
}

#[derive(serde::Deserialize)]
struct Allowed {
    file: String,
    lint: String,
    count: usize,
    reason: String,
}

/// Counts the mentions of clippy's `disallowed_<lint>` lints per (file, lint) in the `.rs`
/// files under `dir`.
fn scan(root: &Path, dir: &Path, found: &mut BTreeMap<(String, String), usize>) {
    // Split so this file doesn't list itself.
    let needle = concat!("clippy::", "disallowed_");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan(root, &path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let file = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let source = std::fs::read_to_string(&path).unwrap();
            for (_, rest) in source.match_indices(needle).map(|(at, _)| source.split_at(at + needle.len())) {
                let lint: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                *found.entry((file.clone(), format!("disallowed_{lint}"))).or_default() += 1;
            }
        }
    }
}

#[test]
fn every_allowed_raw_api_is_registered() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let registry: Registry = toml::from_str(&std::fs::read_to_string(root.join("allowed_raw_io.toml")).unwrap()).unwrap();
    let mut registered = BTreeMap::new();
    for allowed in registry.allow {
        assert!(!allowed.reason.trim().is_empty(), "{} needs a reason", allowed.file);
        assert!(
            registered.insert((allowed.file.clone(), allowed.lint.clone()), allowed.count).is_none(),
            "{} {} is listed twice",
            allowed.file,
            allowed.lint
        );
    }

    let mut found = BTreeMap::new();
    for dir in ["src", "tests"] {
        scan(root, &root.join(dir), &mut found);
    }

    assert_eq!(
        found, registered,
        "the allows of clippy's `disallowed_*` lints in the code (left) differ from allowed_raw_io.toml (right): \
         use the traced API from src/auth/trace.rs, or list the new exception with its reason"
    );
}
