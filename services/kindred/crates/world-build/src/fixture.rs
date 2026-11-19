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

use crate::config::{Aspects, Config};
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
    let interactions = interactions(&synthetic.planted);
    for (name, lines) in [
        ("gr_books", synthetic.books),
        ("gr_works", synthetic.works),
        ("gr_series", synthetic.series),
        ("gr_interactions", interactions),
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
    /// Each synthetic Goodreads work's planted taste vector, and its books' ids.
    pub planted: Vec<Planted>,
}

/// Dimensions of the planted taste model of the synthetic readers.
pub const PLANTED_DIMS: usize = 4;

pub struct Planted {
    pub latent: [f64; PLANTED_DIMS],
    pub books: Vec<u64>,
}

/// Shelves the synthetic books carry: for each planted dimension a pair of aspect shelves (one
/// for each sign), and organisational ones every book has.
const PLANTED_SHELVES: [[&str; 2]; PLANTED_DIMS] = [
    ["fantasy", "historical-fiction"],
    ["dark", "cozy"],
    ["fast-paced", "slow-burn"],
    ["romance", "war"],
];

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
    let mut planted = Vec::new();
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
        let latent: [f64; PLANTED_DIMS] =
            std::array::from_fn(|_| rng.random_range(-1.0..1.0));
        // Readers shelve a work by its planted traits, and everyone files it as read.
        let mut shelves = vec![json!({"count": (ratings * 3).to_string(), "name": "to-read"}),
                               json!({"count": ratings.to_string(), "name": "read"})];
        for (dim, pair) in PLANTED_SHELVES.iter().enumerate() {
            let strength = latent[dim].abs();
            if strength > 0.3 {
                let name = pair[usize::from(latent[dim] < 0.0)];
                let count = ((ratings as f64) * strength * 0.5).round().max(1.0) as u64;
                shelves.push(json!({"count": count.to_string(), "name": name}));
            }
        }
        let mut work_books = Vec::new();
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
            let mut b = book(
                id,
                gr_work,
                isbn,
                isbn13,
                count,
                series.clone(),
                title.clone(),
                &plain,
            );
            b["popular_shelves"] = json!(shelves);
            books.push(b);
            work_books.push(id);
        }
        planted.push(Planted {
            latent,
            books: work_books,
        });
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
        planted,
    })
}

/// Synthetic readers of the fixture works, from a seeded low-rank model (02-world-data.md §10):
/// each reader has a taste vector, rates a random subset of works in random order, and gives
/// stars by how well the work's planted vector matches theirs. The benchmark must recover this
/// model on the fixture world.
pub fn interactions(planted: &[Planted]) -> Vec<Value> {
    const READERS: u64 = 1500;
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(2018);
    let mut out = Vec::new();
    let start = 1_400_000_000i64; // 2014-05
    for reader in 0..READERS {
        let taste: [f64; PLANTED_DIMS] = std::array::from_fn(|_| rng.random_range(-1.0..1.0));
        let count = rng.random_range(60..=110).min(planted.len());
        let mut order: Vec<usize> = (0..planted.len()).collect();
        for i in 0..count {
            let j = rng.random_range(i..order.len());
            order.swap(i, j);
        }
        let mut time = start + rng.random_range(0..50_000_000i64);
        for &w in &order[..count] {
            let work = &planted[w];
            let fit: f64 = taste
                .iter()
                .zip(&work.latent)
                .map(|(a, b)| a * b)
                .sum::<f64>()
                + rng.random_range(-0.3..0.3);
            let stars = match fit {
                f if f > 0.6 => 5,
                f if f > 0.15 => 4,
                f if f > -0.25 => 3,
                f if f > -0.6 => 2,
                _ => 1,
            };
            time += rng.random_range(86_400..30 * 86_400i64);
            let date = chrono::DateTime::from_timestamp(time, 0)
                .expect("a valid time")
                .format("%a %b %d %H:%M:%S +0000 %Y")
                .to_string();
            let book = work.books[rng.random_range(0..work.books.len())];
            out.push(json!({
                "user_id": format!("{reader:032x}"), "book_id": book.to_string(),
                "review_id": format!("{:032x}", reader * 1000 + w as u64), "is_read": true,
                "rating": stars, "review_text_incomplete": "", "date_added": date,
                "date_updated": date, "read_at": "", "started_at": ""
            }));
        }
        // Shelved but unrated: not a rating.
        out.push(json!({"user_id": format!("{reader:032x}"), "book_id": "1", "review_id": "",
            "is_read": false, "rating": 0, "review_text_incomplete": "",
            "date_added": "Fri Sep 08 10:44:24 -0700 2017", "date_updated": "", "read_at": "", "started_at": ""}));
    }
    out
}

/// The build settings of the fixture world: the real configuration with the embedding stand-in
/// and the counts scaled to a few hundred works and synthetic readers.
pub fn settings(
    config: &Config,
    overrides: &crate::config::SeriesOverrides,
    aspects: &Aspects,
) -> crate::build::Settings {
    let mut config = config.clone();
    config.interactions.test_users = 300;
    config.interactions.gate_users = 100;
    config.interactions.tuning_users = 300;
    config.factors.k = 8;
    config.factors.cold_from_year = 2012;
    config.vectors.min_popularity = 1.0;
    config.verify.recommendable = (50, u64::MAX);
    config.bench.accepted_ndcg10_n20 = None;
    crate::build::Settings {
        config,
        overrides: overrides.clone(),
        aspects: aspects.clone(),
        fixture: true,
    }
}

/// Builds the fixture world into `out` and verifies it like a real one.
pub async fn build(
    fixtures: &Path,
    out: &Path,
    config: &Config,
    overrides: &crate::config::SeriesOverrides,
    aspects: &Aspects,
    acceptance: &crate::config::Acceptance,
) -> anyhow::Result<world::Manifest> {
    let scratch = tempfile::tempdir()?;
    build_in(scratch.path(), fixtures, out, config, overrides, aspects, acceptance).await
}

/// [`build`] with a scratch directory the caller keeps (the benchmark's tests read it after).
#[allow(clippy::too_many_arguments, reason = "build's inputs and where to keep the scratch")]
pub async fn build_in(
    scratch: &Path,
    fixtures: &Path,
    out: &Path,
    config: &Config,
    overrides: &crate::config::SeriesOverrides,
    aspects: &Aspects,
    acceptance: &crate::config::Acceptance,
) -> anyhow::Result<world::Manifest> {
    let settings = settings(config, overrides, aspects);
    let config = &settings.config;
    let sources = download::sources(config);
    let downloads = scratch.join("downloads");
    stage_inputs(fixtures, &downloads, &sources)?;
    let inputs = sources
        .iter()
        .map(|s| download::register_local(&downloads, s))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let manifest = crate::build::assemble(scratch, out, &settings, &sources, &inputs).await?;
    crate::build::verify(out, config, acceptance, None).await?;
    Ok(manifest)
}
