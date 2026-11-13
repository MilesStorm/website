//! `media-world fixture-refresh`: downloads the fixture works' records from Open Library's API
//! into fixtures/openlibrary/ as dump lines (open data; Goodreads stays synthetic). A developer
//! tool: run it when works.toml changes. At most one request a second, as Open Library asks of
//! clients without a contact.
//!
//! Ratings and reading-log rows are generated from each work's real totals, divided by
//! [`SCALE`] so the files stay small while the order of popularity stays real.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;

use crate::fixture::{self, OPENLIBRARY_FILES};
use crate::trace::{self, Peer, TracedClient};

const API: &str = "https://openlibrary.org";
/// One fixture rating or reading-log row stands for this many real ones.
const SCALE: f64 = 20.0;
/// Editions kept per work, picked for what the tests need (ISBNs, other titles, series
/// strings, Goodreads ids).
const MAX_EDITIONS: usize = 12;

struct Api {
    client: TracedClient,
}

impl Api {
    async fn json(&self, path: &str) -> anyhow::Result<Value> {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let response = self
            .client
            .get(format!("{API}{path}"))
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }
}

/// A dump line from a record: type, key, revision, last_modified, JSON.
fn dump_line(record: &Value) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        record["type"]["key"].as_str().unwrap_or(""),
        record["key"].as_str().unwrap_or(""),
        record["revision"].as_u64().unwrap_or(1),
        record["last_modified"]["value"].as_str().unwrap_or(""),
        serde_json::to_string(record).expect("a JSON value serialises"),
    )
}

fn pick_editions(work_title: &str, entries: &[Value]) -> Vec<Value> {
    let has_isbn = |e: &Value| e["isbn_13"].is_array() || e["isbn_10"].is_array();
    let english = |e: &Value| {
        e["languages"]
            .as_array()
            .is_some_and(|l| l.iter().any(|l| l["key"] == "/languages/eng"))
    };
    let mut picked: Vec<usize> = Vec::new();
    let mut add = |i: usize| {
        if picked.len() < MAX_EDITIONS && !picked.contains(&i) {
            picked.push(i);
        }
    };
    let indexed = || entries.iter().enumerate();
    for (i, _) in indexed().filter(|(_, e)| has_isbn(e) && english(e)).take(4) {
        add(i);
    }
    let mut titles = BTreeSet::new();
    for (i, e) in indexed() {
        let title = e["title"].as_str().unwrap_or("").to_lowercase();
        if has_isbn(e)
            && title != work_title.to_lowercase()
            && titles.len() < 4
            && titles.insert(title)
        {
            add(i);
        }
    }
    for (i, _) in indexed().filter(|(_, e)| e["series"].is_array()).take(4) {
        add(i);
    }
    for (i, _) in indexed()
        .filter(|(_, e)| e["identifiers"]["goodreads"].is_array())
        .take(2)
    {
        add(i);
    }
    if !entries.is_empty() {
        add(0);
    }
    picked.into_iter().map(|i| entries[i].clone()).collect()
}

fn rows(count: f64) -> u64 {
    if count <= 0.0 {
        0
    } else {
        (count / SCALE).ceil() as u64
    }
}

pub async fn refresh(fixtures: &Path, user_agent: &str) -> anyhow::Result<()> {
    let works = fixture::read_works(fixtures)?;
    let api = Api {
        client: trace::client(
            Peer {
                service: "openlibrary",
            },
            user_agent,
            Duration::from_secs(30),
            Duration::from_secs(60),
        ),
    };
    let mut work_lines = Vec::new();
    let mut edition_lines = Vec::new();
    let mut redirect_lines = Vec::new();
    let mut rating_lines = Vec::new();
    let mut log_lines = Vec::new();
    let mut authors = BTreeSet::new();
    for (i, w) in works.work.iter().enumerate() {
        tracing::info!(work = %w.key, n = i + 1, of = works.work.len(), "fixture work");
        let record = api
            .json(&format!("/works/{}.json", w.key))
            .await
            .with_context(|| w.key.to_string())?;
        if record["type"]["key"] == "/type/redirect" {
            redirect_lines.push(dump_line(&record));
            continue;
        }
        for a in record["authors"].as_array().into_iter().flatten() {
            if let Some(k) = a["author"]["key"].as_str().or_else(|| a["author"].as_str()) {
                authors.insert(k.to_owned());
            }
        }
        work_lines.push(dump_line(&record));
        let title = record["title"].as_str().unwrap_or("");
        let editions = api
            .json(&format!("/works/{}/editions.json?limit=100", w.key))
            .await?;
        for e in pick_editions(
            title,
            editions["entries"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ) {
            edition_lines.push(dump_line(&e));
        }
        let key = format!("/works/{}", w.key);
        let ratings = api.json(&format!("/works/{}/ratings.json", w.key)).await?;
        for (stars, count) in ratings["counts"].as_object().into_iter().flatten() {
            for _ in 0..rows(count.as_f64().unwrap_or(0.0)) {
                rating_lines.push(format!("{key}\t\\N\t{stars}\t2026-11-01"));
            }
        }
        let shelves = api
            .json(&format!("/works/{}/bookshelves.json", w.key))
            .await?;
        for (shelf, name) in [
            ("want_to_read", "Want to Read"),
            ("currently_reading", "Currently Reading"),
            ("already_read", "Already Read"),
        ] {
            for _ in 0..rows(shelves["counts"][shelf].as_f64().unwrap_or(0.0)) {
                log_lines.push(format!("{key}\t\\N\t{name}\t2026-11-01"));
            }
        }
    }
    let mut author_lines = Vec::new();
    for key in &authors {
        let record = api
            .json(&format!("{key}.json"))
            .await
            .with_context(|| key.clone())?;
        author_lines.push(dump_line(&record));
    }

    let out = fixtures.join("openlibrary");
    std::fs::create_dir_all(&out)?;
    let files: BTreeMap<&str, Vec<String>> = [
        ("ol_works", work_lines),
        ("ol_editions", edition_lines),
        ("ol_authors", author_lines),
        ("ol_ratings", rating_lines),
        ("ol_reading_log", log_lines),
        ("ol_redirects", redirect_lines),
    ]
    .into_iter()
    .collect();
    for (name, file) in OPENLIBRARY_FILES {
        // No timestamp in the gzip header: an unchanged refresh leaves the files unchanged.
        let mut gz = flate2::GzBuilder::new().mtime(0).write(
            std::fs::File::create(out.join(file))?,
            flate2::Compression::best(),
        );
        for line in &files[name] {
            gz.write_all(line.as_bytes())?;
            gz.write_all(b"\n")?;
        }
        gz.finish()?;
        tracing::info!(file, lines = files[name].len(), "fixture file written");
    }
    Ok(())
}
