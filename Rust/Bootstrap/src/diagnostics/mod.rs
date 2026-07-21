//! Opt-in deep diagnostics (cargo feature `diagnostics`).
//!
//! Everything in here is compiled out unless the crate is built with `--features diagnostics`, so
//! release builds pay nothing for it.
//!
//! What it provides:
//!   * a single catch-all log file at /sdcard/MelonLoader/debug.log that receives BOTH the native
//!     bootstrap log stream and every managed MelonLoader line (managed logging already funnels
//!     through `melonloader_print_string`, so teeing there captures all of it),
//!   * `debug!` forced on (normally it is silent in release unless --melonloader.debug is passed),
//!   * a full /proc/self/maps snapshot, so any raw address in a crash report can be attributed,
//!   * a signal handler that dumps registers and an unwound, symbol-attributed backtrace before
//!     chaining to whatever handler was installed before it (CoreCLR's).

pub mod crash_handler;

use std::{
    ffi::CString,
    io::Write,
    sync::Mutex,
};

use lazy_static::lazy_static;

pub const DEBUG_LOG_PATH: &str = "/sdcard/MelonLoader/debug.log";

lazy_static! {
    static ref DEBUG_LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
}

/// Truncate and open the catch-all log. Safe to call more than once.
pub fn init() {
    let _ = std::fs::create_dir_all("/sdcard/MelonLoader");
    let _ = std::fs::remove_file(DEBUG_LOG_PATH);

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(DEBUG_LOG_PATH);

    if let Ok(f) = file {
        if let Ok(mut guard) = DEBUG_LOG.lock() {
            *guard = Some(f);
        }
    }

    // Signals the managed side to force MelonDebug on (see MelonLaunchOptions.Load); there is no
    // practical way to pass --melonloader.debug on Android.
    std::env::set_var("LIMELOADER_DIAGNOSTICS", "1");

    write_line("=== LimeLoader diagnostics build ===");
    write_line(&format!("pid {}", std::process::id()));
    dump_maps();

    // Installed early so we still catch anything that blows up before the runtime is up. It is
    // installed a second time after CoreCLR initializes, so that ours runs first and then chains.
    crash_handler::install();
}

/// Append one line to the catch-all log (and mirror it to logcat).
pub fn write_line(msg: &str) {
    if let Ok(mut guard) = DEBUG_LOG.lock() {
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(msg.as_bytes());
            let _ = file.write_all(b"\n");
            let _ = file.flush();
        }
    }
    logcat(msg);
}

/// Mirror to logcat under the MelonLoader tag, so ADB.log alone is still useful.
pub fn logcat(msg: &str) {
    if let (Ok(tag), Ok(text)) = (CString::new("MelonLoader"), CString::new(msg)) {
        unsafe {
            android_liblog_sys::__android_log_write(4, tag.as_ptr(), text.as_ptr());
        }
    }
}

/// Snapshot every mapping, so a raw crash address from any source can be resolved to lib+offset
/// after the fact without needing ASLR to line up between runs.
pub fn dump_maps() {
    match std::fs::read_to_string("/proc/self/maps") {
        Ok(maps) => {
            write_line("--- BEGIN /proc/self/maps ---");
            for line in maps.lines() {
                write_line(&format!("[MAPS] {line}"));
            }
            write_line("--- END /proc/self/maps ---");
        }
        Err(e) => write_line(&format!("could not read /proc/self/maps: {e}")),
    }
}
