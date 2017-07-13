extern crate crossbeam;

use errors::*;
use ignore::{self, DirEntry, WalkState};
use registry::GcRootsChan;
use scan::Scanner;
use std::fs;
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use super::Args;

fn process(
    dent: DirEntry,
    args: &Args,
    scanner: &Scanner,
    softerrs: &AtomicUsize,
    gc: &GcRootsChan,
) -> Result<WalkState> {
    debug!("Scanning {} ...", dent.path().display());
    let sp = scanner.find_paths(dent)?;
    if let Some(err) = sp.error() {
        warn!("{}", err);
        softerrs.fetch_add(1, Ordering::SeqCst);
    }
    if !sp.is_empty() {
        let sp = Arc::new(sp);
        gc.send(sp.clone()).chain_err(|| ErrorKind::WalkAbort)?;
        if args.list {
            println!("{}", sp)
        }
    }
    Ok(WalkState::Continue)
}

fn walk(args: Arc<Args>, softerrs: Arc<AtomicUsize>, gc: GcRootsChan) {
    args.parallel_walker().run(|| {
        let args = args.clone();
        let scanner = args.scanner();
        let softerrs = softerrs.clone();
        let gc = gc.clone();
        Box::new(move |res: ::std::result::Result<
            DirEntry,
            ignore::Error,
        >| {
            res.map_err(From::from)
                .and_then(|dent| process(dent, &args, &scanner, &softerrs, &gc))
                .unwrap_or_else(|err: Error| match err {
                    Error(ErrorKind::WalkAbort, _) => WalkState::Quit,
                    _ => {
                        warn!("{}", err);
                        softerrs.fetch_add(1, Ordering::SeqCst);
                        WalkState::Continue
                    }
                })
        })
    });
}

pub fn run(args: Arc<Args>) -> Result<i32> {
    fs::metadata(&args.startdir).chain_err(|| {
        format!("cannot access start dir {}", args.startdir.display())
    })?;
    let softerrs = Arc::new(AtomicUsize::new(0));
    let mut gcroots = args.gcroots()?;
    let (gcroots_tx, gcroots_rx) = mpsc::channel();
    crossbeam::scope(|scope| -> Result<_> {
        let softerrs = softerrs.clone();
        scope.spawn(move || walk(args, softerrs, gcroots_tx));
        for sp in gcroots_rx {
            gcroots.register(&sp)?;
        }
        Ok(())
    })?;
    let softerrs = softerrs.load(Ordering::SeqCst);
    if softerrs > 0 {
        warn!("{} soft errors", softerrs);
        Ok(1)
    } else {
        Ok(0)
    }
}
