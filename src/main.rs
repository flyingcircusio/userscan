extern crate bytesize;
extern crate env_logger;
extern crate ignore;
extern crate users;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;

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
mod output;
mod registry;
mod scan;
mod walk;

use bytesize::ByteSize;
use output::Output;
use clap::{Arg, App, ArgMatches};
use errors::*;
use registry::{GCRoots, NullGCRoots, Register};
use std::path::{Path, PathBuf};
use std::sync::Arc;

static GC_PREFIX: &str = "/nix/var/nix/gcroots/profiles/per-user";

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    startdir: PathBuf,
    give_up: ByteSize,
    list: bool,
    register: bool,
    output: Output,
}

impl Args {
    fn parallel_walker(&self) -> ignore::WalkParallel {
        ignore::WalkBuilder::new(&self.startdir)
            .hidden(false)
            .git_global(false)
            .git_ignore(false)
            .git_exclude(false)
            .build_parallel()
    }

    fn scanner(&self) -> scan::Scanner {
        scan::Scanner::new(self.give_up)
    }

    fn gcroots(&self) -> Result<Box<Register>> {
        if !self.register {
            return Ok(Box::new(NullGCRoots));
        }
        let username = match users::get_current_username() {
            Some(u) => u,
            _ => return Err("failed to query current user name".into()),
        };
        let gc = GCRoots::new(&Path::new(GC_PREFIX).join(&username))?;
        Ok(Box::new(gc))
    }

    fn output(&self) -> &Output {
        &self.output
    }
}

impl<'a> From<ArgMatches<'a>> for Args {
    fn from(a: ArgMatches) -> Self {
        Args {
            startdir: a.value_of_os("DIRECTORY").unwrap_or_default().into(),
            give_up: ByteSize::mib(1),
            list: a.is_present("l") || a.is_present("R"),
            register: !a.is_present("R"),
            output: Output::new(a.is_present("v"), a.is_present("d"), a.is_present("1")),
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
        .arg(
            arg("l", "list", "Shows files containing Nix store references").display_order(1),
        )
        .arg(arg(
            "R",
            "no-register",
            "Don't register found references, implies --list",
        ))
        .arg(arg(
            "1",
            "oneline",
            "Outputs each file with references on a single line",
        ))
        .arg(arg("v", "verbose", "Additional output"))
        .arg(arg(
            "d",
            "debug",
            "Prints every file opened, implies --verbose",
        ))
        .get_matches()
        .into()
}

fn main() {
    let args = Arc::new(parse_args());
    match walk::run(args) {
        Err(ref err) => {
            error!("{}", output::fmt_error_chain(err));
            std::process::exit(2)
        }
        Ok(exitcode) => std::process::exit(exitcode),
    }
}
