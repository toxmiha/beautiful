//! Localhost control plane for agent MCP (loopback only).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub enum McpCommand {
    Ping,
    OpenPath(String),
    OpenLibraryMatch(String),
    ListLayers,
    SetLayerVisible { idx: usize, visible: bool },
    /// Rapid eye-clicks in one UI tick (optionally sync composite after each).
    ToggleLayerBurst {
        idx: usize,
        times: u32,
        sync_each: bool,
    },
    /// Paint a polyline in document space (x,y,pressure).
    DrawStroke {
        points: Vec<(f32, f32, f32)>,
        sync: bool,
        brush_size: Option<f32>,
    },
    /// Keep requesting repaint for N frames (idle hover / compositor wake probe).
    SpamRepaint(u32),
    ShowProfiler(bool),
    Caps,
    BenchBegin { action: String },
    BenchEnd,
    WaitFrames(u32),
    PerfSnapshot,
    PerfReset,
    GetView,
    /// Set canvas zoom. `percent` is 100 = 1:1. `fit` resets to first-frame fit.
    SetZoom { percent: Option<f32>, fit: bool },
    /// Display-tile freshness vs cover (GPU + CPU flags).
    TileStatus,
    GradientBegin { x0: f32, y0: f32, x1: f32, y1: f32 },
    GradientCommit,
    GradientCancel,
    Quit,
}

struct Pending {
    cmd: McpCommand,
    reply: Sender<Value>,
}

pub struct McpBridge {
    rx: Receiver<Pending>,
    /// Frames left to wait (from wait_frames).
    pub wait_frames_left: u32,
    enabled: bool,
    port: u16,
}

impl McpBridge {
    /// Start if `BEAUTIFUL_MCP=1` / `--mcp` / env port set. Returns None if disabled.
    pub fn maybe_start(args: &[String]) -> Option<Self> {
        let flag = args.iter().any(|a| a == "--mcp")
            || std::env::var("BEAUTIFUL_MCP")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
        if !flag {
            return None;
        }
        let port: u16 = std::env::var("BEAUTIFUL_MCP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8765);

        let (tx, rx) = mpsc::channel::<Pending>();
        let tx = Arc::new(Mutex::new(tx));
        thread::Builder::new()
            .name("beautiful-mcp".into())
            .spawn(move || serve_loop(port, tx))
            .ok()?;
        crate::action_log::log("mcp", &format!("control plane on 127.0.0.1:{port}"));
        crate::perf::set_mode(crate::perf::Mode::Bench);
        Some(Self {
            rx,
            wait_frames_left: 0,
            enabled: true,
            port,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Drain one pending command (non-blocking). Returns reply channel + command.
    pub fn try_recv(&mut self) -> Option<(McpCommand, Sender<Value>)> {
        match self.rx.try_recv() {
            Ok(p) => Some((p.cmd, p.reply)),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }
}

fn serve_loop(port: u16, tx: Arc<Mutex<Sender<Pending>>>) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            crate::action_log::log("mcp", &format!("bind failed: {e}"));
            return;
        }
    };
    let _ = listener.set_nonblocking(false);
    for stream in listener.incoming().flatten() {
        let tx = Arc::clone(&tx);
        thread::spawn(move || {
            let _ = handle_client(stream, tx);
        });
    }
}

fn handle_client(mut stream: TcpStream, tx: Arc<Mutex<Sender<Pending>>>) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    // Read until blank line after headers or full body via Content-Length.
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(resp) = try_http_request(&buf, &tx) {
            stream.write_all(resp.as_bytes())?;
            return Ok(());
        }
        if buf.len() > 2 * 1024 * 1024 {
            let body = json!({"ok": false, "error": "request too large"});
            let resp = http_json(400, &body);
            stream.write_all(resp.as_bytes())?;
            return Ok(());
        }
    }
    Ok(())
}

fn try_http_request(buf: &[u8], tx: &Arc<Mutex<Sender<Pending>>>) -> Option<String> {
    let text = std::str::from_utf8(buf).ok()?;
    let header_end = text.find("\r\n\r\n")?;
    let headers = &text[..header_end];
    let body = &text[header_end + 4..];
    let mut content_length = 0usize;
    for line in headers.lines().skip(1) {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if body.len() < content_length {
        return None;
    }
    let body = &body[..content_length];

    let first = headers.lines().next().unwrap_or("");
    if !first.starts_with("POST ") {
        return Some(http_json(
            405,
            &json!({"ok": false, "error": "POST /cmd only"}),
        ));
    }

    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return Some(http_json(
                400,
                &json!({"ok": false, "error": format!("bad json: {e}")}),
            ));
        }
    };
    let cmd = match parse_cmd(&req) {
        Ok(c) => c,
        Err(e) => {
            return Some(http_json(400, &json!({"ok": false, "error": e})));
        }
    };

    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .lock()
        .ok()?
        .send(Pending {
            cmd,
            reply: reply_tx,
        })
        .is_err()
    {
        return Some(http_json(
            503,
            &json!({"ok": false, "error": "app not accepting commands"}),
        ));
    }
    let reply = reply_rx
        .recv_timeout(Duration::from_secs(120))
        .unwrap_or_else(|_| json!({"ok": false, "error": "timeout waiting for UI thread"}));
    Some(http_json(200, &reply))
}

