#![recursion_limit = "256"]

extern crate atty;
extern crate bytesize;
#[macro_use]
extern crate clap;
extern crate colored;
extern crate env_logger;
#[macro_use]
extern crate error_chain;
extern crate fnv;
extern crate ignore;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate minilzo;
extern crate nix;
extern crate rmp_serde;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate users;
extern crate zip;

mod errors;
mod output;
mod registry;
mod scan;
mod statistics;
mod storepaths;
#[cfg(test)]
mod tests;
mod walk;

use bytesize::ByteSize;
use clap::{Arg, ArgMatches};
use errors::*;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use output::{p2s, Output};
use registry::{GCRoots, NullGCRoots, Register};
use statistics::Statistics;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::fs;
use std::ops::DerefMut;
use std::path::PathBuf;
use std::result;
use storepaths::Cache;
use users::cache::UsersCache;
use users::os::unix::UserExt;
use users::Users;

static STORE: &str = "/nix/store/";
static GC_PREFIX: &str = "/nix/var/nix/gcroots/profiles/per-user";
static DOTEXCLUDE: &str = ".userscan-ignore";

#[derive(Debug, Clone)]
pub struct App {
    cachefile: Option<PathBuf>,
    cachelimit: Option<usize>,
    detailed_statistics: bool,
    output: Output,
    overrides: Vec<String>,
    quickcheck: ByteSize,
    register: bool,
    sleep_ms: Option<f32>,
    startdir: PathBuf,
    excludefrom: Vec<PathBuf>,
    dotexclude: bool,
    unzip: Vec<String>,
}

fn add_dotexclude<U: Users>(mut wb: WalkBuilder, u: &U) -> Result<WalkBuilder> {
    if let Some(me) = u.get_user_by_uid(u.get_effective_uid()) {
        let candidate = me.home_dir().join(DOTEXCLUDE);
        if candidate.exists() {
            if let Some(err) = wb.add_ignore(candidate) {
                return Err(err.into());
            }
        }
        Ok(wb)
    } else {
        Err(format!("failed to locate UID {} in passwd", u.get_effective_uid()).into())
    }
}

impl App {
    /// WalkBuilder configured according to the cmdline arguments
    fn walker(&self) -> Result<WalkBuilder> {
        let startdir = self.startdir()?;
        let mut ov = OverrideBuilder::new(&startdir);
        for o in &self.overrides {
            let _ = ov.add(o)?;
        }

        let mut wb = WalkBuilder::new(startdir);
        wb.parents(false)
            .git_global(false)
            .git_ignore(false)
            .ignore(false)
            .overrides(ov.build()?)
            .hidden(false);
        for p in &self.excludefrom {
            if let Some(err) = wb.add_ignore(p) {
                return Err(err).chain_err(|| "failed to load exclude file".to_owned());
            }
        }
        if self.dotexclude {
            add_dotexclude(wb, &UsersCache::new())
        } else {
            Ok(wb)
        }
    }

    fn scanner(&self) -> Result<scan::Scanner> {
        let mut ob = OverrideBuilder::new(&self.startdir);
        for glob in &self.unzip {
            ob.add(glob)?;
        }
        Ok(scan::Scanner::new(self.quickcheck.as_u64(), ob.build()?))
    }

    fn gcroots(&self) -> Result<Box<Register>> {
        if self.register {
            Ok(Box::new(GCRoots::new(
                GC_PREFIX,
                self.startdir()?,
                &self.output,
            )?))
        } else {
            Ok(Box::new(NullGCRoots::new(&self.output)))
        }
    }

    fn cache(&self) -> Result<Cache> {
        match self.cachefile {
            Some(ref f) => Cache::new(self.cachelimit).open(f),
            None => Ok(Cache::new(self.cachelimit)),
        }
    }

    fn statistics(&self) -> Statistics {
        Statistics::new(self.detailed_statistics, self.output.list)
    }

