use crate::output::{d2s, p2s};
use crate::storepaths::StorePaths;
use atty::{self, Stream};
use bytesize::ByteSize;
use colored::Colorize;
use std::collections::HashMap;
use std::ffi::OsString;
use std::hash::Hash;
use std::ops::{Add, AddAssign};
use std::path::Path;
use std::sync::mpsc;
use std::sync::mpsc::channel;
use std::time;

pub type StatsTx = mpsc::Sender<StatsMsg>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pair {
    files: usize,
    bytes: u64,
}

impl Pair {
    #[allow(unused)]
    fn new(files: usize, bytes: u64) -> Self {
        Pair { files, bytes }
    }
}

impl Add<u64> for Pair {
    type Output = Self;

    fn add(self, inc: u64) -> Pair {
        Pair {
            files: self.files + 1,
            bytes: self.bytes + inc,
        }
    }
}

impl AddAssign<u64> for Pair {
    fn add_assign(&mut self, inc: u64) {
        self.files += 1;
        self.bytes += inc;
    }
}

#[derive(Debug, Clone)]
pub enum StatsMsg {
    SoftError,
    Scan(File),
}

#[derive(Debug, Clone)]
pub struct File {
    scanned: u64,
    ext: OsString,
}

impl<'a> From<&'a StorePaths> for File {
    fn from(sp: &'a StorePaths) -> Self {
        let ext = match sp.path().extension() {
            Some(ext) => ext.to_os_string(),
            None => OsString::from(""),
        };
        File {
            scanned: sp.bytes_scanned(),
            ext,
        }
    }
}

fn map2vec<T>(map: &HashMap<T, Pair>, cutoff: usize) -> Vec<(usize, u64, T)>
where
    T: Eq + Hash + Clone,
{
    let mut res = map
        .iter()
        .map(|e| {
            let (k, p): (&T, &Pair) = e;
            (p.files, p.bytes, k.clone())
        })
        .collect::<Vec<(usize, u64, T)>>();
    res.sort_by(|a, b| a.0.cmp(&b.0).reverse());
    res.truncate(cutoff);
    res
}

#[derive(Debug)]
pub struct Statistics {
    pub softerrors: usize,
    pub total: Pair,
    by_ext: HashMap<OsString, Pair>,
    rx: Option<mpsc::Receiver<StatsMsg>>,
    start: time::Instant,
    detailed: bool,
    progress: bool,
    progress_last: u64,
}

const SHOW_NOT_BEFORE: u64 = 5;

impl Statistics {
    pub fn new(detailed: bool, quiet: bool) -> Self {
        Statistics {
            softerrors: 0,
            total: Pair::default(),
            by_ext: HashMap::new(),
            rx: None,
            start: time::Instant::now(),
            detailed,
            progress: !quiet && atty::is(Stream::Stderr),
            progress_last: SHOW_NOT_BEFORE,
        }
    }

    pub fn softerrors(&self) -> usize {
        self.softerrors
    }

    fn process(&mut self, msg: StatsMsg) {
        match msg {
            StatsMsg::Scan(f) => {
                self.total += f.scanned;
                if self.detailed {
                    let by_ext = self.by_ext.entry(f.ext).or_insert_with(Pair::default);
                    *by_ext += f.scanned;
                }
            }
            StatsMsg::SoftError => self.softerrors += 1,
        }
    }

    fn print_progress(&mut self) {
        let elapsed = self.start.elapsed().as_secs();
        if elapsed > self.progress_last {
            let p = format!(
                "Scanning in progress... {} files ({} read) in {} s     ",
                self.total.files,
                ByteSize::b(self.total.bytes),
                elapsed
            );
            eprint!("\r{}", p.purple());
            self.progress_last = elapsed;
        }
    }

