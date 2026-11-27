//! `world.toml`, `series-overrides.toml` and `search-acceptance.toml`. The repository's copies
//! are compiled in; `--config` and friends read other files.

use serde::{Deserialize, Serialize};

pub const WORLD_TOML: &str = include_str!("../../../world.toml");
pub const SERIES_OVERRIDES_TOML: &str = include_str!("../../../series-overrides.toml");
pub const ACCEPTANCE_TOML: &str = include_str!("../search-acceptance.toml");
pub const ASPECTS_TOML: &str = include_str!("../../../aspects.toml");

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub sources: Sources,
    pub duckdb: DuckDb,
    pub identity: Identity,
    pub catalogue: Catalogue,
    pub popularity: Popularity,
    pub interactions: Interactions,
    pub factors: Factors,
    pub aspects: AspectSettings,
    pub embed: Embed,
    pub text_maps: TextMaps,
    pub vectors: Vectors,
    /// The model's hyperparameters (03-engine.md), set by the benchmark.
    pub model: taste::Hyper,
    pub bench: Bench,
    pub search: Search,
    pub verify: Verify,
    pub publish: Publish,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub user_agent: String,
    pub openlibrary: OpenLibrary,
    pub goodreads: Goodreads,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenLibrary {
    pub base: String,
    pub works: String,
    pub editions: String,
    pub authors: String,
    pub ratings: String,
    pub reading_log: String,
    pub redirects: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goodreads {
    pub base: String,
    pub books: String,
    pub works: String,
    pub series: String,
    pub interactions: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuckDb {
    pub memory_limit: String,
    pub threads: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub min_mapped_share: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    pub min_goodreads_ratings: i64,
    pub max_other_titles: i64,
    pub min_description_chars: i64,
    pub not_a_book_titles: Vec<String>,
    pub not_a_book_subjects: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Popularity {
    pub prior_weight: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interactions {
    pub min_eligible_ratings: i64,
    pub test_users: i64,
    pub gate_users: i64,
    pub tuning_users: i64,
    pub seed: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Factors {
    pub k: usize,
    pub iterations: u32,
    /// Size of the star block (explicit factors on star ratings, 03-engine.md §2).
    pub k_stars: usize,
    /// Weighted-λ regularisation of the star factorization: λ · (ratings of the row + n0).
    pub stars_regularisation: f64,
    /// `n0` of the star factorization: biases are shrunk by `n / (n + n0)` (03-engine.md §2).
    pub stars_prior_count: f64,
    pub stars_iterations: u32,
    pub min_user_likes: i64,
    pub min_work_likes: i64,
    pub cold_from_year: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AspectSettings {
    pub alpha: f64,
    pub subject_weight: f64,
    pub min_shelvings: f64,
    pub report_unmatched: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Embed {
    pub model_dir: std::path::PathBuf,
    pub model_id: String,
    pub dimensions: usize,
    pub max_tokens: usize,
    pub batch: usize,
    /// At most this many tokens (texts × padded length) in one forward pass: long texts go
    /// through in smaller batches, so the GPU memory a pass needs is bounded (the card is shared).
    pub batch_tokens: usize,
    pub max_subjects: usize,
    pub max_description_chars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextMaps {
    pub ridge: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vectors {
    pub min_popularity: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bench {
    pub revealed: Vec<usize>,
    pub neighbours: usize,
    pub max_prior_terms: usize,
    pub min_share: f64,
    /// NDCG@10 at n = 20 on the gate subset when M2 was accepted.
    pub accepted_ndcg10_n20: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    pub popularity_weight: f32,
    pub title_boost: f32,
    pub other_titles_boost: f32,
    pub authors_boost: f32,
    pub series_boost: f32,
    pub whole_title_bonus: f32,
    pub not_a_book_penalty: f32,
    pub retry_below: usize,
    pub typo_min_len: usize,
    pub p95_ms: u64,
}

impl Search {
    pub fn params(&self) -> world::SearchParams {
        world::SearchParams {
            popularity_weight: self.popularity_weight,
            title_boost: self.title_boost,
            other_titles_boost: self.other_titles_boost,
            authors_boost: self.authors_boost,
            series_boost: self.series_boost,
            whole_title_bonus: self.whole_title_bonus,
            not_a_book_penalty: self.not_a_book_penalty,
            retry_below: self.retry_below,
            typo_min_len: self.typo_min_len,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verify {
    pub max_change: f64,
    /// The range the number of recommendable works must be in (02-world-data.md §7).
    pub recommendable: (u64, u64),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
}

/// `series-overrides.toml`: forced series and positions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriesOverrides {
    #[serde(default)]
    pub work: Vec<SeriesOverride>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriesOverride {
    pub key: world::WorkKey,
    pub series: String,
    pub position: Option<f64>,
}

/// `aspects.toml` (03-engine.md §5).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aspects {
    pub aspect: Vec<world::Aspect>,
    pub organisational: Organisational,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Organisational {
    pub shelves: Vec<String>,
    /// Regular expressions, each matched against the whole shelf name.
    pub patterns: Vec<String>,
}

impl Aspects {
    /// Ids are stored in the kindred database: unique, and never empty.
    pub fn check(&self) -> anyhow::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for a in &self.aspect {
            anyhow::ensure!(!a.id.is_empty(), "aspects.toml: an aspect without an id");
            anyhow::ensure!(seen.insert(&a.id), "aspects.toml: {} twice", a.id);
            anyhow::ensure!(
                !a.shelves.is_empty() || !a.subjects.is_empty(),
                "aspects.toml: {} matches nothing",
                a.id
            );
            anyhow::ensure!(
                !a.chip || !a.label.is_empty(),
                "aspects.toml: chip {} has no label",
                a.id
            );
            anyhow::ensure!(
                ["", "light", "dark"].contains(&a.steer.as_str()),
                "aspects.toml: {}: steer is light, dark or empty",
                a.id
            );
        }
        Ok(())
    }
}

/// `search-acceptance.toml`: queries that must find a work.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acceptance {
    pub case: Vec<AcceptanceCase>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceCase {
    pub query: String,
    /// The hit must be within the first `top` results (1: the first).
    #[serde(default = "first")]
    pub top: usize,
    /// What the hit must have; every one given must hold. Compared case- and
    /// accent-insensitively; `title` exactly, the others as "contains".
    pub title: Option<String>,
    pub author: Option<String>,
    pub series: Option<String>,
}

fn first() -> usize {
    1
}

pub fn parse<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> anyhow::Result<T> {
    toml::from_str(text).map_err(|e| anyhow::anyhow!("{what}: {e}"))
}

pub fn read<T: serde::de::DeserializeOwned>(
    path: Option<&std::path::Path>,
    built_in: &str,
    what: &str,
) -> anyhow::Result<T> {
    match path {
        Some(path) => parse(
            what,
            &std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?,
        ),
        None => parse(what, built_in),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_files_parse() {
        let config: Config = parse("world.toml", WORLD_TOML).unwrap();
        assert!(config.identity.min_mapped_share > 0.5);
        let overrides: SeriesOverrides =
            parse("series-overrides.toml", SERIES_OVERRIDES_TOML).unwrap();
        assert!(overrides.work.is_empty(), "starts empty (02-world-data §5)");
        let acceptance: Acceptance = parse("search-acceptance.toml", ACCEPTANCE_TOML).unwrap();
        assert_eq!(acceptance.case.len(), 14);
        let aspects: Aspects = parse("aspects.toml", ASPECTS_TOML).unwrap();
        aspects.check().unwrap();
        assert!(aspects.aspect.len() >= 50);
        for case in &acceptance.case {
            assert!(
                case.title.is_some() || case.author.is_some() || case.series.is_some(),
                "{} expects nothing",
                case.query
            );
        }
    }
}
