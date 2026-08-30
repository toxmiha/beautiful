//! Rolling action log for diagnosing input / zoom / stroke issues.
//!
//! Writes next to the executable (`<exe_dir>/logs/beautiful-actions.log`).
//! Never uses the process cwd — on Windows a GUI launch often has cwd
//! `C:\Windows\System32`, which used to create logs there.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: Mutex<Option<ActionLogInner>> = Mutex::new(None);

struct ActionLogInner {
    file: File,
    path: PathBuf,
    lines: u64,
}

fn try_create(path: PathBuf) -> Option<PathBuf> {
    let parent = path.parent()?;
    fs::create_dir_all(parent).ok()?;
    Some(path)
}

fn log_path() -> PathBuf {
    // 1. Folder of the running exe (dist/, cargo target/, installed copy).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(p) = try_create(dir.join("logs").join("beautiful-actions.log")) {
                return p;
            }
        }
    }
    // 2. %APPDATA%/Beautiful/logs — writable if the exe dir is not (Program Files).
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let p = PathBuf::from(appdata)
            .join("Beautiful")
            .join("logs")
            .join("beautiful-actions.log");
        if let Some(p) = try_create(p) {
            return p;
        }
    }
    // Last resort: temp. Never cwd (System32 / random shortcut directory).
    std::env::temp_dir().join("beautiful-actions.log")
}

fn ensure() -> bool {
    let mut g = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_some() {
        return true;
    }
    let path = log_path();
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            let _ = writeln!(
                file,
                "---- session {} ----",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            );
            let _ = file.flush();
            *g = Some(ActionLogInner {
                file,
                path,
                lines: 0,
            });
            true
        }
        Err(_) => false,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Append one diagnostic line (best-effort, never panics).
pub fn log(kind: &str, detail: &str) {
    if !ensure() {
        return;
    }
    let mut g = LOG.lock().unwrap_or_else(|e| e.into_inner());
    let Some(inner) = g.as_mut() else {
        return;
    };
    let _ = writeln!(inner.file, "{}\t{}\t{}", now_ms(), kind, detail);
    inner.lines += 1;
    // Flush every ~32 lines so a crash still leaves evidence.
    if inner.lines % 32 == 0 {
        let _ = inner.file.flush();
    }
}

/// Force flush — call from panic hook / before risky GPU ops.
pub fn flush() {
    if !ensure() {
        return;
    }
    let mut g = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(inner) = g.as_mut() {
        let _ = inner.file.flush();
    }
}

/// Absolute/relative path currently used (empty if log never opened).
pub fn path_string() -> String {
    if !ensure() {
        return String::new();
    }
    let g = LOG.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref()
        .map(|i| i.path.display().to_string())
        .unwrap_or_default()
}

/// Install process-wide panic hook that writes cause + location into the action log.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".into());
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<Any>".into()
        };
        // One-line summary first (easy to spot in tail).
        log("panic", &format!("at {loc} | {msg}"));
        // Truncate huge wgpu validation dumps but keep the useful head.
        let short = if msg.len() > 1200 {
            format!("{}…", &msg[..1200])
        } else {
            msg.clone()
        };
        log("panic_detail", &short);
        if let Ok(bt) = std::env::var("RUST_BACKTRACE") {
            if bt != "0" {
                let backtrace = std::backtrace::Backtrace::force_capture();
                let bt_s = format!("{backtrace}");
                let bt_short = if bt_s.len() > 4000 {
                    format!("{}…", &bt_s[..4000])
                } else {
                    bt_s
                };
                log("panic_backtrace", &bt_short);
            }
        }
        flush();
        // Keep default stderr / abort behavior.
        prev(info);
    }));
}

#[allow(dead_code)]
pub fn log_fmt(kind: &str, args: std::fmt::Arguments<'_>) {
    log(kind, &format!("{args}"));
}

#[macro_export]
macro_rules! alog {
    ($kind:expr, $($arg:tt)*) => {
        $crate::action_log::log_fmt($kind, format_args!($($arg)*))
    };
}
