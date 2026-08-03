//! Rolling action log for diagnosing input / zoom / stroke issues.
//! Writes to `logs/beautiful-actions.log` under the workspace (or cwd).

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

fn log_path() -> PathBuf {
    // Prefer repo logs/ when running from workspace; else cwd/logs.
    let candidates = [
        PathBuf::from("logs/beautiful-actions.log"),
        PathBuf::from("C:/modding/beautiful/logs/beautiful-actions.log"),
        PathBuf::from("C:/modding/beautiful/dist/logs/beautiful-actions.log"),
    ];
    for p in &candidates {
        if let Some(parent) = p.parent() {
            if fs::create_dir_all(parent).is_ok() {
                return p.clone();
            }
        }
    }
    PathBuf::from("beautiful-actions.log")
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
