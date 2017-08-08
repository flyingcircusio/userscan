extern crate fnv;

use errors::*;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use ignore::{self, DirEntry};
use nix::fcntl;
use output::p2s;
use self::fnv::FnvHashMap;
use serde_json;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::io::prelude::*;
use std::ops::Deref;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub struct StorePaths {
    dent: DirEntry,
    refs: Vec<PathBuf>,
    cached: bool,
}

impl StorePaths {
    pub fn new(dent: DirEntry, refs: Vec<PathBuf>) -> Self {
        StorePaths {
            dent: dent,
            refs: refs,
            cached: false,
        }
    }

    pub fn path(&self) -> &Path {
        self.dent.path()
    }

    pub fn error(&self) -> Option<&ignore::Error> {
        self.dent.error()
    }

    pub fn ino(&self) -> Result<u64> {
        self.dent.ino().ok_or_else(|| {
            ErrorKind::DentNoMetadata(self.path().to_path_buf()).into()
        })
    }

    pub fn metadata(&self) -> Result<fs::Metadata> {
        self.dent.metadata().chain_err(|| {
            ErrorKind::DentNoMetadata(self.path().to_path_buf())
        })
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    pub fn iter_refs<'a>(&'a self) -> Box<Iterator<Item = &Path> + 'a> {
        Box::new(self.refs.iter().map(|p| p.as_path()))
    }

    #[allow(dead_code)] // only used in tests
    pub fn refs(&self) -> &Vec<PathBuf> {
        &self.refs
    }
}

impl fmt::Display for StorePaths {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.refs.is_empty() {
            write!(f, "{}", self.dent.path().display())
        } else {
            write!(f, "{}:", self.dent.path().display())?;
            for r in self.refs.iter() {
                write!(f, " {}", r.display())?;
            }
            Ok(())
        }
    }
}

pub enum Lookup {
    Dir(StorePaths),
    Hit(StorePaths),
    Miss(DirEntry),
}

#[derive(Debug, PartialEq, PartialOrd, Clone, Serialize, Deserialize)]
struct CacheLine {
    len: u64,
    ctime: i64,
    ctime_nsec: i64,
    refs: Vec<PathBuf>,
    #[serde(skip)]
    used: bool,
}

type CacheMap = FnvHashMap<u64, CacheLine>;

