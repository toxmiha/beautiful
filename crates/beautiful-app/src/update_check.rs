//! Optional GitHub release check — offer download, never auto-install.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const REPO: &str = "toxmiha/beautiful";
const USER_AGENT: &str = "Beautiful-App-UpdateCheck";

#[derive(Debug, Clone)]
pub struct UpdateOffer {
    pub tag: String,
    pub name: String,
    pub html_url: String,
}

pub struct UpdateChecker {
    rx: Receiver<Option<UpdateOffer>>,
    offer: Option<UpdateOffer>,
    /// User dismissed this tag for the session.
    dismissed_tag: Option<String>,
    last_check: Option<Instant>,
    in_flight: Arc<Mutex<bool>>,
}

impl UpdateChecker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        // Drop the sender — first `request_check` creates a real worker.
        drop(tx);
        Self {
            rx,
            offer: None,
            dismissed_tag: None,
            last_check: None,
            in_flight: Arc::new(Mutex::new(false)),
        }
    }

    /// Kick a background check (at most once per `min_interval`).
    pub fn request_check(&mut self, min_interval: Duration) {
        if let Some(t) = self.last_check {
            if t.elapsed() < min_interval {
                return;
            }
        }
        {
            let mut g = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            if *g {
                return;
            }
            *g = true;
        }
        self.last_check = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        self.rx = rx;
        let flag = Arc::clone(&self.in_flight);
        let _ = thread::Builder::new()
            .name("beautiful-update-check".into())
            .spawn(move || {
                let offer = fetch_latest_offer();
                let _ = tx.send(offer);
                if let Ok(mut g) = flag.lock() {
                    *g = false;
                }
            });
    }

    pub fn poll(&mut self) {
        match self.rx.try_recv() {
            Ok(Some(offer)) => {
                if self.dismissed_tag.as_deref() == Some(offer.tag.as_str()) {
                    return;
                }
                if version_is_newer(&offer.tag, env!("CARGO_PKG_VERSION")) {
                    self.offer = Some(offer);
                }
            }
            Ok(None) | Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => {}
        }
    }

    pub fn pending(&self) -> Option<&UpdateOffer> {
        self.offer.as_ref()
    }

    pub fn dismiss(&mut self) {
        if let Some(o) = self.offer.take() {
            self.dismissed_tag = Some(o.tag);
        }
    }

    pub fn open_download(&self) {
        if let Some(o) = &self.offer {
            crate::os_win::open_url(&o.html_url);
        }
    }
}

impl Default for UpdateChecker {
    fn default() -> Self {
        Self::new()
    }
}

fn fetch_latest_offer() -> Option<UpdateOffer> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut cmd = std::process::Command::new("curl");
    crate::os_win::hide_console(&mut cmd);
    let out = cmd
        .args([
            "-sS",
            "--max-time",
            "8",
            "-H",
            &format!("User-Agent: {USER_AGENT}"),
            "-H",
            "Accept: application/vnd.github+json",
            &url,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    if v.get("message").is_some() && v.get("tag_name").is_none() {
        // API error / rate limit / no releases
        return None;
    }
    let tag = v.get("tag_name")?.as_str()?.to_string();
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or(&tag)
        .to_string();
    let html_url = v
        .get("html_url")
        .and_then(|x| x.as_str())
        .unwrap_or(&format!("https://github.com/{REPO}/releases"))
        .to_string();
    Some(UpdateOffer {
        tag,
        name,
        html_url,
    })
}

/// Compare semver-ish tags (`v0.4.9` / `0.4.9`) against `CARGO_PKG_VERSION`.
pub fn version_is_newer(remote_tag: &str, local: &str) -> bool {
    let r = parse_ver(remote_tag);
    let l = parse_ver(local);
    r > l
}

fn parse_ver(s: &str) -> (u32, u32, u32) {
    let s = s.trim().trim_start_matches('v').trim_start_matches('V');
    let mut parts = s.split(|c| c == '.' || c == '-' || c == '+');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_tag() {
        assert!(version_is_newer("v0.4.9", "0.4.8"));
        assert!(version_is_newer("v0.4.8", "0.4.7"));
        assert!(!version_is_newer("v0.4.7", "0.4.7"));
        assert!(!version_is_newer("0.4.6", "0.4.7"));
    }
}
