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
    lines: u64,
}

fn log_path() -> PathBuf {
    // Prefer repo logs/ when running from workspace; else cwd/logs.
    let candidates = [
        PathBuf::from("logs/beautiful-actions.log"),
        PathBuf::from("C:/modding/beautiful/logs/beautiful-actions.log"),
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
            *g = Some(ActionLogInner { file, lines: 0 });
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
