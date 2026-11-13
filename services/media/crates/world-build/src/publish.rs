//! Stage publish (02-world-data.md §7): upload a verified world version to
//! `worlds/<version>/` (manifest last), move `current.json` to it, then delete every version but
//! the new one and the one it replaced. Also the rollback: point `current.json` at an older
//! version that is still in the bucket.

use std::path::Path;

use anyhow::{Context as _, bail};
use futures_util::TryStreamExt as _;
use object_store::path::Path as Key;
use object_store::{ObjectStoreExt as _, PutPayload};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use world::{Manifest, files};

use crate::trace::Bucket;

pub const CURRENT: &str = "current.json";
const WORLDS: &str = "worlds";

/// `current.json`: the version the service serves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Current {
    pub version: String,
    pub manifest_sha256: String,
}

fn key(version: &str, file: &str) -> Key {
    Key::from(format!("{WORLDS}/{version}/{file}"))
}

async fn get(bucket: &Bucket, key: &Key) -> anyhow::Result<Option<bytes::Bytes>> {
    let store = bucket.store().clone();
    match bucket
        .call("GetObject", async { store.get(key).await?.bytes().await })
        .await
    {
        Ok(bytes) => Ok(Some(bytes)),
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn put(bucket: &Bucket, key: &Key, bytes: Vec<u8>) -> anyhow::Result<()> {
    let store = bucket.store().clone();
    bucket
        .call("PutObject", async {
            store.put(key, PutPayload::from(bytes)).await
        })
        .await?;
    Ok(())
}

pub async fn current(bucket: &Bucket) -> anyhow::Result<Option<Current>> {
    match get(bucket, &Key::from(CURRENT)).await? {
        Some(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("current.json")?,
        )),
        None => Ok(None),
    }
}

/// The report of the version being served, for the change gates.
pub async fn current_report(bucket: &Bucket) -> anyhow::Result<Option<serde_json::Value>> {
    let Some(current) = current(bucket).await? else {
        return Ok(None);
    };
    let bytes = get(bucket, &key(&current.version, files::MANIFEST))
        .await?
        .with_context(|| {
            format!(
                "current.json names version {}, which has no manifest",
                current.version
            )
        })?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    Ok(Some(manifest.report))
}

/// Uploads the world in `dir` as a new version and makes it current. `base` is
/// `YYYYMMDD-<sha7>`; a second build of the same day and commit gets `-2`, `-3`, ... (versions
/// are immutable). Returns the version.
#[tracing::instrument(name = "publish", skip_all)]
pub async fn publish(bucket: &Bucket, dir: &Path, base: &str) -> anyhow::Result<String> {
    let manifest_bytes = tokio::fs::read(dir.join(files::MANIFEST)).await?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    let mut version = base.to_owned();
    let mut n = 1;
    while get(bucket, &key(&version, files::MANIFEST))
        .await?
        .is_some()
    {
        n += 1;
        version = format!("{base}-{n}");
    }
    for entry in &manifest.files {
        upload(bucket, &dir.join(&entry.path), &key(&version, &entry.path)).await?;
    }
    put(
        bucket,
        &key(&version, files::MANIFEST),
        manifest_bytes.clone(),
    )
    .await?;
    tracing::info!(files = manifest.files.len() + 1, "world uploaded");

    let replaced = current(bucket).await?;
    point_at(bucket, &version, &manifest_bytes).await?;
    tracing::info!(
        replaced = replaced.as_ref().map(|c| c.version.as_str()),
        "current.json moved"
    );

    let keep: Vec<&str> = std::iter::once(version.as_str())
        .chain(replaced.as_ref().map(|c| c.version.as_str()))
        .collect();
    for old in versions(bucket).await? {
        if !keep.contains(&old.as_str()) {
            delete_version(bucket, &old).await?;
            tracing::info!("old world version deleted");
        }
    }
    Ok(version)
}

/// Rollback: point `current.json` at `version`, which must be complete in the bucket.
#[tracing::instrument(name = "publish.point", skip_all)]
pub async fn rollback(bucket: &Bucket, version: &str) -> anyhow::Result<()> {
    let manifest_bytes = get(bucket, &key(version, files::MANIFEST))
        .await?
        .with_context(|| {
            format!("version {version} is not in the bucket (`media-world versions` lists them)")
        })?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    let store = bucket.store().clone();
    for entry in &manifest.files {
        let meta = bucket
            .call("HeadObject", store.head(&key(version, &entry.path)))
            .await?;
        if meta.size != entry.size {
            bail!(
                "{version}/{}: {} bytes in the bucket, the manifest says {}",
                entry.path,
                meta.size,
                entry.size
            );
        }
    }
    point_at(bucket, version, &manifest_bytes).await
}

async fn point_at(bucket: &Bucket, version: &str, manifest_bytes: &[u8]) -> anyhow::Result<()> {
    let current = Current {
        version: version.to_owned(),
        manifest_sha256: hex::encode(Sha256::digest(manifest_bytes)),
    };
    put(
        bucket,
        &Key::from(CURRENT),
        serde_json::to_vec_pretty(&current)?,
    )
    .await
}

/// Every version in the bucket, oldest first.
pub async fn versions(bucket: &Bucket) -> anyhow::Result<Vec<String>> {
    let store = bucket.store().clone();
    let prefix = Key::from(WORLDS);
    let listed = bucket
        .call("ListObjectsV2", async {
            store.list_with_delimiter(Some(&prefix)).await
        })
        .await?;
    let mut versions: Vec<String> = listed
        .common_prefixes
        .iter()
        .filter_map(|p| p.filename().map(str::to_owned))
        .collect();
    versions.sort();
    Ok(versions)
}

async fn delete_version(bucket: &Bucket, version: &str) -> anyhow::Result<()> {
    let store = bucket.store().clone();
    let prefix = Key::from(format!("{WORLDS}/{version}"));
    let objects: Vec<object_store::ObjectMeta> = bucket
        .call("ListObjectsV2", async {
            store.list(Some(&prefix)).try_collect().await
        })
        .await?;
    // The manifest first: a version without one is no longer a version.
    let (manifest, rest): (Vec<_>, Vec<_>) = objects
        .into_iter()
        .partition(|o| o.location.filename() == Some(files::MANIFEST));
    for object in manifest.into_iter().chain(rest) {
        bucket
            .call("DeleteObject", store.delete(&object.location))
            .await?;
    }
    Ok(())
}

/// Streams a file into the bucket (multipart above the buffer size).
async fn upload(bucket: &Bucket, path: &Path, key: &Key) -> anyhow::Result<()> {
    let store = bucket.store().clone();
    bucket
        .call("PutObject", async {
            let mut file =
                tokio::fs::File::open(path)
                    .await
                    .map_err(|e| object_store::Error::Generic {
                        store: "local",
                        source: Box::new(e),
                    })?;
            let mut writer =
                object_store::buffered::BufWriter::with_capacity(store, key.clone(), 32 << 20);
            let io = |e: std::io::Error| object_store::Error::Generic {
                store: "S3",
                source: Box::new(e),
            };
            tokio::io::copy(&mut file, &mut writer).await.map_err(io)?;
            writer.shutdown().await.map_err(io)?;
            Ok(())
        })
        .await
        .with_context(|| format!("upload {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn world_dir(root: &Path, name: &str) -> std::path::PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("search")).unwrap();
        std::fs::write(dir.join("keys.u64"), name.as_bytes()).unwrap();
        std::fs::write(dir.join("search/meta.json"), b"{}").unwrap();
        let entry = |p: &str| {
            let (size, sha256) = world::writer::hash_file(&dir.join(p)).unwrap();
            world::FileEntry {
                path: p.into(),
                size,
                sha256,
            }
        };
        let manifest = Manifest {
            format_version: world::FORMAT_VERSION,
            built_at: "t".into(),
            builder: "test".into(),
            inputs: vec![],
            files: vec![entry("keys.u64"), entry("search/meta.json")],
            search: world::SearchParams {
                popularity_weight: 0.0,
                title_boost: 1.0,
                other_titles_boost: 1.0,
                authors_boost: 1.0,
                series_boost: 1.0,
                whole_title_bonus: 10.0,
                not_a_book_penalty: 10.0,
                retry_below: 3,
                typo_min_len: 5,
            },
            report: serde_json::json!({"searchable_works": name.len()}),
        };
        std::fs::write(
            dir.join(files::MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        dir
    }

    #[tokio::test]
    async fn publishes_keeps_two_versions_and_rolls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let bucket = Bucket::from_store(Arc::new(object_store::memory::InMemory::new()));
        assert_eq!(current(&bucket).await.unwrap(), None);
        assert_eq!(current_report(&bucket).await.unwrap(), None);

        let a = publish(&bucket, &world_dir(tmp.path(), "a"), "20261107-aaaaaaa")
            .await
            .unwrap();
        let b = publish(&bucket, &world_dir(tmp.path(), "bb"), "20261207-bbbbbbb")
            .await
            .unwrap();
        // Same day and commit again: a new version, never an overwrite.
        let b2 = publish(&bucket, &world_dir(tmp.path(), "ccc"), "20261207-bbbbbbb")
            .await
            .unwrap();
        assert_eq!(
            (a.as_str(), b.as_str(), b2.as_str()),
            ("20261107-aaaaaaa", "20261207-bbbbbbb", "20261207-bbbbbbb-2")
        );

        // Only the new version and the one it replaced remain.
        assert_eq!(versions(&bucket).await.unwrap(), [b.clone(), b2.clone()]);
        assert_eq!(current(&bucket).await.unwrap().unwrap().version, b2);
        assert_eq!(
            current_report(&bucket).await.unwrap().unwrap()["searchable_works"],
            3
        );
        let keys = get(&bucket, &key(&b2, "keys.u64")).await.unwrap().unwrap();
        assert_eq!(&keys[..], b"ccc");

        rollback(&bucket, &b).await.unwrap();
        let now = current(&bucket).await.unwrap().unwrap();
        assert_eq!(now.version, b);
        let manifest = get(&bucket, &key(&b, files::MANIFEST))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(now.manifest_sha256, hex::encode(Sha256::digest(&manifest)));

        assert!(rollback(&bucket, &a).await.is_err(), "a was deleted");
    }
}
