extern crate crossbeam;

use cache::{Cache, Lookup};
use errors::*;
use ignore::{self, DirEntry, WalkState};
use output::{p2s, fmt_error_chain};
use registry::{Register, GCRootsTx};
use scan::Scanner;
use statistics::{Statistics, StatsMsg, StatsTx};
use std::ops::DerefMut;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use super::App;

fn process_direntry(
    dent: DirEntry,
    sleep: Duration,
    cache: &Cache,
    scanner: &Scanner,
    stats: &StatsTx,
    gc: &GCRootsTx,
) -> Result<WalkState> {
    let mut sp = match cache.lookup(dent) {
        Lookup::Dir(sp) => sp,
        Lookup::Hit(sp) => sp,
        Lookup::Miss(d) => {
            thread::sleep(sleep);
            scanner.find_paths(d)?
        }
    };
    if let Some(err) = sp.error() {
        warn!("{}", err);
        stats.send(StatsMsg::SoftError).chain_err(
            || ErrorKind::WalkAbort,
        )?;
    }
    cache.insert(&mut sp)?;
    stats.send(StatsMsg::Scan((&sp).into())).chain_err(|| {
        ErrorKind::WalkAbort
    })?;
    if !sp.is_empty() {
        gc.send(sp).chain_err(|| ErrorKind::WalkAbort)?;
    }
    Ok(WalkState::Continue)
}

fn walk(args: Arc<App>, cache: Arc<Cache>, stats: StatsTx, gc: GCRootsTx) -> Result<()> {
    args.walker()?.build_parallel().run(|| {
        let scanner = args.scanner();
        let sleep = Duration::new(0, args.sleep_us * 1000);
        let cache = cache.clone();
        let stats = stats.clone();
        let gc = gc.clone();
        Box::new(move |res: ::std::result::Result<
            DirEntry,
            ignore::Error,
        >| {
            res.map_err(From::from)
                .and_then(|dent| {
                    process_direntry(dent, sleep, &cache, &scanner, &stats, &gc)
                })
                .unwrap_or_else(|err: Error| match err {
                    Error(ErrorKind::WalkContinue, _) => WalkState::Continue,
                    Error(ErrorKind::WalkAbort, _) => WalkState::Quit,
                    _ => {
                        warn!("{}", fmt_error_chain(&err));
                        match stats.send(StatsMsg::SoftError) {
                            Err(_) => WalkState::Quit, // IPC broken
                            Ok(_) => WalkState::Continue,
                        }
                    }
                })
        })
    });
    Ok(())
}

fn spawn_threads(app: Arc<App>, gcroots: &mut Register) -> Result<Statistics> {
    let mut stats = app.statistics();
    let mut cache = Arc::new(app.cache()?);
    info!("{}: Scouting {} ...", crate_name!(), p2s(&app.startdir));

    crossbeam::scope(|threads| -> Result<()> {
        let cache = cache.clone();
        let stats_tx = stats.tx();
        let gcroots_tx = gcroots.tx();
        let walk = threads.spawn(move || walk(app.clone(), cache, stats_tx, gcroots_tx));
        threads.spawn(|| stats.receive_loop());
        gcroots.register_loop()?;
        walk.join()
    })?;
    let mut cache = Arc::get_mut(&mut cache).expect("BUG: pending references to cache object");
    cache.commit()?;
    cache.log_statistics();
    stats.log_summary();
    Ok(stats)
}

pub fn run(app: App) -> Result<i32> {
    let mut gcroots = app.gcroots()?;
    let stats = spawn_threads(Arc::new(app), gcroots.deref_mut())?;
    match stats.softerrors {
        0 => Ok(0),
        _ => Ok(1),
    }
}

#[cfg(test)]
mod tests {
    extern crate tempdir;

    use registry;
    use self::tempdir::TempDir;
    use std::fs::{File, create_dir, set_permissions, Permissions};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use super::*;
    use tests::{app, assert_eq_vecs, FIXTURES};

    #[test]
    fn walk_fixture_dir1() {
        let mut gcroots = registry::tests::FakeGCRoots::new(&*FIXTURES);
        let stats = spawn_threads(Arc::new(app("dir1")), &mut gcroots).unwrap();
        assert_eq_vecs(
            gcroots.registered,
            |s| s.to_owned(),
            &[
                "dir1/duplicated|/nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5",
                "dir1/notignored|/nix/store/00n9gkswhqdgbhgs7lnz2ckqxphavjr8-ChasingBottoms-1.3.1.2.drv",
                "dir1/notignored|/nix/store/00y6xgsdpjx3fyz4v7k5lwivi28yqd9f-initrd-fsinfo.drv",
                "dir1/proto-http.la|/nix/store/9w3ci6fskmz3nw27fb68hybfa5v1r33f-libidn-1.33",
                "dir1/proto-http.la|/nix/store/knvydciispmr4nr2rxg0iyyff3n1v4ax-gcc-6.2.0-lib",
                "dir1/six.py|/nix/store/1b4i3gm31j1ipfbx1v9a3hhgmp2wvyyw-python2.7-six-1.9.0",
            ],
        );
        assert_eq!(stats.softerrors, 0);
    }

    #[test]
    fn walk_softerrors() {
        let t = TempDir::new("test_walk").unwrap();
        let readable = t.path().join("file1");
        writeln!(
            File::create(&readable).unwrap(),
            "/nix/store/i4ai4idhj7d7qdyhv601568hna0b5car-a"
        ).unwrap();

        let unreadable_f = t.path().join("file2");
        writeln!(
            File::create(&unreadable_f).unwrap(),
            "/nix/store/dxscwf37hgq0xafs54h0c8xx47vg6d5g-n"
        ).unwrap();
        set_permissions(&unreadable_f, Permissions::from_mode(0o000)).unwrap();

        let unreadable_d = t.path().join("dir1");
        create_dir(&unreadable_d).unwrap();
        writeln!(
            File::create(&unreadable_d.join("file3")).unwrap(),
            "/nix/store/5hg176hhc19mg8vm2rg3lv2j3vlj166b-m"
        ).unwrap();
        set_permissions(&unreadable_d, Permissions::from_mode(0o111)).unwrap();

        let borked_ignore = t.path().join(".ignore");
        writeln!(File::create(&borked_ignore).unwrap(), "pattern[*").unwrap();

        let mut gcroots = registry::tests::FakeGCRoots::new(t.path());
        let stats = spawn_threads(Arc::new(app(t.path())), &mut gcroots).unwrap();
        println!("registered GC roots: {:?}", gcroots.registered);
        assert_eq!(gcroots.registered.len(), 1);
        assert_eq!(stats.softerrors, 3);

        // otherwise it won't clean up
        set_permissions(&unreadable_d, Permissions::from_mode(0o755)).unwrap();
    }
}
