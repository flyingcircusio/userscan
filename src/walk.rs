extern crate crossbeam;

use cache::{Cache, Lookup};
use errors::*;
use ignore::{self, DirEntry, WalkState};
use output::{p2s, fmt_error_chain};
use registry::{Register, GcRootsTx};
use scan::Scanner;
use std::ops::DerefMut;
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use super::Args;

fn process_direntry(
    dent: DirEntry,
    cache: &Cache,
    scanner: &Scanner,
    softerrs: &AtomicUsize,
    gc: &GcRootsTx,
) -> Result<WalkState> {
    let sp = match cache.lookup(dent) {
        Lookup::Dir(sp) => sp,
        Lookup::Hit(sp) => sp,
        Lookup::Miss(d) => scanner.find_paths(d)?,
    };
    if let Some(err) = sp.error() {
        warn!("{}", err);
        softerrs.fetch_add(1, Ordering::SeqCst);
    }
    let sp = Arc::new(sp);
    if !sp.is_empty() {
        gc.send(sp.clone()).chain_err(|| ErrorKind::WalkAbort)?;
    }
    cache.insert(&sp)?;
    Ok(WalkState::Continue)
}

fn walk(args: Arc<Args>, cache: Arc<Cache>, softerrs: Arc<AtomicUsize>, gc: GcRootsTx) {
    args.walker().build_parallel().run(|| {
        let scanner = args.scanner();
        let cache = cache.clone();
        let softerrs = softerrs.clone();
        let gc = gc.clone();
        Box::new(move |res: ::std::result::Result<
            DirEntry,
            ignore::Error,
        >| {
            res.map_err(From::from)
                .and_then(|dent| {
                    process_direntry(dent, &cache, &scanner, &softerrs, &gc)
                })
                .unwrap_or_else(|err: Error| match err {
                    Error(ErrorKind::WalkContinue, _) => WalkState::Continue,
                    Error(ErrorKind::WalkAbort, _) => WalkState::Quit,
                    _ => {
                        warn!("{}", fmt_error_chain(&err));
                        softerrs.fetch_add(1, Ordering::Relaxed);
                        WalkState::Continue
                    }
                })
        })
    });
}

fn spawn_threads(args: Arc<Args>, gcroots: &mut Register) -> Result<usize> {
    let startdir_abs = args.startdir.canonicalize().chain_err(|| {
        format!("start dir {} not accessible", p2s(&args.startdir))
    })?;
    let mut cache = Arc::new(args.cache()?);
    let softerrs = Arc::new(AtomicUsize::new(0));
    let (gcroots_tx, gcroots_rx) = mpsc::channel();
    info!("Scouting {}", p2s(&startdir_abs));

    crossbeam::scope(|threads| -> Result<()> {
        let softerrs = softerrs.clone();
        let cache = cache.clone();
        threads.spawn(move || walk(args.clone(), cache, softerrs, gcroots_tx));
        gcroots.register_loop(gcroots_rx)
    })?;
    let mut cache = Arc::get_mut(&mut cache).expect("BUG: pending references to cache object");
    cache.commit()?;
    Ok(softerrs.load(Ordering::SeqCst))
}

pub fn run(args: Args) -> Result<i32> {
    let mut gcroots = args.gcroots()?;
    let softerrs = spawn_threads(Arc::new(args), gcroots.deref_mut())?;
    if softerrs > 0 {
        warn!("{} soft error(s)", softerrs);
        Ok(1)
    } else {
        Ok(0)
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
    use tests::{args, assert_eq_vecs};

    #[test]
    fn walk_fixture_dir1() {
        let mut gcroots = registry::tests::FakeGCRoots::new();
        let softerrs = spawn_threads(Arc::new(args("dir1")), &mut gcroots).unwrap();
        assert_eq_vecs(
            gcroots.registered,
            |s| &s,
            &[
                "dir1/duplicated|/nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5",
                "dir1/notignored|/nix/store/00n9gkswhqdgbhgs7lnz2ckqxphavjr8-ChasingBottoms-1.3.1.2.drv",
                "dir1/notignored|/nix/store/00y6xgsdpjx3fyz4v7k5lwivi28yqd9f-initrd-fsinfo.drv",
                "dir1/proto-http.la|/nix/store/9w3ci6fskmz3nw27fb68hybfa5v1r33f-libidn-1.33",
                "dir1/proto-http.la|/nix/store/knvydciispmr4nr2rxg0iyyff3n1v4ax-gcc-6.2.0-lib",
                "dir1/six.py|/nix/store/1b4i3gm31j1ipfbx1v9a3hhgmp2wvyyw-python2.7-six-1.9.0",
            ],
        );
        assert_eq!(softerrs, 0);
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

        let mut gcroots = registry::tests::FakeGCRoots::new();
        let softerrs = spawn_threads(Arc::new(args(t.path())), &mut gcroots).unwrap();
        println!("registered GC roots: {:?}", gcroots.registered);
        assert_eq!(gcroots.registered.len(), 1);
        assert_eq!(softerrs, 3);

        // otherwise it won't clean up
        set_permissions(&unreadable_d, Permissions::from_mode(0o755)).unwrap();
    }
}
