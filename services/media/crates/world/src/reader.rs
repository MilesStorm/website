//! Opens one world version: checks the manifest's format and every file's checksum, then
//! memory-maps the columns and opens the search index.

use std::path::Path;

use tantivy::{Index, IndexReader, ReloadPolicy};

use crate::columns::Column;
use crate::search::{self, Fields};
use crate::writer::hash_file;
use crate::{Error, FORMAT_VERSION, Hit, Manifest, Result, WorkKey, files};

/// Bits of a work's flags column.
pub mod flags {
    /// A summary, study guide, box set or journal: searchable, never picked.
    pub const NOT_A_BOOK: u8 = 1;
    /// A description of 200 characters or more.
    pub const HAS_DESCRIPTION: u8 = 2;
}

/// A row number: private to one world version. Never store one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Row(pub u32);

pub struct World {
    manifest: Manifest,
    keys: Column<u64>,
    popularity: Column<f32>,
    quality: Column<f32>,
    year: Column<i16>,
    flags: Column<u8>,
    series: Column<u32>,
    series_position: Column<f32>,
    series_offsets: Column<u64>,
    series_text: Column<u8>,
    redirects: Column<u64>,
    fields: Fields,
    search: IndexReader,
}

impl World {
    pub fn open(dir: &Path) -> Result<World> {
        let manifest_path = dir.join(files::MANIFEST);
        let raw = std::fs::read(&manifest_path).map_err(|e| Error::io(&manifest_path, e))?;
        // The format first: a newer manifest may not even parse as this one.
        let format: serde_json::Value = serde_json::from_slice(&raw)?;
        let found = format
            .get("format_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if found != u64::from(FORMAT_VERSION) {
            return Err(Error::Format {
                found: found as u32,
            });
        }
        let manifest: Manifest = serde_json::from_slice(&raw)?;
        for entry in &manifest.files {
            let path = dir.join(&entry.path);
            let (size, sha256) = hash_file(&path)?;
            if size != entry.size || sha256 != entry.sha256 {
                return Err(Error::Corrupt {
                    path: entry.path.clone(),
                    what: format!(
                        "size {size} sha256 {sha256}, the manifest says {} {}",
                        entry.size, entry.sha256
                    ),
                });
            }
        }

        let col = |name: &str| dir.join(name);
        let keys = Column::<u64>::open(&col(files::KEYS))?;
        let n = keys.len();
        if keys.as_slice().windows(2).any(|p| p[0] >= p[1]) {
            return Err(Error::Corrupt {
                path: files::KEYS.into(),
                what: "not strictly ascending".into(),
            });
        }
        let per_work = |name: &str| -> Result<()> {
            let len = std::fs::metadata(col(name))
                .map_err(|e| Error::io(&col(name), e))?
                .len() as usize;
            let width = match name {
                files::YEAR => 2,
                files::FLAGS => 1,
                _ => 4,
            };
            if len != n * width {
                return Err(Error::Corrupt {
                    path: name.into(),
                    what: format!("{len} bytes for {n} works"),
                });
            }
            Ok(())
        };
        for name in [
            files::POPULARITY,
            files::QUALITY,
            files::YEAR,
            files::FLAGS,
            files::SERIES,
            files::SERIES_POSITION,
        ] {
            per_work(name)?;
        }
        let series_offsets = Column::<u64>::open(&col(files::SERIES_NAME_OFFSETS))?;
        let series_text = Column::<u8>::open(&col(files::SERIES_NAME_TEXT))?;
        let offsets = series_offsets.as_slice();
        if offsets.first() != Some(&0)
            || offsets.last() != Some(&(series_text.len() as u64))
            || offsets.windows(2).any(|p| p[0] > p[1])
            || std::str::from_utf8(series_text.as_slice()).is_err()
        {
            return Err(Error::Corrupt {
                path: files::SERIES_NAME_OFFSETS.into(),
                what: "bad offsets or text".into(),
            });
        }
        let series = Column::<u32>::open(&col(files::SERIES))?;
        let names = offsets.len() - 1;
        if series
            .as_slice()
            .iter()
            .any(|&s| s != u32::MAX && s as usize >= names)
        {
            return Err(Error::Corrupt {
                path: files::SERIES.into(),
                what: "a row past the series names".into(),
            });
        }
        let redirects = Column::<u64>::open(&col(files::REDIRECTS))?;
        if !redirects.len().is_multiple_of(2) {
            return Err(Error::Corrupt {
                path: files::REDIRECTS.into(),
                what: "odd number of values".into(),
            });
        }

        let index = Index::open_in_dir(dir.join(files::SEARCH_DIR))?;
        search::register_tokenizer(&index);
        let (_, fields) = search::schema();
        let search = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;

        Ok(World {
            manifest,
            keys,
            popularity: Column::open(&col(files::POPULARITY))?,
            quality: Column::open(&col(files::QUALITY))?,
            year: Column::open(&col(files::YEAR))?,
            flags: Column::open(&col(files::FLAGS))?,
            series,
            series_position: Column::open(&col(files::SERIES_POSITION))?,
            series_offsets,
            series_text,
            redirects,
            fields,
            search,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.len() == 0
    }

    /// O(log n), no allocation.
    pub fn row(&self, key: WorkKey) -> Option<Row> {
        self.keys
            .as_slice()
            .binary_search(&key.0)
            .ok()
            .map(|i| Row(i as u32))
    }

    pub fn key(&self, row: Row) -> WorkKey {
        WorkKey(self.keys.as_slice()[row.0 as usize])
    }

    pub fn popularity(&self, row: Row) -> f32 {
        self.popularity.as_slice()[row.0 as usize]
    }

    pub fn quality(&self, row: Row) -> f32 {
        self.quality.as_slice()[row.0 as usize]
    }

    pub fn year(&self, row: Row) -> Option<i16> {
        Some(self.year.as_slice()[row.0 as usize]).filter(|&y| y != i16::MIN)
    }

    pub fn flags(&self, row: Row) -> u8 {
        self.flags.as_slice()[row.0 as usize]
    }

    /// The work's series name and position.
    pub fn series(&self, row: Row) -> Option<(&str, Option<f32>)> {
        let s = self.series.as_slice()[row.0 as usize];
        if s == u32::MAX {
            return None;
        }
        let offsets = self.series_offsets.as_slice();
        let (start, end) = (
            offsets[s as usize] as usize,
            offsets[s as usize + 1] as usize,
        );
        let name =
            std::str::from_utf8(&self.series_text.as_slice()[start..end]).expect("checked at open");
        let position =
            Some(self.series_position.as_slice()[row.0 as usize]).filter(|p| !p.is_nan());
        Some((name, position))
    }

    /// The current key of a work whose key Open Library merged into another; `key` itself
    /// when it was never redirected.
    pub fn current_key(&self, key: WorkKey) -> WorkKey {
        let pairs: &[[u64; 2]] = bytemuck::cast_slice(self.redirects.as_slice());
        match pairs.binary_search_by_key(&key.0, |p| p[0]) {
            Ok(i) => WorkKey(pairs[i][1]),
            Err(_) => key,
        }
    }

    pub fn search(&self, text: &str, limit: usize) -> Result<Vec<Hit>> {
        search::search(
            &self.search,
            &self.fields,
            &self.manifest.search,
            text,
            limit,
        )
    }

    /// What a search result would show for one work, if it is searchable.
    pub fn book(&self, key: WorkKey) -> Result<Option<Hit>> {
        search::by_key(&self.search, &self.fields, key)
    }
}
