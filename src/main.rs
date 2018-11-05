// TODO failure
// TODO --ignore-warnings option
#![recursion_limit = "256"]

extern crate atty;
extern crate bytesize;
extern crate chrono;
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
extern crate structopt;
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
use errors::*;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use output::{p2s, Output};
use registry::{GCRoots, NullGCRoots, Register};
use statistics::Statistics;
use std::fs;
use std::ops::DerefMut;
use std::path::PathBuf;
use std::time::Duration;
use storepaths::Cache;
use structopt::StructOpt;
use users::os::unix::UserExt;

static STORE: &str = "/nix/store/";
static GC_PREFIX: &str = "/nix/var/nix/gcroots/profiles/per-user";
static DOTEXCLUDE: &str = ".userscan-ignore";

fn add_dotexclude<U: users::Users>(mut wb: WalkBuilder, u: &U) -> Result<WalkBuilder> {
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

#[derive(Debug, Clone, Default)]
pub struct App {
    opt: Opt,
    output: Output,
    overrides: Vec<String>,
    register: bool,
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
        for p in &self.opt.excludefrom {
            if let Some(err) = wb.add_ignore(p) {
                return Err(err).chain_err(|| "failed to load exclude file".to_owned());
            }
        }
        add_dotexclude(wb, &users::cache::UsersCache::new())
    }

    fn scanner(&self) -> Result<scan::Scanner> {
        let mut ob = OverrideBuilder::new(&self.opt.startdir);
        for glob in &self.opt.unzip {
            ob.add(glob)?;
        }
        Ok(scan::Scanner::new(self.opt.quickcheck, ob.build()?))
    }

    fn gcroots(&self) -> Result<Box<Register>> {
        if self.opt.register {
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
        match self.opt.cache {
            Some(ref f) => Cache::new(self.opt.cachelimit).open(f),
            None => Ok(Cache::new(self.opt.cachelimit)),
        }
    }

    fn statistics(&self) -> Statistics {
        Statistics::new(self.opt.statistics, self.output.list)
    }

    /// Normalized directory where scanning starts.
    ///
    /// Don't use this for user messages, they should print out `self.opt.startdir` instead.
    fn startdir(&self) -> Result<PathBuf> {
        self.opt
            .startdir
            .canonicalize()
            .chain_err(|| format!("start dir {} is not accessible", p2s(&self.opt.startdir)))
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

impl From<Opt> for App {
    fn from(opt: Opt) -> Self {
        let output = Output::from(&opt);
        let mut overrides = vec![];
        overrides.extend(opt.exclude.iter().map(|e| format!("!{}", e)));
        overrides.extend(opt.include.iter().map(|i| i.to_owned()));
        let register = opt.register || !opt.list;

        App {
            opt,
            output,
            overrides,
            register,
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

fn parse_kb(arg: &str) -> Result<ByteSize> {
    let n = arg.parse()?;
    Ok(ByteSize::kib(n))
}

fn parse_sleep(arg: &str) -> Result<Duration> {
    let s: f32 = arg.parse()?;
    if s < 0.0 || s >= 1000.0 {
        Err(ErrorKind::SleepOutOfBounds(s).into())
    } else {
        Ok(Duration::from_micros((s * 1e3) as u64))
    }
}

#[derive(StructOpt, Debug, Clone, Default)]
#[structopt(
    author = "© Flying Circus Internet Operations GmbH and contributors.",
    raw(usage = "USAGE.as_str()", after_help = "AFTER_HELP.as_str()")
)]
struct Opt {
    /// Starts scan in DIRECTORY
    #[structopt(
        value_name = "DIRECTORY",
        raw(required = "true"),
        parse(from_os_str)
    )]
    startdir: PathBuf,
    /// Only prints Nix store references while scanning (doesn't register)
    #[structopt(
        short = "l",
        long = "list",
        raw(display_order = "1"),
        long_help = "Prints Nix store references while scanning. GC roots are not registered when \
                     this option is active. Specify -r/--register in addition to get both listing \
                     and registration"
    )]
    list: bool,
    /// Registers references even when in list mode
    #[structopt(
        short = "r",
        long = "register",
        raw(display_order = "2"),
        long_help = "Registers references even when in list mode. Registration is enabled by \
                     default if -l/--list not given"
    )]
    register: bool,
    /// Keeps results between runs in FILE
    #[structopt(
        short = "c",
        long = "cache",
        value_name = "FILE",
        parse(from_os_str),
        long_help = "Caches scan results in FILE to avoid re-scanning unchanged files. The cache \
                     is kept as a compressed messagepack file"
    )]
    cache: Option<PathBuf>,
    /// Limits cache to N entries
    #[structopt(
        short = "L",
        long = "cache-limit",
        value_name = "N",
        long_help = "Aborts program execution when trying to store more than N entries in the \
                     cache. This helps to limit memory consumption"
    )]
    cachelimit: Option<usize>,
    /// Prints each file with references on a single line
    #[structopt(short = "1", long = "oneline")]
    oneline: bool,
    /// Funky colorful output
    #[structopt(
        short = "C",
        long = "color",
        value_name = "WHEN",
        default_value = "auto",
        raw(
            takes_value = "true",
            possible_values = r#"&["always", "never", "auto"]"#
        ),
        long_help = r#"Enables colored output. If set to "auto", color is on if run in a terminal"#
    )]
    color: String,
    /// Prints detailed statistics like scans per file type
    #[structopt(short = "S", long = "stats", alias = "statistics")]
    statistics: bool,
    /// Sleeps SLEEP ms after each file access
    #[structopt(
        short = "s",
        long = "stutter",
        value_name = "SLEEP",
        parse(try_from_str = "parse_sleep"),
        long_help = "Sleeps SLEEP milliseconds after each file to reduce I/O load. Files \
                     loaded from the cache are not subject to stuttering"
    )]
    sleep: Option<Duration>,
    /// Additional output
    #[structopt(short = "v", long = "verbose")]
    verbose: bool,
    /// Shows every file opened and lots of other stuff (implies --verbose)
    #[structopt(short = "d", long = "debug", raw(display_order = "100"))]
    debug: bool,
    /// Scans only the first SIZE kB of a file
    #[structopt(
        short = "q",
        long = "quickcheck",
        default_value = "512",
        value_name = "SIZE",
        parse(try_from_str = "parse_kb"),
        long_help = "Gives up if no Nix store references are found in the first SIZE kilobytes of \
                     a file. Assumes that at least one Nix store reference is located near the \
                     beginning. Speeds up scanning large files considerably"
    )]
    quickcheck: ByteSize,
    /// Skips files matching GLOB
    #[structopt(
        short = "e",
        long = "exclude",
        value_name = "GLOB",
        raw(number_of_values = "1"),
        long_help = "Skips files matching GLOB. May be given multiple times"
    )]
    exclude: Vec<String>,
    /// Scans only files matching GLOB
    #[structopt(
        short = "i",
        long = "include",
        value_name = "GLOB",
        raw(number_of_values = "1"),
        long_help = "Scans only files matching GLOB. may be given multiple times. note \
                     including individual files shows no effect if their containing directory is \
                     matched by an exclude glob"
    )]
    include: Vec<String>,
    /// Loads exclude globs from FILE
    #[structopt(
        short = "E",
        long = "excludefrom",
        value_name = "FILE",
        raw(number_of_values = "1"),
        parse(from_os_str),
        long_help = "Loads exclude globs from FILE, which is expected to be in .gitignore format. \
                     May be given multiple times"
    )]
    excludefrom: Vec<PathBuf>,
    /// Scans inside ZIP archives for files matching GLOB
    #[structopt(
        short = "z",
        long = "unzip",
        raw(use_delimiter = "true"),
        long_help = "Unpacks all files with matching GLOB as ZIP archives and scans inside. \
                     Accepts a comma-separated list of glob patterns [example: *.zip,*.egg]"
    )]
    unzip: Vec<String>,
}

fn main() {
    let app = App::from(Opt::from_args());
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
        let mut argv = vec!["userscan"];
        argv.extend_from_slice(opts);
        argv.push("dir");
        App::from(Opt::from_iter(&argv))
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

    #[test]
    fn sleep_value_in_ms() {
        let a = app(&["--stutter=50"]);
        assert_eq!(a.opt.sleep.unwrap(), Duration::from_millis(50));
    }
}
