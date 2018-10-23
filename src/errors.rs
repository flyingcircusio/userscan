#![allow(unused_doc_comments)]

use clap;
use ignore;
use std::path::PathBuf;

error_chain! {
    foreign_links {
        Args(clap::Error);
        Fmt(::std::fmt::Error);
        Float(::std::num::ParseFloatError);
        Ignore(ignore::Error);
        Int(::std::num::ParseIntError);
        Io(::std::io::Error);
        MiniLZO(::minilzo::Error);
        RMPDecode(::rmp_serde::decode::Error);
        RMPEncode(::rmp_serde::encode::Error);
        StripPrefix(::std::path::StripPrefixError);
        Zip(::zip::result::ZipError);
    }

    errors {
        WalkAbort {
            description("internal: abort walk")
        }

        DentNoMetadata(path: PathBuf) {
            description("cannot process direntry which contains no metadata")
            display("DirEntry for {} does not contain metadata; cannot process", path.display())
        }

        CacheFull(max: usize) {
            description("Cache is full - terminate and don't change CG anymore")
            display("cache limit {} exceeded", max)
        }

        SleepOutOfBounds(sleep: f32) {
            description("--sleep argument is either negative or too large")
            display("duration '{}' must be less than 1000ms", sleep),
        }

        FiletypeUnknown(path: PathBuf) {
            description("no idea how to handle this direntry"),
            display("file {} has an unknown file type - don't know how to handle that",
                    path.display())
        }
    }
}
