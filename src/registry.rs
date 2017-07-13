use errors::*;
use scan::StorePaths;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::env;
use std::fs;
use std::io;
use std::os::unix;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

pub type GcRootsChan = mpsc::Sender<Arc<StorePaths>>;

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
    cwd: PathBuf,
    seen: BTreeSet<PathBuf>,
}

impl GCRoots {
    pub fn new(prefix: &Path) -> Result<Self> {
        info!("Registering store paths in GC root {}", prefix.display());
        fs::create_dir_all(&prefix).chain_err(
            || "cannot create GC root",
        )?;
        Ok(GCRoots {
            prefix: prefix.to_path_buf(),
            cwd: env::current_dir().chain_err(
                || "failed to determine current dir",
            )?,
            ..GCRoots::default()
        })
    }

    fn gc_link_dir(&self, scanned: &Path) -> PathBuf {
        let base = scanned.parent().unwrap_or(Path::new("."));
        let dir = self.prefix.join(
            self.cwd.join(base).strip_prefix("/").unwrap(),
        );
        dir
    }

    fn create_link(&self, dir: &Path, target: &Path) -> Result<()> {
        let linkname = dir.join(&OsStr::from_bytes(extract_hash(target)));
        let lndisp = linkname.display();
        match fs::read_link(&linkname) {
            Ok(ref p) => {
                if *p == *target {
                    return Ok(());
                } else {
                    fs::remove_file(&linkname).chain_err(|| {
                        format!("cannot remove {}", lndisp)
                    })?
                }
            }
            Err(e) => {
                match e.kind() {
                    io::ErrorKind::NotFound => (),
                    _ => {
                        return Err(e).chain_err(|| {
                            format!("failed to evaluate existing link {}", lndisp)
                        })
                    }
                }
            }
        }
        eprintln!("* {}", target.display());
        unix::fs::symlink(target, &linkname).chain_err(|| {
            format!("failed to create symlink {}", lndisp)
        })
    }
}

pub trait Register {
    fn register(&mut self, sp: &StorePaths) -> Result<()>;
}

impl Register for GCRoots {
    fn register(&mut self, sp: &StorePaths) -> Result<()> {
        let dir = self.gc_link_dir(sp.path());
        fs::create_dir_all(&dir).chain_err(|| {
            format!("failed to create GC roots dir {}", dir.display())
        })?;
        for p in sp.iter_refs() {
            if self.seen.insert(p.into()) {
                self.create_link(&dir, p)?
            }
        }
        Ok(())
    }
}

pub struct NullGCRoots;

impl Register for NullGCRoots {
    #[inline]
    fn register(&mut self, _: &StorePaths) -> Result<()> {
        Ok(())
    }
}
