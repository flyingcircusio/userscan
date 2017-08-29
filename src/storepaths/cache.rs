use colored::Colorize;
use errors::*;
use ignore::DirEntry;
use output::p2s;
use std::fs;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use super::cacheline::*;
use super::{Lookup, StorePaths};

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

    pub fn open<P: AsRef<Path>>(mut self, path: P) -> Result<Self> {
        self.filename = path.as_ref().to_path_buf();
        info!("Loading cache {}", p2s(&self.filename));
        if let Some(p) = path.as_ref().parent() {
            fs::create_dir_all(p).chain_err(|| {
                format!("cache: failed to create leading directory {}", p2s(p))
            })?;
        }
        let mut cachefile = open_locked(&path)?;
        if cachefile.metadata()?.len() > 0 {
            let map = CacheMap::load(&mut cachefile, &self.filename)?;
            debug!("loaded {} entries from cache", map.len());
            self.map = RwLock::new(map);
            self.dirty = AtomicBool::new(false);
        } else {
            debug!("creating new cache {}", p2s(&path));
            self.map.write().expect("tainted lock").clear();
            self.dirty = AtomicBool::new(true);
        }
        self.file = Some(cachefile);
        Ok(self)
    }

    pub fn commit(&mut self) -> Result<()> {
        if let Some(ref mut file) = self.file {
            if !self.dirty.compare_and_swap(true, false, Ordering::SeqCst) {
                return Ok(());
            }
            let mut map = self.map.write().expect("tainted lock");
            map.retain(|_, ref mut v| v.used);
            debug!("writing {} entries to cache", map.len());
            map.save(file, &self.filename)
        } else {
            // don't do anything if there is no file set except for evicting unused elements
            Ok(())
        }
    }

    fn get(&self, dent: &DirEntry) -> Result<(Vec<PathBuf>, fs::Metadata)> {
        let ino = dent.ino().ok_or(ErrorKind::CacheNotFound)?;
        let mut map = self.map.write().expect("tainted lock");
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
        self.map.write().expect("tainted lock").insert(
            sp.ino()?,
            CacheLine::new(
                meta.ctime(),
                meta.ctime_nsec(),
                &sp.refs,
            ),
        );
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    /* statistics */

    pub fn len(&self) -> usize {
        self.map.read().expect("tainted lock").len()
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


#[cfg(test)]
mod tests {
    extern crate tree_magic;

    use std::fs;
    use super::*;
    use super::Lookup::*;
    use tests::{FIXTURES, dent};
    use tempdir::TempDir;

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
    fn load_save_cache() {
        let td = TempDir::new("load_save_cache").unwrap();
        let cache_file = td.path().join("cache.mp");
        fs::copy(FIXTURES.join("cache.mp"), &cache_file).unwrap();
        let mut c = Cache::new().open(&cache_file).unwrap();
        assert_eq!(12, c.len());
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
        let cache_len = fs::metadata(&cache_file).unwrap().len();
        assert!(cache_len > 60);
    }
}
