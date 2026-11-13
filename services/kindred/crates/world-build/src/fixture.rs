//! The fixture world (02-world-data.md §10): a few hundred works, built by the same stages as
//! the real one. Open Library records are real (fixtures/openlibrary/, open data, refreshed by
//! `kindred-world fixture-refresh`); the Goodreads files are synthetic, generated here from a fixed
//! seed: real Goodreads rows never enter the repository (PLAN.md rule 13).

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rand::{Rng as _, SeedableRng as _};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::download::{self, Source};

/// `fixtures/works.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureWorks {
    pub work: Vec<FixtureWork>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureWork {
    pub key: world::WorkKey,
    pub title: String,
    pub gr_series: Option<String>,
    pub gr_position: Option<f64>,
}

/// The fixture files, by the source name they stand in for.
pub const OPENLIBRARY_FILES: [(&str, &str); 6] = [
    ("ol_works", "works.txt.gz"),
    ("ol_editions", "editions.txt.gz"),
    ("ol_authors", "authors.txt.gz"),
    ("ol_ratings", "ratings.txt.gz"),
    ("ol_reading_log", "reading-log.txt.gz"),
    ("ol_redirects", "redirects.txt.gz"),
];

/// The repository's fixtures directory.
pub fn default_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

pub fn read_works(fixtures: &Path) -> anyhow::Result<FixtureWorks> {
    let path = fixtures.join("works.toml");
    crate::config::parse(
        "fixtures/works.toml",
        &std::fs::read_to_string(&path).with_context(|| path.display().to_string())?,
    )
}

/// Puts the fixture inputs where the build expects its downloads.
pub fn stage_inputs(fixtures: &Path, downloads: &Path, sources: &[Source]) -> anyhow::Result<()> {
    std::fs::create_dir_all(downloads)?;
    let source = |name: &str| {
        sources
            .iter()
            .find(|s| s.name == name)
            .expect("a known source")
    };
    for (name, file) in OPENLIBRARY_FILES {
        let from = fixtures.join("openlibrary").join(file);
        std::fs::copy(&from, download::path(downloads, source(name)))
            .with_context(|| from.display().to_string())?;
    }
    let works = read_works(fixtures)?;
    let ol = |name| {
        fixtures.join("openlibrary").join(
            OPENLIBRARY_FILES
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap()
                .1,
        )
    };
    let synthetic = goodreads(&works, &ol("ol_editions"), &ol("ol_reading_log"))?;
    for (name, lines) in [
        ("gr_books", synthetic.books),
        ("gr_works", synthetic.works),
        ("gr_series", synthetic.series),
    ] {
        write_gz(&download::path(downloads, source(name)), &lines)?;
    }
    Ok(())
}

fn write_gz(path: &Path, lines: &[Value]) -> anyhow::Result<()> {
    let mut gz =
        flate2::write::GzEncoder::new(std::fs::File::create(path)?, flate2::Compression::default());
    for line in lines {
        serde_json::to_writer(&mut gz, line)?;
        gz.write_all(b"\n")?;
    }
    gz.finish()?;
    Ok(())
}

/// Each JSON record of an Open Library dump file.
pub fn records(path: &Path) -> anyhow::Result<Vec<Value>> {
    let file = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let mut out = Vec::new();
    for line in std::io::BufReader::new(flate2::read::MultiGzDecoder::new(file)).lines() {
        let line = line?;
        if let Some(json) = line.split('\t').nth(4) {
            out.push(serde_json::from_str(json)?);
        }
    }
    Ok(out)
}

pub struct Synthetic {
    pub books: Vec<Value>,
    pub works: Vec<Value>,
    pub series: Vec<Value>,
}