    pub fn receive_loop(&mut self) {
        if let Some(rx) = self.rx.take() {
            for msg in rx {
                self.process(msg);
                if self.progress {
                    self.print_progress();
                }
            }
            if self.progress && self.progress_last > SHOW_NOT_BEFORE {
                eprintln!()
            }
        };
    }

    pub fn tx(&mut self) -> StatsTx {
        let (tx, rx) = channel::<StatsMsg>();
        self.rx = Some(rx);
        tx
    }

    pub fn print_details(&self) {
        if self.by_ext.len() <= 1 {
            return;
        }
        println!(
            "Top 10 scanned file extensions:\n\
             extension  #files  read"
        );
        for (files, bytes, ext) in map2vec(&self.by_ext, 10) {
            if !ext.is_empty() {
                println!(
                    "{:-10} {:6}  {}",
                    ext.to_string_lossy(),
                    files,
                    ByteSize::b(bytes)
                );
            }
        }
        println!();
    }

    pub fn log_summary<P: AsRef<Path>>(&self, startdir: P) {
        let elapsed = self.start.elapsed();
        info!(
            "Processed {} files ({} read) in {:5.5}{}",
            self.total.files.to_string().cyan(),
            ByteSize::b(self.total.bytes),
            d2s(elapsed).to_string().cyan(),
            " s".cyan()
        );
        if self.detailed {
            self.print_details()
        }
        let dir = p2s(startdir.as_ref());
        if self.softerrors > 0 {
            warn!(
                "{}: Finished {} with {} soft error(s)",
                crate_name!(),
                dir,
                self.softerrors
            );
        } else {
            info!("{}: Finished {}", crate_name!(), dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::assert_eq_vecs;

    fn _msg_read(bytes: u64, ext: &str) -> StatsMsg {
        StatsMsg::Scan(File {
            scanned: bytes,
            ext: ext.into(),
        })
    }

    #[test]
    fn add_single_item_with_details() {
        let mut s = Statistics::new(true, false);
        s.process(_msg_read(3498, "jpg"));
        assert_eq!(s.total, Pair::new(1, 3498));
        assert_eq!(s.by_ext.len(), 1);
    }

    #[test]
    fn add_single_item_no_details() {
        let mut s = Statistics::new(false, false);
        s.process(_msg_read(3498, "jpg"));
        assert_eq!(s.by_ext.len(), 0);
    }

    #[test]
    fn add_softerrors() {
        let mut s = Statistics::new(false, false);
        s.process(StatsMsg::SoftError);
        s.process(StatsMsg::SoftError);
        s.process(StatsMsg::SoftError);
        assert_eq!(3, s.softerrors);
    }

    #[test]
    fn account_extensions() {
        let mut s = Statistics::new(true, false);
        s.process(_msg_read(45, "png"));
        s.process(_msg_read(21, "jpg"));
        s.process(_msg_read(85, "png"));
        assert_eq_vecs(
            s.by_ext.iter().collect::<Vec<(&OsString, &Pair)>>(),
            |v| format!("{:?} {} {}", v.0, v.1.files, v.1.bytes),
            &["\"png\" 2 130", "\"jpg\" 1 21"],
        );
    }

    #[test]
    fn map2vec_extensions() {
        let mut s = Statistics::new(true, false);
        s.process(_msg_read(45, "png"));
        s.process(_msg_read(21, "jpg"));
        s.process(_msg_read(85, "png"));
        assert_eq!(
            map2vec(&s.by_ext, 2),
            vec![
                (2, 130, OsString::from("png")),
                (1, 21, OsString::from("jpg")),
            ]
        );
    }

    #[test]
    fn map2vec_cutoff() {
        let mut s = Statistics::new(true, false);
        s.process(_msg_read(95, "png"));
        s.process(_msg_read(31, "png"));
        s.process(_msg_read(21, "jpg"));
        s.process(_msg_read(305, "txt"));
        assert_eq!(map2vec(&s.by_ext, 1), vec![(2, 126, OsString::from("png"))]);
    }
}
