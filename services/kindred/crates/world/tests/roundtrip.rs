//! Write a random small world, read it back, compare (02-world-data §9).

use std::collections::BTreeMap;

use proptest::prelude::*;
use world::{BookDoc, SearchParams, WorkKey, WorkRow, World, WorldWriter};

fn params() -> SearchParams {
    SearchParams {
        popularity_weight: 0.3,
        title_boost: 3.0,
        other_titles_boost: 1.0,
        authors_boost: 2.0,
        series_boost: 1.5,
        whole_title_bonus: 10.0,
        not_a_book_penalty: 10.0,
        retry_below: 3,
        typo_min_len: 5,
    }
}

/// popularity, quality, year, not_a_book, has_description, (series row, position)
type Work = (
    f32,
    f32,
    Option<i16>,
    bool,
    bool,
    Option<(u32, Option<f32>)>,
);

#[derive(Debug, Clone)]
struct Input {
    works: BTreeMap<u64, Work>,
    series: Vec<String>,
    redirects: BTreeMap<u64, u64>,
    titles: Vec<String>,
}

fn input() -> impl Strategy<Value = Input> {
    let series = prop::collection::vec("[A-Za-zé ]{1,12}", 0..5);
    series.prop_flat_map(|series| {
        let n = series.len() as u32;
        let series_ref = if n == 0 {
            Just(None).boxed()
        } else {
            prop::option::of((0..n, prop::option::of(0.5f32..20.0))).boxed()
        };
        let work = (
            0.0f32..15.0,
            1.0f32..5.0,
            prop::option::of(1000i16..2030),
            any::<bool>(),
            any::<bool>(),
            series_ref,
        );
        (
            Just(series),
            prop::collection::btree_map(1u64..1_000_000, work, 0..40),
            prop::collection::btree_map(1u64..1_000_000, 1u64..1_000_000, 0..10),
            prop::collection::vec("[a-z]{3,10}( [a-z]{3,10}){0,3}", 40),
        )
            .prop_map(|(series, works, redirects, titles)| Input {
                works,
                series,
                redirects,
                titles,
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn written_world_reads_back(input in input()) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w");
        let mut writer = WorldWriter::create(&path).unwrap();
        let rows: Vec<WorkRow> = input.works.iter().map(|(&k, &(popularity, quality, year, not_a_book, has_description, series))| WorkRow {
            key: WorkKey(k), popularity, quality, year, not_a_book, has_description, series,
        }).collect();
        let books: Vec<BookDoc> = rows.iter().zip(&input.titles).map(|(r, title)| BookDoc {
            key: r.key,
            title: title.clone(),
            subtitle: (r.key.0 % 2 == 0).then(|| "sub".to_string()),
            authors: vec![format!("author{}", r.key.0 % 7)],
            other_titles: vec![],
            series: r.series.map(|(s, _)| input.series[s as usize].clone()),
            series_position: r.series.and_then(|(_, p)| p),
            year: r.year,
            cover: Some(r.key.0 * 3),
            popularity: r.popularity,
            not_a_book: r.not_a_book,
        }).collect();
        for book in &books {
            writer.add_book(book).unwrap();
        }
        // Reversed: the writer sorts.
        writer.write_works(rows.iter().rev().cloned().collect(), &input.series).unwrap();
        writer.write_redirects(input.redirects.iter().map(|(&a, &b)| (WorkKey(a), WorkKey(b))).collect()).unwrap();
        let report = serde_json::json!({"works": rows.len()});
        let manifest = writer.finish("2026-11-13T00:00:00Z".into(), "test".into(), vec![], params(), report).unwrap();

        let world = World::open(&path).unwrap();
        prop_assert_eq!(world.manifest(), &manifest);
        prop_assert_eq!(world.len(), rows.len());
        for r in &rows {
            let row = world.row(r.key).expect("every key has a row");
            prop_assert_eq!(world.key(row), r.key);
            prop_assert_eq!(world.popularity(row), r.popularity);
            prop_assert_eq!(world.quality(row), r.quality);
            prop_assert_eq!(world.year(row), r.year);
            prop_assert_eq!(world.flags(row) & world::reader::flags::NOT_A_BOOK != 0, r.not_a_book);
            prop_assert_eq!(world.flags(row) & world::reader::flags::HAS_DESCRIPTION != 0, r.has_description);
            let expected = r.series.map(|(s, p)| (input.series[s as usize].as_str(), p));
            prop_assert_eq!(world.series(row), expected);
        }
        prop_assert_eq!(world.row(WorkKey(0)), None);
        for (&from, &to) in &input.redirects {
            prop_assert_eq!(world.current_key(WorkKey(from)), WorkKey(to));
        }
        prop_assert_eq!(world.current_key(WorkKey(0)), WorkKey(0));
        for book in &books {
            let hit = world.book(book.key).unwrap().expect("every book is in the index");
            prop_assert_eq!(&hit.title, &book.title);
            prop_assert_eq!(&hit.subtitle, &book.subtitle);
            prop_assert_eq!(&hit.authors, &book.authors);
            prop_assert_eq!(&hit.series, &book.series);
            prop_assert_eq!(hit.series_position, book.series_position);
            prop_assert_eq!(hit.year, book.year);
            prop_assert_eq!(hit.cover, book.cover);
        }
    }
}

#[test]
fn refuses_unknown_format_and_bad_checksums() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w");
    let mut writer = WorldWriter::create(&path).unwrap();
    writer
        .add_book(&BookDoc {
            key: WorkKey(1),
            title: "Dune".into(),
            subtitle: None,
            authors: vec!["Frank Herbert".into()],
            other_titles: vec![],
            series: None,
            series_position: None,
            year: Some(1965),
            cover: None,
            popularity: 10.0,
            not_a_book: false,
        })
        .unwrap();
    let row = WorkRow {
        key: WorkKey(1),
        popularity: 10.0,
        quality: 4.2,
        year: Some(1965),
        not_a_book: false,
        has_description: true,
        series: None,
    };
    writer.write_works(vec![row], &[]).unwrap();
    writer.write_redirects(vec![]).unwrap();
    writer
        .finish(
            "t".into(),
            "test".into(),
            vec![],
            params(),
            serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(
        World::open(&path).unwrap().search("dune", 5).unwrap()[0].key,
        WorkKey(1)
    );

    // A changed byte in a column.
    let quality = path.join(world::files::QUALITY);
    let mut bytes = std::fs::read(&quality).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&quality, &bytes).unwrap();
    assert!(matches!(
        World::open(&path),
        Err(world::Error::Corrupt { .. })
    ));
    bytes[0] ^= 1;
    std::fs::write(&quality, &bytes).unwrap();

    // A format this reader doesn't know.
    let manifest = path.join(world::files::MANIFEST);
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace("\"format_version\": 1", "\"format_version\": 2"),
    )
    .unwrap();
    assert!(matches!(
        World::open(&path),
        Err(world::Error::Format { found: 2 })
    ));
}
