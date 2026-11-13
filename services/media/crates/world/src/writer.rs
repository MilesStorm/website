//! Writes one world version into an empty directory. The build calls, in any order,
//! [`WorldWriter::add_book`] for every searchable work, [`WorldWriter::write_works`] and
//! [`WorldWriter::write_redirects`], then [`WorldWriter::finish`], which writes the manifest
//! last.

use std::fs::File;
use std::io::{BufReader, Read as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tantivy::{Index, IndexWriter, TantivyDocument};

use crate::search::{self, Fields};
use crate::{
    Error, FORMAT_VERSION, FileEntry, InputFile, Manifest, Result, SearchParams, WorkKey, columns,
    files,
};

/// The per-work numbers of one work.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkRow {
    pub key: WorkKey,
    pub popularity: f32,
    pub quality: f32,
    pub year: Option<i16>,
    pub not_a_book: bool,
    pub has_description: bool,
    /// Row in the series names given to [`WorldWriter::write_works`], and the position.
    pub series: Option<(u32, Option<f32>)>,
}

/// What the search index holds for one work.
#[derive(Clone, Debug, PartialEq)]
pub struct BookDoc {
    pub key: WorkKey,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    /// Titles of other editions (translations, retitlings): searched, not shown.
    pub other_titles: Vec<String>,
    pub series: Option<String>,
    pub series_position: Option<f32>,
    pub year: Option<i16>,
    pub cover: Option<u64>,
    pub popularity: f32,
    pub not_a_book: bool,
}

pub struct WorldWriter {
    dir: PathBuf,
    fields: Fields,
    index: IndexWriter,
}

/// The search index writer's memory budget.
const INDEX_HEAP: usize = 512 * 1024 * 1024;

