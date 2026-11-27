//! Stage `neighbours`: each work's nearest works by cosine of co-like counts over the training
//! users (`04-benchmark.md` §5 method I), keeping `[bench] neighbours` per work. The pick-up term
//! of `03-engine.md` §8 reads them, and so does the item-to-item baseline: one similarity, built
//! once. A second set from `train_likes_cold` serves the cold-book test.
//!
//! File format, little-endian: `KNB1`, the number of works (u64), then per work in ascending key
//! order its key (u64), its neighbour count (u32) and that many (key u64, similarity f32).

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};

use crate::bench::{data, methods::Neighbours};
use crate::stages::Run;

const MAGIC: &[u8; 4] = b"KNB1";

pub fn path(model: &Path, cold: bool) -> PathBuf {
    model.join(if cold {
        "neighbours-cold.bin"
    } else {
        "neighbours.bin"
    })
}

pub fn run(run: &Run) -> anyhow::Result<()> {
    for cold in [false, true] {
        let likes = data::likes(run.duck, cold)?;
        let works: HashSet<u64> = likes.by_work.keys().copied().collect();
        let neighbours = Neighbours::build(&likes, &works, run.config.bench.neighbours);
        drop(likes);
        let entries: usize = neighbours.of.values().map(Vec::len).sum();
        tracing::info!(
            cold,
            works = neighbours.of.len(),
            entries,
            "neighbours built"
        );
        write(&path(&run.model, cold), &neighbours)?;
    }
    Ok(())
}

pub fn write(path: &Path, neighbours: &Neighbours) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut out = BufWriter::new(std::fs::File::create(&tmp)?);
    out.write_all(MAGIC)?;
    out.write_all(&(neighbours.of.len() as u64).to_le_bytes())?;
    let mut keys: Vec<u64> = neighbours.of.keys().copied().collect();
    keys.sort_unstable();
    for key in keys {
        let list = &neighbours.of[&key];
        out.write_all(&key.to_le_bytes())?;
        out.write_all(&(list.len() as u32).to_le_bytes())?;
        for &(other, sim) in list {
            out.write_all(&other.to_le_bytes())?;
            out.write_all(&sim.to_le_bytes())?;
        }
    }
    out.into_inner()?.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read(path: &Path) -> anyhow::Result<Neighbours> {
    let mut input = BufReader::new(
        std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?,
    );
    let mut magic = [0u8; 4];
    input.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "{}: not a neighbours file", path.display());
    let mut b8 = [0u8; 8];
    let mut b4 = [0u8; 4];
    input.read_exact(&mut b8)?;
    let n = u64::from_le_bytes(b8) as usize;
    let mut of = HashMap::with_capacity(n);
    for _ in 0..n {
        input.read_exact(&mut b8)?;
        let key = u64::from_le_bytes(b8);
        input.read_exact(&mut b4)?;
        let len = u32::from_le_bytes(b4) as usize;
        let mut list = Vec::with_capacity(len);
        for _ in 0..len {
            input.read_exact(&mut b8)?;
            input.read_exact(&mut b4)?;
            list.push((u64::from_le_bytes(b8), f32::from_le_bytes(b4)));
        }
        of.insert(key, list);
    }
    Ok(Neighbours { of })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("n.bin");
        let n = Neighbours {
            of: [(7u64, vec![(9u64, 0.5f32), (3, 0.25)]), (2, vec![])]
                .into_iter()
                .collect(),
        };
        write(&p, &n).unwrap();
        let back = read(&p).unwrap();
        assert_eq!(back.of, n.of);
    }
}
