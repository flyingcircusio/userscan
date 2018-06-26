use super::STORE;
use colored::Colorize;
use errors::*;
use ignore::{self, DirEntry, WalkBuilder};
use output::{p2s, Output};
use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use std::result;
use std::sync::mpsc;
use storepaths::StorePaths;
use users::get_effective_username;

pub type GCRootsTx = mpsc::Sender<StorePaths>;
pub type GCRootsRx = mpsc::Receiver<StorePaths>;

fn extract_hash(path: &Path) -> &[u8] {
    &path.as_os_str().as_bytes()[..32]
}

#[derive(Debug, Default)]
pub struct GCRoots {
    prefix: PathBuf, // /nix/var/nix/gcroots/profiles/per-user/$USER
    topdir: PathBuf, // e.g., $PREFIX/srv/www if /srv/www was scanned
    cwd: PathBuf,    // current dir when the scan was started
    seen: HashSet<PathBuf>,
    output: Output,
    rx: Option<GCRootsRx>,
    registered: usize,
}

/// IPC endpoint for garbage collection roots registry
pub trait Register {
    fn register_loop(&mut self) -> Result<()>;

    fn clean(&self) -> Result<()> {
        Ok(())
    }

    fn tx(&mut self) -> GCRootsTx;
}

impl GCRoots {
    pub fn new<P: AsRef<Path>>(peruser: &str, startdir: P, output: &Output) -> Result<Self> {
        let user = match get_effective_username() {
            Some(u) => u,
            None => return Err("failed to query current user name".into()),
        };
        let prefix = Path::new(peruser).join(&user);
        let cwd = env::current_dir().chain_err(|| "failed to determine current dir")?;
        Ok(GCRoots {
            topdir: prefix.join(startdir.as_ref().strip_prefix("/")?),
            prefix,
            cwd,
            output: output.to_owned(),
            ..GCRoots::default()
        })
    }

    /// Determines exactly where a GC link should live.
    fn gc_link_dir<P: AsRef<Path>>(&self, scanned: P) -> PathBuf {
        let dir = scanned.as_ref().parent().unwrap_or_else(|| Path::new("."));
        self.prefix
            .join(self.cwd.join(dir).strip_prefix("/").unwrap())
    }

    fn create_link(&mut self, dir: &Path, linkname: PathBuf, target: &Path) -> Result<usize> {
        info!("creating link {}", p2s(&linkname));
        fs::create_dir_all(dir).chain_err(|| format!("failed to create GC dir {}", dir.display()))?;
        unix::fs::symlink(target, &linkname)
            .chain_err(|| format!("failed to create symlink {}", linkname.display()))?;
        self.seen.insert(linkname);
        Ok(1)
    }

    /// Creates or updates a single GC link.
    ///
    /// `target` is assumed to be without leading `/nix/store/` prefix.
    fn link<P: AsRef<Path>, T: AsRef<Path>>(&mut self, dir: P, target: T) -> Result<usize> {
        let linkname = dir.as_ref()
            .join(&OsStr::from_bytes(extract_hash(target.as_ref())));
        let target = Path::new(STORE).join(target);
        if self.seen.contains(&linkname) {
            return Ok(0);
        }
        match fs::read_link(&linkname) {
            Ok(ref p) => {
                if *p == *target {
                    self.seen.insert(linkname);
                    Ok(0)
                } else {
                    fs::remove_file(&linkname)
                        .chain_err(|| format!("cannot remove {}", linkname.display()))?;
                    self.create_link(dir.as_ref(), linkname, &target)
                }
            }
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound => self.create_link(dir.as_ref(), linkname, &target),
                _ => Err(e).chain_err(|| {
                    format!("failed to evaluate existing link {}", linkname.display())
                }),
            },
        }
    }

    /// Registers all Nix store paths with the garbage collector.
    fn register(&mut self, sp: &StorePaths) -> Result<usize> {
        self.output.print_store_paths(sp)?;
        let dir = self.gc_link_dir(sp.path());
        sp.iter_refs().map(|p| self.link(dir.as_path(), p)).sum()
    }

    fn cleanup(&self) -> Result<usize> {
        if !self.topdir.exists() {
            return Ok(0);
        }
        WalkBuilder::new(&self.topdir)
            .git_exclude(false)
            .git_global(false)
            .git_ignore(false)
            .hidden(false)
            .ignore(false)
            .parents(false)
            .build()
            .map(|res: result::Result<DirEntry, ignore::Error>| {
                let dent = res.chain_err(|| "clean up".to_owned())?;
                let path = dent.path();
                match dent.file_type() {
                    Some(ft) if ft.is_dir() => {
                        if fs::remove_dir(path).is_ok() {
                            debug!("removing empty dir {}", path.display())
                        }
                        Ok(0)
                    }
                    Some(ft) if ft.is_symlink() => {
                        if self.seen.contains(path) {
                            Ok(0)
                        } else {
                            info!("removing link {}", p2s(&path));
                            fs::remove_file(path)?;
                            Ok(1)
                        }
                    }
                    _ => Ok(0),
                }
            })
            .sum()
    }
}

