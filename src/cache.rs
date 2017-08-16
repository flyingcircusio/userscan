extern crate fnv;

use colored::Colorize;
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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug)]
pub struct StorePaths {
    dent: DirEntry,
    refs: Vec<PathBuf>,
    cached: bool,
    bytes_scanned: u64,
    metadata: Option<fs::Metadata>,
}

impl StorePaths {
    pub fn new(
        dent: DirEntry,
        refs: Vec<PathBuf>,
        bytes_scanned: u64,
        metadata: Option<fs::Metadata>,
    ) -> Self {
        StorePaths {
            dent,
            refs,
            bytes_scanned,
            cached: false,
            metadata,
        }
    }

    #[inline]
    pub fn path(&self) -> &Path {
        self.dent.path()
    }

    #[inline]
    pub fn error(&self) -> Option<&ignore::Error> {
        self.dent.error()
    }

    #[inline]
    pub fn ino(&self) -> Result<u64> {
        self.dent.ino().ok_or_else(|| {
            ErrorKind::DentNoMetadata(self.path().to_path_buf()).into()
        })
    }

    pub fn metadata(&mut self) -> Result<fs::Metadata> {
        match self.metadata {
            Some(ref m) => Ok(m.clone()),
            None => {
                let m = self.dent.metadata().chain_err(|| {
                    ErrorKind::DentNoMetadata(self.path().to_path_buf())
                })?;
                self.metadata = Some(m.clone());
                Ok(m)
            }
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    #[inline]
    pub fn iter_refs<'a>(&'a self) -> Box<Iterator<Item = &Path> + 'a> {
        Box::new(self.refs.iter().map(|p| p.as_path()))
    }

    #[allow(dead_code)] // only used in tests
    pub fn refs(&self) -> &Vec<PathBuf> {
        &self.refs
    }

    #[inline]
    pub fn bytes_scanned(&self) -> u64 {
        self.bytes_scanned
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
    hits: AtomicUsize,
    misses: AtomicUsize,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&mut self, file: &mut fs::File) -> Result<CacheMap> {
        file.seek(io::SeekFrom::Start(0))?;
        let path = self.filename.as_path();
        let r = io::BufReader::with_capacity(1 << 20, file);
        match path.extension() {
            Some(e) if e == OsStr::new("gz") => serde_json::from_reader(GzDecoder::new(r)?),
            _ => serde_json::from_reader(r),
        }.chain_err(|| {
            format!("format error while reading cache file {}", p2s(path))
        })
    }

