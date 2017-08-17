use atty::{self, Stream};
use ByteSize;
use cache::StorePaths;
use colored::Colorize;
use output::d2s;
use std::collections::HashMap;
use std::hash::Hash;
use std::time;
use std::ffi::OsString;
use std::ops::{Add, AddAssign};
use std::sync::mpsc;

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
    let mut res = map.iter()
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
    pub fn new(detailed: bool) -> Self {
        Statistics {
            softerrors: 0,
            total: Pair::default(),
            by_ext: HashMap::new(),
            rx: None,
            start: time::Instant::now(),
            detailed,
            progress: atty::is(Stream::Stderr),
            progress_last: SHOW_NOT_BEFORE,
        }
    }

    fn process(&mut self, msg: StatsMsg) {
        match msg {
            StatsMsg::Scan(f) => {
                self.total += f.scanned;
                if self.detailed {
                    let by_ext = self.by_ext.entry(f.ext).or_insert(Pair::default());
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
                ByteSize::b(self.total.bytes as usize),
                elapsed
            );
            eprint!("\r{}", p.purple());
            self.progress_last = elapsed;
        }
    }

    pub fn receive_loop(&mut self) {
        match self.rx.take() {
            Some(rx) => {
                for msg in rx {
                    self.process(msg);
                    if self.progress {
                        self.print_progress();
                    }
                }
                if self.progress && self.progress_last > SHOW_NOT_BEFORE {
                    eprintln!()
                }
            }
            None => (),
        }
    }

    pub fn tx(&mut self) -> StatsTx {
        let (tx, rx) = mpsc::channel::<StatsMsg>();
        self.rx = Some(rx);
        tx
    }

    pub fn print_details(&self) {
        if self.by_ext.len() > 0 {
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
                        ByteSize::b(bytes as usize)
                    );
                }
            }
            println!();
        }
    }

    pub fn log_summary(&self) {
        let elapsed = self.start.elapsed();
        info!(
            "Processed {} files ({} read) in {:5.5}{}",
            self.total.files.to_string().cyan(),
            ByteSize::b(self.total.bytes as usize),
            d2s(elapsed).to_string().cyan(),
            " s".cyan()
        );
        if self.detailed {
            self.print_details()
        }
        if self.softerrors > 0 {
            warn!("{} soft error(s)", self.softerrors);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use tests::*;

    fn _msg_read(bytes: u64, ext: &str) -> StatsMsg {
        StatsMsg::Scan(File {
            scanned: bytes,
            ext: ext.into(),
        })
    }

    #[test]
    fn add_single_item_with_details() {
        let mut s = Statistics::new(true);
        s.process(_msg_read(3498, "jpg"));
        assert_eq!(s.total, Pair::new(1, 3498));
        assert_eq!(s.by_ext.len(), 1);
    }

    #[test]
    fn add_single_item_no_details() {
        let mut s = Statistics::new(false);
        s.process(_msg_read(3498, "jpg"));
        assert_eq!(s.by_ext.len(), 0);
    }

    #[test]
    fn add_softerrors() {
        let mut s = Statistics::new(false);
        s.process(StatsMsg::SoftError);
        s.process(StatsMsg::SoftError);
        s.process(StatsMsg::SoftError);
        assert_eq!(3, s.softerrors);
    }

    #[test]
    fn account_extensions() {
        let mut s = Statistics::new(true);
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
        let mut s = Statistics::new(true);
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
        let mut s = Statistics::new(true);
        s.process(_msg_read(95, "png"));
        s.process(_msg_read(31, "png"));
        s.process(_msg_read(21, "jpg"));
        s.process(_msg_read(305, "txt"));
        assert_eq!(map2vec(&s.by_ext, 1), vec![(2, 126, OsString::from("png"))]);
    }
}
