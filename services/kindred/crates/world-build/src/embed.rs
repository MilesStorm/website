//! Stage embed (02-world-data.md §3, 03-engine.md §3): a text embedding for every searchable
//! work with a description, from `BAAI/bge-small-en-v1.5` through ONNX Runtime (`ort`), on the
//! GPU when there is one. The text is the title, the authors, the first subjects and the start of
//! the description. Embeddings are cached on the scratch volume by the SHA-256 of the model id
//! and the text, so a monthly run embeds only new or changed texts. Writes `model/embeddings.bin`.
//!
//! The fixture world uses a stand-in instead of the model (no download, no GPU in CI): the
//! words of the text hashed into the same number of dimensions, so texts that share words are
//! still close.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::config::Embed;
use crate::matrix::Matrix;
use crate::stages::Run;

pub fn path(model: &Path) -> PathBuf {
    model.join("embeddings.bin")
}

/// The model's files inside `embed.model_dir`.
pub const MODEL_FILE: &str = "model.onnx";
pub const TOKENIZER_FILE: &str = "tokenizer.json";

pub fn run(run: &Run) -> anyhow::Result<()> {
    let config = &run.config.embed;
    let mut embedder = if run.fixture {
        Embedder::StandIn {
            dim: config.dimensions,
        }
    } else {
        Embedder::load(config)?
    };
    let texts = texts(run, config)?;
    let cache_dir = run
        .model
        .parent()
        .expect("model/ is in the scratch volume")
        .join("embed-cache");
    std::fs::create_dir_all(&cache_dir)?;
    let mut cache = Cache::open(
        &cache_dir.join(cache_name(&embedder.id())),
        config.dimensions,
    )?;

    let keyed: Vec<(u64, [u8; 32])> = texts
        .iter()
        .map(|(work, text)| (*work, text_hash(&embedder.id(), text)))
        .collect();
    let mut missing: Vec<usize> = (0..texts.len())
        .filter(|&i| !cache.has(&keyed[i].1))
        .collect();
    tracing::info!(
        works = texts.len(),
        cached = texts.len() - missing.len(),
        to_embed = missing.len(),
        "embedding texts"
    );
    // Similar lengths in one batch: less padding.
    missing.sort_by_key(|&i| texts[i].1.len());
    let mut done = 0usize;
    for chunk in missing.chunks(config.batch.max(1)) {
        let batch: Vec<&str> = chunk.iter().map(|&i| texts[i].1.as_str()).collect();
        let vectors = embedder.embed(&batch, config)?;
        for (&i, v) in chunk.iter().zip(vectors.chunks(config.dimensions)) {
            cache.add(keyed[i].1, v)?;
        }
        done += chunk.len();
        if done % 50_000 < chunk.len() {
            tracing::info!(done, of = missing.len(), "embedding progress");
        }
    }
    cache.flush()?;
    // Keep only this run's texts: the cache does not grow with every month's changes.
    cache.compact(keyed.iter().map(|k| k.1))?;

    let mut order: Vec<usize> = (0..texts.len()).collect();
    order.sort_by_key(|&i| keyed[i].0);
    let mut out = Matrix::new(config.dimensions);
    for i in order {
        out.push(keyed[i].0, cache.get(&keyed[i].1).expect("embedded above"));
    }
    out.write(&path(&run.model))
}

