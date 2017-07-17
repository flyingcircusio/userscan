use clap;
use ignore;

error_chain! {
    foreign_links {
        Args(clap::Error);
        Fmt(::std::fmt::Error);
        Io(::std::io::Error);
        StripPrefix(::std::path::StripPrefixError);
        Ignore(ignore::Error);
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
    }
}
