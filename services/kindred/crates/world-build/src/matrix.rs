//! The model stages' files on the scratch volume: a table of `f32` rows keyed by work number
//! (factors, embeddings, aspect numbers, vectors). These are build intermediates derived from the
//! Goodreads data: they stay on the scratch volume (PLAN.md rule 13).

use std::io::{BufReader, BufWriter, Read as _, Write as _};
use std::path::Path;

use anyhow::{Context as _, bail};

const MAGIC: &[u8; 8] = b"KMATRIX1";

/// Rows of `dim` numbers, one per key, keys strictly ascending.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Matrix {
    pub keys: Vec<u64>,
    pub dim: usize,
    pub data: Vec<f32>,
}

impl Matrix {
    pub fn new(dim: usize) -> Matrix {
        Matrix {
            keys: Vec::new(),
            dim,
            data: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Appends a row; keys must arrive in ascending order.
    pub fn push(&mut self, key: u64, row: &[f32]) {
        assert_eq!(row.len(), self.dim, "row width");
        assert!(self.keys.last().is_none_or(|&k| k < key), "keys ascending");
        self.keys.push(key);
        self.data.extend_from_slice(row);
    }

    pub fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }

    pub fn find(&self, key: u64) -> Option<usize> {
        self.keys.binary_search(&key).ok()
    }

    pub fn get(&self, key: u64) -> Option<&[f32]> {
        self.find(key).map(|i| self.row(i))
    }

    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        let part = path.with_extension("part");
        let mut out = BufWriter::new(
            std::fs::File::create(&part).with_context(|| part.display().to_string())?,
        );
        out.write_all(MAGIC)?;
        out.write_all(&(self.keys.len() as u64).to_le_bytes())?;
        out.write_all(&(self.dim as u64).to_le_bytes())?;
        out.write_all(bytemuck::cast_slice(&self.keys))?;
        out.write_all(bytemuck::cast_slice(&self.data))?;
        out.into_inner()?.sync_all()?;
        std::fs::rename(&part, path)?;
        Ok(())
    }

    pub fn read(path: &Path) -> anyhow::Result<Matrix> {
        let mut input =
            BufReader::new(std::fs::File::open(path).with_context(|| path.display().to_string())?);
        let mut head = [0u8; 24];
        input.read_exact(&mut head)?;
        if &head[..8] != MAGIC {
            bail!("{}: not a matrix file", path.display());
        }
        let n = u64::from_le_bytes(head[8..16].try_into()?) as usize;
        let dim = u64::from_le_bytes(head[16..24].try_into()?) as usize;
        let mut keys = vec![0u64; n];
        input.read_exact(bytemuck::cast_slice_mut(&mut keys))?;
        let mut data = vec![0f32; n * dim];
        input.read_exact(bytemuck::cast_slice_mut(&mut data))?;
        if keys.windows(2).any(|p| p[0] >= p[1]) {
            bail!("{}: keys not ascending", path.display());
        }
        Ok(Matrix { keys, dim, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Matrix::new(3);
        m.push(5, &[1.0, 2.0, 3.0]);
        m.push(9, &[-1.0, 0.5, f32::MIN_POSITIVE]);
        let path = dir.path().join("m.bin");
        m.write(&path).unwrap();
        let back = Matrix::read(&path).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.get(9), Some(&[-1.0, 0.5, f32::MIN_POSITIVE][..]));
        assert_eq!(back.get(6), None);
    }
}
