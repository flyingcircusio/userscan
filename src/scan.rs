extern crate memmap;
extern crate regex;
extern crate twoway;

use errors::*;
use ignore::{self, DirEntry};
use self::memmap::{Mmap, Protection};
use self::regex::bytes::Regex;
use std::ffi::OsStr;
use std::fs;
use std::fmt;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct StorePaths {
    dent: DirEntry,
    refs: Vec<PathBuf>,
}

impl StorePaths {
    pub fn path(&self) -> &Path {
        self.dent.path()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    pub fn error(&self) -> Option<&ignore::Error> {
        self.dent.error()
    }

    pub fn iter_refs<'a>(&'a self) -> Box<Iterator<Item = &Path> + 'a> {
        Box::new(self.refs.iter().map(|p| p.as_path()))
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


lazy_static! {
    static ref STORE_RE: Regex = Regex::new(
        r"(?-u)(/nix/store/[0-9a-z]{32}-[0-9a-zA-Z+._?=-]+)").unwrap();
}

const MIN_STOREREF_LEN: u64 = 45;


#[derive(Debug, Clone)]
pub struct Scanner {
    quickcheck: usize,
}

impl Scanner {
    pub fn new(quickcheck: usize) -> Self {
        Scanner { quickcheck: quickcheck }
    }

    fn scan_regular(&self, dent: &DirEntry) -> Result<Vec<PathBuf>> {
        let len = dent.metadata()?.len();
        if len < MIN_STOREREF_LEN {
            // minimum length to fit a single store reference not reached
            return Ok(Vec::new());
        }
        debug!("Scanning {}", dent.path().display());
        let f = fs::File::open(dent.path())?;
        let mmap = Mmap::open(&f, Protection::Read)?;
        let buf: &[u8] = unsafe { mmap.as_slice() };
        if len > self.quickcheck as u64 {
            if twoway::find_bytes(&buf[0..self.quickcheck], b"/nix/store/").is_none() {
                return Ok(vec![]);
            }
        }
        Ok(
            STORE_RE
                .captures_iter(&buf)
                .map(|cap| OsStr::from_bytes(&cap[1]).into())
                .collect(),
        )
    }

    fn scan_symlink(&self, dent: &DirEntry) -> Result<Vec<PathBuf>> {
        debug!("Scanning {}", dent.path().display());
        let target = fs::read_link(dent.path())?;
        if target.starts_with("/nix/store/") {
            Ok(vec![target.into()])
        } else {
            Ok(vec![])
        }
    }

    fn scan(&self, dent: &DirEntry) -> Result<Vec<PathBuf>> {
        match dent.error() {
            Some(ref e) if !e.is_partial() => return Ok(vec![]),
            _ => (),
        }
        match dent.file_type() {
            Some(ft) if ft.is_file() => {
                self.scan_regular(dent).chain_err(|| {
                    format!("{}", dent.path().display())
                })
            }
            Some(ft) if ft.is_symlink() => {
                self.scan_symlink(dent).chain_err(|| {
                    format!("{}", dent.path().display())
                })
            }
            _ => Err(ErrorKind::WalkContinue.into()),
        }
    }

    pub fn find_paths(&self, dent: DirEntry) -> Result<StorePaths> {
        let refs = self.scan(&dent).map(|mut paths| {
            paths.sort();
            paths.dedup();
            paths
        })?;
        Ok(StorePaths {
            dent: dent,
            refs: refs,
        })
    }
}
