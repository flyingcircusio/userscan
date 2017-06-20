use errors::*;
use scan::StorePaths;
use std::collections::BTreeSet;
use std::ffi::{OsString, OsStr};
use std::fs;
use std::io;
use std::os::unix;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

static PROFILES: &str = "/nix/var/nix/gcroots/profiles/per-user";

pub type GcRootsChan = mpsc::Sender<Arc<StorePaths>>;

#[derive(Debug, Clone)]
pub struct GCRoots {
    base: PathBuf,
    seen: BTreeSet<OsString>,
    verbose: bool,
}

fn extract_hash<'a>(path: &'a Path) -> &'a [u8] {
    &path.strip_prefix("/nix/store/").unwrap_or(path).as_os_str().as_bytes()[..32]
}

impl GCRoots {
    pub fn new<P: AsRef<Path>>(username: &str, startdir: P, verbose: bool) -> Result<Self> {
        let startdir_abs = startdir.as_ref().canonicalize()?;
        let startdir_rel = startdir_abs.strip_prefix("/")?;
        let basedir = PathBuf::from(PROFILES).join(username).join(startdir_rel);
        fs::create_dir_all(&basedir)?;
        Ok(GCRoots {
            base: basedir,
            seen: BTreeSet::new(),
            verbose: verbose,
        })
    }

    fn create_link(&self, path: &Path) -> Result<()> {
        let linkname = self.base.join(&OsStr::from_bytes(extract_hash(path)));
        let lndisp = linkname.display();
        match fs::read_link(&linkname) {
            Ok(ref p) => {
                if *p == *path {
                    return Ok(())
                } else {
                    fs::remove_file(&linkname).chain_err(|| format!("cannot remove {}", lndisp))?
                }
            },
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound => (),
                _ => return Err(e)
                    .chain_err(|| format!("failed to evaluate existing link {}", lndisp)),
            }
        }
        if self.verbose {
            eprintln!("* {}", path.display());
        }
        unix::fs::symlink(path, &linkname)
            .chain_err(|| format!("failed to create symlink {}", lndisp))
    }

    pub fn register(&mut self, sp: &StorePaths) -> Result<()> {
        if self.verbose {
            eprintln!("registering store paths in GC root {}", self.base.display())
        }
        for p in sp.iter_refs() {
            if self.seen.insert(p.into()) {
                self.create_link(p)?
            }
        }
        Ok(())
    }
}