/// Synthetic Goodreads books, works and series for the fixture works: each work gets one to
/// three books on its real editions' ISBNs (some as ISBN-10 only, some through the edition's
/// Goodreads id), ratings in proportion to its Open Library reading log, and its series as
/// listed in works.toml. A few works match no edition, as in the real data.
pub fn goodreads(
    works: &FixtureWorks,
    editions: &Path,
    reading_log: &Path,
) -> anyhow::Result<Synthetic> {
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(2017);
    let mut by_work: HashMap<u64, Vec<Value>> = HashMap::new();
    for edition in records(editions)? {
        if let Some(work) = edition["works"][0]["key"]
            .as_str()
            .and_then(|k| k.parse::<world::WorkKey>().ok())
        {
            by_work.entry(work.0).or_default().push(edition);
        }
    }
    let mut log: HashMap<u64, u64> = HashMap::new();
    let file = std::fs::File::open(reading_log)?;
    for line in std::io::BufReader::new(flate2::read::MultiGzDecoder::new(file)).lines() {
        if let Some(work) = line?
            .split('\t')
            .next()
            .and_then(|k| k.parse::<world::WorkKey>().ok())
        {
            *log.entry(work.0).or_default() += 1;
        }
    }

    let mut series_ids: BTreeMap<&str, u64> = BTreeMap::new();
    for w in &works.work {
        if let Some(name) = &w.gr_series {
            let next = 40_000 + series_ids.len() as u64;
            series_ids.entry(name).or_insert(next);
        }
    }
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let mut books = Vec::new();
    let mut gr_works = Vec::new();
    let mut next_book = 9_000_000u64;
    let book = |id: u64,
                work: u64,
                isbn: &str,
                isbn13: &str,
                ratings: u64,
                series: Vec<String>,
                title: String,
                plain: &str| {
        json!({
            "book_id": id.to_string(), "work_id": work.to_string(), "isbn": isbn, "isbn13": isbn13,
            "ratings_count": ratings.to_string(), "series": series, "title": title,
            "title_without_series": plain, "language_code": "eng",
            // Fields the build doesn't read, as in the real file.
            "popular_shelves": [], "authors": [], "description": "", "average_rating": "", "similar_books": []
        })
    };
    for (i, w) in works.work.iter().enumerate() {
        let gr_work = 100_000 + i as u64;
        let editions: Vec<&Value> = by_work
            .get(&w.key.0)
            .map(|e| {
                e.iter()
                    .filter(|e| e["isbn_13"].is_array() || e["isbn_10"].is_array())
                    .collect()
            })
            .unwrap_or_default();
        if editions.is_empty() {
            continue;
        }
        let readers = *log.get(&w.key.0).unwrap_or(&1) as f64;
        let noise: f64 = rng.random_range(-0.5..0.5);
        let ratings = (readers * 40.0 * noise.exp()).round().max(1.0) as u64;
        let mean: f64 = rng.random_range(3.5..4.4);
        gr_works.push(json!({
            "work_id": gr_work.to_string(), "ratings_count": ratings.to_string(),
            "ratings_sum": ((ratings as f64) * mean).round().to_string(),
            "original_publication_year": "", "best_book_id": "", "rating_dist": "", "books_count": ""
        }));
        let series: Vec<String> = w
            .gr_series
            .iter()
            .map(|s| series_ids[s.as_str()].to_string())
            .collect();
        let title = match (&w.gr_series, w.gr_position) {
            (Some(s), Some(p)) => format!("{} ({s}, #{p})", w.title),
            _ => w.title.clone(),
        };
        let shares = [0.7, 0.2, 0.1];
        for (j, edition) in editions.iter().take(3).enumerate() {
            let isbn13 = edition["isbn_13"][0].as_str().unwrap_or("");
            let isbn10 = edition["isbn_10"][0].as_str().unwrap_or("");
            let goodreads_id = edition["identifiers"]["goodreads"][0]
                .as_str()
                .and_then(|g| g.trim().parse::<u64>().ok());
            let id = goodreads_id.unwrap_or_else(|| {
                next_book += 1;
                next_book
            });
            // Every other book is written with its ISBN-10 only: the ingest must convert it.
            let (isbn, isbn13) = if j % 2 == 1 && !isbn10.is_empty() {
                (isbn10, "")
            } else {
                ("", isbn13)
            };
            let (isbn, isbn13) = if isbn.is_empty() && isbn13.is_empty() {
                (isbn10, "")
            } else {
                (isbn, isbn13)
            };
            let count = (ratings as f64 * shares[j]).round() as u64;
            let plain = text(&edition["title"]).unwrap_or_else(|| w.title.clone());
            books.push(book(
                id,
                gr_work,
                isbn,
                isbn13,
                count,
                series.clone(),
                title.clone(),
                &plain,
            ));
        }
    }
    // Goodreads works that match nothing in Open Library (about 2% of ratings).
    let mapped: u64 = gr_works
        .iter()
        .map(|w| w["ratings_count"].as_str().unwrap().parse::<u64>().unwrap())
        .sum();
    for k in 0..5u64 {
        let gr_work = 900_000 + k;
        let ratings = mapped / 250;
        gr_works.push(json!({"work_id": gr_work.to_string(), "ratings_count": ratings.to_string(),
                             "ratings_sum": (ratings * 4).to_string(), "original_publication_year": "2001"}));
        next_book += 1;
        books.push(book(
            next_book,
            gr_work,
            "",
            &format!("97900000000{k:02}"),
            ratings,
            vec![],
            format!("Unmatched {k}"),
            "Unmatched",
        ));
    }
    let series = series_ids
        .iter()
        .map(|(name, id)| json!({"series_id": id.to_string(), "title": name, "numbered": "true", "note": "", "description": ""}))
        .collect();
    Ok(Synthetic {
        books,
        works: gr_works,
        series,
    })
}

/// Builds the fixture world into `out` and verifies it like a real one.
pub async fn build(
    fixtures: &Path,
    out: &Path,
    config: &Config,
    overrides: &crate::config::SeriesOverrides,
    acceptance: &crate::config::Acceptance,
) -> anyhow::Result<world::Manifest> {
    let scratch = tempfile::tempdir()?;
    let sources = download::sources(config);
    let downloads = scratch.path().join("downloads");
    stage_inputs(fixtures, &downloads, &sources)?;
    let inputs = sources
        .iter()
        .map(|s| download::register_local(&downloads, s))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let manifest =
        crate::build::assemble(scratch.path(), out, config, overrides, &sources, &inputs).await?;
    crate::build::verify(out, config, acceptance, None).await?;
    Ok(manifest)
}
