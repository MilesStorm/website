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

/// Every attribute (`#[...]`, `#![...]`) in `source` with the line it starts on, written on
/// one line. Comments are skipped; an attribute may span lines.
fn attributes(source: &str) -> Vec<(usize, String)> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            i += source[i..].find('\n').unwrap_or(source.len() - i);
        } else if bytes[i..].starts_with(b"#[") || bytes[i..].starts_with(b"#![") {
            let start = i;
            let (mut depth, mut in_string) = (0usize, false);
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' if in_string => i += 1,
                    b'"' => in_string = !in_string,
                    b'[' if !in_string => depth += 1,
                    b']' if !in_string => depth -= 1,
                    _ => {}
                }
                i += 1;
                if depth == 0 && bytes[i - 1] == b']' {
                    break;
                }
            }
            let line = source[..start].lines().count() + 1;
            let text: Vec<&str> = source[start..i.min(source.len())].lines().map(str::trim).collect();
            found.push((line, text.join(" ")));
        } else {
            i += 1;
        }
    }
    found
}

/// Lints and groups that take the bans with them: `disallowed_*` are in `clippy::style`.
const TOO_BROAD: [&str; 4] = ["warnings", "clippy::all", "clippy::style", "clippy::disallowed"];

/// `code` with its string literals emptied (`"…"` becomes `""`) and its whitespace removed,
/// so what is left is attribute syntax only.
fn syntax_only(code: &str) -> String {
    let (mut out, mut in_string, mut escaped) = (String::new(), false, false);
    for c in code.chars() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => {
                    in_string = false;
                    out.push('"');
                }
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
            out.push('"');
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

/// The lints each `allow(...)` and `expect(...)` in an attribute names, one list per group:
/// `cfg_attr` can hold several.
fn allow_groups(code: &str) -> Vec<Vec<String>> {
    let syntax = syntax_only(code);
    let mut groups = Vec::new();
    for opener in ["allow(", "expect("] {
        for (at, _) in syntax.match_indices(opener) {
            let list = syntax[at + opener.len()..].split(')').next().unwrap_or_default();
            groups.push(list.split(',').filter(|l| !l.is_empty() && !l.starts_with("reason=")).map(str::to_string).collect());
        }
    }
    groups
}

/// The `#[allow]`/`#[expect]` attributes naming a `clippy::disallowed_*` lint in `source`.
/// Each must give a `reason`. An allow wide enough to cover the bans without naming them
/// fails: it would be an exception the registry never sees.
fn allow_sites(file: &str, source: &str) -> Vec<Site> {
    let mut sites = Vec::new();
    for (line, code) in attributes(source) {
        let groups = allow_groups(&code);
        if let Some(broad) = groups.iter().flatten().find(|lint| TOO_BROAD.contains(&lint.as_str())) {
            panic!("{file}:{line}: `{broad}` is too broad an allow: it turns the I/O bans off (TRACING.md, \"Chokepoints\")");
        }
        let mut lints: Vec<String> = groups
            .iter()
            .flatten()
            .filter_map(|lint| lint.strip_prefix("clippy::"))
            .filter(|lint| lint.starts_with("disallowed_"))
            .map(str::to_string)
            .collect();
        if lints.is_empty() {
            assert!(
                !syntax_only(&code).contains("clippy::disallowed_"),
                "{file}:{line}: an attribute naming a disallowed_* lint that is neither allow nor expect"
            );
            continue;
        }
        lints.sort();
        let reason = code
            .split_once("reason = \"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(reason, _)| reason.to_string())
            .unwrap_or_else(|| panic!("{file}:{line}: a disallowed_* allow needs `reason = \"...\"`"));
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

#[test]
fn an_allow_over_several_lines_is_found() {
    let source = "#[allow(\n    clippy::disallowed_methods,\n    reason = \"a [long] one\"\n)]\nfn f() {}\n#[cfg_attr(test, allow(clippy::disallowed_types, reason = \"tests\"))]";
    assert_eq!(
        allow_sites("a.rs", source),
        [
            ("a.rs".to_string(), vec!["disallowed_methods".to_string()], "a [long] one".to_string()),
            ("a.rs".to_string(), vec!["disallowed_types".to_string()], "tests".to_string()),
        ]
    );
}

#[test]
fn an_allow_that_covers_the_bans_without_naming_them_fails() {
    for broad in [
        "#[allow(clippy::all)]",
        "#![allow(warnings)]",
        "#[allow(\n  clippy::style\n)]",
        "#[expect(clippy::style, reason = \"x\")]",
        "#[cfg_attr(test, allow(clippy::too_many_arguments, reason = \"x\"), allow(clippy::all))]",
        "#[allow(clippy :: all) ]",
    ] {
        let caught = std::panic::catch_unwind(|| allow_sites("a.rs", broad));
        assert!(caught.is_err(), "{broad}");
    }
    // A second group in one attribute is a site too.
    let two = "#[cfg_attr(test, allow(clippy::too_many_arguments, reason = \"x\"), allow(clippy::disallowed_types, reason = \"y\"))]";
    assert_eq!(allow_sites("a.rs", two).len(), 1);
    // Other lints, and these words in a reason, are fine.
    assert!(allow_sites("a.rs", "#[allow(clippy::too_many_arguments, reason = \"not clippy::all, no warnings\")]").is_empty());
}
