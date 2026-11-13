use serde::{Deserialize, Serialize};

/// The on-disk format this crate writes and reads. A reader refuses any other.
pub const FORMAT_VERSION: u32 = 1;

/// `manifest.json`: what a world version contains and how it was made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    /// RFC 3339, UTC.
    pub built_at: String,
    /// The commit the builder was built from.
    pub builder: String,
    /// The downloaded inputs the world was built from.
    pub inputs: Vec<InputFile>,
    /// Every file of the version except the manifest itself, sorted by path.
    pub files: Vec<FileEntry>,
    /// How search ranks (from `world.toml` at build time).
    pub search: SearchParams,
    /// The build report: counts and coverage (02-world-data §7). Numbers only, no titles.
    pub report: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to the version's directory, `/`-separated.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFile {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// The server's `Last-Modified`, when it sent one.
    pub last_modified: Option<String>,
}

/// Search ranking constants. The score of a hit is its text relevance plus
/// `popularity_weight` times the work's popularity (`ln(1 + ratings)`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchParams {
    pub popularity_weight: f32,
    pub title_boost: f32,
    pub other_titles_boost: f32,
    pub authors_boost: f32,
    pub series_boost: f32,
    /// Added when the query is a whole title of the work (its own or an edition's), the last
    /// word possibly unfinished.
    pub whole_title_bonus: f32,
    /// Subtracted from summaries, study guides and box sets.
    pub not_a_book_penalty: f32,
    /// Below this many hits, the query is retried with prefixes on every word and one typo
    /// allowed in words of at least `typo_min_len` letters.
    pub retry_below: usize,
    pub typo_min_len: usize,
}