fn parse_cmd(req: &Value) -> Result<McpCommand, String> {
    let cmd = req
        .get("cmd")
        .and_then(|c| c.as_str())
        .ok_or_else(|| "missing cmd".to_string())?;
    match cmd {
        "ping" => Ok(McpCommand::Ping),
        "open_path" => {
            let path = req
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or("missing path")?
                .to_owned();
            Ok(McpCommand::OpenPath(path))
        }
        "open_library_match" => {
            let q = req
                .get("query")
                .and_then(|p| p.as_str())
                .ok_or("missing query")?
                .to_owned();
            Ok(McpCommand::OpenLibraryMatch(q))
        }
        "list_layers" => Ok(McpCommand::ListLayers),
        "set_layer_visible" => {
            let idx = req
                .get("idx")
                .and_then(|v| v.as_u64())
                .ok_or("missing idx")? as usize;
            let visible = req
                .get("visible")
                .and_then(|v| v.as_bool())
                .ok_or("missing visible")?;
            Ok(McpCommand::SetLayerVisible { idx, visible })
        }
        "toggle_layer_burst" => {
            let idx = req
                .get("idx")
                .and_then(|v| v.as_u64())
                .ok_or("missing idx")? as usize;
            let times = req.get("times").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let sync_each = req
                .get("sync_each")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            Ok(McpCommand::ToggleLayerBurst {
                idx,
                times: times.clamp(1, 500),
                sync_each,
            })
        }
        "draw_stroke" => {
            let brush_size = req
                .get("brush_size")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32);
            let sync = req.get("sync").and_then(|v| v.as_bool()).unwrap_or(true);
            let points: Vec<(f32, f32, f32)> = if let Some(arr) = req.get("points").and_then(|v| v.as_array()) {
                arr.iter()
                    .filter_map(|p| {
                        let a = p.as_array()?;
                        Some((
                            a.first()?.as_f64()? as f32,
                            a.get(1)?.as_f64()? as f32,
                            a.get(2).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
                        ))
                    })
                    .collect()
            } else {
                let x0 = req.get("x0").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
                let y0 = req.get("y0").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
                let x1 = req.get("x1").and_then(|v| v.as_f64()).unwrap_or(800.0) as f32;
                let y1 = req.get("y1").and_then(|v| v.as_f64()).unwrap_or(600.0) as f32;
                let n = req.get("steps").and_then(|v| v.as_u64()).unwrap_or(64) as usize;
                let pressure = req
                    .get("pressure")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.85) as f32;
                let n = n.clamp(2, 2000);
                (0..n)
                    .map(|i| {
                        let t = i as f32 / (n - 1) as f32;
                        (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, pressure)
                    })
                    .collect()
            };
            if points.is_empty() {
                return Err("draw_stroke needs points or x0/y0/x1/y1".into());
            }
            Ok(McpCommand::DrawStroke {
                points,
                sync,
                brush_size,
            })
        }
        "spam_repaint" => {
            let n = req.get("n").and_then(|v| v.as_u64()).unwrap_or(60) as u32;
            Ok(McpCommand::SpamRepaint(n.clamp(1, 600)))
        }
        "show_profiler" => {
            let open = req.get("open").and_then(|v| v.as_bool()).unwrap_or(true);
            Ok(McpCommand::ShowProfiler(open))
        }
        "caps" => Ok(McpCommand::Caps),
        "bench_begin" => {
            let action = req
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("bench")
                .to_owned();
            Ok(McpCommand::BenchBegin { action })
        }
        "bench_end" => Ok(McpCommand::BenchEnd),
        "wait_frames" => {
            let n = req.get("n").and_then(|v| v.as_u64()).unwrap_or(2) as u32;
            Ok(McpCommand::WaitFrames(n.max(1)))
        }
        "perf_snapshot" => Ok(McpCommand::PerfSnapshot),
        "perf_reset" => Ok(McpCommand::PerfReset),
        "get_view" => Ok(McpCommand::GetView),
        "set_zoom" => {
            let percent = req.get("percent").and_then(|v| v.as_f64()).map(|v| v as f32);
            let fit = req.get("fit").and_then(|v| v.as_bool()).unwrap_or(false);
            if percent.is_none() && !fit {
                return Err("set_zoom needs percent or fit=true".into());
            }
            Ok(McpCommand::SetZoom { percent, fit })
        }
        "tile_status" => Ok(McpCommand::TileStatus),
        "gradient_begin" => {
            let x0 = req.get("x0").and_then(|v| v.as_f64()).unwrap_or(80.0) as f32;
            let y0 = req.get("y0").and_then(|v| v.as_f64()).unwrap_or(80.0) as f32;
            let x1 = req.get("x1").and_then(|v| v.as_f64()).unwrap_or(400.0) as f32;
            let y1 = req.get("y1").and_then(|v| v.as_f64()).unwrap_or(80.0) as f32;
            Ok(McpCommand::GradientBegin { x0, y0, x1, y1 })
        }
        "gradient_commit" => Ok(McpCommand::GradientCommit),
        "gradient_cancel" => Ok(McpCommand::GradientCancel),
        "quit" => Ok(McpCommand::Quit),
        other => Err(format!("unknown cmd: {other}")),
    }
}

fn http_json(status: u16, body: &Value) -> String {
    let payload = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )
}
