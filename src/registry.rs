use super::STORE;
use crate::errors::*;
use crate::output::{p2s, Output};
use crate::storepaths::StorePaths;
use crate::system::ExecutionContext;

use colored::Colorize;
use ignore::{self, DirEntry, WalkBuilder};
use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::os::unix::prelude::*;
use std::path::{Path, PathBuf};
use std::result;
use std::sync::mpsc;
use users::get_effective_username;

pub type GCRootsTx = mpsc::Sender<StorePaths>;
pub type GCRootsRx = mpsc::Receiver<StorePaths>;

#[derive(Debug, Default)]
pub struct GCRoots {
    prefix: PathBuf, // /nix/var/nix/gcroots/profiles/per-user/$USER
    topdir: PathBuf, // e.g., $PREFIX/srv/www if /srv/www was scanned
    cwd: PathBuf,    // current dir when the scan was started
    todo: Vec<StorePaths>,
    seen: HashSet<PathBuf>,
    output: Output,
}

/// IPC endpoint for garbage collection roots registry
pub trait Register {
    /// Receives stream of store paths via the `rx` channel. Returns on channel close.
    fn register_loop(&mut self, rx: GCRootsRx);

    /// Creates links for all registered store paths and cleans up unused ones.
    fn commit(&mut self, _ctx: &ExecutionContext) -> Result<()> {
        Ok(())
    }
}

impl GCRoots {
    pub fn new<P: AsRef<Path>>(peruser: &str, startdir: P, output: &Output) -> Result<Self> {
        let user = match get_effective_username() {
            Some(u) => u,
            None => return Err(UErr::WhoAmI),
        };
        let prefix = Path::new(peruser).join(&user);
        let cwd = env::current_dir().map_err(UErr::CWD)?;
        Ok(GCRoots {
            topdir: prefix.join(
                startdir
                    .as_ref()
                    .strip_prefix("/")
                    .map_err(|_| UErr::Relative)?,
            ),
            prefix,
            cwd,
            output: output.to_owned(),
            ..GCRoots::default()
        })
    }
}

impl Register for GCRoots {
    fn register_loop(&mut self, rx: GCRootsRx) {
        for sp in rx {
            self.output.print_store_paths(&sp);
            self.todo.push(sp)
        }
    }

    fn commit(&mut self, ctx: &ExecutionContext) -> Result<()> {
        ctx.with_dropped_privileges(|| {
            let mut worker = RegistryWorker::new(&self.prefix, &self.cwd);
            let cleaned = worker.cleanup(&self.topdir)?;
            let registered = self
                .todo
                .iter()
                .map(|sp| worker.register(sp))
                .sum::<Result<usize>>()?;
            info!(
                "{} references in {}",
                self.seen.len().to_string().cyan(),
                p2s(&self.topdir)
            );
            if registered > 0 || cleaned > 0 {
                info!(
                    "newly registered: {}, cleaned: {}",
                    registered.to_string().green(),
                    cleaned.to_string().purple()
                );
            }
            Ok(())
        })
    }
}

fn extract_hash(path: &Path) -> &[u8] {
    &path.as_os_str().as_bytes()[..32]
}

#[derive(Debug)]
pub struct RegistryWorker<'a> {
    prefix: &'a Path,
    cwd: &'a Path,
    seen: HashSet<PathBuf>,
}

impl<'a> RegistryWorker<'a> {
    /// `prefix` - e.g. /nix/var/nix/gcroots/profiles/per-user/$USER
    /// `cwd` - directory where the scan was started
    fn new(prefix: &'a Path, cwd: &'a Path) -> Self {
        Self {
            prefix,
            cwd,
            seen: HashSet::new(),
        }
    }

