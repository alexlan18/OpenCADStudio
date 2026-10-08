//! Application log file: `cad.log` beside the executable.
//!
//! Every native run appends to `<directory of the executable>/cad.log`
//! (falling back to the per-user config directory when that folder is
//! read-only, e.g. an installation under `Program Files`). The file carries
//! what the `log` facade receives — the assistant's model calls and tool
//! calls, automation requests, panics and third-party warnings — with one
//! UTC timestamp per line, and rotates to `cad.log.1` past 10 MB so it never
//! grows without bound.
//!
//! Levels follow `--log LEVEL` / `RUST_LOG` with env_logger-style directives
//! (`debug`, `OpenCADStudio::app::assistant=trace,wgpu=warn`). Without
//! either, the application's own targets log at `info` and everything else
//! at `warn`, which keeps wgpu / winit chatter out of the file by default.

#![cfg(not(target_arch = "wasm32"))]

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Log file name, next to the executable.
pub const FILE_NAME: &str = "cad.log";
/// Rotate when the file would exceed this.
const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Targets that count as "ours" for the default level.
const APP_TARGET_PREFIX: &str = env!("CARGO_PKG_NAME");

static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Where the log is being written, once [`init`] has run.
pub fn path() -> Option<&'static Path> {
    PATH.get().and_then(|p| p.as_deref())
}

/// One `RUST_LOG` directive: a level for a target prefix (`None` = all).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directive {
    pub target: Option<String>,
    pub level: LevelFilter,
}

/// Parse `RUST_LOG`-style directives. Unknown pieces are ignored rather than
/// refused: a typo in an environment variable must not silence the log.
pub fn parse_directives(spec: &str) -> Vec<Directive> {
    spec.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter_map(|part| match part.split_once('=') {
            Some((target, level)) => parse_level(level).map(|level| Directive {
                target: Some(target.trim().to_string()),
                level,
            }),
            None => match parse_level(part) {
                Some(level) => Some(Directive { target: None, level }),
                // `RUST_LOG=wgpu` means "wgpu at trace", as in env_logger.
                None => Some(Directive {
                    target: Some(part.to_string()),
                    level: LevelFilter::Trace,
                }),
            },
        })
        .collect()
}

fn parse_level(text: &str) -> Option<LevelFilter> {
    match text.trim().to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// The level filter applying to `target`: the most specific directive wins;
/// without one, the application's own modules get `info` and other crates
/// `warn`, unless a bare level directive set a global level.
pub fn level_for(directives: &[Directive], target: &str) -> LevelFilter {
    let mut best: Option<(usize, LevelFilter)> = None;
    for directive in directives {
        match &directive.target {
            Some(prefix) if target == prefix || target.starts_with(&format!("{prefix}::")) => {
                if best.is_none_or(|(len, _)| prefix.len() >= len) {
                    best = Some((prefix.len(), directive.level));
                }
            }
            Some(_) => {}
            None => {
                if best.is_none_or(|(len, _)| len == 0) {
                    best = Some((0, directive.level));
                }
            }
        }
    }
    if let Some((_, level)) = best {
        return level;
    }
    if target == APP_TARGET_PREFIX || target.starts_with(&format!("{APP_TARGET_PREFIX}::")) {
        LevelFilter::Info
    } else {
        LevelFilter::Warn
    }
}

/// `2026-10-08T03:14:15.123Z` for a Unix timestamp in milliseconds.
pub fn timestamp(unix_millis: u64) -> String {
    let seconds = (unix_millis / 1000) as i64;
    let millis = unix_millis % 1000;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One log line (without the trailing newline).
pub fn format_line(unix_millis: u64, level: Level, target: &str, message: &str) -> String {
    // Keep each record on one line so the file greps cleanly.
    let message = message.replace('\n', "\\n");
    format!("{} {:<5} {}: {}", timestamp(unix_millis), level, target, message)
}

/// Whether a file of `len` bytes should be rotated before appending.
pub fn should_rotate(len: u64) -> bool {
    len >= MAX_BYTES
}

/// Candidate locations, most preferred first: beside the executable, then
/// the per-user config directory.
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        out.push(dir.join(FILE_NAME));
    }
    if let Some(dir) = crate::config::config_dir() {
        out.push(dir.join(FILE_NAME));
    }
    out
}

