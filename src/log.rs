//! Logging (port of `OpenLog` / `PrintError` / `PrintDebug` / `PrintLog`).
//!
//! * `print_error!`  - always printed to stdout and written to the log file.
//! * `print_debug!`  - printed to stdout only when debug output is enabled; always written to the log file.
//! * `print_log!`    - log file only.
//! * `eso_error!` / `eso_debug_error!` - same as the print macros but evaluate to an [`EsoError`]
//!   so they can be used as `return Err(eso_error!("..."))`.

use std::fmt::Arguments;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

type LogCallback = Box<dyn Fn(&str) + Send + Sync>;

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
static LOG_CALLBACK: Mutex<Option<LogCallback>> = Mutex::new(None);
static OUTPUT_DEBUG_LOG: AtomicBool = AtomicBool::new(false);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Opens (truncating) the log file.  Returns false if it could not be created.
pub fn open_log(filename: &str) -> bool {
    match File::create(filename) {
        Ok(f) => {
            *lock(&LOG_FILE) = Some(f);
            true
        }
        Err(_) => false,
    }
}

pub fn set_output_debug_log(enabled: bool) {
    OUTPUT_DEBUG_LOG.store(enabled, Ordering::Relaxed);
}

pub fn output_debug_log() -> bool {
    OUTPUT_DEBUG_LOG.load(Ordering::Relaxed)
}

/// Optional hook (used by GUIs) that receives every message that is printed to the console.
pub fn set_log_callback(cb: Option<LogCallback>) {
    *lock(&LOG_CALLBACK) = cb;
}

fn write_log_line(msg: &str) {
    let mut guard = lock(&LOG_FILE);
    if let Some(f) = guard.as_mut() {
        let ts = chrono::Local::now().format("%H:%M:%S%.3f");
        let _ = writeln!(f, "{} -- {}", ts, msg);
        let _ = f.flush();
    }
}

fn console(msg: &str) {
    println!("{}", msg);
    let _ = std::io::stdout().flush();
    if let Some(cb) = lock(&LOG_CALLBACK).as_ref() {
        cb(msg);
    }
}

pub fn print_error(args: Arguments<'_>) {
    let msg = args.to_string();
    console(&msg);
    write_log_line(&msg);
}

pub fn print_debug(args: Arguments<'_>) {
    let msg = args.to_string();
    if output_debug_log() {
        console(&msg);
    }
    write_log_line(&msg);
}

pub fn print_log(args: Arguments<'_>) {
    write_log_line(&args.to_string());
}

#[macro_export]
macro_rules! print_error { ($($arg:tt)*) => { $crate::log::print_error(format_args!($($arg)*)) }; }

#[macro_export]
macro_rules! print_debug { ($($arg:tt)*) => { $crate::log::print_debug(format_args!($($arg)*)) }; }

#[macro_export]
macro_rules! print_log { ($($arg:tt)*) => { $crate::log::print_log(format_args!($($arg)*)) }; }

/// Logs via `print_error!` and evaluates to an `EsoError` with the same text.
#[macro_export]
macro_rules! eso_error {
    ($($arg:tt)*) => {{
        let __m = format!($($arg)*);
        $crate::log::print_error(format_args!("{}", __m));
        $crate::error::EsoError::new(__m)
    }};
}

/// Logs via `print_debug!` and evaluates to an `EsoError` with the same text.
#[macro_export]
macro_rules! eso_debug_error {
    ($($arg:tt)*) => {{
        let __m = format!($($arg)*);
        $crate::log::print_debug(format_args!("{}", __m));
        $crate::error::EsoError::new(__m)
    }};
}
