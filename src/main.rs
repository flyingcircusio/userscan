extern crate bytesize;
extern crate colored;
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
use std::result;
use std::sync::Arc;

static GC_PREFIX: &str = "/nix/var/nix/gcroots/profiles/per-user";

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    startdir: PathBuf,
    quickcheck: ByteSize,
    list: bool,
    register: bool,
    output: Output,
}

impl Args {
    fn parallel_walker(&self) -> ignore::WalkParallel {
        ignore::WalkBuilder::new(&self.startdir)
            .git_exclude(false)
            .git_global(false)
            .git_ignore(false)
            .hidden(false)
            .parents(false)
            .build_parallel()
    }

    fn scanner(&self) -> scan::Scanner {
        scan::Scanner::new(self.quickcheck.as_usize())
    }

    fn gcroots(&self) -> Result<Box<Register>> {
        if !self.register {
            return Ok(Box::new(NullGCRoots));
        }
        let username = match users::get_current_username() {
            Some(u) => u,
            _ => return Err("failed to query current user name".into()),
        };
        let gc = GCRoots::new(&Path::new(GC_PREFIX).join(&username), &self.startdir)?;
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
            list: a.is_present("l") || a.is_present("R"),
            register: !a.is_present("R"),
            output: Output::new(
                a.is_present("v"),
                a.is_present("d"),
                a.is_present("1"),
                if a.is_present("C") { Some(true) } else { None },
            ),
            quickcheck: ByteSize::kib(a.value_of_lossy("q").unwrap().parse::<usize>().unwrap()),
        }
    }
}

fn parse_args() -> Args {
    let arg = |short, long, help| Arg::with_name(short).short(short).long(long).help(help);
    let kb_val = |s: String| -> result::Result<(), String> {
        s.parse::<u32>().map(|_| ()).map_err(|e| e.to_string())
    };
    App::new(crate_name!())
        .version(crate_version!())
        .about(crate_description!())
        .arg(Arg::with_name("DIRECTORY").required(true).help(
            "Start scan in DIRECTORY",
        ))
        .arg(
            arg("l", "list", "Shows files containing Nix store references").display_order(1),
        )
        .arg(
            arg(
                "R",
                "no-register",
                "Don't register found references, implies --list",
            ).display_order(2),
        )
        .arg(arg(
            "1",
            "oneline",
            "Outputs each file with references on a single line",
        ))
        .arg(arg("C", "color", "Funky colorful output"))
        .arg(arg("v", "verbose", "Additional output"))
        .arg(arg(
            "d",
            "debug",
            "Prints every file opened, implies --verbose",
        ))
        .arg(
            arg(
                "q",
                "quickcheck",
                "Give up if no Nix store reference is found in the first <q> kbytes of a file",
            ).takes_value(true)
                .default_value("64")
                .validator(kb_val),
        )
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
