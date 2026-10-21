//! `server openapi` prints the committed OpenAPI document, with no environment at all: no .env,
//! Redis, auth or network (CI and the image's verify stage run it that way).

#[test]
fn openapi_needs_no_environment_and_matches_the_committed_document() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_server"))
        .arg("openapi")
        .env_clear()
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let committed = include_str!("../../../web/src/api/openapi.json");
    assert!(
        String::from_utf8(out.stdout).unwrap() == committed,
        "web/src/api/openapi.json is stale: run `npm run api` in services/frontend/web"
    );
}

/// `server check-pages <dir>` (the image's verify stage) refuses a build with a page `PAGES` doesn't
/// list.
#[test]
fn check_pages_refuses_an_unlisted_page() {
    let dir = std::env::temp_dir().join(format!("web-check-pages-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("secret.html"), "<html><head><meta name=\"bff-inject\"></head></html>").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_server")).arg("check-pages").arg(&dir).env_clear().output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("secret.html"), "{}", String::from_utf8_lossy(&out.stderr));
}
