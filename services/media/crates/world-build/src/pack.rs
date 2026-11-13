//! Stages search-index and pack: the searchable works from DuckDB into a world directory
//! (crate `world`): the tantivy index, the per-work columns, series names and redirects.

use std::collections::HashMap;
use std::path::Path;

use world::{BookDoc, WorkKey, WorkRow, WorldWriter};

use crate::trace::Duck;

/// Unit separator: joins list values in SQL, split here.
const SEP: char = '\u{1f}';

fn split(s: Option<String>) -> Vec<String> {
    s.map(|s| {
        s.split(SEP)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

/// Writes the world's data files into `dir` (empty); the caller finishes the manifest.
pub fn write(duck: &Duck, dir: &Path) -> anyhow::Result<WorldWriter> {
    let mut writer = WorldWriter::create(dir)?;
    let mut rows = Vec::new();
    let mut series_rows: HashMap<String, u32> = HashMap::new();
    let mut series_names: Vec<String> = Vec::new();

    let span = tracing::info_span!("pack.search_index");
    let _entered = span.enter();
    let mut statement = duck.conn().prepare(
        "SELECT c.work, c.title, c.subtitle,
                array_to_string(c.authors, chr(31)), array_to_string(c.other_titles, chr(31)),
                c.year, c.cover, c.not_a_book, c.has_description,
                p.popularity, p.quality, s.series_key, s.name, s.position
         FROM catalogue c
         JOIN popularity p ON p.work = c.work
         LEFT JOIN work_series s ON s.work = c.work
         ORDER BY p.popularity DESC, c.work",
    )?;
    let mut result = statement.query([])?;
    while let Some(r) = result.next()? {
        let key = WorkKey(r.get::<_, i64>(0)? as u64);
        let popularity = r.get::<_, f64>(9)? as f32;
        let series_key: Option<String> = r.get(11)?;
        let series_name: Option<String> = r.get(12)?;
        let position = r.get::<_, Option<f64>>(13)?.map(|p| p as f32);
        // Most popular work first: a series is named as its best-known work's data names it.
        let series = match (series_key, &series_name) {
            (Some(k), Some(name)) => {
                let next = series_names.len() as u32;
                let row = *series_rows.entry(k).or_insert_with(|| {
                    series_names.push(name.clone());
                    next
                });
                Some((row, position))
            }
            _ => None,
        };
        let year = r
            .get::<_, Option<i64>>(5)?
            .and_then(|y| i16::try_from(y).ok());
        writer.add_book(&BookDoc {
            key,
            title: r.get(1)?,
            subtitle: r.get(2)?,
            authors: split(r.get(3)?),
            other_titles: split(r.get(4)?),
            series: series.map(|(row, _)| series_names[row as usize].clone()),
            series_position: position.filter(|_| series.is_some()),
            year,
            cover: r
                .get::<_, Option<i64>>(6)?
                .and_then(|c| u64::try_from(c).ok()),
            popularity,
            not_a_book: r.get(7)?,
        })?;
        rows.push(WorkRow {
            key,
            popularity,
            quality: r.get::<_, f64>(10)? as f32,
            year,
            not_a_book: r.get(7)?,
            has_description: r.get(8)?,
            series,
        });
    }
    drop(_entered);
    tracing::info!(
        works = rows.len(),
        series = series_names.len(),
        "search index written"
    );

    writer.write_works(rows, &series_names)?;
    let mut redirects = Vec::new();
    let mut statement = duck.conn().prepare("SELECT old, new FROM ol_redirects")?;
    let mut result = statement.query([])?;
    while let Some(r) = result.next()? {
        redirects.push((
            WorkKey(r.get::<_, i64>(0)? as u64),
            WorkKey(r.get::<_, i64>(1)? as u64),
        ));
    }
    writer.write_redirects(redirects)?;
    Ok(writer)
}
