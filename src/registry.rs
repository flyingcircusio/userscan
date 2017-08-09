use cache::StorePaths;
use colored::Colorize;
use errors::*;
use ignore::{self, WalkBuilder, DirEntry};
use output::{Output, p2s};
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
    output: Output,
}

impl GCRoots {
    pub fn new(prefix: &Path, startdir: &Path, output: Output) -> Result<Self> {
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
            output: output,
            ..GCRoots::default()
        })
    }

    fn gc_link_dir<P: AsRef<Path>>(&self, scanned: P) -> PathBuf {
        let dir = scanned.as_ref().parent().unwrap_or(Path::new("."));
        self.prefix.join(
            self.cwd.join(dir).strip_prefix("/").unwrap(),
        )
    }

    fn create_link(&mut self, dir: &Path, linkname: PathBuf, target: &Path) -> Result<usize> {
        debug!("Linking {}", linkname.display());
        fs::create_dir_all(dir).chain_err(|| {
            format!("failed to create GC dir {}", dir.display())
        })?;
        unix::fs::symlink(target, &linkname).chain_err(|| {
            format!("failed to create symlink {}", linkname.display())
        })?;
        self.seen.insert(linkname);
        Ok(1)
    }

    fn link<P: AsRef<Path>, T: AsRef<Path>>(&mut self, dir: P, target: T) -> Result<usize> {
        let target = target.as_ref();
        let linkname = dir.as_ref().join(&OsStr::from_bytes(extract_hash(target)));
        if self.seen.contains(&linkname) {
            return Ok(0);
        }
        match fs::read_link(&linkname) {
            Ok(ref p) => {
                if *p == *target {
                    self.seen.insert(linkname);
                    Ok(0)
                } else {
                    fs::remove_file(&linkname).chain_err(|| {
                        format!("cannot remove {}", linkname.display())
                    })?;
                    self.create_link(dir.as_ref(), linkname, target)
                }
            }
            Err(e) => {
                match e.kind() {
                    io::ErrorKind::NotFound => self.create_link(dir.as_ref(), linkname, target),
                    _ => {
                        Err(e).chain_err(|| {
                            format!("failed to evaluate existing link {}", linkname.display())
                        })
                    }
                }
            }
        }
    }

    fn register(&mut self, sp: &StorePaths) -> Result<usize> {
        self.output.print_store_paths(&sp)?;
        let dir = self.gc_link_dir(sp.path());
        sp.iter_refs().map(|p| self.link(dir.as_path(), p)).sum()
    }

    fn cleanup(&self) -> Result<usize> {
        if !self.topdir.exists() {
            return Ok(0);
        }
        WalkBuilder::new(&self.topdir)
            .parents(false)
            .ignore(false)
            .git_global(false)
            .git_ignore(false)
            .git_exclude(false)
            .build()
            .map(|res: result::Result<DirEntry, ignore::Error>| {
                let dent = res.chain_err(|| "clean up".to_owned())?;
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
                            debug!("Unlinking {}", path.display());
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
    use tempdir::TempDir;
    use super::*;

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

    fn is_symlink(p: &Path) -> bool {
        fs::symlink_metadata(p)
            .expect(&format!("symlink {} does not exist", p.display()))
            .file_type()
            .is_symlink()
    }

    #[test]
    fn nonexistent_startdir_should_fail() {
        assert!(
            GCRoots::new(
                &Path::new("/nix/var/nix/gcroots"),
                &env::current_dir().unwrap().join("27866/24235/20772"),
                Output::default(),
            ).is_err()
        )
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
    fn create_link() {
        let (td, mut gc) = _gcroots();
        let storepath = Path::new("/nix/store/gmy86w4020xzjw9s8qzzz0bgx8ldkhhk-e");
        let expected = td.path().join("gmy86w4020xzjw9s8qzzz0bgx8ldkhhk");
        assert_eq!(gc.link(td.path(), storepath).unwrap(), 1);
        assert!(is_symlink(&expected));
        assert!(gc.seen.contains(&expected));
        // second attempt: do nothing
        assert_eq!(gc.link(td.path(), storepath).unwrap(), 0);
    }

    #[test]
    fn create_link_creates_dir() {
        let (td, mut gc) = _gcroots();
        assert!(fs::metadata(td.path().join("d1")).is_err());
        assert_eq!(
            gc.link(
                td.path().join("d1"),
                "/nix/store/gmy86w4020xzjw9s8qzzz0bgx8ldkhhk-e",
            ).unwrap(),
            1
        );
        assert!(
            fs::metadata(td.path().join("d1"))
                .expect("dir d1 not created")
                .is_dir()
        );
    }

    #[test]
    fn create_link_corrects_existing_link() {
        let (td, mut gc) = _gcroots();
        let link = td.path().join("f0vdg3cb0005ksjb0fd5qs6f56zg2qs5");
        unix::fs::symlink("changeme", &link).unwrap();
        let _ = gc.link(td.path(), "/nix/store/f0vdg3cb0005ksjb0fd5qs6f56zg2qs5-v");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            PathBuf::from("/nix/store/f0vdg3cb0005ksjb0fd5qs6f56zg2qs5-v")
        );
    }

    #[test]
    fn cleanup_nonexistent_dir_should_succeed() {
        let (td, mut gc) = _gcroots();
        gc.topdir = td.path().join("no/such/dir");
        assert_eq!(gc.cleanup().expect("unexpected cleanup failure"), 0);
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
