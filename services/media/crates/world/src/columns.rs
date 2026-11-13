//! Flat little-endian arrays: written with one `write_all`, read back memory-mapped and cast
//! in place, without parsing or copying.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;

use bytemuck::Pod;
use memmap2::Mmap;

use crate::{Error, Result};

pub(crate) fn write<T: Pod>(path: &Path, values: &[T]) -> Result<()> {
    let mut file = File::create(path).map_err(|e| Error::io(path, e))?;
    file.write_all(bytemuck::cast_slice(values))
        .map_err(|e| Error::io(path, e))?;
    file.sync_all().map_err(|e| Error::io(path, e))
}

/// A memory-mapped column of `T`. An empty file maps to an empty slice (a zero-length file
/// can't be mapped).
pub(crate) struct Column<T> {
    map: Option<Mmap>,
    len: usize,
    _t: std::marker::PhantomData<T>,
}

impl<T: Pod> Column<T> {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        let size = file.metadata().map_err(|e| Error::io(path, e))?.len() as usize;
        let width = std::mem::size_of::<T>();
        if !size.is_multiple_of(width) {
            return Err(Error::Corrupt {
                path: path.display().to_string(),
                what: format!("{size} bytes is not a whole number of {width}-byte values"),
            });
        }
        let map = if size == 0 {
            None
        } else {
            // SAFETY: the world's files are immutable once written (a new build is a new
            // directory), so the mapping isn't changed under us.
            Some(unsafe { Mmap::map(&file) }.map_err(|e| Error::io(path, e))?)
        };
        let column = Column {
            map,
            len: size / width,
            _t: std::marker::PhantomData,
        };
        // A mapping starts on a page boundary, so any `T` is aligned; check once anyway.
        if let Some(map) = &column.map {
            bytemuck::try_cast_slice::<u8, T>(map).map_err(|e| Error::Corrupt {
                path: path.display().to_string(),
                what: format!("{e:?}"),
            })?;
        }
        Ok(column)
    }

    pub(crate) fn as_slice(&self) -> &[T] {
        self.map.as_deref().map_or(&[], bytemuck::cast_slice)
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}
