extern crate crossbeam;

use errors::*;
use ignore::{self, DirEntry, WalkState};
use output::print_error_chain;
use registry::GcRootsChan;
use scan::Scanner;
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use super::Args;

fn process(
    dent: DirEntry,
    args: &Args,
    scanner: &Scanner,
    softerrs: &AtomicUsize,
    gc: &GcRootsChan,
) -> Result<WalkState>
{
    if args.debug {
        eprintln!("Scanning {} ...", dent.path().display())
    }
    let sp = scanner.find_paths(dent)?;
    if let Some(err) = sp.error() {
        eprintln!("{}", err);
        softerrs.fetch_add(1, Ordering::SeqCst);
    }
    if sp.len() > 0 {
        let sp = Arc::new(sp);
        if args.register {
            gc.send(sp.clone()).chain_err(|| ErrorKind::WalkAbort)?;
        }
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
        Box::new(move |res: ::std::result::Result<DirEntry, ignore::Error>| {
            res.map_err(From::from)
                .and_then(|dent| process(dent, &args, &scanner, &softerrs, &gc))
                .unwrap_or_else(|err: Error| match err {
                    Error(ErrorKind::WalkAbort, _) => WalkState::Quit,
                    _ => {
                        print_error_chain(&err);
                        softerrs.fetch_add(1, Ordering::SeqCst);
                        WalkState::Continue
                    }
                })
        })
    });
}

pub fn run(args: Arc<Args>) -> Result<i32> {
    let softerrs = Arc::new(AtomicUsize::new(0));
    let mut gcroots = args.gcroots()?;
    let (gcroots_tx, gcroots_rx) = mpsc::channel();
    crossbeam::scope(|scope| -> Result<()> {
        let softerrs = softerrs.clone();
        scope.spawn(move || walk(args, softerrs, gcroots_tx));
        for sp in gcroots_rx {
            gcroots.register(&sp)?;
        }
        Ok(())
    })?;
    let softerrs = softerrs.load(Ordering::SeqCst);
    if softerrs > 0 {
        eprintln!("{}: {} soft errors", crate_name!(), softerrs);
        Ok(1)
    } else {
        Ok(0)
    }
}
