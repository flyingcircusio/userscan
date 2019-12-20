#![recursion_limit = "256"]

#[macro_use]
extern crate clap;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;

mod cachemap;
mod errors;
mod output;
mod registry;
mod scan;
mod statistics;
mod storepaths;
#[cfg(test)]
mod tests;
mod walk;

use anyhow::{Context, Result};
use bytesize::ByteSize;
use errors::UErr;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use output::{p2s, Output};
use registry::{GCRoots, NullGCRoots, Register};
use statistics::Statistics;
use std::fs;
use std::ops::DerefMut;
use std::path::PathBuf;
use storepaths::Cache;
use structopt::StructOpt;
use users::os::unix::UserExt;

static STORE: &str = "/nix/store/";
static GC_PREFIX: &str = "/nix/var/nix/gcroots/per-user";
static DOTEXCLUDE: &str = ".userscan-ignore";

fn add_dotexclude<U: users::Users>(mut wb: WalkBuilder, u: &U) -> Result<WalkBuilder> {
    if let Some(me) = u.get_user_by_uid(u.get_effective_uid()) {
        let candidate = me.home_dir().join(DOTEXCLUDE);
        if candidate.exists() {
            if let Some(err) = wb.add_ignore(&candidate) {
                warn!("Invlid entry in ignore file {}: {}", p2s(candidate), err);
            }
        }
        Ok(wb)
    } else {
        Err(UErr::UnknownUser(u.get_effective_uid()).into())
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
                warn!("Problem with ignore file {}: {}", p2s(p), err);
            }
        }
        add_dotexclude(wb, &users::cache::UsersCache::new())
    }

    fn scanner(&self) -> Result<scan::Scanner> {
        let mut ob = OverrideBuilder::new(&self.opt.startdir);
        for glob in &self.opt.unzip {
            ob.add(glob)?;
        }
        let baseline = probes::load::read()?.fifteen;
        let max_load = match self.opt.load_increase {
            inc if inc <= 0.0 => 0.0,
            inc => baseline + inc * num_cpus::get() as f32,
        };
        debug!("Baseline load: {}, limit: {}", baseline, max_load);
        Ok(scan::Scanner::new(
            self.opt.quickcheck,
            ob.build()?,
            max_load,
        ))
    }

    fn gcroots(&self) -> Result<Box<dyn Register>> {
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
            Some(ref f) => Ok(Cache::new(self.opt.cache_limit).open(f)?),
            None => Ok(Cache::new(self.opt.cache_limit)),
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
            .with_context(|| format!("start dir {} is not accessible", p2s(&self.opt.startdir)))
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

#[derive(StructOpt, Debug, Clone, Default)]
#[structopt(
    author = "© Flying Circus Internet Operations GmbH and contributors.",
    after_help = AFTER_HELP.as_str()
)]
struct Opt {
    /// Starts scan in DIRECTORY
    #[structopt(value_name = "DIRECTORY", parse(from_os_str))]
    startdir: PathBuf,
    /// Only prints Nix store references while scanning (doesn't register)
    ///
    /// GC roots are not registered when this option is active. Specify -r/--register in addition
    /// to get both listing and registration.
    #[structopt(short, long, display_order(1))]
    list: bool,
    /// Registers references (enabled by default if --list if not given)
    #[structopt(short, long, display_order(2))]
    register: bool,
    /// Keeps results between runs in FILE
    ///
    /// Caches scan results in FILE to avoid re-scanning unchanged files. The cache is kept as a
    /// compressed messagepack file.
    #[structopt(short, long, value_name = "FILE", parse(from_os_str))]
    cache: Option<PathBuf>,
    /// Limits cache to N entries
    ///
    /// Aborts program execution when trying to store more than N entries in the cache. This helps
    /// to limit memory consumption.
    #[structopt(short = "L", long, value_name = "N")]
    cache_limit: Option<usize>,
    /// Prints each file with references on a single line
    #[structopt(short = "1", long)]
    oneline: bool,
    /// Funky colorful output
    ///
    /// Enables colored output. If set to "auto", color is on if run in a terminal.
    #[structopt(short = "C", long, value_name = "WHEN", default_value = "auto",
                possible_values(&["always", "never", "auto"])
    )]
    color: String,
    /// Prints detailed statistics like scans per file type
    #[structopt(short = "S", long = "stats", alias = "statistics")]
    statistics: bool,
    /// Displays additional output like scan times
    #[structopt(short, long)]
    verbose: bool,
    /// Shows every file opened and lots of other stuff (implies --verbose)
    #[structopt(short, long, display_order(100))]
    debug: bool,
    /// Scans only the first SIZE kB of a file
    ///
    /// Gives up if no Nix store references are found in the first SIZE kilobytes of a file.
    /// Assumes that at least one Nix store reference is located near the beginning. Speeds up
    /// scanning large files considerably.
    #[structopt(short, long, default_value = "512", value_name = "SIZE",
                parse(try_from_str = parse_kb))]
    quickcheck: ByteSize,
    /// Skips files matching GLOB
    ///
    /// Skips files matching GLOB. May be given multiple times.
    #[structopt(short, long, value_name = "GLOB", number_of_values(1))]
    exclude: Vec<String>,
    /// Scans only files matching GLOB
    ///
    /// Scans only files matching GLOB. may be given multiple times. note including individual
    /// files shows no effect if their containing directory is matched by an exclude glob.
    #[structopt(short, long, value_name = "GLOB", number_of_values(1))]
    include: Vec<String>,
    /// Loads exclude globs from FILE
    ///
    /// Loads exclude globs from FILE, which is expected to be in .gitignore format. May be given
    /// multiple times.
    #[structopt(
        short = "E",
        long,
        value_name = "FILE",
        number_of_values(1),
        parse(from_os_str)
    )]
    excludefrom: Vec<PathBuf>,
    /// Scans inside ZIP archives for files matching GLOB
    ///
    /// Unpacks all files with matching GLOB as ZIP archives and scans inside. Accepts a
    /// comma-separated list of glob patterns [example: *.zip,*.egg].
    #[structopt(short, long, use_delimiter(true))]
    unzip: Vec<String>,
    /// Pauses scanning if the current load1 goes over load15+L
    ///
    /// The baseline is determined at program startup. If there are multiple CPUs present,
    /// the increase is granted per CPU. Use '0.0' to disable.
    #[structopt(
        short = "p",
        long = "pause-load",
        default_value = "0.5",
        value_name = "L"
    )]
    load_increase: f32,
}

fn main() {
    let app = App::from(Opt::from_args());
    match app.run() {
        Err(ref err) => {
            error!("{:#?}", err);
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
}
