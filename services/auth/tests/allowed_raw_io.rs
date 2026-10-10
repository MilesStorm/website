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

/// The lints an `allow(...)` or `expect(...)` in `source` names, wherever it is written
/// (an attribute over several lines, inside `cfg_attr`).
fn allowed_lints(source: &str) -> Vec<String> {
    let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    let mut lints = Vec::new();
    for opener in ["allow(", "expect("] {
        for (at, _) in compact.match_indices(opener) {
            let list = compact[at + opener.len()..].split([')', '"']).next().unwrap_or_default();
            lints.extend(list.split(',').map(|lint| lint.trim_end_matches("reason=").to_string()));
        }
    }
    lints
}

/// An allow wide enough to turn the bans off without naming them (`disallowed_*` are in
/// `clippy::style`) would be an exception the registry never sees.
#[test]
fn no_allow_is_wide_enough_to_cover_the_bans() {
    fn check(dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                check(&path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let broad: Vec<_> = allowed_lints(&std::fs::read_to_string(&path).unwrap())
                    .into_iter()
                    .filter(|lint| ["warnings", "clippy::all", "clippy::style"].contains(&lint.as_str()))
                    .collect();
                assert!(broad.is_empty(), "{}: {broad:?} turns the I/O bans off: allow the one lint, with a reason", path.display());
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    check(&root.join("src"));

    let source = "#[allow(\n    clippy::all,\n    reason = \"x\"\n)]\n#[cfg_attr(test, expect(warnings))]\n#[allow(clippy::too_many_arguments, reason = \"not clippy::style\")]";
    assert_eq!(allowed_lints(source), ["clippy::all", "", "clippy::too_many_arguments", "", "warnings"]);
}
