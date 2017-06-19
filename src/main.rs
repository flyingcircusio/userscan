#[macro_use]
extern crate clap;

mod walker;

use walker::Walker;
use std::path::PathBuf;
use std::io::Write;


macro_rules! die {
    ( $( $e:expr ),* ) => {{
        writeln!(::std::io::stderr(), $($e),*).expect("error while writing to stderr");
        ::std::process::exit(2);
    }}
}

fn main() {
    let args = clap_app!(guardix =>
        (version: crate_version!())
        (about: crate_description!())
        (@arg DIRECTORY: +required "Start scan in DIRECTORY")
    ).get_matches();

    let startdir: PathBuf = args.value_of_os("DIRECTORY").unwrap().into();

    let walker = match Walker::new(&startdir).walk() {
        Ok(walker) => walker,
        Err(e) => die!("{}: error while traversing directories: {}", crate_name!(), e),
    };

    for res in walker {
        match res {
            Ok(dent) => println!("{}", dent.path().display()),
            Err(e) => die!("{}: skipped: {}", crate_name!(), e),
        }
    }
}
