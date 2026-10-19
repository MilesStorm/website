//! `web openapi` prints the committed OpenAPI document, with no environment at all: no .env,
//! Redis, auth or network (CI and the image's verify stage run it that way).

#[test]
fn openapi_needs_no_environment_and_matches_the_committed_document() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_web"))
        .arg("openapi")
        .env_clear()
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let committed = include_str!("../../../web/src/api/openapi.json");
    assert!(
        String::from_utf8(out.stdout).unwrap() == committed,
        "web/src/api/openapi.json is stale: run `npm run api` in services/frontend"
    );
}
