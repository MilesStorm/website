//! Stage download: the inputs onto the scratch volume (02-world-data.md §2). Open Library files
//! are fetched again when the remote size or `Last-Modified` changed; Goodreads files once.
//! A download goes to `<name>.part` and is renamed when complete; size and SHA-256 of every input
//! go into the manifest.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use world::InputFile;

use crate::config::Config;
use crate::trace::TracedClient;

/// One input file.
#[derive(Clone, Debug)]
pub struct Source {
    /// The name the SQL knows it by (`getvariable('<name>')`).
    pub name: &'static str,
    pub url: String,
    /// File name on the scratch volume.
    pub file: String,
    /// Never changes upstream: download once, keep.
    pub once: bool,
}

pub fn sources(config: &Config) -> Vec<Source> {
    let ol = &config.sources.openlibrary;
    let gr = &config.sources.goodreads;
    let open = |name, file: &str| Source {
        name,
        url: format!("{}/{file}", ol.base),
        file: file.to_owned(),
        once: false,
    };
    let once = |name, file: &str| Source {
        name,
        url: format!("{}/{file}", gr.base),
        file: file.to_owned(),
        once: true,
    };
    vec![
        open("ol_works", &ol.works),
        open("ol_editions", &ol.editions),
        open("ol_authors", &ol.authors),
        open("ol_ratings", &ol.ratings),
        open("ol_reading_log", &ol.reading_log),
        open("ol_redirects", &ol.redirects),
        once("gr_books", &gr.books),
        once("gr_works", &gr.works),
        once("gr_series", &gr.series),
    ]
}

pub fn path(dir: &Path, source: &Source) -> PathBuf {
    dir.join(&source.file)
}

fn meta_path(dir: &Path, source: &Source) -> PathBuf {
    dir.join(format!("{}.meta.json", source.file))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Meta {
    url: String,
    size: u64,
    sha256: String,
    last_modified: Option<String>,
}

impl Meta {
    fn input(&self, source: &Source) -> InputFile {
        InputFile {
            name: source.name.to_owned(),
            url: self.url.clone(),
            size: self.size,
            sha256: self.sha256.clone(),
            last_modified: self.last_modified.clone(),
        }
    }
}

/// What a file already on disk is, if its record matches it.
fn cached(dir: &Path, source: &Source) -> Option<Meta> {
    let meta: Meta = serde_json::from_slice(&std::fs::read(meta_path(dir, source)).ok()?).ok()?;
    let size = std::fs::metadata(path(dir, source)).ok()?.len();
    (size == meta.size && meta.url == source.url).then_some(meta)
}

#[tracing::instrument(name = "download.all", skip_all)]
pub async fn fetch_all(
    client: &TracedClient,
    dir: &Path,
    sources: &[Source],
) -> anyhow::Result<Vec<InputFile>> {
    tokio::fs::create_dir_all(dir).await?;
    let mut inputs = Vec::new();
    for source in sources {
        inputs.push(
            fetch(client, dir, source)
                .await
                .with_context(|| format!("download {}", source.name))?,
        );
    }
    Ok(inputs)
}

#[tracing::instrument(name = "download.file", skip_all, fields(input = source.name, cached = tracing::field::Empty))]
async fn fetch(client: &TracedClient, dir: &Path, source: &Source) -> anyhow::Result<InputFile> {
    let span = tracing::Span::current();
    let have = cached(dir, source);
    if let (Some(meta), true) = (&have, source.once) {
        span.record("cached", true);
        return Ok(meta.input(source));
    }
    let head = client.head(&source.url).send().await?.error_for_status()?;
    let remote_size = head.content_length().filter(|&n| n > 0);
    let last_modified = head
        .headers()
        .get(http::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(meta) = &have
        && remote_size.is_some_and(|n| n == meta.size)
        && last_modified == meta.last_modified
    {
        span.record("cached", true);
        return Ok(meta.input(source));
    }
    span.record("cached", false);
    tracing::info!(input = source.name, bytes = remote_size, "downloading");

    let part = dir.join(format!("{}.part", source.file));
    let response = client.get(&source.url).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(&part).await?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut next_log = 1u64 << 30;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        size += chunk.len() as u64;
        if size >= next_log {
            tracing::info!(input = source.name, bytes = size, "download progress");
            next_log += 1 << 30;
        }
    }
    file.sync_all().await?;
    drop(file);
    if let Some(expected) = remote_size
        && expected != size
    {
        bail!(
            "{}: got {size} bytes, the server announced {expected}",
            source.name
        );
    }
    let meta = Meta {
        url: source.url.clone(),
        size,
        sha256: hex::encode(hasher.finalize()),
        last_modified,
    };
    tokio::fs::rename(&part, path(dir, source)).await?;
    tokio::fs::write(meta_path(dir, source), serde_json::to_vec_pretty(&meta)?).await?;
    tracing::info!(input = source.name, bytes = size, "downloaded");
    Ok(meta.input(source))
}

/// Records files that were put in place without downloading (the fixture world).
pub fn register_local(dir: &Path, source: &Source) -> anyhow::Result<InputFile> {
    let (size, sha256) = world::writer::hash_file(&path(dir, source))?;
    let meta = Meta {
        url: source.url.clone(),
        size,
        sha256,
        last_modified: None,
    };
    std::fs::write(meta_path(dir, source), serde_json::to_vec_pretty(&meta)?)?;
    Ok(meta.input(source))
}
