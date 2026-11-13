//! The fixture world (02-world-data.md §10): every stage runs on it, it passes verify (gates,
//! the search acceptance list and its latency), and the world invariants (§1) hold.

use std::path::Path;

use std::sync::Arc;
use world::reader::flags;
use world::{World, files};

use world_build::config::{self, Acceptance, Config, SeriesOverrides};
use world_build::trace::Bucket;
use world_build::{build, fixture, publish, verify};

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

/// Invariant 1: the builder reads only the environment in `build::ENVIRONMENT` (no database
/// credentials), its CronJob sets nothing else, and it links no database client.
#[test]
fn reads_only_its_listed_environment() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    scan(&crate_dir.join("src"), &mut |path| {
        let text = std::fs::read_to_string(path).unwrap();
        for (at, _) in text.match_indices("var(\"") {
            let name: String = text[at + 5..].chars().take_while(|c| *c != '"').collect();
            assert!(
                build::ENVIRONMENT.contains(&name.as_str()),
                "{} reads {name}, which build::ENVIRONMENT doesn't list",
                path.display()
            );
        }
        assert!(
            !text.contains("env::vars"),
            "{} reads the whole environment",
            path.display()
        );
    });

    let cronjob =
        std::fs::read_to_string(crate_dir.join("../../../../crds/media-world/cronjob.yaml"))
            .unwrap();
    let env_names: Vec<&str> = cronjob
        .lines()
        .skip_while(|l| l.trim() != "env:")
        .take_while(|l| l.trim() != "volumeMounts:")
        .filter_map(|l| l.trim().strip_prefix("- name: "))
        .collect();
    assert!(!env_names.is_empty());
    for name in env_names {
        assert!(
            build::ENVIRONMENT.contains(&name),
            "the CronJob sets {name}, which build::ENVIRONMENT doesn't list"
        );
    }

    let manifest: toml::Table =
        toml::from_str(&std::fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap()).unwrap();
    let deps = manifest["dependencies"].as_table().unwrap();
    for client in ["sqlx", "postgres", "tokio-postgres", "diesel", "sea-orm"] {
        assert!(
            !deps.contains_key(client),
            "world-build depends on the database client {client}"
        );
    }
}

/// Invariant 5: no Goodreads data is in the repository (the fixture's is generated).
#[test]
fn no_goodreads_files_in_the_fixtures() {
    scan(&fixture::default_dir(), &mut |path| {
        let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(
            !name.contains("goodreads"),
            "{} looks like Goodreads data",
            path.display()
        );
    });
}

/// §1 invariant 3 and the §7 gates, on the fixture world: a failed verify publishes nothing
/// and leaves current.json alone; the change gate and the mapped-share gate stop a world.
#[tokio::test(flavor = "multi_thread")]
async fn gates_stop_a_world_and_nothing_is_published_after_a_failed_verify() {
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

    // The gates, directly.
    let world = World::open(&out).unwrap();
    verify::verify(&world, &config, &acceptance, Some(&manifest.report)).unwrap();
    let mut grown = manifest.report.clone();
    let searchable = grown["searchable_works"].as_f64().unwrap();
    grown["searchable_works"] = (searchable / 1.3).round().into();
    let err = verify::verify(&world, &config, &acceptance, Some(&grown))
        .unwrap_err()
        .to_string();
    assert!(err.contains("searchable_works"), "{err}");
    grown["searchable_works"] = (searchable / 1.1).round().into();
    verify::verify(&world, &config, &acceptance, Some(&grown)).unwrap();
    let mut strict = config.clone();
    strict.identity.min_mapped_share = 0.99;
    let err = verify::verify(&world, &strict, &acceptance, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("mapped"), "{err}");
    drop(world);

    // A served world, then a build that fails verify: nothing changes in the bucket.
    let bucket = Bucket::from_store(Arc::new(object_store::memory::InMemory::new()));
    let first = build::verify_then_publish(
        &out,
        &config,
        &acceptance,
        Some(&bucket),
        "20261108-aaaaaaa",
    )
    .await
    .unwrap()
    .unwrap();
    let before = publish::current(&bucket).await.unwrap().unwrap();
    assert_eq!(before.version, first);
    let impossible: Acceptance = config::parse(
        "acceptance",
        "[[case]]\nquery = \"dune\"\ntitle = \"No Such Book\"\n",
    )
    .unwrap();
    assert!(
        build::verify_then_publish(
            &out,
            &config,
            &impossible,
            Some(&bucket),
            "20261208-bbbbbbb"
        )
        .await
        .is_err()
    );
    assert_eq!(publish::current(&bucket).await.unwrap().unwrap(), before);
    assert_eq!(publish::versions(&bucket).await.unwrap(), [first]);
}
