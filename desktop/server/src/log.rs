//! One log line on stderr, prefixed with local wall-clock time: the server's
//! journal can then date a burst of errors.

pub fn log(line: &str) {
    eprintln!("{} {line}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
}
