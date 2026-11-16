//! Minimal stderr logger for `-v` / `-vv`.

use log::{LevelFilter, Log, Metadata, Record};

struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, r: &Record<'_>) {
        eprintln!("[{}] {}", r.level().as_str().to_lowercase(), r.args());
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

/// 0 = errors only, 1 = warnings, 2+ = debug.
pub fn init(verbosity: u8) {
    let level = match verbosity {
        0 => LevelFilter::Error,
        1 => LevelFilter::Warn,
        _ => LevelFilter::Debug,
    };
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(level);
    }
}
