//! The build's stages, in order (02-world-data.md §3): the SQL stages (ingest → identity →
//! catalogue → series → popularity → interactions → aspects) run against the build's DuckDB
//! file; the model stages (factors → embed → text-maps → vectors → prior → bench) are Rust and
//! keep their results in the scratch volume's `model/` directory. A stage writes a done-marker
//! holding the hash of its inputs (the previous stage's hash, its SQL or code version, its
//! variables and settings); a rerun skips a stage whose marker still matches, so a failed
//! monthly run resumes where it stopped.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, bail};
use sha2::{Digest as _, Sha256};
use world::InputFile;

use crate::config::{Aspects, Config, SeriesOverrides};
use crate::download::{self, Source};
use crate::trace::{Duck, Value};

pub const MACROS: &str = include_str!("../sql/macros.sql");

/// DuckDB's memory limit while a Rust stage runs: its queries there only stream rows.
const RUST_STAGE_DUCKDB: &str = "1GB";

/// A stage: its name and what it runs.
/// A model stage's Rust: its version and the function.
pub type RustStage = (&'static str, fn(&Run) -> anyhow::Result<()>);

pub struct Stage {
    pub name: &'static str,
    pub sql: &'static str,
    /// A model stage: the Rust that runs after the SQL (which is then usually empty). Its
    /// version string is part of the hash: change it when the stage's code changes its output.
    pub rust: Option<RustStage>,
}

pub const STAGES: [Stage; 14] = [
    Stage {
        name: "ingest",
        sql: include_str!("../sql/ingest.sql"),
        rust: None,
    },
    Stage {
        name: "identity",
        sql: include_str!("../sql/identity.sql"),
        rust: None,
    },
    Stage {
        name: "catalogue",
        sql: include_str!("../sql/catalogue.sql"),
        rust: None,
    },
    Stage {
        name: "series",
        sql: include_str!("../sql/series.sql"),
        rust: None,
    },
    Stage {
        name: "popularity",
        sql: include_str!("../sql/popularity.sql"),
        rust: None,
    },
    Stage {
        name: "interactions",
        sql: include_str!("../sql/interactions.sql"),
        rust: None,
    },
    Stage {
        name: "aspects",
        sql: include_str!("../sql/aspects.sql"),
        rust: Some(("1", crate::aspects::report_unmatched)),
    },
    Stage {
        name: "factors",
        sql: "",
        rust: Some(("1", crate::factors::run)),
    },
    Stage {
        name: "embed",
        sql: "",
        rust: Some(("1", crate::embed::run)),
    },
    Stage {
        name: "text-maps",
        sql: "",
        rust: Some(("1", crate::text_maps::run)),
    },
    Stage {
        name: "vectors",
        sql: "",
        rust: Some(("1", crate::vectors::run)),
    },
    Stage {
        name: "neighbours",
        sql: "",
        rust: Some(("1", crate::neighbours::run)),
    },
    Stage {
        name: "prior",
        sql: "",
        rust: Some(("1", crate::bench::fit_prior)),
    },
    Stage {
        name: "bench",
        sql: "",
        rust: Some(("1", crate::bench::gate)),
    },
];

/// Goodreads fields each file must have (02-world-data.md §2).
const GOODREADS_FIELDS: [(&str, &[&str]); 4] = [
    (
        "gr_books",
        &[
            "book_id",
            "work_id",
            "isbn",
            "isbn13",
            "ratings_count",
            "series",
            "title",
            "title_without_series",
            "language_code",
            "popular_shelves",
        ],
    ),
    (
        "gr_works",
        &[
            "work_id",
            "ratings_count",
            "ratings_sum",
            "original_publication_year",
        ],
    ),
    ("gr_series", &["series_id", "title"]),
    (
        "gr_interactions",
        &["user_id", "book_id", "rating", "date_added", "date_updated"],
    ),
];

/// How long each stage took this run, `None` when it was skipped.
pub type Durations = Vec<(String, Option<f64>)>;

pub struct Run<'a> {
    pub duck: &'a Duck,
    pub config: &'a Config,
    pub overrides: &'a SeriesOverrides,
    pub aspects: &'a Aspects,
    /// The model stages' files (factors, embeddings, maps, vectors), on the scratch volume.
    pub model: PathBuf,
    /// The fixture world: the text embedding is the seeded stand-in, never the model.
    pub fixture: bool,
    pub downloads: &'a Path,
    pub markers: PathBuf,
    pub sources: &'a [Source],
    pub inputs: &'a [InputFile],
}

