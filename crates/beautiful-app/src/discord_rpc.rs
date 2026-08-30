//! Discord Rich Presence — local IPC only (named pipe / Discord desktop).
//!
//! No canvas thumbnails, no HTTP, no curl, no third-party hosts.
//! Application ID is baked in. Client Secret is never used.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

/// Official Beautiful Discord Application ID (public — not a secret).
pub const BEAUTIFUL_DISCORD_CLIENT_ID: &str = "1533882048182222968";

/// Discord Developer Portal → Rich Presence → Art Assets key for the app logo.
pub const DISCORD_ASSET_LOGO: &str = "logo";

/// Asset / presence payload for one push.
#[derive(Clone, Debug)]
pub struct ActivityUpdate {
    pub details: String,
    pub state: String,
}

/// Snapshot for Preferences status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RpcUiStatus {
    Off = 0,
    Connecting = 2,
    Connected = 3,
    Error = 4,
}

impl RpcUiStatus {
    pub fn from_u8(v: u8) -> Self {
        match v {
            2 => Self::Connecting,
            3 => Self::Connected,
            4 => Self::Error,
            _ => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Выключено",
            Self::Connecting => "Подключение к Discord…",
            Self::Connected => "Подключено",
            Self::Error => "Ошибка IPC (запущен Discord desktop?)",
        }
    }
}

enum Cmd {
    Configure { enabled: bool },
    Activity(ActivityUpdate),
    Shutdown,
}

pub struct DiscordRpc {
    tx: Sender<Cmd>,
    status: Arc<AtomicU8>,
    shutting_down: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl DiscordRpc {
    pub fn start(enabled: bool) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let status = Arc::new(AtomicU8::new(RpcUiStatus::Off as u8));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let status_w = Arc::clone(&status);
        let stop_w = Arc::clone(&shutting_down);
        let join = thread::Builder::new()
            .name("beautiful-discord-rpc".into())
            .spawn(move || worker(rx, status_w, stop_w))
            .ok();
        let rpc = Self {
            tx: tx.clone(),
            status,
            shutting_down,
            join,
        };
        let _ = rpc.tx.send(Cmd::Configure { enabled });
        rpc
    }

    pub fn configure(&self, enabled: bool) {
        let _ = self.tx.send(Cmd::Configure { enabled });
    }

    pub fn set_activity(&self, update: ActivityUpdate) {
        let _ = self.tx.send(Cmd::Activity(update));
    }

    pub fn ui_status(&self) -> RpcUiStatus {
        RpcUiStatus::from_u8(self.status.load(Ordering::Relaxed))
    }
}

impl Drop for DiscordRpc {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(h) = self.join.take() {
            std::mem::forget(h);
        }
    }
}

fn set_status(status: &AtomicU8, s: RpcUiStatus) {
    status.store(s as u8, Ordering::Relaxed);
}

fn stop_requested(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Relaxed)
}

fn clear_and_close(client: &mut Option<DiscordIpcClient>) {
    if let Some(mut c) = client.take() {
        let _ = c.close();
    }
}

fn worker(rx: mpsc::Receiver<Cmd>, status: Arc<AtomicU8>, stop: Arc<AtomicBool>) {
    let mut enabled = false;
    let mut client: Option<DiscordIpcClient> = None;
    let mut last = ActivityUpdate {
        details: "Beautiful".into(),
        state: "Idle".into(),
    };
    let mut last_push = Instant::now() - Duration::from_secs(60);
    let mut last_reconnect = Instant::now() - Duration::from_secs(60);
    let mut need_push = false;
    let start_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    loop {
        if stop_requested(&stop) {
            clear_and_close(&mut client);
            set_status(&status, RpcUiStatus::Off);
            break;
        }

        let cmd = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(c) => Some(c),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if let Some(cmd) = cmd {
            match cmd {
                Cmd::Shutdown => {
                    clear_and_close(&mut client);
                    set_status(&status, RpcUiStatus::Off);
                    break;
                }
                Cmd::Configure { enabled: en } => {
                    if en != enabled {
                        enabled = en;
                        clear_and_close(&mut client);
                        last_reconnect = Instant::now() - Duration::from_secs(60);
                        need_push = true;
                    }
                }
                Cmd::Activity(upd) => {
                    if stop_requested(&stop) {
                        clear_and_close(&mut client);
                        set_status(&status, RpcUiStatus::Off);
                        break;
                    }
                    if upd.details != last.details || upd.state != last.state {
                        last = upd;
                        need_push = true;
                        last_push = Instant::now() - Duration::from_secs(60);
                    }
                }
            }
        }

        while let Ok(extra) = rx.try_recv() {
            match extra {
                Cmd::Shutdown => {
                    clear_and_close(&mut client);
                    set_status(&status, RpcUiStatus::Off);
                    return;
                }
                Cmd::Configure { enabled: en } => {
                    if en != enabled {
                        enabled = en;
                        clear_and_close(&mut client);
                        last_reconnect = Instant::now() - Duration::from_secs(60);
                        need_push = true;
                    }
                }
                Cmd::Activity(upd) => {
                    if stop_requested(&stop) {
                        clear_and_close(&mut client);
                        set_status(&status, RpcUiStatus::Off);
                        return;
                    }
                    last = upd;
                    need_push = true;
                    last_push = Instant::now() - Duration::from_secs(60);
                }
            }
        }

        if stop_requested(&stop) {
            clear_and_close(&mut client);
            set_status(&status, RpcUiStatus::Off);
            break;
        }

        if !enabled {
            clear_and_close(&mut client);
            set_status(&status, RpcUiStatus::Off);
            continue;
        }

        if client.is_none() {
            if last_reconnect.elapsed() < Duration::from_secs(2) {
                set_status(&status, RpcUiStatus::Connecting);
                continue;
            }
            last_reconnect = Instant::now();
            set_status(&status, RpcUiStatus::Connecting);
            let mut c = DiscordIpcClient::new(BEAUTIFUL_DISCORD_CLIENT_ID);
            match c.connect() {
                Ok(()) => {
                    crate::action_log::log("discord", "RPC connected");
                    if stop_requested(&stop) {
                        let _ = c.close();
                        set_status(&status, RpcUiStatus::Off);
                        break;
                    }
                    client = Some(c);
                    need_push = true;
                    last_push = Instant::now() - Duration::from_secs(60);
                }
                Err(_) => {
                    crate::action_log::log("discord", "RPC connect failed");
                    set_status(&status, RpcUiStatus::Error);
                    continue;
                }
            }
        }

        let Some(c) = client.as_mut() else {
            continue;
        };

        if !need_push && last_push.elapsed() < Duration::from_secs(15) {
            set_status(&status, RpcUiStatus::Connected);
            continue;
        }
        if last_push.elapsed() < Duration::from_secs(3) {
            continue;
        }

        let act = activity::Activity::new()
            .activity_type(activity::ActivityType::Playing)
            .details(last.details.as_str())
            .state(last.state.as_str())
            .timestamps(activity::Timestamps::new().start(start_unix))
            .assets(
                activity::Assets::new()
                    .large_image(DISCORD_ASSET_LOGO)
                    .large_text("Beautiful"),
            );

        match c.set_activity(act) {
            Ok(()) => {
                need_push = false;
                last_push = Instant::now();
                set_status(&status, RpcUiStatus::Connected);
            }
            Err(_) => {
                crate::action_log::log("discord", "RPC set_activity failed");
                let _ = c.close();
                client = None;
                last_reconnect = Instant::now() - Duration::from_secs(60);
                set_status(&status, RpcUiStatus::Error);
            }
        }
    }
}
