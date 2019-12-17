use crate::output::p2s;

use fnv::FnvHashMap;
use minilzo;
use nix::fcntl;
use rmp_serde::{decode, encode};
use std::fs;
use std::io;
use std::io::prelude::*;
use std::ops::{Deref, DerefMut};
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error")]
    IO(#[from] io::Error),
    #[error("LZO error")]
    LZO(#[from] minilzo::Error),
    #[error("MessagePack decode error")]
    RmpDE(#[from] rmp_serde::decode::Error),
    #[error("MessagePack encode error")]
    RmpEN(#[from] rmp_serde::encode::Error),
    #[error("Cannot acquire lock")]
    Lock(#[from] nix::Error),
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, PartialOrd, Clone, Serialize, Deserialize)]
pub struct CacheLine {
    pub ctime: i64,
    pub ctime_nsec: u8,
    pub refs: Vec<PathBuf>,
    #[serde(skip)]
    pub used: bool,
}

impl PartialEq for CacheLine {
    fn eq(&self, other: &CacheLine) -> bool {
        self.ctime == other.ctime
            && self.ctime_nsec == other.ctime_nsec
            && self.refs == other.refs
    }
}

impl CacheLine {
    pub fn new(ctime: i64, ctime_nsec: u8, refs: &[PathBuf]) -> Self {
        Self {
            ctime,
            ctime_nsec,
            refs: refs.to_vec(),
            used: true,
        }
    }
}

/// Creates or opens a file with an exclusive flock
pub fn open_locked<P: AsRef<Path>>(path: P) -> Result<fs::File> {
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    fcntl::flock(f.as_raw_fd(), fcntl::FlockArg::LockExclusiveNonblock)?;
    Ok(f)
}

/// Persistent cache data structure. Maps inode numbers to cache lines.
#[derive(Debug, Default, PartialEq, Clone, Serialize, Deserialize)]
pub struct CacheMap {
    map: FnvHashMap<u64, CacheLine>,
}

impl CacheMap {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a cache file into a CacheMap structure
    pub fn load<P: AsRef<Path>>(file: &mut fs::File, filename: P) -> Result<CacheMap> {
        let mut compr = Vec::new();
        file.seek(io::SeekFrom::Start(0))?;
        file.read_to_end(&mut compr)?;
        match minilzo::decompress(&compr, compr.len() * 10)
            .map_err(Error::from)
            .and_then(|data| decode::from_slice(&data).map_err(Error::from))
        {
            Ok(cachemap) => Ok(cachemap),
            Err(err) => {
                warn!(
                    "Problem while trying to load cache from {}: {} - continuing with empty cache",
                    p2s(&filename),
                    err
                );
                Ok(Self::default())
            }
        }
    }

    /// Writes a CacheMap structure into an open file
    pub fn save(&self, file: &mut fs::File) -> Result<()> {
        file.seek(io::SeekFrom::Start(0))?;
        file.set_len(0)?;
        Ok(file.write_all(&minilzo::compress(&encode::to_vec(self)?)?)?)
    }
}

impl Deref for CacheMap {
    type Target = FnvHashMap<u64, CacheLine>;

    fn deref(&self) -> &FnvHashMap<u64, CacheLine> {
        &self.map
    }
}

impl DerefMut for CacheMap {
    fn deref_mut(&mut self) -> &mut FnvHashMap<u64, CacheLine> {
        &mut self.map
    }
}

#[cfg(test)]
mod tests {
    extern crate tempdir;
    use self::tempdir::TempDir;
    use super::*;
    use crate::tests::FIXTURES;

    #[test]
    fn cacheline_should_compare_regardless_of_used_flag() {
        assert_eq!(
            CacheLine {
                ctime: 1,
                ctime_nsec: 2,
                refs: vec![],
                used: true,
            },
            CacheLine {
                ctime: 1,
                ctime_nsec: 2,
                refs: vec![],
                used: false,
            }
        )
    }

    fn dummy_cachemap() -> CacheMap {
        let mut cm = FnvHashMap::default();
        cm.insert(1, CacheLine::new(10, 11, &[PathBuf::from("/nix/ref1")][..]));
        cm.insert(
            2,
            CacheLine::new(
                20,
                21,
                &[PathBuf::from("/nix/ref1"), PathBuf::from("/nix/ref2")][..],
            ),
        );
        CacheMap { map: cm }
    }

    #[test]
    fn save_should_create_file() {
        let tempdir = TempDir::new("save-cache").expect("failed to create tempdir");
        let filename = tempdir.path().join("cache");
        {
            let mut f = open_locked(&filename).unwrap();
            assert!(dummy_cachemap().save(&mut f).is_ok());
        }
        assert!(fs::metadata(&filename).unwrap().len() > 0);
    }

    #[test]
    fn load_should_decompress_cachefile() {
        let tempdir = TempDir::new("load-cache").expect("failed to create tempdir");
        let filename = tempdir.path().join("cache.ok");
        fs::copy(FIXTURES.join("cache.mp"), &filename).unwrap();
        let mut f = open_locked(&filename).unwrap();
        let cm = CacheMap::load(&mut f, &filename).unwrap();
        assert_eq!(12, cm.map.len());
    }

    #[test]
    fn load_should_ignore_broken_cachefile() {
        let tempdir = TempDir::new("load-cache").expect("failed to create tempdir");
        let filename = tempdir.path().join("cache.truncated");
        fs::copy(FIXTURES.join("cache.mp"), &filename).unwrap();
        let mut f = open_locked(&filename).unwrap();
        f.set_len(500).unwrap();
        let cm = CacheMap::load(&mut f, &filename).expect("should ignore truncated cache file");
        assert_eq!(cm.map.len(), 0);
    }
}
