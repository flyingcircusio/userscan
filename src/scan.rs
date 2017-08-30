extern crate memmap;
extern crate regex;
extern crate twoway;

use errors::*;
use ignore::{DirEntry, Match};
use ignore::overrides::Override;
use output::p2s;
use self::memmap::{Mmap, Protection};
use self::regex::bytes::Regex;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::prelude::*;
use std::path::PathBuf;
use storepaths::StorePaths;
use zip::read::ZipArchive;
use zip::result::ZipError;

lazy_static! {
    static ref STORE_RE: Regex = Regex::new(
        r"(?-u)/nix/store/([0-9a-z]{32}-[0-9a-zA-Z+._?=-]+)").unwrap();
}

const MIN_STOREREF_LEN: u64 = 45;

struct ScanResult {
    refs: Vec<PathBuf>,
    meta: fs::Metadata,
    bytes_scanned: u64,
}

#[derive(Debug, Clone)]
pub struct Scanner {
    /// Skips the rest of a file if there is no Nix store reference in the first QUICKCHECK bytes.
    quickcheck: usize,
    /// Unzips files matched by the given globs and scans inside.
    unzip: Override,
}

impl Default for Scanner {
    fn default() -> Self {
        Scanner {
            quickcheck: 0,
            unzip: Override::empty(),
        }
    }
}

fn scan_regular_quickcheck(
    dent: &DirEntry,
    meta: fs::Metadata,
    quickcheck: usize,
) -> Result<ScanResult> {
    debug!("scanning {}", dent.path().display());
    let mmap = Mmap::open_path(dent.path(), Protection::Read)?;
    let buf: &[u8] = unsafe { mmap.as_slice() };
    let cutoff = quickcheck as u64;
    if cutoff > 0 && meta.len() > cutoff {
        if twoway::find_bytes(&buf[0..quickcheck], b"/nix/store/").is_none() {
            return Ok(ScanResult {
                refs: vec![],
                meta,
                bytes_scanned: cutoff,
            });
        }
    }
    let bytes_scanned = meta.len();
    Ok(ScanResult {
        refs: STORE_RE
            .captures_iter(&buf)
            .map(|cap| OsStr::from_bytes(&cap[1]).into())
            .collect(),
        meta,
        bytes_scanned,
    })
}

fn scan_regular(dent: &DirEntry, quickcheck: usize) -> Result<ScanResult> {
    let meta = dent.metadata()?;
    if meta.len() < MIN_STOREREF_LEN {
        // minimum length to fit a single store reference not reached
        let bytes_scanned = meta.len();
        Ok(ScanResult {
            refs: vec![],
            meta,
            bytes_scanned,
        })
    } else {
        scan_regular_quickcheck(dent, meta, quickcheck)
    }
}

fn scan_zip_archive(dent: &DirEntry) -> Result<ScanResult> {
    debug!("Scanning ZIP archive {}", dent.path().display());
    let meta = dent.metadata()?;
    let mut archive = match ZipArchive::new(fs::File::open(&dent.path())?) {
        Ok(a) => a,
        Err(ZipError::InvalidArchive(e)) |
        Err(ZipError::UnsupportedArchive(e)) => {
            warn!(
                "{}: failed to unpack ZIP archive: {}",
                dent.path().display(),
                e
            );
            return scan_regular_quickcheck(dent, meta, 0);
        }
        Err(e) => return Err(e.into()),
    };
    let mut buf = Vec::new();
    let mut refs = Vec::new();
    for i in 0..archive.len() {
        let mut f = archive.by_index(i)?;
        f.read_to_end(&mut buf)?;
        refs.extend(STORE_RE.captures_iter(&buf).map(|cap| {
            OsStr::from_bytes(&cap[1]).into()
        }));
    }
    let bytes_scanned = meta.len();
    Ok(ScanResult {
        refs,
        meta,
        bytes_scanned,
    })
}

