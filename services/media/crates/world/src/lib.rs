//! The world: the read-only book data every user shares (catalogue, search index, per-work
//! numbers), built monthly by `media-world` and memory-mapped by the media service.
//!
//! One version of a world is a directory of files listed in its `manifest.json`, each with
//! its size and SHA-256. [`writer::WorldWriter`] writes one; [`World::open`] reads one and
//! refuses a format it does not know or a file that does not match the manifest. Works are
//! identified by their Open Library work key ([`WorkKey`]); row numbers are private to one
//! version.

#[cfg(not(target_endian = "little"))]
compile_error!("the world's columns are little-endian and memory-mapped as they are");

mod columns;
mod key;
mod manifest;
pub mod reader;
pub mod search;
pub mod writer;

pub use key::{KeyError, WorkKey};
pub use manifest::{FORMAT_VERSION, FileEntry, InputFile, Manifest, SearchParams};
pub use reader::World;
pub use search::Hit;
pub use writer::{BookDoc, WorkRow, WorldWriter};

/// Names of the files in a world version, relative to its directory.
pub mod files {
    pub const MANIFEST: &str = "manifest.json";
    pub const REPORT: &str = "report.json";
    /// Work numbers (`u64`), ascending: a work's row is its index here.
    pub const KEYS: &str = "keys.u64";
    pub const POPULARITY: &str = "popularity.f32";
    pub const QUALITY: &str = "quality.f32";
    /// First publication year, `i16::MIN` when unknown.
    pub const YEAR: &str = "year.i16";
    /// Bit flags per work, see [`crate::reader::flags`].
    pub const FLAGS: &str = "flags.u8";
    /// Row in the series table, `u32::MAX` when the work is in no series.
    pub const SERIES: &str = "series.u32";
    /// Position in the series, NaN when unknown.
    pub const SERIES_POSITION: &str = "series_position.f32";
    /// Series names: `u64` offsets (one more than there are names) into the UTF-8 text.
    pub const SERIES_NAME_OFFSETS: &str = "series_names.u64";
    pub const SERIES_NAME_TEXT: &str = "series_names.utf8";
    /// Redirected work numbers: (old, current) pairs of `u64`, ascending by old.
    pub const REDIRECTS: &str = "redirects.u64";
    /// The tantivy search index directory.
    pub const SEARCH_DIR: &str = "search";
}

/// What can go wrong reading or writing a world.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o on {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("world format {found} is not supported (this reader knows {FORMAT_VERSION})")]
    Format { found: u32 },
    #[error("{path}: {what}")]
    Corrupt { path: String, what: String },
    #[error("search index: {0}")]
    Search(#[from] tantivy::TantivyError),
}

impl Error {
    fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Error::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