/// (work, text) for every searchable work with a description.
fn texts(run: &Run, config: &Embed) -> anyhow::Result<Vec<(u64, String)>> {
    let mut statement = run.duck.conn().prepare(
        "SELECT work, title, array_to_string(authors, ', '),
                array_to_string(subjects[1:?], ', '), left(description, ?)
         FROM catalogue WHERE has_description ORDER BY work",
    )?;
    let rows = statement.query_map(
        duckdb::params![
            config.max_subjects as i64,
            config.max_description_chars as i64
        ],
        |r| {
            let work = r.get::<_, i64>(0)? as u64;
            let parts: [Option<String>; 4] = [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?];
            Ok((work, text_of(&parts)))
        },
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Title, authors, subjects and description, one per line, empty ones left out.
pub fn text_of(parts: &[Option<String>]) -> String {
    parts
        .iter()
        .flatten()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn text_hash(model_id: &str, text: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(model_id.as_bytes());
    h.update([0]);
    h.update(text.as_bytes());
    h.finalize().into()
}

fn cache_name(model_id: &str) -> String {
    let safe: String = model_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.bin")
}

/// The embedding cache: records of (SHA-256, `dim` f32), appended.
struct Cache {
    path: PathBuf,
    dim: usize,
    index: HashMap<[u8; 32], usize>,
    data: Vec<f32>,
    pending: Vec<([u8; 32], usize)>,
}

impl Cache {
    fn open(path: &Path, dim: usize) -> anyhow::Result<Cache> {
        let mut cache = Cache {
            path: path.to_owned(),
            dim,
            index: HashMap::new(),
            data: Vec::new(),
            pending: Vec::new(),
        };
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        else {
            return Ok(cache);
        };
        let record = 32 + 4 * dim;
        let len = file.metadata()?.len() as usize;
        // A run that died mid-write leaves a partial record: cut it off, so appends stay aligned.
        let whole = len / record;
        if whole * record != len {
            file.set_len((whole * record) as u64)?;
        }
        let mut input = BufReader::with_capacity(1 << 20, file);
        let mut buf = vec![0u8; record];
        let mut row = vec![0f32; dim];
        cache.data.reserve(whole * dim);
        for at in 0..whole {
            input.read_exact(&mut buf)?;
            let hash: [u8; 32] = buf[..32].try_into()?;
            bytemuck::cast_slice_mut::<f32, u8>(&mut row).copy_from_slice(&buf[32..]);
            cache.data.extend_from_slice(&row);
            cache.index.insert(hash, at);
        }
        Ok(cache)
    }

    fn has(&self, hash: &[u8; 32]) -> bool {
        self.index.contains_key(hash)
    }

    fn get(&self, hash: &[u8; 32]) -> Option<&[f32]> {
        self.index
            .get(hash)
            .map(|&i| &self.data[i * self.dim..(i + 1) * self.dim])
    }

    fn add(&mut self, hash: [u8; 32], row: &[f32]) -> anyhow::Result<()> {
        if self.has(&hash) {
            return Ok(());
        }
        let at = self.data.len() / self.dim;
        self.data.extend_from_slice(row);
        self.index.insert(hash, at);
        self.pending.push((hash, at));
        if self.pending.len() >= 10_000 {
            self.flush()?;
        }
        Ok(())
    }

    /// Rewrites the file with only `keep`'s records (written aside, then renamed).
    fn compact(&mut self, keep: impl Iterator<Item = [u8; 32]>) -> anyhow::Result<()> {
        self.flush()?;
        let part = self.path.with_extension("part");
        let mut out = BufWriter::new(std::fs::File::create(&part)?);
        let mut seen = std::collections::HashSet::new();
        for hash in keep {
            if seen.insert(hash)
                && let Some(row) = self.get(&hash)
            {
                out.write_all(&hash)?;
                out.write_all(bytemuck::cast_slice(row))?;
            }
        }
        out.into_inner()?.sync_all()?;
        std::fs::rename(&part, &self.path)?;
        Ok(())
    }

    /// Appends the new records; a crash loses at most what was not flushed yet.
    fn flush(&mut self) -> anyhow::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| self.path.display().to_string())?;
        let mut out = BufWriter::new(file);
        for (hash, at) in self.pending.drain(..) {
            out.write_all(&hash)?;
            out.write_all(bytemuck::cast_slice(
                &self.data[at * self.dim..(at + 1) * self.dim],
            ))?;
        }
        out.into_inner()?.sync_all()?;
        Ok(())
    }
}

/// The embedding model and its tokenizer.
pub struct Onnx {
    /// The model file's SHA-256 and the token limit: part of the cache key, so a new model
    /// revision or limit never reuses old embeddings.
    fingerprint: String,
    session: ort::session::Session,
    tokenizer: tokenizers::Tokenizer,
    /// The model takes `token_type_ids` too.
    token_types: bool,
}

pub enum Embedder {
    Model(Box<Onnx>),
    StandIn { dim: usize },
}

impl Embedder {
    /// The model from `config.model_dir`, on the GPU when ONNX Runtime's CUDA provider loads,
    /// otherwise on the CPU (02-world-data.md, D12).
    pub fn load(config: &Embed) -> anyhow::Result<Embedder> {
        let model = config.model_dir.join(MODEL_FILE);
        let tokenizer = config.model_dir.join(TOKENIZER_FILE);
        if !model.exists() || !tokenizer.exists() {
            bail!(
                "the embedding model is not in {} (it is in the image; only the fixture world builds without it)",
                config.model_dir.display()
            );
        }
        let context = |what: &str, e: &dyn std::fmt::Display| anyhow::anyhow!("{what}: {e}");
        let cuda = ort::ep::CUDA::default()
            .with_arena_extend_strategy(ort::ep::ArenaExtendStrategy::SameAsRequested)
            .build();
        let session = ort::session::Session::builder()
            .map_err(|e| context("onnxruntime", &e))?
            .with_execution_providers([cuda])
            .map_err(|e| context("execution providers", &e))?
            .commit_from_file(&model)
            .map_err(|e| context("loading the embedding model", &e))?;
        let token_types = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");
        let (_, model_sha) = world::writer::hash_file(&model)?;
        let fingerprint = format!(
            "{}@{}:{}",
            config.model_id,
            &model_sha[..16],
            config.max_tokens
        );
        let mut tokenizer = tokenizers::Tokenizer::from_file(&tokenizer)
            .map_err(|e| context("loading the tokenizer", &e))?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: config.max_tokens,
                ..Default::default()
            }))
            .map_err(|e| context("tokenizer truncation", &e))?;
        tokenizer.with_padding(Some(tokenizers::PaddingParams::default()));
        tracing::info!(model = %config.model_id, "embedding model loaded");
        Ok(Embedder::Model(Box::new(Onnx {
            fingerprint,
            session,
            tokenizer,
            token_types,
        })))
    }

    /// What the cache is keyed by: the model, or the stand-in.
    pub fn id(&self) -> String {
        match self {
            Embedder::Model(onnx) => onnx.fingerprint.clone(),
            Embedder::StandIn { dim } => format!("stand-in-{dim}"),
        }
    }

    /// `texts.len() * dimensions` numbers, each vector of unit length.
    pub fn embed(&mut self, texts: &[&str], config: &Embed) -> anyhow::Result<Vec<f32>> {
        let dim = config.dimensions;
        let mut out = match self {
            Embedder::StandIn { dim } => texts.iter().flat_map(|t| stand_in(t, *dim)).collect(),
            Embedder::Model(onnx) => {
                let Onnx {
                    fingerprint: _,
                    session,
                    tokenizer,
                    token_types,
                } = onnx.as_mut();
                let encodings = tokenizer
                    .encode_batch(texts.to_vec(), true)
                    .map_err(|e| anyhow::anyhow!("tokenizing: {e}"))?;
                // The batch is padded to its longest text; split it so that no forward pass has
                // more than `batch_tokens` tokens.
                let len = encodings.first().map_or(1, |e| e.get_ids().len().max(1));
                let per_pass = (config.batch_tokens / len).max(1);
                let mut out = Vec::with_capacity(encodings.len() * dim);
                for encodings in encodings.chunks(per_pass) {
                    let batch = encodings.len();
                    let len = encodings.first().map_or(0, |e| e.get_ids().len());
                    let column = |f: &dyn Fn(&tokenizers::Encoding) -> &[u32]| -> Vec<i64> {
                        encodings
                            .iter()
                            .flat_map(|e| f(e).iter().map(|&v| i64::from(v)))
                            .collect()
                    };
                    let tensor = |values: Vec<i64>| {
                        ort::value::Tensor::from_array(([batch, len], values))
                            .map_err(|e| anyhow::anyhow!("input tensor: {e}"))
                    };
                    let ids = tensor(column(&|e| e.get_ids()))?;
                    let mask = tensor(column(&|e| e.get_attention_mask()))?;
                    let outputs = if *token_types {
                        let types = tensor(column(&|e| e.get_type_ids()))?;
                        session.run(ort::inputs![
                            "input_ids" => ids,
                            "attention_mask" => mask,
                            "token_type_ids" => types
                        ])
                    } else {
                        session.run(ort::inputs!["input_ids" => ids, "attention_mask" => mask])
                    }
                    .map_err(|e| anyhow::anyhow!("embedding forward pass: {e}"))?;
                    let (shape, hidden) = outputs[0]
                        .try_extract_tensor::<f32>()
                        .map_err(|e| anyhow::anyhow!("embedding output: {e}"))?;
                    if shape.len() != 3 || shape[2] as usize != dim || shape[0] as usize != batch {
                        bail!("embedding output has shape {shape:?}");
                    }
                    let seq = shape[1] as usize;
                    // CLS pooling: the first token's hidden state (the model card's pooling).
                    for b in 0..batch {
                        out.extend_from_slice(&hidden[b * seq * dim..b * seq * dim + dim]);
                    }
                }
                out
            }
        };
        for v in out.chunks_mut(dim) {
            normalise(v);
        }
        Ok(out)
    }
}