fn scan_symlink(dent: &DirEntry) -> Result<ScanResult> {
    debug!("scanning {}", dent.path().display());
    let meta = dent.metadata()?;
    let target = fs::read_link(dent.path())?;
    let len = target.as_os_str().len() as u64;
    let refs = match STORE_RE.captures(target.as_os_str().as_bytes()) {
        Some(cap) => vec![OsStr::from_bytes(&cap[1]).into()],
        None => vec![],
    };
    Ok(ScanResult {
        refs,
        meta,
        bytes_scanned: len,
    })
}

impl Scanner {
    pub fn new(quickcheck: usize, unzip: Override) -> Self {
        Scanner { quickcheck, unzip }
    }

    /// Scans a thing that has a file type.
    ///
    /// Returns Some(result) if a scan strategy was found, None otherwise.
    fn scan_inode(&self, dent: &DirEntry, ft: fs::FileType) -> Option<Result<ScanResult>> {
        if ft.is_file() {
            if !self.unzip.is_empty() {
                if let Match::Whitelist(_) = self.unzip.matched(dent.path(), false) {
                    return Some(scan_zip_archive(dent));
                }
            }
            return Some(scan_regular(dent, self.quickcheck));
        }
        if ft.is_symlink() {
            return Some(scan_symlink(dent));
        }
        None
    }

    /// Scans anything. Returns an empty result if the dent is not scannable.
    fn scan(&self, dent: &DirEntry) -> Result<ScanResult> {
        match dent.error() {
            Some(e) if !e.is_partial() => {
                return Err(format!("{}: {}", p2s(dent.path()), e.description()).into())
            }
            _ => (),
        }
        if let Some(ft) = dent.file_type() {
            if let Some(res) = self.scan_inode(dent, ft) {
                return res.chain_err(|| format!("{}", p2s(dent.path())));
            }
        }
        // silent fall-through: no idea how to handle this DirEntry
        Err(ErrorKind::WalkContinue.into())
    }

    pub fn find_paths(&self, dent: DirEntry) -> Result<StorePaths> {
        self.scan(&dent).map(|mut r| {
            r.refs.sort();
            r.refs.dedup();
            StorePaths::new(dent, r.refs, r.bytes_scanned, Some(r.meta))
        })
    }
}


#[cfg(test)]
mod tests {
    use ignore::overrides::OverrideBuilder;
    use std::path::Path;
    use super::*;
    use tests::{assert_eq_vecs, dent, FIXTURES};

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
            &["q3wx1gab2ysnk5nyvyyg56ana2v4r2ar-glibc-2.24"],
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

    #[test]
    fn should_unpack_eggs() {
        let sp = Scanner::default()
            .find_paths(dent("miniegg-1-py3.5.egg"))
            .unwrap();
        assert!(sp.refs().is_empty());

        let unzip = OverrideBuilder::new(&*FIXTURES)
            .add("*.egg")
            .unwrap()
            .build()
            .unwrap();
        let sp = Scanner::new(0, unzip)
            .find_paths(dent("miniegg-1-py3.5.egg"))
            .unwrap();
        assert_eq!(
            vec![Path::new("76lhp1gvc3wbl6q4p2qgn2n7245imyvr-perl-5.22.3")],
            *sp.refs()
        );
        assert_eq!(2226, sp.bytes_scanned());
    }

    #[test]
    fn fallback_to_regular_scan_if_invalid_zip() {
        let unzip = OverrideBuilder::new(&*FIXTURES)
            .add("*")
            .unwrap()
            .build()
            .unwrap();
        let sp = Scanner::new(0, unzip)
            .find_paths(dent("dir2/lftp"))
            .expect("mask ZIP error");
        assert_eq!(
            vec![Path::new("q3wx1gab2ysnk5nyvyyg56ana2v4r2ar-glibc-2.24")],
            *sp.refs()
        );
    }
}
