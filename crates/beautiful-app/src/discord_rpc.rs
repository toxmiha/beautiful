//! Discord Rich Presence — background IPC, non-blocking for the UI thread.
//!
//! Client ID is baked/project-owned (like every game RPC). End users never type it.
//! Optional one-time override: `%APPDATA%/Beautiful/discord_app_id.txt` or env
//! `BEAUTIFUL_DISCORD_CLIENT_ID` (for local builds before the official ID is set).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

/// Official Beautiful Discord Application ID (public — not a secret).
/// Fill once after creating Application "Beautiful" at discord.com/developers.
/// End users never see or edit this.
pub const BEAUTIFUL_DISCORD_CLIENT_ID: &str = "";

/// Asset / presence payload for one push.
#[derive(Clone, Debug)]
pub struct ActivityUpdate {
    pub details: String,
    pub state: String,
    /// When true, prefer canvas preview as large image (falls back to logo).
    pub show_preview: bool,
    /// JPEG bytes of a small canvas thumb (RGBA already encoded as JPEG).
    pub preview_jpeg: Option<Vec<u8>>,
}

/// Snapshot for Preferences status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RpcUiStatus {
    Off = 0,
    MissingClientId = 1,
    Connecting = 2,
    Connected = 3,
    Error = 4,
}

impl RpcUiStatus {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::MissingClientId,
            2 => Self::Connecting,
            3 => Self::Connected,
            4 => Self::Error,
            _ => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Выключено",
            Self::MissingClientId => "Нужен одноразовый Application ID проекта (см. ниже)",
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
        // JoinHandle::drop joins the worker. Litterbox curl can take seconds, so a
        // mid-upload exit would freeze quit. Detach — process teardown reaps the
        // thread; Discord presence clears best-effort in the background.
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
        // Prefer close over clear_activity — both are local IPC; close is enough for exit.
        let _ = c.close();
    }
}

fn worker(rx: mpsc::Receiver<Cmd>, status: Arc<AtomicU8>, stop: Arc<AtomicBool>) {
    let mut enabled = false;
    let mut client: Option<DiscordIpcClient> = None;
    let mut last = ActivityUpdate {
        details: "Beautiful".into(),
        state: "Idle".into(),
        show_preview: false,
        preview_jpeg: None,
    };
    let mut last_push = Instant::now() - Duration::from_secs(60);
    let mut last_reconnect = Instant::now() - Duration::from_secs(60);
    let mut need_push = false;
    let logo_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let preview_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
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
                    let preview_changed = upd.preview_jpeg.is_some()
                        && upd.preview_jpeg.as_ref().map(|b| b.len())
                            != last.preview_jpeg.as_ref().map(|b| b.len());
                    if upd.details != last.details
                        || upd.state != last.state
                        || upd.show_preview != last.show_preview
                        || preview_changed
                    {
                        if let Some(jpeg) = upd.preview_jpeg.clone() {
                            if let Ok(url) =
                                upload_bytes_litterbox(&jpeg, "preview.jpg", &stop)
                            {
                                if let Ok(mut g) = preview_url.lock() {
                                    *g = Some(url);
                                }
                            }
                        }
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
                    if let Some(jpeg) = upd.preview_jpeg.clone() {
                        if let Ok(url) =
                            upload_bytes_litterbox(&jpeg, "preview.jpg", &stop)
                        {
                            if let Ok(mut g) = preview_url.lock() {
                                *g = Some(url);
                            }
                        }
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

        let client_id = resolve_client_id();
        if client_id.is_empty() {
            clear_and_close(&mut client);
            set_status(&status, RpcUiStatus::MissingClientId);
            continue;
        }

        if client.is_none() {
            if last_reconnect.elapsed() < Duration::from_secs(2) {
                set_status(&status, RpcUiStatus::Connecting);
                continue;
            }
            last_reconnect = Instant::now();
            set_status(&status, RpcUiStatus::Connecting);
            let mut c = DiscordIpcClient::new(&client_id);
            match c.connect() {
                Ok(()) => {
                    crate::action_log::log("discord", "RPC connected");
                    // Warm logo URL once per process (skip if quitting).
                    if !stop_requested(&stop)
                        && logo_url.lock().ok().and_then(|g| g.clone()).is_none()
                    {
                        if let Ok(png) = make_logo_png() {
                            if let Ok(url) =
                                upload_bytes_litterbox(&png, "beautiful_logo.png", &stop)
                            {
                                if let Ok(mut g) = logo_url.lock() {
                                    *g = Some(url);
                                }
                            }
                        }
                    }
                    if stop_requested(&stop) {
                        let _ = c.close();
                        set_status(&status, RpcUiStatus::Off);
                        break;
                    }
                    client = Some(c);
                    need_push = true;
                    last_push = Instant::now() - Duration::from_secs(60);
                }
                Err(e) => {
                    crate::action_log::log("discord", &format!("RPC connect failed: {e}"));
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

        let logo = logo_url.lock().ok().and_then(|g| g.clone());
        let preview = preview_url.lock().ok().and_then(|g| g.clone());

        let (large, small) = if last.show_preview {
            match (preview, logo.clone()) {
                (Some(p), Some(l)) => (Some(p), Some(l)),
                (Some(p), None) => (Some(p), None),
                (None, Some(l)) => (Some(l), None),
                (None, None) => (None, None),
            }
        } else {
            (logo, None)
        };

        let mut act = activity::Activity::new()
            .activity_type(activity::ActivityType::Playing)
            .details(last.details.as_str())
            .state(last.state.as_str())
            .timestamps(activity::Timestamps::new().start(start_unix));

        if large.is_some() || small.is_some() {
            let mut assets = activity::Assets::new().large_text("Beautiful");
            if let Some(ref l) = large {
                assets = assets.large_image(l.as_str());
            }
            if let Some(ref s) = small {
                assets = assets.small_image(s.as_str()).small_text("Beautiful");
            }
            act = act.assets(assets);
        }

        match c.set_activity(act) {
            Ok(()) => {
                need_push = false;
                last_push = Instant::now();
                set_status(&status, RpcUiStatus::Connected);
            }
            Err(e) => {
                crate::action_log::log("discord", &format!("RPC set_activity failed: {e}"));
                let _ = c.close();
                client = None;
                last_reconnect = Instant::now() - Duration::from_secs(60);
                set_status(&status, RpcUiStatus::Error);
            }
        }
    }
}

pub fn resolve_client_id() -> String {
    let baked = BEAUTIFUL_DISCORD_CLIENT_ID.trim();
    if !baked.is_empty() {
        return baked.to_owned();
    }
    if let Ok(env) = std::env::var("BEAUTIFUL_DISCORD_CLIENT_ID") {
        let t = env.trim();
        if !t.is_empty() {
            return t.to_owned();
        }
    }
    if let Some(p) = appdata_client_id_path() {
        if let Ok(s) = std::fs::read_to_string(p) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_owned();
            }
        }
    }
    String::new()
}

pub fn appdata_client_id_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Beautiful").join("discord_app_id.txt"))
}

pub fn save_appdata_client_id(id: &str) -> Result<(), String> {
    let path = appdata_client_id_path().ok_or_else(|| "APPDATA missing".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, id.trim().as_bytes()).map_err(|e| e.to_string())
}

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("beautiful_discord_rpc")
}

