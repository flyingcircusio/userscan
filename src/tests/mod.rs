extern crate nix;

use self::nix::unistd::chdir;
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

lazy_static! {
    pub static ref FIXTURES: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
}

/// Quick creation of an App instance for testing.
pub fn app<P: AsRef<Path>>(startdir: P) -> App {
    chdir(&*FIXTURES).expect("chdir(fixtures) failed");
    let mut a = App::default();
    a.opt.unzip = vec!["*.zip".into()];
    a.opt.startdir = PathBuf::from(startdir.as_ref());
    a
}

/// Quick creation of an DirEntry instance for testing.
pub fn dent<P: AsRef<Path>>(path: P) -> ignore::DirEntry {
    app(&path)
        .walker()
        .unwrap()
        .build()
        .next()
        .unwrap_or_else(|| panic!("didn't find path: {}", path.as_ref().display()))
        .unwrap_or_else(|e| panic!("unable to read path: {}", e))
}

/// Tests equality of two Vecs with verbose diff reporting.
pub fn assert_eq_vecs<R, F>(result: Vec<R>, map_res: F, expect: &[&str])
where
    F: for<'a> Fn(&'a R) -> String,
{
    let mut expected: HashSet<&str> = expect.into_iter().map(|p| *p).collect();
    let mut unexpected = Vec::new();
    for r in result {
        let key = map_res(&r);
        if !expected.remove(&*key) {
            unexpected.push(key);
        }
    }
    if !unexpected.is_empty() {
        panic!("unexpected results: {:?}", unexpected);
    }
    if !expected.is_empty() {
        panic!("missing expected results: {:?}", expected);
    }
}