fn open_log(path: &Path) -> std::io::Result<File> {
    if let Ok(metadata) = std::fs::metadata(path) {
        if should_rotate(metadata.len()) {
            let rotated = path.with_extension("log.1");
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(path, &rotated);
        }
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    OpenOptions::new().create(true).append(true).open(path)
}

struct FileLogger {
    file: Mutex<File>,
    directives: Vec<Directive>,
    /// Echo to stderr too (set when `--log` / `RUST_LOG` asked for output).
    echo: bool,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= level_for(&self.directives, metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            now_millis(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{line}");
        }
        if self.echo {
            eprintln!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// Install the file logger. Call once, early; a second call is a no-op.
///
/// `spec` is the `--log` value when given; `RUST_LOG` is read otherwise.
pub fn init(spec: Option<&str>) {
    let spec = spec
        .map(str::to_owned)
        .or_else(|| std::env::var("RUST_LOG").ok())
        .filter(|s| !s.trim().is_empty());
    let directives = spec.as_deref().map(parse_directives).unwrap_or_default();
    let echo = spec.is_some();

    let mut opened = None;
    for candidate in candidate_paths() {
        match open_log(&candidate) {
            Ok(file) => {
                opened = Some((candidate, file));
                break;
            }
            Err(error) => eprintln!("cad.log: cannot open {}: {error}", candidate.display()),
        }
    }
    let Some((path, file)) = opened else {
        let _ = PATH.set(None);
        return;
    };

    let max = directives
        .iter()
        .map(|d| d.level)
        .max()
        .unwrap_or(LevelFilter::Info);
    let logger = FileLogger {
        file: Mutex::new(file),
        directives,
        echo,
    };
    if log::set_boxed_logger(Box::new(logger)).is_err() {
        return;
    }
    log::set_max_level(max);
    let _ = PATH.set(Some(path.clone()));

    log::info!(
        target: APP_TARGET_PREFIX,
        "---- {} {} ({}) started, pid {}, log {} ----",
        APP_TARGET_PREFIX,
        env!("OCS_APP_VERSION"),
        env!("OCS_GIT_REV"),
        std::process::id(),
        path.display()
    );

    // Panics reach the file too; the crash-log hook installed before this
    // keeps writing its own detailed report.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        log::error!(target: APP_TARGET_PREFIX, "panic: {payload} at {location}");
        log::logger().flush();
        previous(info);
    }));
}

/// Cut a string for the log: at most `max` chars, marking the cut.
pub fn preview(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_iso8601_utc_with_millis() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(timestamp(1_791_009_600_123), "2026-10-03T06:40:00.123Z");
        assert_eq!(timestamp(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn directives_parse_like_env_logger() {
        assert_eq!(parse_directives("debug"), vec![Directive { target: None, level: LevelFilter::Debug }]);
        let parsed = parse_directives("OpenCADStudio::app::assistant=trace, wgpu=warn,bogus=nope,naga");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].target.as_deref(), Some("OpenCADStudio::app::assistant"));
        assert_eq!(parsed[0].level, LevelFilter::Trace);
        assert_eq!(parsed[1].target.as_deref(), Some("wgpu"));
        assert_eq!(parsed[2], Directive { target: Some("naga".into()), level: LevelFilter::Trace });
    }

    #[test]
    fn default_levels_keep_third_party_noise_out() {
        let none: Vec<Directive> = Vec::new();
        assert_eq!(level_for(&none, "OpenCADStudio::app::assistant::agent"), LevelFilter::Info);
        assert_eq!(level_for(&none, "OpenCADStudio"), LevelFilter::Info);
        assert_eq!(level_for(&none, "wgpu_core::device"), LevelFilter::Warn);
        let global = parse_directives("debug");
        assert_eq!(level_for(&global, "wgpu_core::device"), LevelFilter::Debug);
        let mixed = parse_directives("warn,OpenCADStudio::app=trace");
        assert_eq!(level_for(&mixed, "OpenCADStudio::app::assistant"), LevelFilter::Trace);
        assert_eq!(level_for(&mixed, "OpenCADStudio::appendix"), LevelFilter::Warn);
        assert_eq!(level_for(&mixed, "winit"), LevelFilter::Warn);
    }

    #[test]
    fn lines_are_single_line_and_rotation_threshold_holds() {
        let line = format_line(0, Level::Info, "OpenCADStudio::x", "first\nsecond");
        assert_eq!(line, "1970-01-01T00:00:00.000Z INFO  OpenCADStudio::x: first\\nsecond");
        assert!(!should_rotate(MAX_BYTES - 1));
        assert!(should_rotate(MAX_BYTES));
        assert_eq!(preview("héllo", 3), "hél…");
        assert_eq!(preview("hi", 3), "hi");
    }

    #[test]
    fn candidate_paths_start_beside_the_executable() {
        let paths = candidate_paths();
        assert!(!paths.is_empty());
        assert!(paths.iter().all(|p| p.file_name().is_some_and(|n| n == FILE_NAME)));
        let exe_dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        assert_eq!(paths[0].parent().unwrap(), exe_dir);
    }

    #[test]
    fn rotation_moves_the_old_file_aside() {
        let dir = std::env::temp_dir().join(format!("ocs-applog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        {
            let mut file = File::create(&path).unwrap();
            file.set_len(MAX_BYTES).unwrap();
            file.write_all(b"x").unwrap();
        }
        let mut file = open_log(&path).unwrap();
        writeln!(file, "fresh").unwrap();
        drop(file);
        assert!(dir.join("cad.log.1").exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
