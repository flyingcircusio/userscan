extern crate bytesize;
extern crate ignore;
extern crate users;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate lazy_static;

macro_rules! eprintln {
    ($($tt:tt)*) => {{
        use std::io::{BufWriter, Write};
        let stderr = ::std::io::stderr();
        let stderr = stderr.lock();
        let mut stderr = BufWriter::new(stderr);
        if let Err(_) = writeln!(stderr, $($tt)*) {
            ::std::process::exit(32) // broken pipe
        }
    }}
}

mod errors;
mod scan;
mod walk;
mod output;
mod registry;

use bytesize::ByteSize;
use clap::{Arg, App, ArgMatches};
use errors::*;
use registry::GCRoots;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    startdir: PathBuf,
    cache: PathBuf,
    give_up: ByteSize,
    list: bool,
    verbose: bool,
    debug: bool,
    register: bool,
}

impl Args {
    fn parallel_walker(&self) -> ignore::WalkParallel {
        ignore::WalkBuilder::new(&self.startdir).hidden(false).build_parallel()
    }

    fn scanner(&self) -> scan::Scanner {
        scan::Scanner::new(self.give_up)
    }

    fn gcroots(&self) -> Result<GCRoots> {
        let username = match users::get_current_username() {
            Some(u) => u,
            _ => return Err("failed to query current user name".into())
        };
        GCRoots::new(&username, &self.startdir, self.verbose)
            .chain_err(|| "Failed to create gcroots registry")
    }
}

impl Default for Args {
    fn default() -> Self {
        Args {
            startdir: PathBuf::new(),
            cache: PathBuf::new(),
            give_up: ByteSize::mib(1),
            list: false,
            verbose: false,
            debug: false,
            register: false,
        }
    }
}

impl<'a> From<ArgMatches<'a>> for Args {
    fn from(a: ArgMatches) -> Self {
        Args {
            startdir: a.value_of_os("DIRECTORY").unwrap_or_default().into(),
            list: a.is_present("l"),
            register: !a.is_present("R"),
            verbose: a.is_present("v") || a.is_present("d"),
            debug: a.is_present("d"),
            ..Args::default()
        }
    }
}

fn parse_args() -> Args {
    let arg = |short, long, help| Arg::with_name(short).short(short).long(long).help(help);
    App::new(crate_name!())
        .version(crate_version!())
        .about(crate_description!())
        .arg(Arg::with_name("DIRECTORY").required(true).help(
            "Start scan in DIRECTORY",
        ))
        .arg(arg("l", "list", "Shows files containing Nix store references"))
        .arg(arg("R", "no-register", "Don't register found references with the Nix GC"))
        .arg(arg("v", "verbose", "Additional output"))
        .arg(arg("d", "debug", "Prints every file opened"))
        .get_matches()
        .into()
}

fn main() {
    let args = Arc::new(parse_args());
    match walk::run(args) {
        Err(ref err) => {
            eprintln!("{}: ERROR: {}", crate_name!(), output::fmt_error_chain(err));
            std::process::exit(2)
        }
        Ok(exitcode) => std::process::exit(exitcode),
    }
}