fn upload_bytes_litterbox(
    bytes: &[u8],
    filename: &str,
    stop: &AtomicBool,
) -> Result<String, String> {
    if stop_requested(stop) {
        return Err("shutdown".into());
    }
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(filename);
    {
        let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
    }
    upload_file_litterbox(&path, stop)
}

fn upload_file_litterbox(path: &Path, stop: &AtomicBool) -> Result<String, String> {
    if stop_requested(stop) {
        return Err("shutdown".into());
    }
    // Windows 10+ ships curl. Litterbox returns a plain URL body.
    // Exit no longer joins this thread, but keep max-time modest anyway.
    let file_arg = format!("fileToUpload=@{}", path.display());
    let out = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "8",
            "-F",
            "reqtype=fileupload",
            "-F",
            "time=72h",
            "-F",
            &file_arg,
            "https://litterbox.catbox.moe/resources/internals/api.php",
        ])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if stop_requested(stop) {
        return Err("shutdown".into());
    }
    if !out.status.success() {
        return Err(format!(
            "curl exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("unexpected upload response: {url}"));
    }
    Ok(url)
}

/// Simple 256×256 dark tile with orange “B” — used as large logo / small corner mark.
fn make_logo_png() -> Result<Vec<u8>, String> {
    const S: u32 = 256;
    let mut rgba = vec![0u8; (S * S * 4) as usize];
    for y in 0..S {
        for x in 0..S {
            let i = ((y * S + x) * 4) as usize;
            // Dark charcoal
            rgba[i] = 28;
            rgba[i + 1] = 28;
            rgba[i + 2] = 32;
            rgba[i + 3] = 255;
        }
    }
    // Orange rounded square
    let accent = [255u8, 140, 66];
    for y in 40..216 {
        for x in 40..216 {
            let dx = (x as i32 - 128).unsigned_abs();
            let dy = (y as i32 - 128).unsigned_abs();
            if dx > 78 && dy > 78 {
                continue; // rough corner cut
            }
            let i = ((y * S + x) * 4) as usize;
            rgba[i] = accent[0];
            rgba[i + 1] = accent[1];
            rgba[i + 2] = accent[2];
        }
    }
    // Carve a simple “B” by punching vertical bars (dark)
    for y in 70..186 {
        for x in 90..110 {
            let i = ((y * S + x) * 4) as usize;
            rgba[i] = 28;
            rgba[i + 1] = 28;
            rgba[i + 2] = 32;
        }
    }
    for y in 70..100 {
        for x in 110..160 {
            let i = ((y * S + x) * 4) as usize;
            rgba[i] = 28;
            rgba[i + 1] = 28;
            rgba[i + 2] = 32;
        }
    }
    for y in 120..150 {
        for x in 110..155 {
            let i = ((y * S + x) * 4) as usize;
            rgba[i] = 28;
            rgba[i + 1] = 28;
            rgba[i + 2] = 32;
        }
    }
    for y in 156..186 {
        for x in 110..165 {
            let i = ((y * S + x) * 4) as usize;
            rgba[i] = 28;
            rgba[i + 1] = 28;
            rgba[i + 2] = 32;
        }
    }

    let mut png = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut png);
    let encoder = image::codecs::png::PngEncoder::new(&mut cursor);
    use image::ImageEncoder;
    encoder
        .write_image(&rgba, S, S, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

/// Encode RGBA thumb to JPEG for Discord preview uploads.
pub fn encode_preview_jpeg(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || rgba.len() < (w as usize * h as usize * 4) {
        return None;
    }
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
    let mut out = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut out);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 75);
    enc.encode(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}
