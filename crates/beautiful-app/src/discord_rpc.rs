//! Discord Rich Presence — background IPC, non-blocking for the UI thread.

use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

enum Cmd {
    Configure { enabled: bool, client_id: String },
    Activity { details: String, state: String },
    Shutdown,
}

pub struct DiscordRpc {
    tx: Sender<Cmd>,
    _join: Option<JoinHandle<()>>,
}

impl DiscordRpc {
    pub fn start(enabled: bool, client_id: String) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let join = thread::Builder::new()
            .name("beautiful-discord-rpc".into())
            .spawn(move || worker(rx))
            .ok();
        let rpc = Self {
            tx: tx.clone(),
            _join: join,
        };
        let _ = rpc.tx.send(Cmd::Configure { enabled, client_id });
        rpc
    }

    pub fn configure(&self, enabled: bool, client_id: String) {
        let _ = self.tx.send(Cmd::Configure { enabled, client_id });
    }

    pub fn set_activity(&self, details: impl Into<String>, state: impl Into<String>) {
        let _ = self.tx.send(Cmd::Activity {
            details: details.into(),
            state: state.into(),
        });
    }
}

impl Drop for DiscordRpc {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

fn worker(rx: mpsc::Receiver<Cmd>) {
    let mut enabled = false;
    let mut client_id = String::new();
    let mut client: Option<DiscordIpcClient> = None;
    let mut last_details = String::new();
    let mut last_state = String::new();
    let mut last_push = Instant::now() - Duration::from_secs(60);
    let mut last_reconnect = Instant::now() - Duration::from_secs(60);
    let start_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    loop {
        // Drain pending commands; also wake periodically to reconnect / refresh.
        let cmd = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(c) => Some(c),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if let Some(cmd) = cmd {
            match cmd {
                Cmd::Shutdown => {
                    if let Some(mut c) = client.take() {
                        let _ = c.clear_activity();
                        let _ = c.close();
                    }
                    break;
                }
                Cmd::Configure {
                    enabled: en,
                    client_id: id,
                } => {
                    let id = resolve_client_id(&id);
                    let changed = en != enabled || id != client_id;
                    enabled = en;
                    client_id = id;
                    if changed {
                        if let Some(mut c) = client.take() {
                            let _ = c.clear_activity();
                            let _ = c.close();
                        }
                        last_reconnect = Instant::now() - Duration::from_secs(60);
                    }
                    if !enabled {
                        continue;
                    }
                }
                Cmd::Activity { details, state } => {
                    last_details = details;
                    last_state = state;
                    // Force push soon.
                    last_push = Instant::now() - Duration::from_secs(60);
                }
            }
        }

        // Drain any burst of Activity updates without reconnecting repeatedly.
        while let Ok(extra) = rx.try_recv() {
            match extra {
                Cmd::Shutdown => {
                    if let Some(mut c) = client.take() {
                        let _ = c.clear_activity();
                        let _ = c.close();
                    }
                    return;
                }
                Cmd::Configure {
                    enabled: en,
                    client_id: id,
                } => {
                    let id = resolve_client_id(&id);
                    let changed = en != enabled || id != client_id;
                    enabled = en;
                    client_id = id;
                    if changed {
                        if let Some(mut c) = client.take() {
                            let _ = c.clear_activity();
                            let _ = c.close();
                        }
                        last_reconnect = Instant::now() - Duration::from_secs(60);
                    }
                }
                Cmd::Activity { details, state } => {
                    last_details = details;
                    last_state = state;
                }
            }
        }

        if !enabled || client_id.is_empty() {
            if let Some(mut c) = client.take() {
                let _ = c.clear_activity();
                let _ = c.close();
            }
            continue;
        }

        if client.is_none() && last_reconnect.elapsed() >= Duration::from_secs(8) {
            last_reconnect = Instant::now();
            let mut c = DiscordIpcClient::new(&client_id);
            match c.connect() {
                Ok(()) => {
                    crate::action_log::log("discord", "RPC connected");
                    client = Some(c);
                    last_push = Instant::now() - Duration::from_secs(60);
                }
                Err(e) => {
                    crate::action_log::log("discord", &format!("RPC connect failed: {e}"));
                }
            }
        }

        let Some(c) = client.as_mut() else {
            continue;
        };

        if last_push.elapsed() < Duration::from_secs(4) {
            continue;
        }
        last_push = Instant::now();

        let details = if last_details.is_empty() {
            "Painting"
        } else {
            last_details.as_str()
        };
        let state = if last_state.is_empty() {
            "Beautiful"
        } else {
            last_state.as_str()
        };

        let act = activity::Activity::new()
            .details(details)
            .state(state)
            .timestamps(activity::Timestamps::new().start(start_unix))
            .assets(
                activity::Assets::new()
                    .large_text("Beautiful")
                    .small_text(state),
            );

        if let Err(e) = c.set_activity(act) {
            crate::action_log::log("discord", &format!("RPC set_activity failed: {e}"));
            let _ = c.close();
            client = None;
            last_reconnect = Instant::now() - Duration::from_secs(60);
        }
    }
}

fn resolve_client_id(settings_id: &str) -> String {
    if let Ok(env) = std::env::var("BEAUTIFUL_DISCORD_CLIENT_ID") {
        let t = env.trim();
        if !t.is_empty() {
            return t.to_owned();
        }
    }
    settings_id.trim().to_owned()
}