    /// Removes dangling symlinks below `topdir`
    fn cleanup(&self, topdir: &Path) -> Result<usize> {
        if !topdir.exists() {
            return Ok(0);
        }
        WalkBuilder::new(topdir)
            .hidden(false)
            .ignore(false)
            .build()
            .map(|res: result::Result<DirEntry, ignore::Error>| {
                let dent = res?;
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

    /// Determines exactly where a GC link should live.
    fn gc_link_dir<P: AsRef<Path>>(&self, scanned: P) -> PathBuf {
        let dir = scanned.as_ref().parent().unwrap_or_else(|| Path::new("."));
        self.prefix
            .join(self.cwd.join(dir).strip_prefix("/").unwrap())
    }

    fn create_link(&mut self, dir: &Path, linkname: PathBuf, target: &Path) -> Result<usize> {
        info!("creating link {}", p2s(&linkname));
        fs::create_dir_all(dir).map_err(|e| UErr::Create(dir.to_owned(), e))?;
        symlink(target, &linkname).map_err(|e| UErr::Create(linkname.to_owned(), e))?;
        self.seen.insert(linkname);
        Ok(1)
    }

    /// Creates or updates a single GC link.
    ///
    /// `target` is assumed to be without leading `/nix/store/` prefix.
    fn link<P: AsRef<Path>, T: AsRef<Path>>(&mut self, dir: P, target: T) -> Result<usize> {
        let linkname = dir
            .as_ref()
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
                    fs::remove_file(&linkname).map_err(|e| UErr::Remove(linkname.to_owned(), e))?;
                    self.create_link(dir.as_ref(), linkname, &target)
                }
            }
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound => self.create_link(dir.as_ref(), linkname, &target),
                _ => Err(e).map_err(|e| UErr::ReadLink(linkname.to_owned(), e)),
            },
        }
    }

    /// Registers all Nix store paths with the garbage collector.
    fn register(&mut self, sp: &StorePaths) -> Result<usize> {
        let dir = self.gc_link_dir(sp.path());
        sp.iter_refs().map(|p| self.link(dir.as_path(), p)).sum()
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
    fn register_loop(&mut self, rx: GCRootsRx) {
        for storepaths in rx {
            self.output.print_store_paths(&storepaths);
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::tests::FIXTURES;

    use std::fs::read_dir;
    use std::sync::mpsc::channel;
    use tempdir::TempDir;

    fn _gcroots() -> (TempDir, GCRoots) {
        let tempdir = TempDir::new("gcroots").expect("failed to create gcroots tempdir");
        let mut gc = GCRoots::new("/", Path::new("/"), &Output::default()).unwrap();
        gc.prefix = tempdir.path().to_owned();
        gc.topdir = PathBuf::from("/home/user/www");
        gc.cwd = PathBuf::from("/home/user");
        (tempdir, gc)
    }

    fn _worker(tempdir: &TempDir) -> RegistryWorker {
        RegistryWorker::new(tempdir.path(), Path::new("/home/user"))
    }

    fn is_symlink(p: &Path) -> bool {
        fs::symlink_metadata(p)
            .expect(&format!("symlink {} does not exist", p.display()))
            .file_type()
            .is_symlink()
    }

    #[test]
    fn linkdir() {
        let td = TempDir::new("linkdir").unwrap();
        let w = _worker(&td);
        assert_eq!(td.path().join("home/user"), w.gc_link_dir("file2"));
        assert_eq!(
            td.path().join("home/user/www/d"),
            w.gc_link_dir("/home/user/www/d/file1")
        );
        assert_eq!(td.path().join("home/user/rel"), w.gc_link_dir("rel/file3"));
    }

    #[test]
    fn should_create_link() {
        let td = TempDir::new("createlink").unwrap();
        let mut w = _worker(&td);
        let storepath = Path::new("gmy86w4020xzjw9s8qzzz0bgx8ldkhhk-e34kjk");
        let expected = td.path().join("gmy86w4020xzjw9s8qzzz0bgx8ldkhhk");
        assert_eq!(w.link(td.path(), storepath).expect("link 1 failed"), 1);
        assert!(is_symlink(&expected));
        assert!(w.seen.contains(&expected));
        // second attempt: do nothing
        assert_eq!(w.link(td.path(), storepath).expect("link 2 failed"), 0);
    }

    #[test]
    fn create_link_should_create_dir() {
        let td = TempDir::new("createdir").unwrap();
        let mut w = _worker(&td);
        assert!(fs::metadata(td.path().join("d1")).is_err());
        assert_eq!(
            w.link(td.path().join("d1"), "gmy86w4020xzjw9s8qzzz0bgx8ldkhhk-e")
                .unwrap(),
            1
        );
        assert!(fs::metadata(td.path().join("d1"))
            .expect("dir d1 not created")
            .is_dir());
    }

    #[test]
    fn create_link_should_correct_existing_link() {
        let td = TempDir::new("correctlink").unwrap();
        let mut w = _worker(&td);
        let link = td.path().join("f0vdg3cb0005ksjb0fd5qs6f56zg2qs5");
        symlink("changeme", &link).unwrap();
        w.link(td.path(), "f0vdg3cb0005ksjb0fd5qs6f56zg2qs5-v")
            .unwrap();
        assert_eq!(
            PathBuf::from("/nix/store/f0vdg3cb0005ksjb0fd5qs6f56zg2qs5-v"),
            fs::read_link(&link).unwrap()
        );
    }

    #[test]
    fn cleanup_nonexistent_dir_should_succeed() {
        let td = TempDir::new("cleanup").unwrap();
        let w = _worker(&td);
        assert_eq!(w.cleanup(&td.path().join("no/such/dir")).unwrap(), 0);
    }

    #[test]
    fn should_create_links_no_earlier_than_in_commit() -> Result<()> {
        let (td, mut gc) = _gcroots();
        let (tx, rx) = channel::<StorePaths>();
        let dent = ignore::Walk::new(td.path()).into_iter().next().unwrap()?;
        tx.send(StorePaths::new(
            dent,
            vec![
                PathBuf::from("11111111111111111111111111111111-foo"),
                PathBuf::from("22222222222222222222222222222222-bar"),
            ],
            1000,
            None,
        ))
        .unwrap();
        drop(tx);

        let contents = |base: &Path| -> Vec<PathBuf> {
            read_dir(base)
                .expect("base dir missing")
                .into_iter()
                .map(|e| e.unwrap().path())
                .collect()
        };

        gc.register_loop(rx);
        assert!(
            contents(td.path()).is_empty(),
            "register_loop() should not create links"
        );

        gc.commit(&ExecutionContext::new())?;
        let base = td.path().join("tmp");
        assert_eq!(
            contents(&base),
            &[
                PathBuf::from(base.join("11111111111111111111111111111111")),
                PathBuf::from(base.join("22222222222222222222222222222222")),
            ],
        );
        Ok(())
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

    pub fn fake_gc() -> FakeGCRoots {
        FakeGCRoots::new(&*FIXTURES)
    }

    impl Register for FakeGCRoots {
        fn register_loop(&mut self, rx: GCRootsRx) {
            for storepaths in rx {
                for r in storepaths.refs() {
                    let relpath = storepaths.path().strip_prefix(&self.prefix).unwrap();
                    self.registered
                        .push(format!("{}|{}", relpath.display(), r.display()));
                }
            }
        }
    }
}
