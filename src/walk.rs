extern crate crossbeam;

use super::App;
use errors::*;
use ignore::{self, DirEntry, WalkParallel, WalkState};
use output::{fmt_error_chain, p2s};
use registry::{GCRootsTx, Register};
use scan::Scanner;
use statistics::{Statistics, StatsMsg, StatsTx};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use storepaths::{Cache, Lookup};

#[derive(Clone, Debug)]
struct ProcessingContext {
    startdev: u64,
    sleep: Option<Duration>,
    cache: Arc<Cache>,
    scanner: Arc<Scanner>,
    stats: StatsTx,
    gc: GCRootsTx,
    abort: Arc<AtomicBool>,
}

impl ProcessingContext {
    fn create(app: &App, stats: &mut Statistics, gcroots: &mut Register) -> Result<Self> {
        Ok(Self {
            startdev: app.start_meta()?.dev(),
            sleep: app.opt.sleep,
            cache: Arc::new(app.cache()?),
            scanner: Arc::new(app.scanner()?),
            stats: stats.tx(),
            gc: gcroots.tx(),
            abort: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Scans a single DirEntry.
    ///
    /// The cache is queried first. Results (scanned or cached) are sent to the registry and
    /// statistics collector.
    fn process_direntry(&self, dent: DirEntry) -> Result<WalkState> {
        let mut sp = match self.cache.lookup(dent) {
            Lookup::Dir(sp) | Lookup::Hit(sp) => sp,
            Lookup::Miss(d) => {
                if let Some(dur) = self.sleep {
                    thread::sleep(dur);
                }
                self.scanner.find_paths(d)?
            }
        };
        if let Some(err) = sp.error() {
            warn!("{}", err);
            self.stats
                .send(StatsMsg::SoftError)
                .chain_err(|| ErrorKind::WalkAbort)?;
        }
        if sp.metadata()?.dev() != self.startdev {
            return Ok(WalkState::Skip);
        }
        self.cache
            .insert(&mut sp)
            .chain_err(|| ErrorKind::WalkAbort)?;
        self.stats
            .send(StatsMsg::Scan((&sp).into()))
            .chain_err(|| ErrorKind::WalkAbort)?;
        if !sp.is_empty() {
            self.gc.send(sp).chain_err(|| ErrorKind::WalkAbort)?;
        }
        Ok(WalkState::Continue)
    }

    /// Walks through a directory hierachy and processes each found DirEntry.
    fn walk(self, walker: WalkParallel) -> Result<Arc<Cache>> {
        walker.run(|| {
            let pctx = self.clone();
            Box::new(move |res: ::std::result::Result<DirEntry, ignore::Error>| {
                res.map_err(From::from)
                    .and_then(|dent| pctx.process_direntry(dent))
                    .unwrap_or_else(|err: Error| match err {
                        Error(ErrorKind::WalkContinue, _) => WalkState::Continue,
                        Error(ErrorKind::WalkAbort, _) => {
                            error!("{}", &fmt_error_chain(&err)[2..]);
                            pctx.abort.store(true, Ordering::SeqCst);
                            WalkState::Quit
                        }
                        _ => {
                            warn!("{}", fmt_error_chain(&err));
                            match pctx.stats.send(StatsMsg::SoftError) {
                                Err(_) => WalkState::Quit, // IPC broken
                                Ok(_) => WalkState::Continue,
                            }
                        }
                    })
            })
        });
        if self.abort.load(Ordering::SeqCst) {
            Err("Aborting program execution".into())
        } else {
            Ok(self.cache)
        }
    }
}

pub fn spawn_threads(app: &App, gcroots: &mut Register) -> Result<Statistics> {
    let mut stats = app.statistics();
    let mut cache = crossbeam::scope(|threads| -> Result<Arc<Cache>> {
        let pctx = ProcessingContext::create(app, &mut stats, gcroots)?;
        let walker = app.walker()?.build_parallel();
        info!("{}: Scouting {}", crate_name!(), p2s(&app.opt.startdir));
        if let Some(dur) = pctx.sleep {
            debug!("stutter {:?}", dur);
        }
        let walk_hdl = threads.spawn(move || pctx.walk(walker));
        threads.spawn(|| stats.receive_loop());
        gcroots.register_loop()?;
        walk_hdl.join().expect("panic in subthread")
    })?;
    if app.register {
        gcroots.clean()?;
        // don't touch cache if in no-register mode
        Arc::get_mut(&mut cache)
            .expect("BUG: dangling references to cache")
            .commit()?;
        cache.log_statistics();
    }
    stats.log_summary(&app.opt.startdir);
    Ok(stats)
}

#[cfg(test)]
mod tests {
    extern crate tempdir;

    use self::tempdir::TempDir;
    use super::*;
    use ignore::WalkBuilder;
    use registry;
    use std::fs::{create_dir, set_permissions, File, Permissions};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use tests::{app, assert_eq_vecs, fake_gc, FIXTURES};
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
                "dir1/six.py|1b4i3gm31j1ipfbx1v9a3hhgmp2wvyyw-python2.7-six-1.9.0",
            ],
        );
        assert_eq!(stats.softerrors, 0);
    }

    #[test]
    fn walk_softerrors() {
        let t = TempDir::new("test_walk").unwrap();
        let p = t.path();

        wfile(
            p.join("readable1"),
            "/nix/store/i4ai4idhj7d7qdyhv601568hna0b5car-a",
        );

        let unreadable_f = p.join("unreadable2");
        wfile(
            &unreadable_f,
            "/nix/store/dxscwf37hgq0xafs54h0c8xx47vg6d5g-n",
        );
        set_permissions(&unreadable_f, Permissions::from_mode(0o000)).unwrap();

        let unreadable_d = p.join("unreadable-dir1");
        create_dir(&unreadable_d).unwrap();
        wfile(
            &unreadable_d.join("file3"),
            "/nix/store/5hg176hhc19mg8vm2rg3lv2j3vlj166b-m",
        );
        set_permissions(&unreadable_d, Permissions::from_mode(0o111)).unwrap();

        let mut gcroots = registry::tests::FakeGCRoots::new(p);
        let stats = spawn_threads(&app(p), &mut gcroots).unwrap();
        assert_eq!(gcroots.registered.len(), 1);
        assert_eq!(stats.softerrors, 2);

        // otherwise it won't clean up
        set_permissions(&unreadable_d, Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn should_not_cross_devices() {
        let app = app("dir1");
        let mut pctx =
            ProcessingContext::create(&app, &mut app.statistics(), &mut fake_gc()).unwrap();
        pctx.startdev = 0;
        let dent = app.walker().unwrap().build().next().unwrap().unwrap();
        assert_eq!(WalkState::Skip, pctx.process_direntry(dent).unwrap());
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
            ].into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>(),
            walk2vec(&app.walker().unwrap(), &*FIXTURES)
        );
    }

    #[test]
    fn walk_should_obey_excludefile() {
        let t = TempDir::new("test_excludefile").unwrap();
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
            .and_then(|wb| ::add_dotexclude(wb, &users))
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
