//! The registry of raw I/O uses (TRACING.md, "Chokepoints"): clippy.toml bans the untraced
//! constructors, a legitimate use carries `#[allow(clippy::disallowed_*, reason = "...")]`,
//! and every such site must be listed in `services/frontend/allowed_raw_io.toml`.

use std::path::{Path, PathBuf};

/// One allow site, or one entry of the registry: file, lints (sorted) and reason.
type Site = (String, Vec<String>, String);

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // Build output, not ours.
            if path.file_name().is_some_and(|n| n != "target" && n != "dist") {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The `#[allow]`/`#[expect]` attributes naming a `clippy::disallowed_*` lint in `source`.
/// Each must fit on one line and give a `reason`.
fn allow_sites(file: &str, source: &str) -> Vec<Site> {
    let mut sites = Vec::new();
    for (n, line) in source.lines().enumerate() {
        let code = line.trim_start();
        let is_attribute = code.starts_with("#[") || code.starts_with("#![");
        if !is_attribute || !code.contains("clippy::disallowed_") {
            continue;
        }
        assert!(
            code.contains("allow(") || code.contains("expect("),
            "{file}:{}: an attribute naming a disallowed_* lint that is neither allow nor expect",
            n + 1,
        );
        let mut lints: Vec<String> = code
            .split("clippy::")
            .skip(1)
            .map(|rest| rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect())
            .filter(|lint: &String| lint.starts_with("disallowed_"))
            .collect();
        lints.sort();
        let reason = code
            .split_once("reason = \"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(reason, _)| reason.to_string())
            .unwrap_or_else(|| panic!("{file}:{}: a disallowed_* allow needs `reason = \"...\"` on the same line", n + 1));
        sites.push((file.to_string(), lints, reason));
    }
    sites
}

#[test]
fn every_raw_io_allow_is_in_the_registry() {
    let frontend = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let this_file = Path::new(file!()).file_name().unwrap();

    let mut files = Vec::new();
    for package in ["packages/api", "packages/web"] {
        rust_files(&frontend.join(package), &mut files);
    }
    let mut found = Vec::new();
    for path in files.iter().filter(|p| p.file_name() != Some(this_file)) {
        let relative = path.strip_prefix(&frontend).unwrap().to_string_lossy().replace('\\', "/");
        found.extend(allow_sites(&relative, &std::fs::read_to_string(path).unwrap()));
    }

    let registry: toml::Table = std::fs::read_to_string(frontend.join("allowed_raw_io.toml")).unwrap().parse().unwrap();
    let mut listed: Vec<Site> = registry["allow"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let text = |key: &str| entry[key].as_str().unwrap().to_string();
            let mut lints: Vec<String> =
                entry["lints"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect();
            lints.sort();
            (text("file"), lints, text("reason"))
        })
        .collect();

    found.sort();
    listed.sort();
    let unlisted: Vec<_> = found.iter().filter(|site| !listed.contains(site)).collect();
    let stale: Vec<_> = listed.iter().filter(|entry| !found.contains(entry)).collect();
    assert!(unlisted.is_empty(), "allow sites missing from allowed_raw_io.toml: {unlisted:#?}");
    assert!(stale.is_empty(), "allowed_raw_io.toml entries with no allow site: {stale:#?}");
    assert_eq!(found.len(), listed.len(), "the same site or entry appears twice");
}

#[test]
fn allow_sites_are_found_with_their_lints_and_reason() {
    let source = r#"
        #[allow(clippy::disallowed_types, clippy::disallowed_methods, reason = "the chokepoint")]
        fn client() {}
        // clippy::disallowed_methods in a comment is not a site
        #[allow(clippy::too_many_arguments)]
        #![expect(clippy::disallowed_methods, reason = "tests")]
    "#;
    assert_eq!(
        allow_sites("a.rs", source),
        [
            ("a.rs".to_string(), vec!["disallowed_methods".to_string(), "disallowed_types".to_string()], "the chokepoint".to_string()),
            ("a.rs".to_string(), vec!["disallowed_methods".to_string()], "tests".to_string()),
        ]
    );
}

#[test]
#[should_panic(expected = "needs `reason")]
fn an_allow_without_a_reason_fails() {
    allow_sites("a.rs", "#[allow(clippy::disallowed_methods)]");
}
