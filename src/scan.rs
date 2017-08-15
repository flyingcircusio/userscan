extern crate memmap;
extern crate regex;
extern crate twoway;

use cache::StorePaths;
use errors::*;
use ignore::DirEntry;
use self::memmap::{Mmap, Protection};
use self::regex::bytes::Regex;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::prelude::*;
use std::path::PathBuf;

lazy_static! {
    static ref STORE_RE: Regex = Regex::new(
        r"(?-u)(/nix/store/[0-9a-z]{32}-[0-9a-zA-Z+._?=-]+)").unwrap();
}

const MIN_STOREREF_LEN: u64 = 45;


#[derive(Debug, Clone, Default)]
pub struct Scanner {
    quickcheck: usize,
}

impl Scanner {
    pub fn new(quickcheck: usize) -> Self {
        Scanner { quickcheck: quickcheck }
    }

    fn scan_regular(&self, dent: &DirEntry) -> Result<(Vec<PathBuf>, u64)> {
        let len = dent.metadata()?.len();
        if len < MIN_STOREREF_LEN {
            // minimum length to fit a single store reference not reached
            return Ok((vec![], len));
        }
        debug!("scanning {}", dent.path().display());
        let mmap = Mmap::open_path(dent.path(), Protection::Read)?;
        let buf: &[u8] = unsafe { mmap.as_slice() };
        let cutoff = self.quickcheck as u64;
        if cutoff > 0 && len > cutoff {
            if twoway::find_bytes(&buf[0..self.quickcheck], b"/nix/store/").is_none() {
                return Ok((vec![], cutoff));
            }
        }
        Ok((
            STORE_RE
                .find_iter(&buf)
                .map(|match_| OsStr::from_bytes(match_.as_bytes()).into())
                .collect(),
            len,
        ))
    }

    fn scan_symlink(&self, dent: &DirEntry) -> Result<(Vec<PathBuf>, u64)> {
        debug!("scanning {}", dent.path().display());
        let target = fs::read_link(dent.path())?;
        let len = target.as_os_str().len() as u64;
        if let Some(match_) = STORE_RE.find(&target.as_os_str().as_bytes()) {
            Ok((vec![OsStr::from_bytes(match_.as_bytes()).into()], len))
        } else {
            Ok((vec![], len))
        }
    }

    fn scan(&self, dent: &DirEntry) -> Result<(Vec<PathBuf>, u64)> {
        match dent.error() {
            Some(ref e) if !e.is_partial() => return Ok((vec![], 0)),
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
        self.scan(&dent).map(|res| {
            let (mut paths, bytes_scanned) = res;
            paths.sort();
            paths.dedup();
            StorePaths::new(dent, paths, bytes_scanned)
        })
    }
}

#[cfg(test)]
mod tests {
    use tests::{assert_eq_vecs, dent};
    use super::*;

    #[test]
    fn should_not_look_further_than_quickcheck() {
        let mut scanner = Scanner::default();
        assert_eq_vecs(
            scanner
                .find_paths(dent("dir2/lftp.offset"))
                .unwrap()
                .refs()
                .to_vec(),
            |path| path.to_string_lossy().into_owned(),
            &["/nix/store/q3wx1gab2ysnk5nyvyyg56ana2v4r2ar-glibc-2.24"],
        );
        scanner.quickcheck = 4096;
        assert_eq_vecs(
            scanner
                .find_paths(dent("dir2/lftp.offset"))
                .unwrap()
                .refs()
                .to_vec(),
            |path| path.to_string_lossy().into_owned(),
            &[],
        );
    }
}
