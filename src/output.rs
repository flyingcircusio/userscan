use errors::*;

pub fn fmt_error_chain(err: &Error) -> String {
    err.iter().map(|e| format!("{}", e)).collect::<Vec<_>>().join(": ")
}

pub fn print_error_chain(err: &Error) {
    eprintln!("{}", fmt_error_chain(err))
}