impl Run<'_> {
    /// The variables each stage's SQL reads.
    fn variables(&self, stage: &str) -> Vec<(&'static str, Value)> {
        let c = self.config;
        let any = |patterns: &[String]| {
            Value::Text(
                patterns
                    .iter()
                    .map(|p| format!("(?:{p})"))
                    .collect::<Vec<_>>()
                    .join("|"),
            )
        };
        match stage {
            "ingest" => self
                .sources
                .iter()
                .map(|s| {
                    (
                        s.name,
                        Value::Text(download::path(self.downloads, s).display().to_string()),
                    )
                })
                .collect(),
            "catalogue" => vec![
                (
                    "min_goodreads_ratings",
                    Value::Int(c.catalogue.min_goodreads_ratings),
                ),
                ("max_other_titles", Value::Int(c.catalogue.max_other_titles)),
                (
                    "min_description_chars",
                    Value::Int(c.catalogue.min_description_chars),
                ),
                ("not_a_book_titles", any(&c.catalogue.not_a_book_titles)),
                ("not_a_book_subjects", any(&c.catalogue.not_a_book_subjects)),
            ],
            "popularity" => vec![("prior_weight", Value::Float(c.popularity.prior_weight))],
            "interactions" => {
                let i = &c.interactions;
                let f = &c.factors;
                vec![
                    ("gr_interactions", self.source_path("gr_interactions")),
                    ("min_eligible_ratings", Value::Int(i.min_eligible_ratings)),
                    ("test_users", Value::Int(i.test_users)),
                    ("gate_users", Value::Int(i.gate_users)),
                    ("tuning_users", Value::Int(i.tuning_users)),
                    ("split_seed", Value::Text(i.seed.to_string())),
                    ("min_user_likes", Value::Int(f.min_user_likes)),
                    ("min_work_likes", Value::Int(f.min_work_likes)),
                    ("cold_from_year", Value::Int(f.cold_from_year)),
                ]
            }
            "aspects" => {
                let a = &c.aspects;
                vec![
                    ("gr_books", self.source_path("gr_books")),
                    ("org_patterns", any(&self.aspects.organisational.patterns)),
                    ("aspect_alpha", Value::Float(a.alpha)),
                    ("subject_weight", Value::Float(a.subject_weight)),
                    ("min_shelvings", Value::Float(a.min_shelvings)),
                    ("report_unmatched", Value::Int(a.report_unmatched)),
                ]
            }
            _ => vec![],
        }
    }

    fn source_path(&self, name: &str) -> Value {
        let source = self
            .sources
            .iter()
            .find(|s| s.name == name)
            .expect("a known source");
        Value::Text(download::path(self.downloads, source).display().to_string())
    }

    /// The settings a model stage's output depends on, hashed into its marker.
    fn settings(&self, stage: &str) -> anyhow::Result<Vec<u8>> {
        let c = self.config;
        Ok(match stage {
            "aspects" => serde_json::to_vec(self.aspects)?,
            "factors" => serde_json::to_vec(&c.factors)?,
            "embed" => serde_json::to_vec(&(&c.embed, self.fixture))?,
            "text-maps" => serde_json::to_vec(&c.text_maps)?,
            "vectors" => serde_json::to_vec(&(&c.vectors, &c.model))?,
            "neighbours" => serde_json::to_vec(&c.bench.neighbours)?,
            "prior" | "bench" => serde_json::to_vec(&(&c.model, &c.bench))?,
            _ => Vec::new(),
        })
    }

    /// Runs every stage that isn't done yet. Returns each stage's duration.
    pub fn all(&self) -> anyhow::Result<Durations> {
        std::fs::create_dir_all(&self.markers)?;
        // Downloads count by content, not by path.
        let mut hash = hex::encode(Sha256::digest(serde_json::to_vec(self.inputs)?));
        let mut durations = Vec::new();
        for stage in &STAGES {
            let variables = self.variables(stage.name);
            let mut h = Sha256::new();
            h.update(hash.as_bytes());
            h.update(MACROS);
            h.update(stage.sql);
            for (name, value) in &variables {
                // Downloads count by content (above), not by where they are.
                if !self.sources.iter().any(|s| s.name == *name) {
                    h.update(format!("{name}={value:?};"));
                }
            }
            if let Some((version, _)) = stage.rust {
                // The commit too: a new image's code reruns the model stages, whatever the
                // hand-kept version says.
                h.update(format!("rust {version} {};", crate::build::builder()));
            }
            h.update(self.settings(stage.name)?);
            if stage.name == "series" {
                h.update(serde_json::to_vec(self.overrides)?);
            }
            hash = hex::encode(h.finalize());
            let marker = self.markers.join(format!("{}.done", stage.name));
            if std::fs::read_to_string(&marker).is_ok_and(|done| done.trim() == hash) {
                tracing::info!(stage = stage.name, "stage already done");
                durations.push((stage.name.to_owned(), None));
                continue;
            }
            let _ = std::fs::remove_file(&marker);
            let span = tracing::info_span!("stage", otel.name = format!("stage {}", stage.name));
            let started = Instant::now();
            span.in_scope(|| self.one(stage, &variables))
                .with_context(|| format!("stage {}", stage.name))?;
            let seconds = started.elapsed().as_secs_f64();
            tracing::info!(stage = stage.name, seconds, "stage done");
            durations.push((stage.name.to_owned(), Some(seconds)));
            std::fs::write(&marker, &hash)?;
        }
        Ok(durations)
    }

    fn one(&self, stage: &Stage, variables: &[(&'static str, Value)]) -> anyhow::Result<()> {
        self.duck.exec("macros", MACROS)?;
        for (name, value) in variables {
            self.duck.set(name, value)?;
        }
        match stage.name {
            "ingest" => {
                for (name, fields) in GOODREADS_FIELDS {
                    let source = self
                        .sources
                        .iter()
                        .find(|s| s.name == name)
                        .expect("a Goodreads source");
                    check_fields(&download::path(self.downloads, source), fields)?;
                }
            }
            "series" => self.load_overrides()?,
            "aspects" => crate::aspects::load(self.duck, self.aspects)?,
            _ => {}
        }
        if !stage.sql.is_empty() {
            self.duck.sql_stage(stage.name, stage.sql)?;
        }
        if let Some((_, rust)) = stage.rust {
            std::fs::create_dir_all(&self.model)?;
            // The Rust stages hold their own data (the factorization's pairs, the embeddings):
            // DuckDB gives back its buffer pool first, so the two never add up.
            self.duck.exec(
                "release memory",
                &format!("SET memory_limit = '{RUST_STAGE_DUCKDB}';"),
            )?;
            let result = rust(self);
            self.duck.exec(
                "restore memory",
                &format!(
                    "SET memory_limit = '{}';",
                    self.config.duckdb.memory_limit.replace('\'', "")
                ),
            )?;
            result?;
        }
        if stage.name == "identity" {
            let share = mapped_share(self.duck)?;
            tracing::info!(share, "Goodreads ratings on mapped works");
            if share < self.config.identity.min_mapped_share {
                bail!(
                    "only {:.1}% of Goodreads ratings map to an Open Library work (the gate is {:.0}%): check ISBN normalisation",
                    share * 100.0,
                    self.config.identity.min_mapped_share * 100.0
                );
            }
        }
        Ok(())
    }

    fn load_overrides(&self) -> anyhow::Result<()> {
        self.duck.exec(
            "series_overrides",
            "CREATE OR REPLACE TABLE series_overrides (work BIGINT, name VARCHAR, position DOUBLE);",
        )?;
        let mut appender = self.duck.conn().appender("series_overrides")?;
        for o in &self.overrides.work {
            appender.append_row(duckdb::params![o.key.0 as i64, o.series, o.position])?;
        }
        appender.flush()?;
        Ok(())
    }
}

/// Share of all Goodreads ratings on works that map to an Open Library work.
pub fn mapped_share(duck: &Duck) -> anyhow::Result<f64> {
    Ok(duck.conn().query_row(
        "SELECT coalesce(sum(w.ratings_count) FILTER (WHERE m.gr_work IS NOT NULL) / nullif(sum(w.ratings_count), 0), 0)::DOUBLE
         FROM gr_works w LEFT JOIN gr_work_map m ON m.gr_work = w.work_id",
        [],
        |r| r.get(0),
    )?)
}

/// The first record of a Goodreads file must have every field the ingest reads; otherwise
/// stop and show the fields it does have.
pub fn check_fields(path: &Path, fields: &[&str]) -> anyhow::Result<()> {
    let file = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let mut line = String::new();
    std::io::BufReader::new(flate2::read::MultiGzDecoder::new(file)).read_line(&mut line)?;
    let record: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&line)
        .with_context(|| format!("{}: first line is not a JSON object", path.display()))?;
    let missing: Vec<&str> = fields
        .iter()
        .copied()
        .filter(|f| !record.contains_key(*f))
        .collect();
    if !missing.is_empty() {
        let have: Vec<&String> = record.keys().collect();
        bail!(
            "{}: fields {missing:?} are missing; the file has {have:?}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn duck() -> Duck {
        let duck = Duck::memory();
        duck.exec("macros", MACROS).unwrap();
        duck
    }

    fn one<T: duckdb::types::FromSql>(duck: &Duck, sql: &str) -> T {
        duck.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn isbns_normalise_to_isbn13() {
        let duck = duck();
        for (written, expected) in [
            ("0-306-40615-2", Some("9780306406157")),
            ("0441013597", Some("9780441013593")),
            ("0-8044-2957-X", Some("9780804429573")),
            ("080442957x", Some("9780804429573")),
            ("978-0-306-40615-7", Some("9780306406157")),
            ("9780593135204", Some("9780593135204")),
            ("ISBN 0 7653 2635 X", Some("9780765326355")),
            ("12345", None),
            ("1234567890123", None),
            ("", None),
        ] {
            let got: Option<String> = one(&duck, &format!("SELECT isbn13('{written}')"));
            assert_eq!(got.as_deref(), expected, "{written}");
        }
    }

    #[test]
    fn series_strings_parse() {
        let duck = duck();
        for (written, name, number) in [
            (
                "The Stormlight Archive #1",
                "The Stormlight Archive",
                Some(1.0),
            ),
            (
                "The Stormlight Archive, #2",
                "The Stormlight Archive",
                Some(2.0),
            ),
            ("Harry Potter, 3", "Harry Potter", Some(3.0)),
            ("Discworld (5)", "Discworld", Some(5.0)),
            (
                "El Archivo de las Tormentas, 5",
                "El Archivo de las Tormentas",
                Some(5.0),
            ),
            ("Mistborn, book 2", "Mistborn", Some(2.0)),
            ("The Expanse ; bk. 1", "The Expanse", Some(1.0)),
            ("Dune Chronicles, Vol. 3", "Dune Chronicles", Some(3.0)),
            ("The Empyrean", "The Empyrean", None),
            ("Wheel of Time no. 4.5", "Wheel of Time", Some(4.5)),
            ("Discworld novel", "Discworld novel", None),
        ] {
            let got: (String, Option<f64>) = duck
                .conn()
                .query_row(
                    &format!(
                        "SELECT p.name, p.number FROM (SELECT series_parse('{written}') AS p)"
                    ),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(got, (name.to_owned(), number), "{written}");
        }
    }

    #[test]
    fn series_names_normalise() {
        let duck = duck();
        for (a, b) in [
            ("The Stormlight Archive", "stormlight archive"),
            (
                "Hitchhiker's Guide to the Galaxy",
                "hitchhikers guide to the galaxy",
            ),
            ("The Hunger Games Trilogy", "hunger games"),
            ("Dune Chronicles", "dune"),
            ("Mistborn Saga", "mistborn"),
            ("A Song of Ice and Fire", "a song of ice and fire"),
            ("Épée-Series", "epee"),
        ] {
            let got: String = one(
                &duck,
                &format!("SELECT series_norm('{}')", a.replace('\'', "''")),
            );
            assert_eq!(got, b, "{a}");
        }
    }

    #[test]
    fn keys_and_years() {
        let duck = duck();
        assert_eq!(one::<i64>(&duck, "SELECT ol_num('/works/OL45804W')"), 45804);
        assert_eq!(
            one::<Option<i64>>(&duck, "SELECT ol_num('/works/OL45804')"),
            None
        );
        assert_eq!(one::<i16>(&duck, "SELECT year_of('August 1965')"), 1965);
        assert_eq!(one::<Option<i16>>(&duck, "SELECT year_of('n.d.')"), None);
    }

    #[test]
    fn missing_goodreads_fields_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json.gz");
        let mut gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        std::io::Write::write_all(&mut gz, b"{\"work_id\": \"1\", \"rating_count\": \"3\"}\n")
            .unwrap();
        gz.finish().unwrap();
        check_fields(&path, &["work_id"]).unwrap();
        let err = check_fields(&path, &["work_id", "ratings_count"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ratings_count") && err.contains("rating_count"),
            "{err}"
        );
    }
}