/// Reference embeddings of a few texts from the model card's own implementation
/// (sentence-transformers, normalised CLS pooling), at the revision the image pins.
pub const REFERENCE: &str = include_str!("../../../fixtures/embedding-reference.json");
/// Largest difference allowed in any number (03-engine.md §3).
pub const PARITY: f32 = 1e-3;

#[derive(Deserialize)]
struct Reference {
    model: String,
    texts: Vec<String>,
    embeddings: Vec<Vec<f32>>,
}

/// The parity check: the model through `ort` and the Rust tokenizer must give the reference
/// embeddings. Returns the largest difference. The image runs it when it is built
/// (`kindred-world embed-check`), so a release can't ship an embedder that disagrees.
pub fn check(config: &Embed) -> anyhow::Result<f32> {
    let reference: Reference = serde_json::from_str(REFERENCE)?;
    anyhow::ensure!(
        reference.model == config.model_id,
        "the reference is for {}, the build uses {}",
        reference.model,
        config.model_id
    );
    let mut embedder = Embedder::load(config)?;
    let texts: Vec<&str> = reference.texts.iter().map(String::as_str).collect();
    let got = embedder.embed(&texts, config)?;
    let mut worst = 0f32;
    for (want, got) in reference
        .embeddings
        .iter()
        .zip(got.chunks(config.dimensions))
    {
        anyhow::ensure!(want.len() == got.len(), "reference dimensions differ");
        for (a, b) in want.iter().zip(got) {
            worst = worst.max((a - b).abs());
        }
    }
    anyhow::ensure!(
        worst <= PARITY,
        "the embedding differs from the reference by {worst} (at most {PARITY})"
    );
    Ok(worst)
}

