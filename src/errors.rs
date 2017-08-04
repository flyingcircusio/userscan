use clap;
use ignore;
use std::path::PathBuf;

error_chain! {
    foreign_links {
        Args(clap::Error);
        Fmt(::std::fmt::Error);
        Ignore(ignore::Error);
        Io(::std::io::Error);
        StripPrefix(::std::path::StripPrefixError);
    }

    errors {
        WalkAbort {
            description("internal: abort walk")
            display("")
        }

        WalkContinue {
            description("internal: skip this entry and continue")
            display("")
        }

        DentNoMetadata(path: PathBuf) {
            description("cannot process direntry which contains no metadata")
            display("DirEntry for {} does not contain metadata; cannot process", path.display())
        }

        CacheNotFound {
            description("internal: cache miss")
            display("")
        }
    }
}
