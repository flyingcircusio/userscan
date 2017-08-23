extern crate atty;
extern crate bytesize;
extern crate colored;
extern crate env_logger;
extern crate flate2;
extern crate ignore;
extern crate nix;
extern crate serde;
extern crate serde_json;
extern crate users;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;

#[cfg(test)]
extern crate tempdir;

mod cache;
mod errors;
mod output;
mod registry;
mod scan;
mod statistics;
mod walk;
#[cfg(test)]
mod tests;

use cache::Cache;
use bytesize::ByteSize;
use clap::{Arg, ArgMatches};
use errors::*;
use ignore::overrides::OverrideBuilder;
use output::{Output, p2s};
use registry::{GCRoots, NullGCRoots, Register};
use statistics::Statistics;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::fs;
use std::ops::DerefMut;
use std::path::{Path, PathBuf};
use std::result;
use users::os::unix::UserExt;

static GC_PREFIX: &str = "/nix/var/nix/gcroots/profiles/per-user";

fn canonical_startdir<P: AsRef<Path>>(startdir: P) -> Result<PathBuf> {
    let s = startdir.as_ref();
    if s.as_os_str().is_empty() {
        Ok(
            users::get_user_by_uid(users::get_effective_uid())
                .ok_or("cannot determine current user")?
                .home_dir()
                .to_path_buf(),
        )
    } else {
        s.canonicalize()
    }.chain_err(|| format!("start dir {} is not accessible", p2s(&startdir)))
}

#[derive(Debug, Clone)]
pub struct App {
    startdir: PathBuf,
    quickcheck: ByteSize,
    register: bool,
    output: Output,
    cachefile: Option<PathBuf>,
    detailed_statistics: bool,
    sleep_us: u32,
    overrides: Vec<String>,
}

impl App {
    fn walker(&self) -> Result<ignore::WalkBuilder> {
        let startdir = self.startdir()?;
        let mut ov = OverrideBuilder::new(&startdir);
        for o in self.overrides.iter() {
            let _ = ov.add(o)?;
        }
        let mut wb = ignore::WalkBuilder::new(startdir);
        wb.parents(false)
            .git_exclude(false)
            .git_global(false)
            .git_ignore(false)
            .ignore(false)
            .overrides(ov.build()?)
            .hidden(false);
        Ok(wb)
    }

    fn scanner(&self) -> scan::Scanner {
        scan::Scanner::new(self.quickcheck.as_usize())
    }

    fn gcroots(&self) -> Result<Box<Register>> {
        if self.register {
            Ok(Box::new(
                GCRoots::new(GC_PREFIX, self.startdir()?, &self.output)?,
            ))
        } else {
            Ok(Box::new(NullGCRoots::new(&self.output)))
        }
    }

    fn cache(&self) -> Result<Cache> {
        match self.cachefile {
            Some(ref f) => Cache::new().open(f),
            None => Ok(Cache::new()),
        }
    }

    fn statistics(&self) -> Statistics {
        Statistics::new(self.detailed_statistics)
    }

    fn startdir(&self) -> Result<PathBuf> {
        canonical_startdir(&self.startdir)
    }

    /// The Metadata entry of the start directory.
    ///
    /// Needed for crossdev detection.
    fn start_meta(&self) -> Result<fs::Metadata> {
        fs::metadata(self.startdir()?).map_err(|e| e.into())
    }

