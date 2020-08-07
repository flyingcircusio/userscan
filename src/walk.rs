use crate::errors::UErr;
use crate::output::p2s;
use crate::registry::{GCRootsTx, Register};
use crate::scan::Scanner;
use crate::statistics::{Statistics, StatsMsg, StatsTx};
use crate::storepaths::{Cache, Lookup, StorePaths};
use crate::App;

use anyhow::{Context, Result};
use ignore::{self, DirEntry, WalkParallel, WalkState};
use std::io::{self, ErrorKind};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;

#[derive(Clone, Debug)]
struct ProcessingContext {
    startdev: u64,
    cache: Arc<Cache>,
    scanner: Arc<Scanner>,
    stats: StatsTx,
    gc: GCRootsTx,
    abort: Arc<AtomicBool>,
}

impl ProcessingContext {
    fn create(app: &App, stats: &mut Statistics, gc: GCRootsTx) -> Result<Self> {
        Ok(Self {
            startdev: app.start_meta()?.dev(),
            cache: Arc::new(app.cache()?),
            scanner: Arc::new(app.scanner()?),
            stats: stats.tx(),
            gc,
            abort: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Scans a single DirEntry.
    ///
    /// The cache is queried first. Results (scanned or cached) are sent to the registry and
    /// statistics collector.
    fn scan_entry(&self, dent: DirEntry) -> Result<WalkState> {
        let mut sp = match self.cache.lookup(dent) {
            Lookup::Dir(sp) | Lookup::Hit(sp) => sp,
            Lookup::Miss(d) => self.scanner.find_paths(d)?,
        };
        if let Some(err) = sp.error() {
            if err.is_partial() {
                warn!("{}", err);
                self.stats.send(StatsMsg::SoftError).unwrap();
            } else {
                return Err(err.clone().into());
            }
        }
        if sp.metadata()?.dev() != self.startdev {
            return Ok(WalkState::Skip);
        }
        self.cache.insert(&mut sp).context(UErr::WalkAbort)?;
        self.stats.send(StatsMsg::Scan((&sp).into())).unwrap();
        if !sp.is_empty() {
            self.gc.send(sp).unwrap();
        }
        Ok(WalkState::Continue)
    }

    /// Walks through a directory hierachy and processes each found DirEntry.
    fn walk(self, walker: WalkParallel) -> Result<Arc<Cache>> {
        walker.run(|| {
            let pctx = self.clone();
            Box::new(move |res: Result<DirEntry, ignore::Error>| {
                res.map_err(From::from)
                    .and_then(|dent| pctx.scan_entry(dent))
                    .unwrap_or_else(|err| {
                        if let Some(UErr::WalkAbort) = err.downcast_ref::<UErr>() {
                            error!("Traversal error: {:#}", &err);
                            pctx.abort.store(true, Ordering::SeqCst);
                            return WalkState::Quit;
                        } else if let Some(e) = err.downcast_ref::<ignore::Error>() {
                            error!("Traversal failure: {:#}", &e);
                            pctx.abort.store(true, Ordering::SeqCst);
                            return WalkState::Quit;
                        } else if let Some(e) = err.downcast_ref::<io::Error>() {
                            if e.kind() == ErrorKind::PermissionDenied {
                                error!("I/O error: {:#}", &err);
                                pctx.abort.store(true, Ordering::SeqCst);
                                return WalkState::Quit;
                            }
                        }
                        warn!("{:#}", &err);
                        pctx.stats.send(StatsMsg::SoftError).unwrap();
                        WalkState::Continue
                    })
            })
        });
        if self.abort.load(Ordering::SeqCst) {
            Err(UErr::WalkAbort.into())
        } else {
            Ok(self.cache)
        }
    }
}

/// Creates threads, starts parallel scanning and collects results.
pub fn spawn_threads(app: &App, gcroots: &mut dyn Register) -> Result<Statistics> {
    let mut stats = app.statistics();
    let (gc_tx, gc_rx) = channel::<StorePaths>();
    let mut cache = crossbeam::scope(|sc| -> Result<Arc<Cache>> {
        let pctx = ProcessingContext::create(app, &mut stats, gc_tx)?;
        let walker = app.walker()?.build_parallel();
        info!("{}: Scouting {}", crate_name!(), p2s(&app.opt.startdir));
        let walk_hdl = sc.spawn(|_| pctx.walk(walker));
        sc.spawn(|_| stats.receive_loop());
        gcroots.register_loop(gc_rx);
        walk_hdl.join().expect("subthread panic")
    })
    .expect("thread panic")?;
    if app.register {
        gcroots.commit(&app.exectx)?;
        // don't touch cache if in no-register mode
        Arc::get_mut(&mut cache)
            .expect("dangling cache references (all threads terminated?)")
            .commit(&app.exectx)?;
        cache.log_statistics();
    }
    stats.log_summary(&app.opt.startdir);
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;
    use crate::registry::tests::{fake_gc, FakeGCRoots};
    use crate::tests::{app, assert_eq_vecs, FIXTURES};

    use ignore::WalkBuilder;
    use std::fs;
    use std::fs::{create_dir, set_permissions, File, Permissions};
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::channel;
    use tempfile::TempDir;
    use users::mock::{MockUsers, User};
    use users::os::unix::UserExt;

    // helper functions

    fn wfile<P: AsRef<Path>>(path: P, contents: &str) {
        let mut file = File::create(path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
    }

    /// Walks whatever a given WalkBuilder builds and collects path relative to the fixtures dir.
    /// Hard errors lead to a panic, partial errors are silently ignored.
    pub fn walk2vec(wb: &WalkBuilder, prefix: &Path) -> Vec<PathBuf> {
        let mut paths = vec![];
        let prefix = prefix.canonicalize().unwrap();
        for r in wb.build() {
            if let Ok(dent) = r {
                let p = dent.path().strip_prefix(&prefix).unwrap();
                paths.push(p.to_owned());
            }
        }
        paths.sort();
        paths
    }

    struct TestDir {
        temp: TempDir,
    }

    /// Create and remove directory for running tests. Provides an easy way to execute setup code.
    impl TestDir {
        fn new<F>(setup: F) -> Self
        where
            F: FnOnce(&Path),
        {
            let temp = TempDir::new().unwrap();
            setup(&temp.path());
            Self { temp }
        }

        fn path(&self) -> &Path {
            self.temp.path()
        }
    }

    impl Drop for TestDir {
        /// Set read/exec bits everywhere -- else TempDir's cleanup might fail
        fn drop(&mut self) {
            for entry in fs::read_dir(self.temp.path()).unwrap() {
                if let Ok(f) = entry {
                    set_permissions(f.path(), Permissions::from_mode(0o755)).ok();
                }
            }
        }
    }

    #[test]
    fn walk_fixture_dir1() {
        let mut gcroots = fake_gc();
        let stats = spawn_threads(&app("dir1"), &mut gcroots).unwrap();
        assert_eq_vecs(
            gcroots.registered,
            |s| s.to_owned(),
            &[
                "dir1/duplicated|010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5",
                "dir1/notignored|00n9gkswhqdgbhgs7lnz2ckqxphavjr8-ChasingBottoms-1.3.1.2.drv",
                "dir1/notignored|00y6xgsdpjx3fyz4v7k5lwivi28yqd9f-initrd-fsinfo.drv",
                "dir1/proto-http.la|9w3ci6fskmz3nw27fb68hybfa5v1r33f-libidn-1.33",
                "dir1/proto-http.la|knvydciispmr4nr2rxg0iyyff3n1v4ax-gcc-6.2.0-lib",
                "dir1/script.zip|9v78r3afqy9xn9zwdj9wfys6sk3vc01d-coreutils-8.31",
                "dir1/six.py|1b4i3gm31j1ipfbx1v9a3hhgmp2wvyyw-python2.7-six-1.9.0",
            ],
        );
        assert_eq!(stats.softerrors, 0);
    }

    #[test]
    fn harderror_on_unreadable_file() {
        let t = TestDir::new(|p| {
            let f = p.join("unreadable_file");
            wfile(&f, "/nix/store/dxscwf37hgq0xafs54h0c8xx47vg6d5g-n");
            set_permissions(&f, Permissions::from_mode(0o000)).unwrap();
        });
        assert!(spawn_threads(&app(t.path()), &mut FakeGCRoots::new(t.path())).is_err());
    }

    #[test]
    fn harderror_on_unreadable_dir() {
        let t = TestDir::new(|p| {
            let d = p.join("unreadable_dir");
            create_dir(&d).unwrap();
            wfile(
                &d.join("file3"),
                "/nix/store/5hg176hhc19mg8vm2rg3lv2j3vlj166b-m",
            );
            set_permissions(&d, Permissions::from_mode(0o111)).unwrap();
        });
        assert!(spawn_threads(&app(t.path()), &mut FakeGCRoots::new(t.path())).is_err());
    }

    #[test]
    fn harderror_on_traversable_dir() {
        let t = TestDir::new(|p| {
            let d = p.join("untraversable_dir");
            create_dir(&d).unwrap();
            set_permissions(&d, Permissions::from_mode(0o000)).unwrap();
        });
        assert!(spawn_threads(&app(t.path()), &mut FakeGCRoots::new(t.path())).is_err());
    }

    #[test]
    fn ignore_dangling_link() {
        let t = TestDir::new(|p| {
            symlink(p.join("no/where"), p.join("symlink")).unwrap();
        });
        let stats = spawn_threads(&app(t.path()), &mut FakeGCRoots::new(t.path())).unwrap();
        assert_eq!(stats.softerrors, 0);
    }

    #[test]
    fn softfail_on_broken_zip_archive() {
        let t = TestDir::new(|p| {
            fs::write(
                p.join("broken.zip"),
                &fs::read(&*FIXTURES.join("dir1/script.zip")).unwrap()[..200],
            )
            .unwrap()
        });
        let stats = spawn_threads(&app(t.path()), &mut FakeGCRoots::new(t.path())).unwrap();
        assert_eq!(stats.softerrors, 1);
    }

    #[test]
    fn walk_infiniteloop() {
        let t = TempDir::new().unwrap();
        let p = t.path();
        create_dir(p.join("dir1")).unwrap();
        create_dir(p.join("dir2")).unwrap();
        symlink("../dir2/file2", p.join("dir1/file1")).unwrap();
        symlink("../dir1/file1", p.join("dir2/file2")).unwrap();
        symlink(".", p.join("recursive")).unwrap();
        let mut gcroots = registry::tests::FakeGCRoots::new(p);
        let stats = spawn_threads(&app(p), &mut gcroots).unwrap();
        assert_eq!(gcroots.registered.len(), 0);
        assert_eq!(stats.softerrors, 0);
    }

    #[test]
    fn should_not_cross_devices() {
        let app = app("dir1");
        let (tx, _) = channel::<StorePaths>();
        let mut pctx = ProcessingContext::create(&app, &mut app.statistics(), tx).unwrap();
        pctx.startdev = 0;
        let dent = app.walker().unwrap().build().next().unwrap().unwrap();
        assert_eq!(WalkState::Skip, pctx.scan_entry(dent).unwrap());
    }

    #[test]
    fn walk_should_obey_exclude() {
        let mut app = app(".");
        app.overrides = vec![
            "!dir1".to_owned(),
            "!lftp*".to_owned(),
            "!cache*".to_owned(),
        ];
        assert_eq!(
            vec![
                "",
                "dir2",
                "dir2/ignored",
                "dir2/link",
                "miniegg-1-py3.5.egg",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>(),
            walk2vec(&app.walker().unwrap(), &*FIXTURES)
        );
    }

    #[test]
    fn walk_should_obey_excludefile() {
        let t = TempDir::new().unwrap();
        let p = t.path();

        let mut users = MockUsers::with_current_uid(100);
        users.add_user(User::new(100, "johndoe", 100).with_home_dir(&*p.to_string_lossy()));
        let app = app(p);

        wfile(p.join(".userscan-ignore"), "file2\n*.jpg\ndata*\n");
        for f in vec!["file1", "file2", "pic.jpg", "data.json"] {
            File::create(p.join(f)).unwrap();
        }

        let walker = app
            .walker()
            .and_then(|wb| crate::add_dotexclude(wb, &users))
            .unwrap();
        assert_eq!(
            vec!["", ".userscan-ignore", "file1"]
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>(),
            walk2vec(&walker, p)
        );
    }
}
