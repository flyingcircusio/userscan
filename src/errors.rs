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
            description("abort walk")
            display("abort walk due to an error condition")
        }
    }
}
