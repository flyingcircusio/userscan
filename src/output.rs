extern crate atty;

use colored::{self, Colorize, ColoredString};
use env_logger::LogBuilder;
use errors::*;
use log::{LogLevel, LogLevelFilter, LogRecord};
use scan::StorePaths;
use self::atty::Stream;
use std::io;
use std::io::prelude::*;
use std::path::Path;

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
}

impl Output {
    fn log_init(self) -> Self {
        if let Some(colorcontrol) = self.color {
            colored::control::set_override(colorcontrol)
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

    pub fn new(verbose: bool, debug: bool, oneline: bool, color: Option<bool>) -> Output {
        Output {
            level: match (verbose, debug) {
                (_, true) => LogLevelFilter::Debug,
                (true, _) => LogLevelFilter::Info,
                _ => LogLevelFilter::Warn,
            },
            oneline: oneline,
            color: color.or_else(|| {
                Some(atty::is(Stream::Stdout) && atty::is(Stream::Stderr))
            }),
        }.log_init()
    }

    fn write_store_paths(&self, w: &mut Write, sp: &StorePaths) -> io::Result<()> {
        let filename = format!(
            "{}{}",
            sp.path().display(),
            if sp.is_empty() { "" } else { ":" }
        );
        write!(w, "{}", filename.purple())?;
        let sep = if self.oneline { " " } else { "\n" };
        for r in sp.iter_refs() {
            write!(w, "{}{}", sep, r.display())?
        }
        writeln!(w, "{}", if self.oneline { "" } else { "\n" })
    }

    pub fn print_store_paths(&self, sp: &StorePaths) -> Result<()> {
        let w = io::stdout();
        let mut w = io::BufWriter::new(w.lock());
        self.write_store_paths(&mut w, sp).chain_err(
            || ErrorKind::WalkAbort,
        )
    }
}

pub fn p2s(path: &Path) -> ColoredString {
    path.display().to_string().green()
}