impl Register for GCRoots {
    fn register_loop(&mut self) -> Result<()> {
        match self.rx.take() {
            Some(rx) => {
                self.registered = rx.iter()
                    .map(|sp| self.register(&sp))
                    .sum::<Result<usize>>()?;
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn clean(&self) -> Result<()> {
        let cleaned = self.cleanup()?;
        info!(
            "{} references in {}",
            self.seen.len().to_string().cyan(),
            p2s(&self.topdir)
        );
        if self.registered > 0 || cleaned > 0 {
            info!(
                "newly registered: {}, cleaned: {}",
                self.registered.to_string().green(),
                cleaned.to_string().purple()
            );
        }
        Ok(())
    }

    fn tx(&mut self) -> GCRootsTx {
        let (tx, rx) = mpsc::channel::<StorePaths>();
        self.rx = Some(rx);
        tx
    }
}

#[derive(Debug, Default)]
pub struct NullGCRoots {
    output: Output,
    rx: Option<GCRootsRx>,
}

impl NullGCRoots {
    pub fn new(output: &Output) -> Self {
        NullGCRoots {
            output: output.clone(),
            rx: None,
        }
    }
}

impl Register for NullGCRoots {
    fn register_loop(&mut self) -> Result<()> {
        match self.rx.take() {
            Some(ref rx) => {
                for storepaths in rx {
                    self.output.print_store_paths(&storepaths)?;
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn tx(&mut self) -> GCRootsTx {
        let (tx, rx) = mpsc::channel::<StorePaths>();
        self.rx = Some(rx);
        tx
    }
}

#[cfg(test)]
pub mod tests {
    extern crate tempdir;

    use self::tempdir::TempDir;
    use super::*;

    fn _gcroots() -> (TempDir, GCRoots) {
        let tempdir = TempDir::new("gcroots").expect("failed to create gcroots tempdir");
        let mut gc = GCRoots::new("/", Path::new("/"), &Output::default()).unwrap();
        gc.prefix = tempdir.path().to_owned();
        gc.topdir = PathBuf::from("/home/user/www");
        gc.cwd = PathBuf::from("/home/user");
        (tempdir, gc)
    }

    fn is_symlink(p: &Path) -> bool {
        fs::symlink_metadata(p)
            .expect(&format!("symlink {} does not exist", p.display()))
            .file_type()
            .is_symlink()
    }

    #[test]
    fn linkdir() {
        let (td, gc) = _gcroots();
        assert_eq!(td.path().join("home/user"), gc.gc_link_dir("file2"));
        assert_eq!(
            td.path().join("home/user/www/d"),
            gc.gc_link_dir("/home/user/www/d/file1")
        );
        assert_eq!(td.path().join("home/user/rel"), gc.gc_link_dir("rel/file3"));
    }

    #[test]
    fn should_create_link() {
        let (td, mut gc) = _gcroots();
        let storepath = Path::new("gmy86w4020xzjw9s8qzzz0bgx8ldkhhk-e34kjk");
        let expected = td.path().join("gmy86w4020xzjw9s8qzzz0bgx8ldkhhk");
        assert_eq!(gc.link(td.path(), storepath).expect("link 1 failed"), 1);
        assert!(is_symlink(&expected));
        assert!(gc.seen.contains(&expected));
        // second attempt: do nothing
        assert_eq!(gc.link(td.path(), storepath).expect("link 2 failed"), 0);
    }

    #[test]
    fn create_link_should_create_dir() {
        let (td, mut gc) = _gcroots();
        assert!(fs::metadata(td.path().join("d1")).is_err());
        assert_eq!(
            gc.link(td.path().join("d1"), "gmy86w4020xzjw9s8qzzz0bgx8ldkhhk-e")
                .unwrap(),
            1
        );
        assert!(
            fs::metadata(td.path().join("d1"))
                .expect("dir d1 not created")
                .is_dir()
        );
    }

    #[test]
    fn create_link_should_correct_existing_link() {
        let (td, mut gc) = _gcroots();
        let link = td.path().join("f0vdg3cb0005ksjb0fd5qs6f56zg2qs5");
        unix::fs::symlink("changeme", &link).unwrap();
        let _ = gc.link(td.path(), "f0vdg3cb0005ksjb0fd5qs6f56zg2qs5-v");
        assert_eq!(
            PathBuf::from("/nix/store/f0vdg3cb0005ksjb0fd5qs6f56zg2qs5-v"),
            fs::read_link(&link).unwrap()
        );
    }

    #[test]
    fn cleanup_nonexistent_dir_should_succeed() {
        let (td, mut gc) = _gcroots();
        gc.topdir = td.path().join("no/such/dir");
        assert_eq!(gc.cleanup().expect("unexpected cleanup failure"), 0);
    }

    /*
     * passive GCRoots consumer to test walker/scanner
     */

    #[derive(Debug)]
    pub struct FakeGCRoots {
        pub registered: Vec<String>,
        prefix: PathBuf,
        rx: Option<GCRootsRx>,
    }

    impl FakeGCRoots {
        pub fn new(reldir: &Path) -> Self {
            FakeGCRoots {
                registered: Vec::new(),
                prefix: reldir.canonicalize().unwrap(),
                rx: None,
            }
        }
    }

    impl Register for FakeGCRoots {
        fn register_loop(&mut self) -> Result<()> {
            match self.rx.take() {
                Some(ref rx) => {
                    for storepaths in rx {
                        for r in storepaths.refs() {
                            let relpath = storepaths.path().strip_prefix(&self.prefix).unwrap();
                            self.registered
                                .push(format!("{}|{}", relpath.display(), r.display()));
                        }
                    }
                    Ok(())
                }
                None => Ok(()),
            }
        }

        fn tx(&mut self) -> GCRootsTx {
            let (tx, rx) = mpsc::channel::<StorePaths>();
            self.rx = Some(rx);
            tx
        }
    }
}