    pub fn run(&self) -> Result<i32> {
        self.output.log_init();
        info!("{}: Scouting {} ...", crate_name!(), p2s(&self.startdir));
        match walk::spawn_threads(self, self.gcroots()?.deref_mut())?
            .softerrors() {
            0 => Ok(0),
            _ => Ok(1),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        App {
            startdir: PathBuf::new(),
            quickcheck: ByteSize::b(0),
            register: false,
            output: Output::default(),
            cachefile: None,
            detailed_statistics: false,
            sleep_us: 0,
            overrides: vec![],
        }
    }
}

impl<'a> From<ArgMatches<'a>> for App {
    fn from(a: ArgMatches) -> Self {
        let output = Output::new(
            a.is_present("verbose"),
            a.is_present("debug"),
            a.is_present("oneline"),
            a.value_of("color"),
            a.is_present("list") || a.is_present("no-register"),
        );

        let mut overrides: Vec<String> = vec![];
        if let Some(excl) = a.values_of("exclude") {
            overrides.extend(excl.map(|e| format!("!{}", e)))
        }
        if let Some(incl) = a.values_of("include") {
            overrides.extend(incl.map(|i| i.to_owned()))
        }

        App {
            startdir: a.value_of_os("DIRECTORY").unwrap_or(OsStr::new(".")).into(),
            quickcheck: ByteSize::kib(
                a.value_of_lossy("quickcheck")
                    .unwrap_or(Cow::from("0"))
                    .parse::<usize>()
                    .unwrap(),
            ),
            output,
            register: !a.is_present("no-register"),
            cachefile: a.value_of_os("cache").map(PathBuf::from),
            detailed_statistics: a.is_present("stats"),
            sleep_us: a.value_of("stutter").unwrap_or("0").parse::<u32>().unwrap(),
            overrides,
        }
    }
}

fn parse_args() -> App {
    let a = |short, long, help| Arg::with_name(long).short(short).long(long).help(help);
    let cachefile_val = |s: String| -> result::Result<(), String> {
        if s.ends_with(".json") || s.ends_with("json.gz") {
            Ok(())
        } else {
            Err(format!(
                "extension must be either {} or {}",
                p2s(".json"),
                p2s(".json.gz")
            ))
        }
    };
    let sleep_val = |s: String| -> result::Result<(), String> {
        let val = s.parse::<u64>().map_err(|e| e.to_string())?;
        if val < 1_000_000 {
            Ok(())
        } else {
            Err(
                "who wants to sleep longer than 1 second per file?".to_owned(),
            )
        }
    };

    clap::App::new(crate_name!())
        .version(crate_version!())
        .author("© Flying Circus Internet Operations GmbH and contributors")
        .about(crate_description!())
        .arg(Arg::with_name("DIRECTORY").help("Starts scan in DIRECTORY"))
        .arg(
            a("l", "list", "Shows files containing Nix store references").display_order(1),
        )
        .arg(
            a("c", "cache", "Keeps results between runs in FILE (JSON)")
                .value_name("FILE")
                .long_help(
                    "Caches scan results in FILE to avoid re-scanning unchanged files. \
                     File extension must be one of `.json` or `.json.gz`",
                )
                .takes_value(true)
                .validator(cachefile_val),
        )
        .arg(
            a(
                "R",
                "no-register",
                "Don't register found references, implies --list",
            ).display_order(2),
        )
        .arg(a(
            "1",
            "oneline",
            "Prints each file with references on a single line",
        ))
        .arg(
            a("C", "color", "Funky colorful output")
                .value_name("WHEN")
                .takes_value(true)
                .possible_values(&["always", "never", "auto"])
                .default_value("auto")
                .long_help(
                    "Turns on funky colorful output. If set to \"auto\", color is on if \
                     run in a terminal",
                ),
        )
        .arg(a(
            "S",
            "stats",
            "Prints detailed statistics like scans per file type.",
        ))
        .arg(
            a("s", "stutter", "Sleeps SLEEP µs after each file access")
                .value_name("SLEEP")
                .long_help(
                    "Sleeps SLEEP microseconds after each file to reduce I/O load. Files \
                     loaded from the cache are not subject to stuttering",
                )
                .takes_value(true)
                .validator(sleep_val),
        )
        .arg(a("v", "verbose", "Additional output"))
        .arg(a(
            "d",
            "debug",
            "Prints every file opened, implies --verbose",
        ))
        .arg(
            a("q", "quickcheck", "Scans only the first SIZE kB of a file")
                .takes_value(true)
                .long_help(
                    "Gives up if no Nix store references are found in the first SIZE kilobytes of \
                     a file. Assumes that at least one Nix store reference is located near the \
                     beginning. Speeds up scanning large files considerably",
                )
                .value_name("SIZE")
                .default_value("512")
                .validator(|s: String| -> result::Result<(), String> {
                    s.parse::<u32>().map(|_| ()).map_err(|e| e.to_string())
                }),
        )
        .arg(
            a("e", "exclude", "Skips files matching GLOB")
                .value_name("GLOB")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1),
        )
        .arg(
            a("i", "include", "Undoes excludes for files matching GLOB")
                .value_name("GLOB")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1),
        )
        .get_matches()
        .into()
}

fn main() {
    match parse_args().run() {
        Err(ref err) => {
            error!("{}", output::fmt_error_chain(err));
            std::process::exit(2)
        }
        Ok(exitcode) => std::process::exit(exitcode),
    }
}

#[cfg(test)]
pub mod test {
    use super::*;

    #[test]
    fn startdir_should_default_to_home() {
        let user = users::get_user_by_uid(users::get_effective_uid()).unwrap();
        assert_eq!(user.home_dir(), canonical_startdir("").unwrap());
    }

    #[test]
    fn overrides_should_be_collected_and_prefixed() {
        let m = clap::App::new("app")
            .arg(
                Arg::with_name("include")
                    .short("i")
                    .takes_value(true)
                    .multiple(true),
            )
            .arg(
                Arg::with_name("exclude")
                    .short("e")
                    .takes_value(true)
                    .multiple(true),
            )
            .get_matches_from(vec!["app", "-i", "glob1", "-e", "glob2", "-i", "glob3"]);
        let a = App::from(m);
        assert_eq!(vec!["!glob2", "glob1", "glob3"], a.overrides);
    }
}
