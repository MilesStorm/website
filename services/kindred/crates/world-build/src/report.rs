//! The build report (02-world-data.md §7): counts and coverage, numbers only. It goes into the
//! world (report.json and the manifest) and into one log line.

use serde_json::{Map, Value, json};

use crate::stages::{self, Durations};
use crate::trace::Duck;

/// Counts the gates compare with the previous version.
pub const COMPARED: [&str; 3] = ["ol_works", "ol_editions", "searchable_works"];

pub fn compute(duck: &Duck, durations: &Durations) -> anyhow::Result<Value> {
    let count =
        |sql: &str| -> anyhow::Result<i64> { Ok(duck.conn().query_row(sql, [], |r| r.get(0))?) };
    let mut report = Map::new();
    let mut put = |name: &str, value: Value| {
        report.insert(name.to_owned(), value);
    };
    put("ol_works", json!(count("SELECT count(*) FROM ol_works")?));
    put(
        "ol_editions",
        json!(count("SELECT count(*) FROM ol_editions")?),
    );
    put(
        "ol_authors",
        json!(count("SELECT count(*) FROM ol_authors")?),
    );
    put(
        "ol_redirected_works",
        json!(count("SELECT count(*) FROM ol_redirects")?),
    );
    put("gr_books", json!(count("SELECT count(*) FROM gr_books")?));
    put("gr_works", json!(count("SELECT count(*) FROM gr_works")?));
    put(
        "gr_works_mapped",
        json!(count("SELECT count(*) FROM gr_work_map")?),
    );
    put(
        "gr_ratings_mapped_share",
        json!(stages::mapped_share(duck)?),
    );
    put(
        "searchable_works",
        json!(count("SELECT count(*) FROM catalogue")?),
    );
    put(
        "searchable_with_description",
        json!(count(
            "SELECT count(*) FROM catalogue WHERE has_description"
        )?),
    );
    put(
        "searchable_not_a_book",
        json!(count("SELECT count(*) FROM catalogue WHERE not_a_book")?),
    );
    put(
        "searchable_with_cover",
        json!(count(
            "SELECT count(*) FROM catalogue WHERE cover IS NOT NULL"
        )?),
    );
    put(
        "searchable_with_goodreads",
        json!(count("SELECT count(*) FROM catalogue WHERE gr_n > 0")?),
    );
    put(
        "searchable_with_series",
        json!(count(
            "SELECT count(*) FROM catalogue c JOIN work_series s ON s.work = c.work"
        )?),
    );
    put(
        "searchable_with_series_position",
        json!(count(
            "SELECT count(*) FROM catalogue c JOIN work_series s ON s.work = c.work WHERE s.position IS NOT NULL"
        )?),
    );
    // O3 in PLAN.md: of the 1,000 most popular works first published after 2017 that look like
    // part of a series, how many got a position.
    let (series_after_2017, positioned): (i64, i64) = duck.conn().query_row(
        "WITH candidates AS (
             SELECT c.work, p.popularity FROM catalogue c JOIN popularity p ON p.work = c.work
             WHERE c.year > 2017
               AND (list_contains(list_transform(c.subjects, lambda s: lower(s)), 'series')
                    OR c.work IN (SELECT e.work FROM ol_editions e WHERE list_contains(list_transform(e.series, lambda s: contains(s, '#')), true)))
             ORDER BY p.popularity DESC LIMIT 1000)
         SELECT count(*), count(s.position) FROM candidates c LEFT JOIN work_series s ON s.work = c.work",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    put("series_after_2017_top", json!(series_after_2017));
    put("series_after_2017_top_with_position", json!(positioned));
    let (a, b, fitted_on): (f64, f64, i64) =
        duck.conn()
            .query_row("SELECT a, b, fitted_on FROM popularity_fit", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
    put(
        "popularity_fit",
        json!({"a": a, "b": b, "works": fitted_on}),
    );
    put(
        "stage_seconds",
        Value::Object(
            durations
                .iter()
                .map(|(name, s)| (name.clone(), s.map_or(Value::Null, |s| json!(s))))
                .collect(),
        ),
    );
    Ok(Value::Object(report))
}
