//! The fixture world (02-world-data.md §10): every stage runs on it, it passes verify (gates,
//! the search acceptance list and its latency), and the world invariants (§1) hold.

use std::path::Path;

use world::reader::flags;
use world::{World, files};
use world_build::config::{self, Acceptance, Config, SeriesOverrides};
use world_build::fixture;

fn configs() -> (Config, SeriesOverrides, Acceptance) {
    (
        config::parse("world.toml", config::WORLD_TOML).unwrap(),
        config::parse("series-overrides.toml", config::SERIES_OVERRIDES_TOML).unwrap(),
        config::parse("search-acceptance.toml", config::ACCEPTANCE_TOML).unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn fixture_world_builds_and_passes_verify() {
    let (config, overrides, acceptance) = configs();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("world");
    let manifest = fixture::build(
        &fixture::default_dir(),
        &out,
        &config,
        &overrides,
        &acceptance,
    )
    .await
    .unwrap();
    let world = World::open(&out).unwrap();
    let report = &manifest.report;
    let n = |name: &str| {
        report[name]
            .as_i64()
            .unwrap_or_else(|| panic!("{name} not in the report"))
    };

    let listed = fixture::read_works(&fixture::default_dir())
        .unwrap()
        .work
        .len() as i64;
    assert!(n("ol_works") >= listed - 5, "{report}");
    assert!(n("searchable_works") >= listed * 9 / 10, "{report}");
    assert_eq!(world.len() as i64, n("searchable_works"));
    assert!(
        report["gr_ratings_mapped_share"].as_f64().unwrap() > 0.95,
        "{report}"
    );
    assert!(n("searchable_with_series_position") >= 30, "{report}");
    assert!(n("searchable_with_description") > listed / 2, "{report}");
    assert!(n("searchable_not_a_book") >= 4, "the decoys: {report}");

    // Decoys rank below the real book and are flagged.
    let dune = world.search("dune", 10).unwrap();
    let guide = dune
        .iter()
        .find(|h| h.title.to_lowercase().contains("study guide"))
        .expect("the Dune study guide");
    assert_ne!(dune[0].key, guide.key);
    let row = world.row(guide.key).unwrap();
    assert!(world.flags(row) & flags::NOT_A_BOOK != 0);
    assert!(world.flags(world.row(dune[0].key).unwrap()) & flags::NOT_A_BOOK == 0);
    assert!(world.popularity(world.row(dune[0].key).unwrap()) > world.popularity(row));

    // Series from both sources: Goodreads (synthetic) and Open Library's edition strings.
    let kings = world.search("way of kings", 1).unwrap().remove(0);
    let (name, position) = world.series(world.row(kings.key).unwrap()).unwrap();
    assert!(name.contains("Stormlight"), "{name}");
    assert_eq!(position, Some(1.0));
    assert_eq!(kings.series.as_deref(), Some(name));

    // Invariant 1: the world holds only the files of the format (no user data).
    let known = [
        files::REPORT,
        files::KEYS,
        files::POPULARITY,
        files::QUALITY,
        files::YEAR,
        files::FLAGS,
        files::SERIES,
        files::SERIES_POSITION,
        files::SERIES_NAME_OFFSETS,
        files::SERIES_NAME_TEXT,
        files::REDIRECTS,
    ];
    for entry in &manifest.files {
        assert!(
            known.contains(&entry.path.as_str())
                || entry.path.starts_with(&format!("{}/", files::SEARCH_DIR)),
            "unexpected file {}",
            entry.path
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_acceptance_case_stops_the_build() {
    let (config, overrides, _) = configs();
    let impossible: Acceptance = config::parse(
        "acceptance",
        "[[case]]\nquery = \"dune\"\ntitle = \"No Such Book\"\n",
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let err = fixture::build(
        &fixture::default_dir(),
        &tmp.path().join("w"),
        &config,
        &overrides,
        &impossible,
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("case 1"), "{err:#}");
}

/// Invariants 1 and 5: the builder never reads database credentials, and no Goodreads data is
/// in the repository (the fixture's is generated).
#[test]
fn no_database_credentials_and_no_goodreads_files() {
    fn scan(dir: &Path, f: &mut dyn FnMut(&Path)) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, f)
            } else {
                f(&path)
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    scan(&src, &mut |path| {
        let text = std::fs::read_to_string(path).unwrap();
        for word in ["DATABASE_URL", "PG_PASSWORD", "sqlx", "postgres"] {
            assert!(!text.contains(word), "{} mentions {word}", path.display());
        }
    });
    scan(&fixture::default_dir(), &mut |path| {
        let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(
            !name.contains("goodreads"),
            "{} looks like Goodreads data",
            path.display()
        );
    });
}