    fn startdir(&self) -> Result<PathBuf> {
        self.startdir
            .canonicalize()
            .chain_err(|| format!("start dir {} is not accessible", p2s(&self.startdir)))
    }

    /// The Metadata entry of the start directory.
    ///
    /// Needed for crossdev detection.
    fn start_meta(&self) -> Result<fs::Metadata> {
        fs::metadata(self.startdir()?).map_err(|e| e.into())
    }

    /// Main entry point
    pub fn run(&self) -> Result<i32> {
        self.output.log_init();
        match walk::spawn_threads(self, self.gcroots()?.deref_mut())?.softerrors() {
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
            cachelimit: None,
            detailed_statistics: false,
            sleep_ms: None,
            overrides: vec![],
            excludefrom: vec![],
            dotexclude: true,
            unzip: vec![],
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
            a.is_present("list"),
        );

        let mut overrides: Vec<String> = vec![];
        if let Some(excl) = a.values_of("exclude") {
            overrides.extend(excl.map(|e| format!("!{}", e)))
        }
        if let Some(incl) = a.values_of("include") {
            overrides.extend(incl.map(|i| i.to_owned()))
        }

        App {
            startdir: a
                .value_of_os("DIRECTORY")
                .unwrap_or_else(|| OsStr::new("."))
                .into(),
            quickcheck: ByteSize::kib(
                a.value_of_lossy("quickcheck")
                    .unwrap_or_else(|| Cow::from("0"))
                    .parse::<u64>()
                    .unwrap(),
            ),
            output,
            register: !a.is_present("list") || a.is_present("register"),
            cachefile: a.value_of_os("cache").map(PathBuf::from),
            cachelimit: a
                .value_of("cache-limit")
                .map(|val| val.parse::<usize>().unwrap()),
            detailed_statistics: a.is_present("stats"),
            sleep_ms: a.value_of("stutter").map(|val| val.parse::<f32>().unwrap()),
            overrides,
            excludefrom: a
                .values_of_os("exclude-from")
                .map(|vals| vals.map(PathBuf::from).collect())
                .unwrap_or_default(),
            dotexclude: true,
            unzip: a
                .values_of("unzip")
                .map(|vals| vals.map(String::from).collect())
                .unwrap_or_default(),
        }
    }
}

lazy_static! {
    static ref USAGE: String = format!("{} [OPTIONS] <DIRECTORY>", crate_name!());
    static ref AFTER_HELP: String = format!(
        "Ignore globs are always loaded from ~/{} if it exists. For the format of all ignore \
         files refer to the gitignore(5) man page.",
        DOTEXCLUDE
    );
}