fn normalise(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

/// The fixture's embedding: each lower-cased word adds ±1 to a dimension chosen by its hash.
fn stand_in(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0f32; dim];
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
    {
        let h = Sha256::digest(word.to_lowercase().as_bytes());
        let i = u64::from_le_bytes(h[..8].try_into().expect("8 bytes")) as usize % dim;
        v[i] += if h[8] & 1 == 0 { 1.0 } else { -1.0 };
    }
    if v.iter().all(|&x| x == 0.0) {
        v[0] = 1.0;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stand_in_is_unit_and_word_based() {
        let config: crate::config::Config =
            crate::config::parse("world.toml", crate::config::WORLD_TOML).unwrap();
        let mut e = Embedder::StandIn {
            dim: config.embed.dimensions,
        };
        let v = e
            .embed(
                &["dragons and magic", "magic and dragons", "tax law"],
                &config.embed,
            )
            .unwrap();
        let d = config.embed.dimensions;
        let (a, b, c) = (&v[..d], &v[d..2 * d], &v[2 * d..]);
        let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>();
        assert!((dot(a, a) - 1.0).abs() < 1e-5);
        assert!((dot(a, b) - 1.0).abs() < 1e-5, "same words, same vector");
        assert!(dot(a, c).abs() < 0.5);
    }

    #[test]
    fn cache_keeps_records_across_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.bin");
        let mut c = Cache::open(&path, 3).unwrap();
        c.add([1; 32], &[1.0, 2.0, 3.0]).unwrap();
        c.flush().unwrap();
        // A partial record at the end is ignored.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[7; 10])
            .unwrap();
        let mut c = Cache::open(&path, 3).unwrap();
        assert_eq!(c.get(&[1; 32]), Some(&[1.0, 2.0, 3.0][..]));
        assert!(!c.has(&[2; 32]));
        // Compaction keeps only the records still wanted.
        c.add([2; 32], &[4.0, 5.0, 6.0]).unwrap();
        c.compact([[2u8; 32]].into_iter()).unwrap();
        let c = Cache::open(&path, 3).unwrap();
        assert!(!c.has(&[1; 32]));
        assert_eq!(c.get(&[2; 32]), Some(&[4.0, 5.0, 6.0][..]));
    }

    #[test]
    fn the_reference_is_for_the_configured_model() {
        let config: crate::config::Config =
            crate::config::parse("world.toml", crate::config::WORLD_TOML).unwrap();
        let reference: Reference = serde_json::from_str(REFERENCE).unwrap();
        assert_eq!(reference.model, config.embed.model_id);
        assert_eq!(reference.texts.len(), reference.embeddings.len());
        for e in &reference.embeddings {
            assert_eq!(e.len(), config.embed.dimensions);
            let norm: f32 = e.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn text_skips_empty_parts() {
        let t = text_of(&[
            Some("Dune".into()),
            None,
            Some(" ".into()),
            Some("Spice.".into()),
        ]);
        assert_eq!(t, "Dune\nSpice.");
    }
}
