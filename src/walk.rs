extern crate crossbeam;

use errors::*;
use ignore::{self, DirEntry, WalkState};
use output::{Output, p2s, fmt_error_chain};
use registry::GcRootsTx;
use scan::Scanner;
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use super::Args;

fn process(
    dent: DirEntry,
    args: &Args,
    scanner: &Scanner,
    output: &Output,
    softerrs: &AtomicUsize,
    gc: &GcRootsTx,
) -> Result<WalkState> {
    let sp = scanner.find_paths(dent)?;
    if let Some(err) = sp.error() {
        warn!("{}", err);
        softerrs.fetch_add(1, Ordering::SeqCst);
    }
    if !sp.is_empty() {
        let sp = Arc::new(sp);
        gc.send(sp.clone()).chain_err(|| ErrorKind::WalkAbort)?;
        if args.list {
            output.print_store_paths(&sp)?
        }
    }
    Ok(WalkState::Continue)
}

fn walk(args: Arc<Args>, softerrs: Arc<AtomicUsize>, gc: GcRootsTx) {
    args.parallel_walker().run(|| {
        let args = args.clone();
        let output = args.output().clone();
        let scanner = args.scanner();
        let softerrs = softerrs.clone();
        let gc = gc.clone();
        Box::new(move |res: ::std::result::Result<
            DirEntry,
            ignore::Error,
        >| {
            res.map_err(From::from)
                .and_then(|dent| {
                    process(dent, &args, &scanner, &output, &softerrs, &gc)
                })
                .unwrap_or_else(|err: Error| match err {
                    Error(ErrorKind::WalkContinue, _) => WalkState::Continue,
                    Error(ErrorKind::WalkAbort, _) => WalkState::Quit,
                    _ => {
                        warn!("{}", fmt_error_chain(&err));
                        softerrs.fetch_add(1, Ordering::SeqCst);
                        WalkState::Continue
                    }
                })
        })
    });
}

pub fn run(args: Arc<Args>) -> Result<i32> {
    let startdir_abs = args.startdir.canonicalize().chain_err(|| {
        format!("start dir {} not accessible", p2s(&args.startdir))
    })?;
    let softerrs = Arc::new(AtomicUsize::new(0));
    let mut gcroots = args.gcroots()?;
    let (gcroots_tx, gcroots_rx) = mpsc::channel();
    info!("Scouting {}", p2s(&startdir_abs));

    crossbeam::scope(|scope| -> Result<_> {
        let softerrs = softerrs.clone();
        scope.spawn(move || walk(args, softerrs, gcroots_tx));
        gcroots.register_loop(gcroots_rx)
        // FIXME statistics
    })?;

    let softerrs = softerrs.load(Ordering::SeqCst);
    if softerrs > 0 {
        warn!("{} soft errors", softerrs);
        Ok(1)
    } else {
        Ok(0)
    }
}