impl WorldWriter {
    /// `dir` must not exist yet or be empty.
    pub fn create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        if std::fs::read_dir(dir)
            .map_err(|e| Error::io(dir, e))?
            .next()
            .is_some()
        {
            return Err(Error::Corrupt {
                path: dir.display().to_string(),
                what: "is not empty".into(),
            });
        }
        let search_dir = dir.join(files::SEARCH_DIR);
        std::fs::create_dir(&search_dir).map_err(|e| Error::io(&search_dir, e))?;
        let (schema, fields) = search::schema();
        let index = Index::create_in_dir(&search_dir, schema)?;
        search::register_tokenizer(&index);
        let index = index.writer_with_num_threads(1, INDEX_HEAP)?;
        Ok(WorldWriter {
            dir: dir.to_owned(),
            fields,
            index,
        })
    }

    pub fn add_book(&mut self, book: &BookDoc) -> Result<()> {
        let f = &self.fields;
        let mut doc = TantivyDocument::default();
        doc.add_u64(f.key, book.key.0);
        doc.add_text(f.title, search::normalize(&book.title));
        doc.add_text(f.title_shown, &book.title);
        if let Some(subtitle) = &book.subtitle {
            doc.add_text(f.subtitle, subtitle);
        }
        for author in &book.authors {
            doc.add_text(f.authors, search::normalize(author));
            doc.add_text(f.authors_shown, author);
        }
        for title in &book.other_titles {
            doc.add_text(f.other_titles, search::normalize(title));
        }
        if let Some(series) = &book.series {
            doc.add_text(f.series, search::normalize(series));
            doc.add_text(f.series_shown, series);
        }
        if let Some(position) = book.series_position {
            doc.add_f64(f.series_position, f64::from(position));
        }
        if let Some(year) = book.year {
            doc.add_i64(f.year, i64::from(year));
        }
        if let Some(cover) = book.cover {
            doc.add_u64(f.cover, cover);
        }
        doc.add_f64(f.popularity, f64::from(book.popularity));
        doc.add_u64(f.not_a_book, u64::from(book.not_a_book));
        self.index.add_document(doc)?;
        Ok(())
    }

    /// The per-work columns, in any order (they are sorted by key here), and the series names
    /// their `series` rows point into.
    pub fn write_works(&self, mut works: Vec<WorkRow>, series_names: &[String]) -> Result<()> {
        works.sort_unstable_by_key(|w| w.key);
        if let Some(pair) = works.windows(2).find(|p| p[0].key == p[1].key) {
            return Err(Error::Corrupt {
                path: files::KEYS.into(),
                what: format!("{} twice", pair[0].key),
            });
        }
        if let Some(w) = works.iter().find(|w| {
            w.series
                .is_some_and(|(s, _)| s as usize >= series_names.len())
        }) {
            return Err(Error::Corrupt {
                path: files::SERIES.into(),
                what: format!("{} has no such series", w.key),
            });
        }
        let col = |name: &str| self.dir.join(name);
        columns::write(
            &col(files::KEYS),
            &works.iter().map(|w| w.key.0).collect::<Vec<u64>>(),
        )?;
        columns::write(
            &col(files::POPULARITY),
            &works.iter().map(|w| w.popularity).collect::<Vec<f32>>(),
        )?;
        columns::write(
            &col(files::QUALITY),
            &works.iter().map(|w| w.quality).collect::<Vec<f32>>(),
        )?;
        columns::write(
            &col(files::YEAR),
            &works
                .iter()
                .map(|w| w.year.unwrap_or(i16::MIN))
                .collect::<Vec<i16>>(),
        )?;
        let flags: Vec<u8> = works
            .iter()
            .map(|w| {
                (u8::from(w.not_a_book) * crate::reader::flags::NOT_A_BOOK)
                    | (u8::from(w.has_description) * crate::reader::flags::HAS_DESCRIPTION)
            })
            .collect();
        columns::write(&col(files::FLAGS), &flags)?;
        columns::write(
            &col(files::SERIES),
            &works
                .iter()
                .map(|w| w.series.map_or(u32::MAX, |(s, _)| s))
                .collect::<Vec<u32>>(),
        )?;
        columns::write(
            &col(files::SERIES_POSITION),
            &works
                .iter()
                .map(|w| w.series.and_then(|(_, p)| p).unwrap_or(f32::NAN))
                .collect::<Vec<f32>>(),
        )?;
        let mut offsets = Vec::with_capacity(series_names.len() + 1);
        let mut text = String::new();
        offsets.push(0u64);
        for name in series_names {
            text.push_str(name);
            offsets.push(text.len() as u64);
        }
        columns::write(&col(files::SERIES_NAME_OFFSETS), &offsets)?;
        columns::write(&col(files::SERIES_NAME_TEXT), text.as_bytes())
    }

    /// Old work key → current key. Chains must already be resolved.
    pub fn write_redirects(&self, mut redirects: Vec<(WorkKey, WorkKey)>) -> Result<()> {
        redirects.sort_unstable();
        redirects.dedup_by_key(|r| r.0);
        let flat: Vec<u64> = redirects
            .iter()
            .flat_map(|&(from, to)| [from.0, to.0])
            .collect();
        columns::write(&self.dir.join(files::REDIRECTS), &flat)
    }

    /// Commits the search index, writes the report (adding `world_bytes`, the size of the
    /// data files), hashes every file and writes the manifest last.
    pub fn finish(
        mut self,
        built_at: String,
        builder: String,
        inputs: Vec<InputFile>,
        search: SearchParams,
        mut report: serde_json::Value,
    ) -> Result<Manifest> {
        self.index.commit()?;
        self.index.wait_merging_threads()?;
        if let serde_json::Value::Object(map) = &mut report {
            map.insert("world_bytes".into(), dir_size(&self.dir)?.into());
        }
        let report_path = self.dir.join(files::REPORT);
        std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
            .map_err(|e| Error::io(&report_path, e))?;

        let mut entries = Vec::new();
        hash_tree(&self.dir, &self.dir, &mut entries)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        for required in [files::KEYS, files::REDIRECTS, files::SERIES_NAME_TEXT] {
            if !entries.iter().any(|e| e.path == required) {
                return Err(Error::Corrupt {
                    path: required.into(),
                    what: "was never written".into(),
                });
            }
        }
        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            built_at,
            builder,
            inputs,
            files: entries,
            search,
            report,
        };
        let path = self.dir.join(files::MANIFEST);
        std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)
            .map_err(|e| Error::io(&path, e))?;
        Ok(manifest)
    }
}

/// Size and SHA-256 of a file.
pub fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut reader =
        BufReader::with_capacity(1 << 20, File::open(path).map_err(|e| Error::io(path, e))?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| Error::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, hex::encode(hasher.finalize())))
}

fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let meta = entry.metadata().map_err(|e| Error::io(&entry.path(), e))?;
        total += if meta.is_dir() {
            dir_size(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}

fn hash_tree(root: &Path, dir: &Path, out: &mut Vec<FileEntry>) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let path = entry.map_err(|e| Error::io(dir, e))?.path();
        if path.is_dir() {
            hash_tree(root, &path, out)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .expect("under the root")
            .to_string_lossy()
            .replace('\\', "/");
        // tantivy's lock files are runtime state, not data.
        if rel == files::MANIFEST || rel.ends_with(".lock") {
            continue;
        }
        let (size, sha256) = hash_file(&path)?;
        out.push(FileEntry {
            path: rel,
            size,
            sha256,
        });
    }
    Ok(())
}
