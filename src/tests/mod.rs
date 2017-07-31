extern crate nix;

use self::nix::unistd::chdir;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use super::*;

lazy_static! {
    static ref FIXTURES: PathBuf = Path::new(file!()).parent().unwrap().join("../../fixtures")
        .canonicalize().unwrap();
}

pub fn args<P: AsRef<Path>>(startdir: P) -> Args {
    chdir(&*FIXTURES).expect("chdir(fixtures) failed");
    let mut a = Args::default();
    a.startdir = PathBuf::from(startdir.as_ref());
    a
}

pub fn assert_eq_vecs<R, F>(result: Vec<R>, map_res: F, expect: &[&str])
where
    F: for<'a> Fn(&'a R) -> &'a str,
{
    let mut expected: HashSet<&str> = expect.into_iter().map(|p| *p).collect();
    let mut unexpected = Vec::new();
    for r in result {
        let key = map_res(&r);
        if !expected.remove(key) {
            unexpected.push(key.to_owned());
        }
    }
    if !unexpected.is_empty() {
        panic!("unexpected results: {:?}", unexpected);
    }
    if !expected.is_empty() {
        panic!("missing expected results: {:?}", expected);
    }
}

pub fn dent(path: &str) -> ignore::DirEntry {
    args(path)
        .walker()
        .build()
        .next()
        .unwrap_or_else(|| panic!("didn't find path: {}", path))
        .unwrap_or_else(|e| panic!("unable to read path: {}", e))
}
