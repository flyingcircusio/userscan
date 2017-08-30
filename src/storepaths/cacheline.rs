use errors::*;
use fnv::FnvHashMap;
use minilzo;
use nix::fcntl;
use output::p2s;
use rmp_serde::{encode, decode};
use std::fs;
use std::io;
use std::io::prelude::*;
use std::ops::{Deref, DerefMut};
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialOrd, Clone, Serialize, Deserialize)]
pub struct CacheLine {
    pub ctime: i64,
    pub ctime_nsec: i64,
    pub refs: Vec<PathBuf>,
    #[serde(skip)]
    pub used: bool,
}

impl PartialEq for CacheLine {
    fn eq(&self, other: &CacheLine) -> bool {
        self.ctime == other.ctime && self.ctime_nsec == other.ctime_nsec && self.refs == other.refs
    }
}

impl CacheLine {
    #[allow(dead_code)]
    pub fn new(ctime: i64, ctime_nsec: i64, refs: &[PathBuf]) -> Self {
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
        .open(&path)
        .chain_err(|| format!("failed to open cache file {}", p2s(&path)))?;
    fcntl::flock(f.as_raw_fd(), fcntl::FlockArg::LockExclusiveNonblock)
        .chain_err(|| {
            format!(
                "failed to lock cache file {}: another instance running?",
                p2s(&path)
            )
        })?;
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
        file.read_to_end(&mut compr).chain_err(|| {
            format!("error while reading {}", p2s(&filename))
        })?;
        decode::from_slice(&minilzo::decompress(&compr, compr.len() * 10).chain_err(
            || {
                format!("failed to decompress {}", p2s(&filename))
            },
        )?).chain_err(|| {
            format!("format error {} (try to delete and re-run)", p2s(&filename))
        })
    }

    /// Writes a CacheMap structure into an open file
    pub fn save<P: AsRef<Path>>(&self, file: &mut fs::File, filename: P) -> Result<()> {
        file.seek(io::SeekFrom::Start(0))?;
        file.set_len(0)?;
        file.write_all(&minilzo::compress(&encode::to_vec(self)?)?)
            .chain_err(|| format!("error while writing {}", p2s(&filename)))
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
    use super::*;

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

    #[allow(dead_code)]
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
}