#[derive(Debug, Default)]
pub struct Cache {
    map: RwLock<CacheMap>,
    filename: PathBuf,
    file: Option<fs::File>,
    dirty: AtomicBool,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&mut self, file: &mut fs::File) -> Result<CacheMap> {
        file.seek(io::SeekFrom::Start(0))?;
        let path = self.filename.as_path();
        let r = io::BufReader::new(file);
        match path.extension() {
            Some(e) if e == OsStr::new("gz") => serde_json::from_reader(GzDecoder::new(r)?),
            _ => serde_json::from_reader(r),
        }.chain_err(|| format!("format error while reading cache file {}", p2s(path)))
    }

    pub fn open<P: AsRef<Path>>(mut self, path: P) -> Result<Self> {
        self.filename = path.as_ref().to_path_buf();
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .chain_err(|| format!("failed to open cache file {}", p2s(&path)))?;
        fcntl::flock(file.as_raw_fd(), fcntl::FlockArg::LockExclusiveNonblock)
            .chain_err(|| {
                format!(
                    "failed to lock cache file {}: another instance running?",
                    p2s(&path)
                )
            })?;
        if file.metadata()?.len() > 0 {
            self.map = RwLock::new(self.read(&mut file)?);
            self.dirty = AtomicBool::new(false);
            debug!("Loaded {} entries from cache", self.map.read().unwrap().len());
        } else {
            debug!("Creating new cache {}", p2s(&path));
            self.map.write().unwrap().clear();
            self.dirty = AtomicBool::new(true);
        }
        self.file = Some(file);
        Ok(self)
    }

    pub fn commit(&mut self) -> Result<()> {
        let path = self.filename.as_path();
        if let Some(ref mut file) = self.file {
            if !self.dirty.compare_and_swap(true, false, Ordering::SeqCst) {
                return Ok(());
            }
            let mut map = self.map.write().unwrap();
            map.retain(|_, ref mut v| v.used);
            debug!("Writing {} entries to cache", map.len());
            file.seek(io::SeekFrom::Start(0))?;
            file.set_len(0)?;
            let w = io::BufWriter::new(file);
            match path.extension() {
                Some(e) if e == OsStr::new("gz") => {
                    serde_json::to_writer(GzEncoder::new(w, Compression::Default), map.deref())
                }
                _ => serde_json::to_writer(w, map.deref()),
            }.chain_err(|| format!("cannot write cache file {}", p2s(path)))
        } else {
            // don't do anything if there is no file set except for evicting unused elements
            Ok(())
        }
    }

    fn get(&self, dent: &DirEntry) -> Result<Vec<PathBuf>> {
        let ino = dent.ino().ok_or(ErrorKind::CacheNotFound)?;
        let mut map = self.map.write().unwrap();
        let c = map.get_mut(&ino).ok_or(ErrorKind::CacheNotFound)?;
        let meta = dent.metadata()?;
        if c.len == meta.len() && c.ctime == meta.ctime() && c.ctime_nsec == meta.ctime_nsec() {
            c.used = true;
            Ok(c.refs.clone())
        } else {
            Err(ErrorKind::CacheNotFound.into())
        }
    }

    pub fn lookup(&self, dent: DirEntry) -> Lookup {
        if let Some(ft) = dent.file_type() {
            if ft.is_dir() {
                return Lookup::Dir(StorePaths {
                    dent: dent,
                    refs: vec![],
                    cached: true,
                });
            }
        }
        match self.get(&dent) {
            Ok(refs) => {
                debug!("Cache hit: {}", dent.path().display());
                Lookup::Hit(StorePaths {
                    dent: dent,
                    refs: refs,
                    cached: true,
                })
            }
            Err(_) => Lookup::Miss(dent),
        }
    }

    pub fn insert(&self, sp: &StorePaths) -> Result<()> {
        if sp.cached {
            return Ok(());
        }
        let meta = sp.metadata()?;
        self.map.write().unwrap().insert(
            sp.ino()?,
            CacheLine {
                len: meta.len(),
                ctime: meta.ctime(),
                ctime_nsec: meta.ctime_nsec(),
                refs: sp.refs.clone(),
                used: true,
            },
        );
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }
}

impl ToString for Cache {
    fn to_string(&self) -> String {
        serde_json::to_string_pretty(&self.map.read().unwrap().deref())
            // must be something *really* weird since we control cache contents
            .unwrap_or_else(|e| format!("fatal: unrepresentable content: {} ({:?})", e, self))
    }
}


#[cfg(test)]
mod tests {
    use std::fs;
    use super::*;
    use super::Lookup::*;
    use tests::dent;

    #[test]
    fn insert_cacheline() {
        let c = Cache::new();
        let dent = tests::dent("dir1/proto-http.la");
        c.insert(&StorePaths {
            dent: dent,
            refs: vec![],
            cached: false,
        }).expect("insert failed");
        println!("Cache: {}", c.to_string());

        let dent = tests::dent("dir1/proto-http.la");
        let map = c.map.read().unwrap();
        let entry = map.get(&dent.ino().unwrap()).expect(
            "cache entry not found",
        );
        assert_eq!(entry.len, 1157);
        assert_eq!(entry.ctime, fs::metadata("dir1/proto-http.la").unwrap().ctime());
        assert!(c.dirty.load(Ordering::SeqCst));
    }

    #[test]
    fn lookup_should_miss_on_changed_metadata() {
        let c = Cache::new();
        let dent = tests::dent("dir2/lftp");
        let ino = dent.ino().unwrap();
        c.insert(&StorePaths {
            dent: dent,
            refs: vec![PathBuf::from("/nix/store/ref")],
            cached: false,
        }).expect("insert failed");

        match c.lookup(tests::dent("dir2/lftp")) {
            Hit(sp) => assert_eq!(vec![PathBuf::from("/nix/store/ref")], sp.refs),
            _ => panic!("test failure: did not find dir2/lftp in cache"),
        }

        c.map.write().unwrap().get_mut(&ino).unwrap().len = 9999;
        match c.lookup(tests::dent("dir2/lftp")) {
            Miss(_) => (),
            _ => panic!("should not hit: dir2/lftp"),
        }
    }

    /*
     * TODO testing
     * test "used" logic
     * "cached" logic
     * test gzip actually takes place
     * cache hit stats
     */
}
