extern crate regex;
extern crate memmap;

use bytesize::ByteSize;
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

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.refs.len()
    }

    #[allow(dead_code)]
    pub fn is_err(&self) -> bool {
        self.dent.error().is_some()
    }

    pub fn error(&self) -> Option<&ignore::Error> {
        self.dent.error()
    }

    #[allow(dead_code)]
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

fn scan_regular(dent: &DirEntry) -> Result<Vec<PathBuf>> {
    if dent.metadata()?.len() < 45 {
        // minimum length to fit a single store reference not reached
        return Ok(Vec::new());
    }
    debug!("Scanning {}", dent.path().display());
    let mmap = Mmap::open_path(dent.path(), Protection::Read)?;
    let buf: &[u8] = unsafe { mmap.as_slice() };
    Ok(
        STORE_RE
            .captures_iter(&buf)
            .map(|cap| OsStr::from_bytes(&cap[1]).into())
            .collect(),
    )
}

fn scan_symlink(dent: &DirEntry) -> Result<Vec<PathBuf>> {
    let target = fs::read_link(dent.path())?;
    debug!("Scanning {}", dent.path().display());
    if let Some(cap) = STORE_RE.captures(target.as_os_str().as_bytes()) {
        Ok(vec![OsStr::from_bytes(&cap[1]).into()])
    } else {
        Ok(vec![])
    }
}

fn scan(dent: &DirEntry) -> Result<Vec<PathBuf>> {
    match dent.error() {
        Some(ref e) if !e.is_partial() => return Ok(vec![]),
        _ => (),
    }
    match dent.file_type() {
        Some(ft) if ft.is_file() => scan_regular(dent),
        Some(ft) if ft.is_symlink() => scan_symlink(dent),
        _ => Ok(vec![]),
    }.chain_err(|| format!("{}", dent.path().display()))
}

#[derive(Debug, Clone)]
pub struct Scanner {
    give_up: ByteSize,
}

impl Scanner {
    pub fn new(give_up: ByteSize) -> Self {
        Scanner { give_up: give_up }
    }

    pub fn find_paths(&self, dent: DirEntry) -> Result<StorePaths> {
        let refs = scan(&dent).map(|mut paths| {
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
