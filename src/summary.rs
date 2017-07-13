use bytesize::ByteSize;
use errors::*;
use scan::{StorePaths, ScanResult};
use std::fmt;
use std::iter::FromIterator;

#[derive(Debug, Clone, PartialOrd)]
pub struct Summary {
    files: usize,
    read: ByteSize,
    refs: usize,
}

impl FromIterator<StorePaths> for Summary {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = ScanResult>,
    {
        let mut res = Summary {
            nfiles: 0,
            read: ByteSize::b(0),
            nrefs: 0,
            nonfatal: 0,
        };
        for sp in iter {
            res.nrefs += sp.len();
            if let Ok(meta) = sp.dent.metadata() {
                res.read = res.read + ByteSize::b(meta.len() as usize);
            }
        }
        res
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} files processed, {} warnings, {} read, {} store references",
            self.nfiles,
            self.nwarn,
            self.read,
            self.nrefs
        )
    }
}
