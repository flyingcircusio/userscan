use super::STORE;
use atty::{self, Stream};
use colored::{self, ColoredString, Colorize};
use env_logger::Builder;
use errors::*;
use log::{Level, LevelFilter};
use std::io;
use std::io::prelude::*;
use std::path::Path;
use std::time::Duration;
use storepaths::StorePaths;

pub fn fmt_error_chain(err: &Error) -> String {
    err.iter()
        .map(|e| format!("{}", e))
        .collect::<Vec<_>>()
        .join(": ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub level: LevelFilter,
    pub oneline: bool,
    pub color: Option<bool>,
    pub list: bool,
}

impl Default for Output {
    fn default() -> Self {
        Output {
            level: LevelFilter::Off,
            oneline: false,
            color: None,
            list: false,
        }
    }
}

impl Output {
    pub fn new(
        verbose: bool,
        debug: bool,
        oneline: bool,
        color: Option<&str>,
        list: bool,
    ) -> Output {
        Output {
            level: match (verbose, debug) {
                (_, true) => LevelFilter::Debug,
                (true, _) => LevelFilter::Info,
                _ => LevelFilter::Warn,
            },
            color: match color {
                Some("always") => Some(true),
                Some("never") => Some(false),
                Some("auto") => Some(atty::is(Stream::Stdout) && atty::is(Stream::Stderr)),
                _ => None,
            },
            oneline,
            list,
        }
    }

    pub fn log_init(&self) {
        if let Some(v) = self.color {
            colored::control::set_override(v)
        }
        Builder::new()
            .format(|buf, r| match r.level() {
                Level::Error => {
                    writeln!(buf, "{}: {}", r.level().to_string().red().bold(), r.args())
                }
                Level::Warn => writeln!(buf, "{}: {}", r.level().to_string().yellow(), r.args()),
                Level::Info => writeln!(buf, "{}", r.args()),
                _ => writeln!(buf, "{}", r.args().to_string().blue()),
            })
            .filter(None, self.level)
            .init();
    }

    /// Outputs the name of a scanned file together with the store paths found inside.
    ///
    /// Depending on the desired output format the files are either space- or newline-separated.
    pub fn write_store_paths(&self, w: &mut Write, sp: &StorePaths) -> io::Result<()> {
        let filename = format!(
            "{}{}",
            sp.path().display(),
            if self.oneline { ":" } else { "" }
        );
        write!(w, "{}", filename.purple().bold())?;
        let sep = if self.oneline { " " } else { "\n" };
        for r in sp.iter_refs() {
            write!(w, "{}{}{}", sep, STORE, r.display())?
        }
        writeln!(w, "{}", if self.oneline { "" } else { "\n" })
    }

    #[inline]
    pub fn print_store_paths(&self, sp: &StorePaths) -> Result<()> {
        if !self.list {
            return Ok(());
        }
        let w = io::stdout();
        let mut w = io::BufWriter::new(w.lock());
        self.write_store_paths(&mut w, sp)
            .chain_err(|| ErrorKind::WalkAbort)
    }
}

/// Path to String with coloring
pub fn p2s<P: AsRef<Path>>(path: P) -> ColoredString {
    path.as_ref().display().to_string().green()
}

/// Duration to seconds
///
/// Converts a `time::Duration` value into a floating-point seconds value.
pub fn d2s(d: Duration) -> f32 {
    d.as_secs() as f32 + (d.subsec_nanos() as f32) / 1e9
}
