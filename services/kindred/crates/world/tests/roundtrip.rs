//! Write a random small world, read it back, compare (02-world-data §9).

use std::collections::BTreeMap;

use proptest::prelude::*;
use world::{BookDoc, Model, SearchParams, VectorTable, WorkKey, WorkRow, World, WorldWriter};

/// A small vector layout: d = 1 + 2 + 2 + 1 + 5.
fn model() -> Model {
    Model {
        layout: taste::Layout { k: 2, r: 2, a: 1 },
        aspects: vec![world::Aspect {
            id: "dark".into(),
            group: "tone".into(),
            label: "how dark it is".into(),
            phrase: "a dark tone".into(),
            shelves: vec!["dark".into()],
            subjects: vec![],
            chip: true,
            steer: "dark".into(),
        }],
        hyper: taste::Hyper::default(),
        prior: taste::Prior {
            m0_meta: [0.5, 0.25, 0.0, 0.0, 0.1],
        },
        confidence_cuts: [0.5, 0.8],
        bench: serde_json::json!({"ndcg10_n20": 0.1}),
    }
}

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
            key: WorkKey(k), popularity, quality, year, not_a_book, has_description,
            // Every third work has a vector; of those, every other one is recommendable.
            recommendable: k % 6 == 0, series,
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
        let d = model().layout.d();
        // Vectors for every third work, plus one for a work that is not in the world.
        let mut table = VectorTable { d, keys: vec![], data: vec![], source: vec![] };
        let mut vector_keys: Vec<u64> = rows.iter().map(|r| r.key.0).filter(|k| k % 3 == 0).collect();
        vector_keys.push(1_000_003);
        vector_keys.sort_unstable();
        vector_keys.dedup();
        for &k in &vector_keys {
            table.keys.push(WorkKey(k));
            table.data.extend((0..d).map(|i| (k as f32) * 0.001 + i as f32));
            table.source.push((k % 4) as u8 & 3);
        }
        let recommendable: Vec<WorkKey> = rows.iter().filter(|r| r.recommendable).map(|r| r.key).collect();
        writer.write_vectors(&table, &recommendable).unwrap();
        // Neighbours for every second work: the first three works (not itself), weakest first,
        // plus one work that is not in the world; and a list for a work that is not in it.
        let mut sorted_keys: Vec<WorkKey> = rows.iter().map(|r| r.key).collect();
        sorted_keys.sort_unstable();
        let expected_neighbours = |k: WorkKey| -> Vec<(WorkKey, f32)> {
            if k.0 % 2 != 0 {
                return vec![];
            }
            let mut v: Vec<(WorkKey, f32)> = sorted_keys
                .iter()
                .copied()
                .filter(|&o| o != k)
                .take(3)
                .enumerate()
                .map(|(i, o)| (o, 0.5 / (i as f32 + 1.0)))
                .collect();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            v
        };
        let mut lists: Vec<(WorkKey, Vec<(WorkKey, f32)>)> = sorted_keys
            .iter()
            .map(|&k| {
                let mut l = expected_neighbours(k);
                l.reverse();
                l.push((WorkKey(1_000_003), 0.9));
                (k, l)
            })
            .collect();
        lists.push((WorkKey(1_000_003), sorted_keys.first().map(|&k| (k, 0.1)).into_iter().collect()));
        writer.write_neighbours(&lists).unwrap();
        let report = serde_json::json!({"works": rows.len()});
        let manifest = writer.finish("2026-11-13T00:00:00Z".into(), "test".into(), vec![], params(), report, model()).unwrap();

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
            prop_assert_eq!(world.flags(row) & world::reader::flags::RECOMMENDABLE != 0, r.recommendable);
            match world.vector(row) {
                Some(v) => {
                    prop_assert!(r.key.0 % 3 == 0);
                    prop_assert_eq!(v.len(), d);
                    prop_assert_eq!(v[1], (r.key.0 as f32) * 0.001 + 1.0);
                    prop_assert_eq!(world.vector_source(row), (r.key.0 % 4) as u8 & 3);
                    prop_assert_eq!(taste::Vectors::phi(&world, taste::Row(row.0)), Some(v));
                }
                None => prop_assert!(r.key.0 % 3 != 0),
            }
            let got: Vec<(WorkKey, f32)> = world.neighbours(row).map(|(o, s)| (world.key(o), s)).collect();
            prop_assert_eq!(got, expected_neighbours(r.key));
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
        recommendable: true,
        series: None,
    };
    writer.write_works(vec![row], &[]).unwrap();
    writer.write_redirects(vec![]).unwrap();
    let d = model().layout.d();
    // A recommendable work must have a vector.
    let none = VectorTable {
        d,
        keys: vec![],
        data: vec![],
        source: vec![],
    };
    assert!(writer.write_vectors(&none, &[WorkKey(1)]).is_err());
    let table = VectorTable {
        d,
        keys: vec![WorkKey(1)],
        data: vec![0.5; d],
        source: vec![0],
    };
    writer.write_vectors(&table, &[WorkKey(1)]).unwrap();
    writer.write_neighbours(&[]).unwrap();
    writer
        .finish(
            "t".into(),
            "test".into(),
            vec![],
            params(),
            serde_json::json!({}),
            model(),
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
        text.replace("\"format_version\": 4", "\"format_version\": 5"),
    )
    .unwrap();
    assert!(matches!(
        World::open(&path),
        Err(world::Error::Format { found: 5 })
    ));
}
