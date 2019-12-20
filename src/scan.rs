use crate::errors::*;
use crate::output::p2s;
use crate::storepaths::StorePaths;

use anyhow::Context;
use anyhow::Result as AResult;
use bytesize::ByteSize;
use ignore::overrides::Override;
use ignore::{DirEntry, Match};
use memmap::Mmap;
use regex::bytes::Regex;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::prelude::*;
use std::path::PathBuf;
use zip::read::ZipArchive;

lazy_static! {
    static ref STORE_RE: Regex =
        Regex::new(r"(?-u)/nix/store/([0-9a-z]{32}-[0-9a-zA-Z+._?=-]+)").unwrap();
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
    quickcheck: ByteSize,
    /// Unzips files matched by the given globs and scans inside.
    unzip: Override,
}

impl Default for Scanner {
    fn default() -> Self {
        Scanner {
            quickcheck: ByteSize::b(0),
            unzip: Override::empty(),
        }
    }
}

/// Scans a regular file.
///
/// Only the first `quickcheck` bytes are considered. The whole file is read if `quickcheck` is 0.
fn scan_regular_quickcheck(
    dent: &DirEntry,
    meta: fs::Metadata,
    quickcheck: u64,
) -> AResult<ScanResult> {
    debug!("Scanning {}", dent.path().display());
    let mmap = unsafe { Mmap::map(&fs::File::open(dent.path())?)? };
    if quickcheck > 0
        && meta.len() > quickcheck
        && twoway::find_bytes(&mmap[0..(quickcheck as usize)], b"/nix/store/").is_none()
    {
        return Ok(ScanResult {
            refs: vec![],
            meta,
            bytes_scanned: quickcheck,
        });
    }
    let bytes_scanned = meta.len();
    Ok(ScanResult {
        refs: STORE_RE
            .captures_iter(&mmap)
            .map(|cap| OsStr::from_bytes(&cap[1]).into())
            .collect(),
        meta,
        bytes_scanned,
    })
}

fn scan_regular(dent: &DirEntry, quickcheck: ByteSize) -> AResult<ScanResult> {
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
        scan_regular_quickcheck(dent, meta, quickcheck.as_u64())
    }
}

/// Unpacks a ZIP archive on the fly and scans its contents.
fn scan_zip_archive(dent: &DirEntry) -> AResult<ScanResult> {
    debug!("Scanning ZIP archive {}", dent.path().display());
    let meta = dent.metadata()?;
    let mut archive = match ZipArchive::new(fs::File::open(&dent.path())?) {
        Ok(a) => a,
        Err(e) => return Err(UErr::ZIP(dent.path().to_owned(), e).into()),
    };
    let mut buf = Vec::new();
    let mut refs = Vec::new();
    if archive.len() > 1000 || meta.len() > 2 << 20 {
        warn!(
            "{}: unpacking large ZIP archives may be slow",
            p2s(dent.path())
        );
    }
    for i in 0..archive.len() {
        let mut f = archive
            .by_index(i)
            .map_err(|e| UErr::ZIP(dent.path().to_owned(), e))?;
        f.read_to_end(&mut buf)?;
        refs.extend(
            STORE_RE
                .captures_iter(&buf)
                .map(|cap| OsStr::from_bytes(&cap[1]).into()),
        );
    }
    let bytes_scanned = meta.len();
    Ok(ScanResult {
        refs,
        meta,
        bytes_scanned,
    })
}

/// Scans the symlink's target name (i.e., readlink() output).
fn scan_symlink(dent: &DirEntry) -> AResult<ScanResult> {
    debug!("Scanning link {}", dent.path().display());
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
    pub fn new(quickcheck: ByteSize, unzip: Override) -> Self {
        Scanner { quickcheck, unzip }
    }

    /// Scans a thing that has a file type.
    ///
    /// Returns Some(result) if a scan strategy was found, None otherwise.
    fn scan_inode(&self, dent: &DirEntry, ft: fs::FileType) -> Option<AResult<ScanResult>> {
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

    /// Decodes the DirEntry and scans it if feasible.
    fn scan(&self, dent: &DirEntry) -> AResult<ScanResult> {
        match dent.error() {
            Some(e) if !e.is_partial() => {
                return Err(e.clone())
                    .with_context(|| format!("{}: metadata error", p2s(dent.path())))
            }
            _ => (),
        }
        if let Some(ft) = dent.file_type() {
            if let Some(res) = self.scan_inode(dent, ft) {
                return res.with_context(|| format!("{}: scan failed", p2s(dent.path())));
            }
        }
        // fall-through: no idea how to handle this DirEntry
        Err(UErr::FiletypeUnknown(dent.path().to_owned()).into())
    }

    /// Scans `dent` and transforms results into a StorePaths object.
    pub fn find_paths(&self, dent: DirEntry) -> AResult<StorePaths> {
        self.scan(&dent).map(|mut r| {
            r.refs.sort();
            r.refs.dedup();
            StorePaths::new(dent, r.refs, r.bytes_scanned, Some(r.meta))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{assert_eq_vecs, dent, FIXTURES};
    use ignore::overrides::OverrideBuilder;
    use std::path::Path;

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
        scanner.quickcheck = ByteSize::kib(4);
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
        let sp = Scanner::new(ByteSize::default(), unzip)
            .find_paths(dent("miniegg-1-py3.5.egg"))
            .unwrap();
        assert_eq!(
            vec![Path::new("76lhp1gvc3wbl6q4p2qgn2n7245imyvr-perl-5.22.3")],
            *sp.refs()
        );
        assert_eq!(2226, sp.bytes_scanned());
    }
}