    pub fn open<P: AsRef<Path>>(mut self, path: P) -> Result<Self> {
        self.filename = path.as_ref().to_path_buf();
        info!("Loading cache {}", p2s(&self.filename));
        let mut cachefile = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .chain_err(|| format!("failed to open cache file {}", p2s(&path)))?;
        fcntl::flock(
            cachefile.as_raw_fd(),
            fcntl::FlockArg::LockExclusiveNonblock,
        ).chain_err(|| {
            format!(
                "failed to lock cache file {}: another instance running?",
                p2s(&path)
            )
        })?;
        if cachefile.metadata()?.len() > 0 {
            self.map = RwLock::new(self.read(&mut cachefile)?);
            self.dirty = AtomicBool::new(false);
            debug!(
                "loaded {} entries from cache",
                self.map.read().unwrap().len()
            );
        } else {
            debug!("creating new cache {}", p2s(&path));
            self.map.write().unwrap().clear();
            self.dirty = AtomicBool::new(true);
        }
        self.file = Some(cachefile);
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
            debug!("writing {} entries to cache", map.len());
            file.seek(io::SeekFrom::Start(0))?;
            file.set_len(0)?;
            let w = io::BufWriter::with_capacity(1 << 20, file);
            match path.extension() {
                Some(e) if e == OsStr::new("gz") => {
                    serde_json::to_writer(GzEncoder::new(w, Compression::Fast), map.deref())
                }
                _ => serde_json::to_writer(w, map.deref()),
            }.chain_err(|| format!("cannot write cache file {}", p2s(path)))
        } else {
            // don't do anything if there is no file set except for evicting unused elements
            Ok(())
        }
    }

    fn get(&self, dent: &DirEntry) -> Result<(Vec<PathBuf>, fs::Metadata)> {
        let ino = dent.ino().ok_or(ErrorKind::CacheNotFound)?;
        let mut map = self.map.write().unwrap();
        let c = map.get_mut(&ino).ok_or(ErrorKind::CacheNotFound)?;
        let meta = dent.metadata()?;
        if c.ctime == meta.ctime() && c.ctime_nsec == meta.ctime_nsec() {
            c.used = true;
            Ok((c.refs.clone(), meta))
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
                    bytes_scanned: 0,
                    metadata: None,
                });
            }
        }
        match self.get(&dent) {
            Ok((refs, metadata)) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Lookup::Hit(StorePaths {
                    dent,
                    refs,
                    cached: true,
                    bytes_scanned: 0,
                    metadata: Some(metadata),
                })
            }
            Err(_) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                Lookup::Miss(dent)
            }
        }
    }

    pub fn insert(&self, sp: &mut StorePaths) -> Result<()> {
        if sp.cached {
            return Ok(());
        }
        let meta = sp.metadata()?;
        self.map.write().unwrap().insert(
            sp.ino()?,
            CacheLine {
                ctime: meta.ctime(),
                ctime_nsec: meta.ctime_nsec(),
                refs: sp.refs.clone(),
                used: true,
            },
        );
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    /* statistics */

    pub fn len(&self) -> usize {
        self.map.read().unwrap().len()
    }

    #[allow(dead_code)]
    pub fn nrefs(&self) -> usize {
        self.map
            .read()
            .unwrap()
            .values()
            .map(|cl| cl.refs.len())
            .sum()
    }

    pub fn hit_ratio(&self) -> f32 {
        let h = self.hits.load(Ordering::SeqCst);
        let m = self.misses.load(Ordering::SeqCst);
        if h == 0 {
            0.0
        } else {
            h as f32 / (h as f32 + m as f32)
        }
    }

    pub fn log_statistics(&self) {
        if self.file.is_some() {
            info!(
                "Cache saved to {}, {} entries, hit ratio {}%",
                p2s(&self.filename),
                self.len().to_string().cyan(),
                ((self.hit_ratio() * 100.0) as u32).to_string().cyan()
            )
        }
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
    extern crate tree_magic;

    use std::fs;
    use super::*;
    use super::Lookup::*;
    use tests::{FIXTURES, dent};
    use tempdir::TempDir;

    #[test]
    fn insert_cacheline() {
        let c = Cache::new();
        let dent = tests::dent("dir1/proto-http.la");
        c.insert(&mut StorePaths {
            dent: dent,
            refs: vec![],
            cached: false,
            bytes_scanned: 0,
            metadata: None,
        }).expect("insert failed");
        println!("Cache: {}", c.to_string());

        let dent = tests::dent("dir1/proto-http.la");
        let map = c.map.read().unwrap();
        let entry = map.get(&dent.ino().unwrap()).expect(
            "cache entry not found",
        );
        assert_eq!(
            entry.ctime,
            fs::metadata("dir1/proto-http.la").unwrap().ctime()
        );
    }

    fn sp_dummy() -> StorePaths {
        let dent = tests::dent("dir2/lftp");
        StorePaths {
            dent: dent,
            refs: vec![
                PathBuf::from("/nix/store/q3wx1gab2ysnk5nyvyyg56ana2v4r2ar-glibc-2.24"),
            ],
            cached: false,
            bytes_scanned: 0,
            metadata: None,
        }
    }

    #[test]
    fn lookup_should_miss_on_changed_metadata() {
        let c = Cache::new();
        let ino = tests::dent("dir2/lftp").ino().unwrap();
        c.insert(&mut sp_dummy()).expect("insert failed");

        match c.lookup(tests::dent("dir2/lftp")) {
            Hit(sp) => {
                assert_eq!(
                    vec![
                        PathBuf::from("/nix/store/q3wx1gab2ysnk5nyvyyg56ana2v4r2ar-glibc-2.24"),
                    ],
                    sp.refs
                )
            }
            _ => panic!("test failure: did not find dir2/lftp in cache"),
        }

        c.map.write().unwrap().get_mut(&ino).unwrap().ctime = 6674;
        match c.lookup(tests::dent("dir2/lftp")) {
            Miss(_) => (),
            _ => panic!("should not hit: dir2/lftp"),
        }
    }

    #[test]
    fn load_save_cache_roundtrip() {
        let td = TempDir::new("load_cache_json").unwrap();
        let cache_file = td.path().join("cache.json");
        fs::copy(FIXTURES.join("cache.json"), &cache_file).unwrap();
        let mut c = Cache::new().open(&cache_file).unwrap();
        assert_eq!(10, c.len());
        assert!(!c.dirty.load(Ordering::SeqCst));
        for ref cl in c.map.read().unwrap().values() {
            assert!(!cl.used);
        }

        c.insert(&mut sp_dummy()).unwrap();
        assert!(c.dirty.load(Ordering::SeqCst));
        // exactly the newly inserted cacheline should have the "used" flag set
        assert_eq!(
            1,
            c.map
                .read()
                .unwrap()
                .values()
                .filter(|cl| cl.used)
                .collect::<Vec<_>>()
                .len()
        );

        c.commit().unwrap();
        assert_eq!(1, c.len());
        let json_len = fs::metadata(&cache_file).unwrap().len();
        println!("json_len: {}", json_len);
        assert_eq!(json_len, 122);
    }

    #[test]
    fn commit_actually_gzips_file() {
        let td = TempDir::new("commit_actually_gzips_file").unwrap();
        let file = td.path().join("cache.json.gz");
        let mut c = Cache::new().open(&file).unwrap();
        // open should create empty file
        assert_eq!(0, fs::metadata(&file).unwrap().len());
        c.insert(&mut sp_dummy()).unwrap();
        c.commit().unwrap();
        assert_eq!("application/gzip", tree_magic::from_filepath(&file));
    }
}
