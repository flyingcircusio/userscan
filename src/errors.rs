use crate::cachemap;
use std::io;
use std::path::PathBuf;
use thiserror::Error;
use users::uid_t;
use zip::result::ZipError;

#[derive(Debug, Error)]
pub enum UErr {
    #[error("internal: abort directory walk")]
    WalkAbort,
    #[error("DirEntry for '{0}' does not contain metadata; cannot process")]
    DentNoMetadata(PathBuf),
    #[error("Cache limit {0} exceeded")]
    CacheFull(usize),
    #[error("File '{0}' has an unknown file type - don't know how to handle that")]
    FiletypeUnknown(PathBuf),
    #[error("Failed to locate UID {0} in passwd database")]
    UnknownUser(uid_t),
    #[error("Failed to unpack ZIP archive '{0}': {1}")]
    ZIP(PathBuf, #[source] ZipError),
    #[error("Cannot determine current user. Who am I?")]
    WhoAmI,
    #[error("startdir must be an absolute path")]
    Relative,
    #[error("Directory traversal error")]
    Traverse(#[from] ignore::Error),
    #[error("Failed to create '{0}'")]
    Create(PathBuf, #[source] io::Error),
    #[error("Failed to remove '{0}'")]
    Remove(PathBuf, #[source] io::Error),
    #[error("Failed to read '{0}'")]
    Read(PathBuf, #[source] io::Error),
    #[error("Failed to read link '{0}'")]
    ReadLink(PathBuf, #[source] io::Error),
    #[error("Failed to open '{0}'")]
    Open(PathBuf, #[source] io::Error),
    #[error("Failed to determine current directory")]
    CWD(#[source] io::Error),
    #[error("Failed to load cache from '{0}'")]
    LoadCache(PathBuf, #[source] cachemap::Error),
    #[error("Failed to save cache to '{0}'")]
    SaveCache(PathBuf, #[source] cachemap::Error),
    #[error("I/O error")]
    IO(#[from] io::Error),
}

pub type Result<T, E = UErr> = ::std::result::Result<T, E>;
