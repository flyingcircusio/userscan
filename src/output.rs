use crate::storepaths::StorePaths;
use crate::{Opt, STORE};

use atty::{self, Stream};
use colored::{self, ColoredString, Colorize};
use env_logger::Builder;
use log::{Level, LevelFilter};
use std::io;
use std::io::prelude::*;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub level: LevelFilter,
    pub oneline: bool,
    pub color: bool,
    pub list: bool,
}

impl Default for Output {
    fn default() -> Self {
        Output {
            level: LevelFilter::Off,
            oneline: false,
            color: false,
            list: false,
        }
    }
}

impl Output {
    pub fn new(verbose: bool, debug: bool, oneline: bool, color: &str, list: bool) -> Output {
        Output {
            level: match (verbose, debug) {
                (_, true) => LevelFilter::Debug,
                (true, _) => LevelFilter::Info,
                _ => LevelFilter::Warn,
            },
            color: match color {
                "always" => true,
                "never" => false,
                _ => atty::is(Stream::Stdout) && atty::is(Stream::Stderr),
            },
            oneline,
            list,
        }
    }

    pub fn log_init(&self) {
        colored::control::set_override(self.color);
        Builder::new()
            .format(|buf, r| match r.level() {
                Level::Error => {
                    writeln!(buf, "{}: {}", r.level().to_string().red().bold(), r.args())
                }
                Level::Warn => {
                    writeln!(buf, "{}: {}", r.level().to_string().yellow(), r.args())
                }
                Level::Info => writeln!(buf, "{}", r.args()),
                _ => writeln!(buf, "{}", r.args().to_string().blue()),
            })
            .filter(None, self.level)
            .init();
    }

    /// Outputs the name of a scanned file together with the store paths found inside.
    ///
    /// Depending on the desired output format the files are either space- or newline-separated.
    pub fn write_store_paths(&self, w: &mut dyn Write, sp: &StorePaths) -> io::Result<()> {
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
    pub fn print_store_paths(&self, sp: &StorePaths) {
        if !self.list {
            return;
        }
        let w = io::stdout();
        let mut w = io::BufWriter::new(w.lock());
        self.write_store_paths(&mut w, sp).ok();
    }
}

impl<'a> From<&'a Opt> for Output {
    fn from(opt: &'a Opt) -> Self {
        Output::new(opt.verbose, opt.debug, opt.oneline, &opt.color, opt.list)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_default_argument() {
        let o = Output::new(false, false, false, "never", false);
        assert!(!o.color);

        let o = Output::new(false, false, false, "always", false);
        assert!(o.color);
    }
}
