use colored::Colorize;
use errors::*;
use ignore::{self, WalkBuilder, DirEntry};
use output::p2s;
use scan::StorePaths;
use std::collections::HashSet;
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

fn extract_hash<'a>(path: &'a Path) -> &'a [u8] {
    &path.strip_prefix("/nix/store/")
        .unwrap_or(path)
        .as_os_str()
        .as_bytes()
        [..32]
}

#[derive(Debug, Clone, Default)]
pub struct GCRoots {
    prefix: PathBuf, // /nix/var/nix/gcroots/profiles/per-user/$USER
    topdir: PathBuf, // e.g., $PREFIX/srv/www if /srv/www was scanned
    cwd: PathBuf, // current dir when the scan was started
    seen: HashSet<PathBuf>,
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

    fn gc_link_dir<P: AsRef<Path>>(&self, scanned: P) -> PathBuf {
        let dir = scanned.as_ref().parent().unwrap_or(Path::new("."));
        self.prefix.join(
            self.cwd.join(dir).strip_prefix("/").unwrap(),
        )
    }

    fn create_link<P: AsRef<Path>, T: AsRef<Path>>(&mut self, dir: P, target: T) -> Result<usize> {
        let target = target.as_ref();
        let linkname = dir.as_ref().join(&OsStr::from_bytes(extract_hash(target)));
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
        fs::create_dir_all(&dir).chain_err(|| {
            format!("failed to create GC dir {}", dir.as_ref().display())
        })?;
        unix::fs::symlink(target, &linkname).chain_err(|| {
            format!("failed to create symlink {}", linkname.display())
        })?;
        self.seen.insert(linkname);
        Ok(1)
    }

    fn register(&mut self, sp: &StorePaths) -> Result<usize> {
        let dir = self.gc_link_dir(sp.path());
        sp.iter_refs()
            .map(|p| self.create_link(dir.as_path(), p))
            .sum()
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
    fn register_loop(&mut self, rx: GcRootsRx) -> Result<()>;
}

impl Register for GCRoots {
    fn register_loop(&mut self, rx: GcRootsRx) -> Result<()> {
        let registered = rx.iter()
            .map(|sp| self.register(&sp))
            .sum::<Result<usize>>()?;
        let cleaned = self.cleanup()?;
        info!(
            "Registered {} store ref(s), cleaned {} store ref(s) in {}",
            registered.to_string().green(),
            cleaned.to_string().cyan(),
            p2s(&self.topdir)
        );
        Ok(())
    }
}

pub struct NullGCRoots;

impl Register for NullGCRoots {
    fn register_loop(&mut self, rx: GcRootsRx) -> Result<()> {
        for _ in rx {
            () // consume the channel
        }
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    extern crate tempdir;

    use self::tempdir::TempDir;
    use std::fs::{metadata, symlink_metadata};
    use super::*;

    #[test]
    fn nonexistent_startdir_should_fail() {
        assert!(
            GCRoots::new(
                &Path::new("/nix/var/nix/gcroots"),
                &env::current_dir().unwrap().join("27866/24235/20772"),
            ).is_err()
        )
    }

    fn _gcroots() -> (TempDir, GCRoots) {
        let tempdir = TempDir::new("gcroots").expect("failed to create gcroots tempdir");
        let prefix = tempdir.path().to_owned();
        (
            tempdir,
            GCRoots {
                prefix: prefix,
                topdir: PathBuf::from("/home/user/www"),
                cwd: PathBuf::from("/home/user"),
                ..GCRoots::default()
            },
        )
    }

    #[test]
    fn gc_link_dir() {
        let (td, gc) = _gcroots();
        assert_eq!(td.path().join("home/user"), gc.gc_link_dir("file2"));
        assert_eq!(
            td.path().join("home/user/www/d"),
            gc.gc_link_dir("/home/user/www/d/file1")
        );
        assert_eq!(td.path().join("home/user/rel"), gc.gc_link_dir("rel/file3"));
    }

    fn is_symlink(p: &Path) -> bool {
        symlink_metadata(p)
            .expect(&format!("symlink {} does not exist", p.display()))
            .file_type()
            .is_symlink()
    }

    #[test]
    fn create_link() {
        let (td, mut gc) = _gcroots();
        assert_eq!(
            gc.create_link(td.path(), "/nix/store/gmy86w4020xzjw9s8qzzz0bgx8ldkhhk-e")
                .unwrap(),
            1
        );
        let exp = td.path().join("gmy86w4020xzjw9s8qzzz0bgx8ldkhhk");
        assert!(is_symlink(&exp));
        assert!(gc.seen.contains(&exp));
    }

    #[test]
    fn create_link_creates_dir() {
        let (td, mut gc) = _gcroots();
        assert!(metadata(td.path().join("d1")).is_err());
        assert_eq!(
            gc.create_link(
                td.path().join("d1"),
                "/nix/store/gmy86w4020xzjw9s8qzzz0bgx8ldkhhk-e",
            ).unwrap(),
            1
        );
        assert!(
            metadata(td.path().join("d1"))
                .expect("dir d1 not created")
                .is_dir()
        );
    }

    /*
     * passive GCRoots dummy to test walker/scanner
     */

    #[derive(Debug, Clone)]
    pub struct FakeGCRoots {
        pub registered: Vec<String>,
    }

    impl FakeGCRoots {
        pub fn new() -> Self {
            FakeGCRoots { registered: Vec::new() }
        }
    }

    impl Register for FakeGCRoots {
        fn register_loop(&mut self, rx: GcRootsRx) -> Result<()> {
            for storepaths in rx {
                for r in storepaths.refs() {
                    self.registered.push(format!(
                        "{}|{}",
                        storepaths.path().display(),
                        r.display()
                    ));
                }
            }
            Ok(())
        }
    }

}