fn args<'a, 'b>() -> clap::App<'a, 'b> {
    let a = |short, long, help| Arg::with_name(long).short(short).long(long).help(help);
    let validate_stutter = |s: String| -> result::Result<(), String> {
        let val = s.parse::<f32>().map_err(|e| e.to_string())?;
        if val < 1e3 {
            Ok(())
        } else {
            Err("who wants to sleep longer than 1 second per file?".to_owned())
        }
    };

    clap::App::new(crate_name!())
        .version(crate_version!())
        .author("© Flying Circus Internet Operations GmbH and contributors.")
        .about(crate_description!())
        .usage(USAGE.as_str())
        .after_help(AFTER_HELP.as_str())
        .arg(
            Arg::with_name("DIRECTORY")
                .help("Starts scan in DIRECTORY")
                .required(true),
        )
        .arg(
            a(
                "l",
                "list",
                "Only prints Nix store references while scanning (doesn't register)",
            ).long_help(
                "Prints Nix store references while scanning. GC roots are not registered when \
                 this option is active. Specify -r/--register in addition to get both listing and \
                 registration.",
            )
                .display_order(1),
        )
        .arg(
            a(
                "r",
                "register",
                "Registers references even when in list mode",
            ).long_help(
                "Registers references even when in list mode. Registration is enabled by default \
                 if -l/--list not given.",
            )
                .display_order(2),
        )
        .arg(
            a("c", "cache", "Keeps results between runs in FILE")
                .value_name("FILE")
                .long_help(
                    "Caches scan results in FILE to avoid re-scanning unchanged files. \
                     The cache is kept as a compressed messagepack file.",
                )
                .takes_value(true),
        )
        .arg(
            a("L", "cache-limit", "Limit cache to N entries")
                .value_name("N")
                .long_help(
                    "Aborts program execution when trying to store more than N entries in the \
                     cache. This helps to limit memory consumption.",
                )
                .takes_value(true)
                .validator(|s: String| -> result::Result<(), String> {
                    s.parse::<usize>().map(|_| ()).map_err(|e| e.to_string())
                }),
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
                     run in a terminal.",
                ),
        )
        .arg(
            a(
                "S",
                "stats",
                "Prints detailed statistics like scans per file type",
            ).alias("statistics"),
        )
        .arg(
            a("s", "stutter", "Sleeps SLEEP ms after each file access")
                .value_name("SLEEP")
                .long_help(
                    "Sleeps SLEEP milliseconds after each file to reduce I/O load. Files \
                     loaded from the cache are not subject to stuttering.",
                )
                .takes_value(true)
                .validator(validate_stutter),
        )
        .arg(a("v", "verbose", "Additional output"))
        .arg(
            a(
                "d",
                "debug",
                "Shows every file opened and lots of other stuff. Implies --verbose.",
            ).display_order(1000),
        )
        .arg(
            a("q", "quickcheck", "Scans only the first SIZE kB of a file")
                .takes_value(true)
                .long_help(
                    "Gives up if no Nix store references are found in the first SIZE kilobytes of \
                     a file. Assumes that at least one Nix store reference is located near the \
                     beginning. Speeds up scanning large files considerably.",
                )
                .value_name("SIZE")
                .default_value("256")
                .validator(|s: String| -> result::Result<(), String> {
                    s.parse::<u32>().map(|_| ()).map_err(|e| e.to_string())
                }),
        )
        .arg(
            a("e", "exclude", "Skips files matching GLOB")
                .long_help("Skips files matching GLOB. May be given multiple times.")
                .value_name("GLOB")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1),
        )
        .arg(
            a("i", "include", "Scans only files matching GLOB")
                .long_help(
                    "Scans only files matching GLOB. May be given multiple times. Note \
                     including individual files shows no effect if their containing directory is \
                     matched by an exclude glob.",
                )
                .value_name("GLOB")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1),
        )
        .arg(
            a("E", "exclude-from", "Loads exclude globs from FILE")
                .long_help(
                    "Loads exclude globs from FILE, which is expected to be in .gitignore format. \
                     May be given multiple times.",
                )
                .value_name("FILE")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1),
        )
        .arg(
            a(
                "z",
                "unzip",
                "Scans inside ZIP archives for files matching GLOB.",
            ).long_help(
                "Unpacks all files with matching GLOB as ZIP archives and scans inside. \
                 Accepts a comma-separated list of glob patterns. [example: *.zip,*.egg]",
            )
                .value_name("GLOB,...")
                .takes_value(true)
                .use_delimiter(true),
        )
}

fn main() {
    let app = App::from(args().get_matches());
    match app.run() {
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

    fn app(opts: &[&str]) -> App {
        let mut cmdline = vec!["app"];
        cmdline.extend_from_slice(&opts);
        cmdline.push("dir");
        App::from(args().get_matches_from(cmdline))
    }

    #[test]
    fn overrides_should_be_collected_and_prefixed() {
        let a = app(&["-i", "glob1", "-e", "glob2", "-i", "glob3"]);
        assert_eq!(vec!["!glob2", "glob1", "glob3"], a.overrides);
    }

    #[test]
    fn list_should_disable_register() {
        let a = app(&[]);
        assert!(!a.output.list);
        assert!(a.register);

        let a = app(&["--list"]);
        assert!(a.output.list);
        assert!(!a.register);

        let a = app(&["--list", "--register"]);
        assert!(a.output.list);
        assert!(a.register);
    }
}
