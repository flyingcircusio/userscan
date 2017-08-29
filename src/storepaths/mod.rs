use errors::*;
use ignore::{self, DirEntry};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

mod cacheline;
mod cache;
pub use self::cache::Cache;

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
