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
    magic: String,
}

impl<'a> From<&'a StorePaths> for File {
    fn from(sp: &'a StorePaths) -> Self {
        let ext = match sp.path().extension() {
            Some(ext) => ext.to_os_string(),
            None => OsString::from(""),
        };
        // XXX incomplete data
        File {
            scanned: sp.bytes_scanned(),
            ext,
            magic: "".to_owned(),
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
    by_magic: HashMap<String, Pair>,
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
            by_magic: HashMap::new(),
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
                    let by_magic = self.by_magic.entry(f.magic).or_insert(Pair::default());
                    *by_magic += f.scanned;
                }
            }
            StatsMsg::SoftError => self.softerrors += 1,
        }
    }

    fn print_progress(&mut self) {
        let elapsed = self.start.elapsed().as_secs();
        if elapsed > self.progress_last {
            let p = format!(
                "Scanning in progress... {} files ({} read) in {} s",
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

    pub fn log_details(&self) {
        // XXX
    }

    pub fn log_summary(&self) {
        let elapsed = self.start.elapsed();
        info!(
            "Processed {} files ({} read) in {:4.4}{}",
            self.total.files.to_string().cyan(),
            ByteSize::b(self.total.bytes as usize),
            d2s(elapsed).to_string().cyan(),
            " s".cyan()
        );
        if self.detailed {
            self.log_details()
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

    fn _msg_read(bytes: u64, ext: &str, magic: &str) -> StatsMsg {
        StatsMsg::Scan(File {
            scanned: bytes,
            ext: ext.into(),
            magic: magic.to_owned(),
        })
    }

    #[test]
    fn add_single_item_with_details() {
        let mut s = Statistics::new(true);
        s.process(_msg_read(3498, "jpg", "image/jpeg"));
        assert_eq!(s.total, Pair::new(1, 3498));
        assert_eq!(s.by_ext.len(), 1);
        assert_eq!(s.by_magic.len(), 1);
    }

    #[test]
    fn add_single_item_no_details() {
        let mut s = Statistics::new(false);
        s.process(_msg_read(3498, "jpg", "image/jpeg"));
        assert_eq!(s.by_ext.len(), 0);
        assert_eq!(s.by_magic.len(), 0);
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
        s.process(_msg_read(45, "png", "image/x-png"));
        s.process(_msg_read(21, "jpg", "image/jpeg"));
        s.process(_msg_read(85, "png", "image/png"));
        assert_eq_vecs(
            s.by_ext.iter().collect::<Vec<(&OsString, &Pair)>>(),
            |v| format!("{:?} {} {}", v.0, v.1.files, v.1.bytes),
            &["\"png\" 2 130", "\"jpg\" 1 21"],
        );
    }

    #[test]
    fn account_magics() {
        let mut s = Statistics::new(true);
        s.process(_msg_read(45, "png", "image/png"));
        s.process(_msg_read(21, "jpg", "image/jpeg"));
        s.process(_msg_read(85, "jpeg", "image/jpeg"));
        assert_eq_vecs(
            s.by_magic.iter().collect::<Vec<(&String, &Pair)>>(),
            |v| format!("{} {} {}", v.0, v.1.files, v.1.bytes),
            &["image/png 1 45", "image/jpeg 2 106"],
        );
    }

    #[test]
    fn fmt_map2vec_osstr() {
        let mut s = Statistics::new(true);
        s.process(_msg_read(45, "png", "image/x-png"));
        s.process(_msg_read(21, "jpg", "image/jpeg"));
        s.process(_msg_read(85, "png", "image/png"));
        assert_eq!(
            map2vec(&s.by_ext, 2),
            vec![
                (2, 130, OsString::from("png")),
                (1, 21, OsString::from("jpg")),
            ]
        );
    }

    #[test]
    fn fmt_map2vec_cutoff() {
        let mut s = Statistics::new(true);
        s.process(_msg_read(95, "png", "image/png"));
        s.process(_msg_read(31, "png", "image/png"));
        s.process(_msg_read(21, "jpg", "image/jpeg"));
        s.process(_msg_read(305, "txt", "application/text"));
        assert_eq!(map2vec(&s.by_ext, 1), vec![(2, 126, OsString::from("png"))]);
    }
}
