use env_logger::LogBuilder;
use errors::*;
use log::{LogLevel, LogLevelFilter, LogRecord};

pub fn fmt_error_chain(err: &Error) -> String {
    err.iter()
        .map(|e| format!("{}", e))
        .collect::<Vec<_>>()
        .join(": ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    level: LogLevelFilter,
    oneline: bool,
    stdout_tty: bool,
    stderr_tty: bool,
}

impl Output {
    fn loginit(self) -> Self {
        let fmt = |r: &LogRecord| match r.level() {
            LogLevel::Warn | LogLevel::Error => {
                format!("{} {}: {}", crate_name!(), r.level(), r.args())
            }
            LogLevel::Info => format!("{}: {}", crate_name!(), r.args()),
            _ => format!("{}", r.args()),
        };
        LogBuilder::new()
            .format(fmt)
            .filter(None, self.level)
            .init()
            .expect("log init may only be called once");
        self
    }

    pub fn new(verbose: bool, debug: bool, oneline: bool) -> Output {
        Output {
            level: match (verbose, debug) {
                (_, true) => LogLevelFilter::Debug,
                (true, _) => LogLevelFilter::Info,
                _ => LogLevelFilter::Warn,
            },
            oneline: oneline,
            stdout_tty: false,
            stderr_tty: false,
        }.loginit()
    }
}
