use colored::Colorize;
use errors::*;
use ignore::{self, WalkBuilder, DirEntry};
use output::p2s;
use scan::StorePaths;
use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix;
use std::os::unix::prelude::*;
use std::result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

pub type GcRootsTx = mpsc::Sender<Arc<StorePaths>>;
pub type GcRootsRx = mpsc::Receiver<Arc<StorePaths>>;

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cleanup {
    registered: usize,
    cleaned: usize,
}

fn extract_hash<'a>(path: &'a Path) -> &'a [u8] {
    &path.strip_prefix("/nix/store/")
        .unwrap_or(path)
        .as_os_str()
        .as_bytes()
        [..32]
}

#[derive(Debug, Clone, Default)]
pub struct GCRoots {
    prefix: PathBuf,
    topdir: PathBuf,
    cwd: PathBuf,
    seen: BTreeSet<PathBuf>,
}

impl GCRoots {
    pub fn new(prefix: &Path, startdir: &Path) -> Result<Self> {
        let cwd = env::current_dir().chain_err(
            || "failed to determine current dir",
        )?;
        let startdir = startdir.canonicalize().chain_err(|| {
            format!("start dir {} does not appear to exist", p2s(startdir))
        })?;
        Ok(GCRoots {
            prefix: prefix.to_path_buf(),
            topdir: prefix.join(startdir.strip_prefix("/").unwrap()),
            cwd: cwd,
            ..GCRoots::default()
        })
    }

    fn gc_link_dir(&self, scanned: &Path) -> PathBuf {
        let base = scanned.parent().unwrap_or(Path::new("."));
        self.prefix.join(
            self.cwd.join(base).strip_prefix("/").unwrap(),
        )
    }

    fn create_link(&mut self, dir: &Path, target: &Path) -> Result<usize> {
        let linkname = dir.join(&OsStr::from_bytes(extract_hash(target)));
        match fs::read_link(&linkname) {
            Ok(ref p) => {
                if *p == *target {
                    self.seen.insert(linkname);
                    return Ok(0);
                } else {
                    fs::remove_file(&linkname).chain_err(|| {
                        format!("cannot remove {}", linkname.display())
                    })?
                }
            }
            Err(e) => {
                match e.kind() {
                    io::ErrorKind::NotFound => (),
                    _ => {
                        return Err(e).chain_err(|| {
                            format!("failed to evaluate existing link {}", linkname.display())
                        })
                    }
                }
            }
        }
        debug!("Linking {}", linkname.display());
        unix::fs::symlink(target, &linkname).chain_err(|| {
            format!("failed to create symlink {}", linkname.display())
        })?;
        self.seen.insert(linkname);
        Ok(1)
    }

    fn register(&mut self, sp: &StorePaths) -> Result<usize> {
        let dir = self.gc_link_dir(sp.path());
        fs::create_dir_all(&dir).chain_err(|| {
            format!("failed to create GC roots dir {}", dir.display())
        })?;
        sp.iter_refs().map(|p| self.create_link(&dir, p)).sum()
    }

    fn cleanup(&self) -> Result<usize> {
        WalkBuilder::new(&self.topdir)
            .parents(false)
            .ignore(false)
            .git_global(false)
            .git_ignore(false)
            .git_exclude(false)
            .build()
            .map(|res: result::Result<DirEntry, ignore::Error>| {
                let dent = res?;
                let path = dent.path();
                let meta = dent.metadata().chain_err(
                    || format!("stat({}) failed", path.display()),
                )?;
                match meta.file_type() {
                    ft if ft.is_dir() => {
                        for _removed in fs::remove_dir(path) {
                            debug!("Removing empty dir {}", path.display())
                        }
                        Ok(0)
                    }
                    ft if ft.is_symlink() => {
                        if self.seen.contains(path) {
                            Ok(0)
                        } else {
                            fs::remove_file(path)?;
                            debug!("Unlinking {}", path.display());
                            Ok(1)
                        }
                    }
                    _ => Ok(0),
                }
            })
            .sum()
    }
}

pub trait Register {
    fn register_loop(&mut self, rx: GcRootsRx) -> Result<Cleanup>;
}

impl Register for GCRoots {
    fn register_loop(&mut self, rx: GcRootsRx) -> Result<Cleanup> {
        info!("Registering refs in {}", p2s(&self.topdir));
        let registered = rx.iter()
            .map(|sp| self.register(&sp))
            .sum::<Result<usize>>()?;
        let cleaned = self.cleanup()?;
        info!(
            "Registered {} store ref(s), cleaned {} store ref(s)",
            registered.to_string().green(),
            cleaned.to_string().cyan()
        );
        Ok(Cleanup {
            registered: registered,
            cleaned: cleaned,
        })
    }
}

pub struct NullGCRoots;

impl Register for NullGCRoots {
    fn register_loop(&mut self, rx: GcRootsRx) -> Result<Cleanup> {
        for _ in rx {
            () // consume the channel
        }
        Ok(Cleanup::default())
    }
}
