extern crate ignore;

use std::path::Path;
use std::error::Error;

#[derive(Debug, Clone)]
pub struct Walker<'a> {
    startdir: &'a Path,
}

impl<'a> Walker<'a> {
    pub fn new(path: &Path) -> Walker {
        Walker { startdir: path }
    }

    pub fn walk(&self) -> Result<ignore::Walk, Box<Error>> {
        Ok(ignore::WalkBuilder::new(self.startdir).hidden(false).build())
    }
}
