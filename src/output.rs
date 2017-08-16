use atty::{self, Stream};
use cache::StorePaths;
use colored::{self, Colorize, ColoredString};
use env_logger::LogBuilder;
use errors::*;
use log::{LogLevel, LogLevelFilter, LogRecord};
use std::io;
use std::io::prelude::*;
use std::path::Path;
use std::time::Duration;

pub fn fmt_error_chain(err: &Error) -> String {
    err.iter()
        .map(|e| format!("{}", e))
        .collect::<Vec<_>>()
        .join(": ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    level: LogLevelFilter,
    oneline: bool,
    color: Option<bool>,
    list: bool,
}

impl Default for Output {
    fn default() -> Self {
        Output {
            level: LogLevelFilter::Off,
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
                (_, true) => LogLevelFilter::Debug,
                (true, _) => LogLevelFilter::Info,
                _ => LogLevelFilter::Warn,
            },
            oneline: oneline,
            color: match color {
                Some("always") => Some(true),
                Some("never") => Some(false),
                Some("auto") => Some(atty::is(Stream::Stdout) && atty::is(Stream::Stderr)),
                _ => None,
            },
            list: list,
        }
    }

    pub fn log_init(self) -> Self {
        if let Some(v) = self.color {
            colored::control::set_override(v)
        }
        let fmt = |r: &LogRecord| match r.level() {
            LogLevel::Error => {
                format!(
                    "{} {}: {}",
                    crate_name!(),
                    r.level().to_string().red().bold(),
                    r.args()
                )
            }
            LogLevel::Warn => {
                format!(
                    "{} {}: {}",
                    crate_name!(),
                    r.level().to_string().yellow(),
                    r.args()
                )
            }
            LogLevel::Info => format!("{}: {}", crate_name!(), r.args()),
            _ => format!("{}", r.args().to_string().blue()),
        };
        LogBuilder::new()
            .format(fmt)
            .filter(None, self.level)
            .init()
            .expect("log init may only be called once");
        self
    }

    pub fn write_store_paths(&self, w: &mut Write, sp: &StorePaths) -> io::Result<()> {
        let filename = format!(
            "{}{}",
            sp.path().display(),
            if sp.is_empty() { "" } else { ":" }
        );
        write!(w, "{}", filename.purple().bold())?;
        let sep = if self.oneline { " " } else { "\n" };
        for r in sp.iter_refs() {
            write!(w, "{}{}", sep, r.display())?
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
        self.write_store_paths(&mut w, sp).chain_err(
            || ErrorKind::WalkAbort,
        )
    }
}

pub fn p2s<P: AsRef<Path>>(path: P) -> ColoredString {
    path.as_ref().display().to_string().green()
}

/// Duration to seconds
///
/// Converts a time::Duration value into a floating-point seconds value.
pub fn d2s(d: Duration) -> f32 {
    d.as_secs() as f32 + (d.subsec_nanos() as f32) / 1e9
}
