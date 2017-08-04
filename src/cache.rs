extern crate fnv;

use errors::*;
use ignore::{self, DirEntry};
use self::fnv::FnvHashMap;
use serde_json;
use std::fmt;
use std::fs;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug)]
pub struct StorePaths {
    dent: DirEntry,
    refs: Vec<PathBuf>,
}

impl StorePaths {
    pub fn new(dent: DirEntry, refs: Vec<PathBuf>) -> Self {
        StorePaths {
            dent: dent,
            refs: refs,
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
    Miss(DirEntry),
    Hit(StorePaths),
}

#[derive(Debug, PartialEq, PartialOrd, Clone, Serialize)]
struct CacheLine {
    len: u64,
    mode: u32,
    mtime: SystemTime,
    refs: Vec<PathBuf>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Cache {
    vers: u32,
    prog: &'static str,
    map: FnvHashMap<u64, CacheLine>,
    #[serde(skip)]
    dirty: bool,
}

impl Cache {
    pub fn new() -> Self {
        Cache {
            vers: 1,
            prog: crate_name!(),
            ..Self::default()
        }
    }

    pub fn open<P: AsRef<Path>>(self, file: P) -> Result<Self> {
        warn!(
            "cannot open cache file {}: not implemented",
            file.as_ref().display()
        );
        Ok(self)
    }

    fn get(&self, dent: &DirEntry) -> Result<&CacheLine> {
        let ino = dent.ino().ok_or(ErrorKind::CacheNotFound)?;
        let c = self.map.get(&ino).ok_or(ErrorKind::CacheNotFound)?;
        let meta = dent.metadata()?;
        if c.len == meta.len() && c.mode == meta.permissions().mode() &&
            c.mtime == meta.modified()?
        {
            Ok(c)
        } else {
            Err(ErrorKind::CacheNotFound.into())
        }
    }

    pub fn lookup(&self, dent: DirEntry) -> Lookup {
        match self.get(&dent) {
            Ok(c) => Lookup::Hit(StorePaths {
                dent: dent,
                refs: c.refs.clone(),
            }),
            Err(_) => Lookup::Miss(dent),
        }
    }

    pub fn insert(&mut self, sp: &StorePaths) -> Result<()> {
        let meta = sp.metadata()?;
        self.map.insert(
            sp.ino()?,
            CacheLine {
                len: meta.len(),
                mode: meta.permissions().mode(),
                mtime: meta.modified()?,
                refs: sp.refs.clone(),
            },
        );
        self.dirty = true;
        Ok(())
    }
}

impl ToString for Cache {
    fn to_string(&self) -> String {
        serde_json::to_string_pretty(self)
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
        let mut c = Cache::new();
        let dent = tests::dent("dir1/proto-http.la");
        c.insert(&StorePaths {
            dent: dent,
            refs: vec![],
        }).expect("insert failed");
        println!("Cache: {}", c.to_string());

        let dent = tests::dent("dir1/proto-http.la");
        let entry = c.map.get(&dent.ino().unwrap()).expect(
            "cache entry not found",
        );
        assert_eq!(entry.len, 1157);
        assert_eq!(entry.mode, 0o644);
        assert_eq!(
            entry.mtime,
            fs::metadata("dir1/proto-http.la")
                .unwrap()
                .modified()
                .unwrap()
        );
        assert!(c.dirty);
    }

    #[test]
    fn lookup_should_miss_on_changed_metadata() {
        let mut c = Cache::new();
        let dent = tests::dent("dir2/lftp");
        let ino = dent.ino().unwrap();
        c.insert(&StorePaths {
            dent: dent,
            refs: vec![PathBuf::from("/nix/store/ref")],
        }).expect("insert failed");

        match c.lookup(tests::dent("dir2/lftp")) {
            Hit(sp) => assert_eq!(vec![PathBuf::from("/nix/store/ref")], sp.refs),
            Miss(d) => panic!("test failure: did not find {:?} in cache", d),
        }

        c.map.get_mut(&ino).unwrap().len = 9999;
        match c.lookup(tests::dent("dir2/lftp")) {
            Hit(sp) => panic!("should not hit: {:?}", sp),
            Miss(_) => (),
        }
    }
}
