//! Encode a demo journal into MP4 / WebM / GIF via ffmpeg.
//!
//! Composites only when the document actually changes (event-driven), then
//! repeats the last RGB frame to fill the chosen fps. Idle gaps are already
//! collapsed in the journal, so the video is a timelapse, not a screen capture.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use beautiful_core::{blend_over, play_until, spawn_replay_document, BlendMode, DemoFile, Document};

const HOLD_LAST_MS: f32 = 600.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoFormat {
    Mp4,
    Webm,
    Gif,
}

impl VideoFormat {
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "webm" => Self::Webm,
            "gif" => Self::Gif,
            _ => Self::Mp4,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
            Self::Gif => "gif",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Webm => "WebM",
            Self::Gif => "GIF",
        }
    }
}

/// Size / bitrate tradeoff for the exported file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressPreset {
    Quality,
    Balanced,
    Small,
    Tiny,
}

impl CompressPreset {
    pub const ALL: [Self; 4] = [Self::Quality, Self::Balanced, Self::Small, Self::Tiny];

    pub fn label(self) -> &'static str {
        match self {
            Self::Quality => "Качество",
            Self::Balanced => "Баланс",
            Self::Small => "Компакт",
            Self::Tiny => "Минимум",
        }
    }

    fn max_side(self) -> u32 {
        match self {
            Self::Quality => 1920,
            Self::Balanced => 1920,
            Self::Small => 1280,
            Self::Tiny => 720,
        }
    }

    fn fps(self, format: VideoFormat) -> u32 {
        match (format, self) {
            (VideoFormat::Gif, Self::Quality) => 20,
            (VideoFormat::Gif, Self::Balanced) => 15,
            (VideoFormat::Gif, Self::Small) => 12,
            (VideoFormat::Gif, Self::Tiny) => 10,
            (_, Self::Quality) => 30,
            (_, Self::Balanced) => 30,
            (_, Self::Small) => 24,
            (_, Self::Tiny) => 15,
        }
    }

    fn crf(self) -> u32 {
        match self {
            Self::Quality => 18,
            Self::Balanced => 23,
            Self::Small => 28,
            Self::Tiny => 32,
        }
    }

    fn h264_bitrate(self) -> &'static str {
        match self {
            Self::Quality => "12M",
            Self::Balanced => "6M",
            Self::Small => "2.5M",
            Self::Tiny => "1M",
        }
    }

    fn webm_bitrate(self) -> &'static str {
        match self {
            Self::Quality => "6M",
            Self::Balanced => "3M",
            Self::Small => "1.2M",
            Self::Tiny => "500k",
        }
    }

    fn gif_colors(self) -> u32 {
        match self {
            Self::Quality => 256,
            Self::Balanced => 128,
            Self::Small => 96,
            Self::Tiny => 64,
        }
    }

    fn gif_max_side(self) -> u32 {
        match self {
            Self::Quality => 1280,
            Self::Balanced => 960,
            Self::Small => 720,
            Self::Tiny => 480,
        }
    }
}

#[derive(Clone)]
pub struct WatermarkBlit {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub angle_deg: f32,
    pub opacity: f32,
    pub blend: BlendMode,
}

pub struct DemoExportOpts {
    pub speed: f32,
    pub compress: CompressPreset,
    pub watermark: Option<WatermarkBlit>,
    pub audio_path: Option<PathBuf>,
    pub audio_volume: f32,
}

impl Default for DemoExportOpts {
    fn default() -> Self {
        Self {
            speed: 4.0,
            compress: CompressPreset::Balanced,
            watermark: None,
            audio_path: None,
            audio_volume: 0.8,
        }
    }
}

pub fn export_demo_video(
    file: DemoFile,
    dest: PathBuf,
    opts: DemoExportOpts,
    progress: Arc<AtomicU8>,
) -> Result<(), String> {
    let ffmpeg = crate::audio::ffmpeg_path().ok_or_else(|| {
        "ffmpeg не найден. Скопируй ffmpeg.exe в dist/ffmpeg/ или поставь в PATH.".to_string()
    })?;
    let speed = opts.speed.clamp(0.1, 16.0);
    let format = VideoFormat::from_path(&dest);
    let compress = opts.compress;

    progress.store(1, Ordering::Relaxed);

    let mut probe = spawn_replay_document(&file);
    let mut max_w = probe.width.max(1);
    let mut max_h = probe.height.max(1);
    let mut applied = 0usize;
    while applied < file.events.len() {
        let before = applied;
        applied = play_until(&mut probe, &file, applied, file.events[applied].t());
        max_w = max_w.max(probe.width);
        max_h = max_h.max(probe.height);
        if applied <= before {
            break;
        }
    }
    drop(probe);

    let max_side = if format == VideoFormat::Gif {
        compress.gif_max_side()
    } else {
        compress.max_side()
    };
    let (ow, oh) = even_fit(max_w, max_h, max_side);
    let duration_ms = file.events.last().map(|e| e.t()).unwrap_or(0) as f32;
    let video_ms = (duration_ms / speed).max(1.0) + HOLD_LAST_MS;
    let fps = compress.fps(format);
    let nframes = ((video_ms / 1000.0) * fps as f32).ceil() as u32;
    let nframes = nframes.clamp(1, fps * 60 * 10);

    let encoder_args = encoder_args(format, &ffmpeg, ow, oh, compress)?;
    let audio = opts
        .audio_path
        .as_ref()
        .filter(|p| p.is_file() && format != VideoFormat::Gif);
    let vol = opts.audio_volume.clamp(0.0, 2.0);

    let mut cmd = Command::new(&ffmpeg);
    crate::os_win::hide_console(&mut cmd);
    cmd.arg("-y").args([
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
        &format!("{ow}x{oh}"),
        "-r",
        &fps.to_string(),
        "-i",
        "pipe:0",
    ]);
    if let Some(path) = audio {
        cmd.args(["-stream_loop", "-1", "-i"])
            .arg(path)
            .args(["-filter:a", &format!("volume={vol:.3}")])
            .args(audio_codec_args(format))
            .arg("-shortest");
    } else {
        cmd.arg("-an");
    }
    cmd.args(&encoder_args)
        .arg(&dest)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("не удалось запустить ffmpeg: {e}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ffmpeg stdin missing".to_string())?;
    let mut stdin = BufWriter::with_capacity((ow * oh * 3) as usize, stdin);

    let mut doc = spawn_replay_document(&file);
    let mut applied = 0usize;
    let mut rgb = frame_rgb(&doc, ow, oh, opts.watermark.as_ref());
    let mut last_applied = 0usize;

    for i in 0..nframes {
        let demo_t = (i as f32 / fps as f32) * speed * 1000.0;
        applied = play_until(&mut doc, &file, applied, demo_t as u32);
        if applied != last_applied {
            rgb = frame_rgb(&doc, ow, oh, opts.watermark.as_ref());
            last_applied = applied;
        }
        if let Err(e) = stdin.write_all(&rgb) {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("запись кадра: {e}"));
        }
        let pct = (5 + (i as u32 + 1) * 90 / nframes).min(99) as u8;
        progress.store(pct, Ordering::Relaxed);
    }
    if let Err(e) = stdin.flush() {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("запись кадра: {e}"));
    }
    drop(stdin);

    let out = child
        .wait_with_output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "ffmpeg: {}",
            err.chars().take(500).collect::<String>()
        ));
    }
    progress.store(100, Ordering::Relaxed);
    Ok(())
}

fn audio_codec_args(format: VideoFormat) -> Vec<String> {
    match format {
        VideoFormat::Webm => vec!["-c:a".into(), "libopus".into(), "-b:a".into(), "96k".into()],
        _ => vec!["-c:a".into(), "aac".into(), "-b:a".into(), "128k".into()],
    }
}

fn encoder_args(
    format: VideoFormat,
    ffmpeg: &Path,
    w: u32,
    h: u32,
    compress: CompressPreset,
) -> Result<Vec<String>, String> {
    match format {
        VideoFormat::Mp4 => {
            let enc = pick_h264(ffmpeg);
            let mut args = vec!["-c:v".into(), enc.into(), "-pix_fmt".into(), "yuv420p".into()];
            if enc == "libx264" {
                args.extend([
                    "-preset".into(),
                    "veryfast".into(),
                    "-crf".into(),
                    compress.crf().to_string(),
                    "-movflags".into(),
                    "+faststart".into(),
                ]);
            } else if enc == "h264_mf" {
                args.extend(["-b:v".into(), compress.h264_bitrate().into()]);
            } else {
                args.extend(["-b:v".into(), compress.h264_bitrate().into()]);
            }
            let _ = (w, h);
            Ok(args)
        }
        VideoFormat::Webm => Ok(vec![
            "-c:v".into(),
            "libvpx".into(),
            "-b:v".into(),
            compress.webm_bitrate().into(),
            "-deadline".into(),
            "good".into(),
            "-cpu-used".into(),
            if matches!(compress, CompressPreset::Tiny | CompressPreset::Small) {
                "4".into()
            } else {
                "8".into()
            },
        ]),
        VideoFormat::Gif => {
            let colors = compress.gif_colors();
            Ok(vec![
                "-vf".into(),
                format!(
                    "fps={fps},scale={w}:{h}:flags=neighbor,split[s0][s1];[s0]palettegen=max_colors={colors}:stats_mode=diff[p];[s1][p]paletteuse=dither=bayer",
                    fps = compress.fps(VideoFormat::Gif)
                ),
                "-loop".into(),
                "0".into(),
            ])
        }
    }
}

fn pick_h264(ffmpeg: &Path) -> &'static str {
    let mut cmd = Command::new(ffmpeg);
    crate::os_win::hide_console(&mut cmd);
    let Ok(out) = cmd.args(["-hide_banner", "-encoders"]).output() else {
        return "mpeg4";
    };
    let s = String::from_utf8_lossy(&out.stdout);
    if s.contains("libx264") {
        "libx264"
    } else if s.contains("h264_mf") {
        "h264_mf"
    } else {
        "mpeg4"
    }
}

fn even_fit(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let w = w.max(2);
    let h = h.max(2);
    let side = w.max(h);
    let (mut nw, mut nh) = if side <= max_side {
        (w, h)
    } else {
        let s = max_side as f32 / side as f32;
        (
            ((w as f32 * s).round() as u32).max(2),
            ((h as f32 * s).round() as u32).max(2),
        )
    };
    nw &= !1;
    nh &= !1;
    (nw.max(2), nh.max(2))
}

fn frame_rgb(doc: &Document, ow: u32, oh: u32, wm: Option<&WatermarkBlit>) -> Vec<u8> {
    let rgba = doc.composite_rgba_copy();
    let sw = doc.width.max(1);
    let sh = doc.height.max(1);
    let mut out = scale_rgba_to_rgb(&rgba, sw, sh, ow, oh);
    if let Some(wm) = wm {
        let fit = (ow as f32 / sw as f32).min(oh as f32 / sh as f32);
        let fw = (sw as f32 * fit).max(1.0);
        let fh = (sh as f32 * fit).max(1.0);
        let ox = ((ow as f32 - fw) * 0.5).round();
        let oy = ((oh as f32 - fh) * 0.5).round();
        blit_watermark(&mut out, ow, oh, 3, ox, oy, fw, fh, wm);
    }
    out
}

fn scale_rgba_to_rgb(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; dw as usize * dh as usize * 3];
    if sw == 0 || sh == 0 {
        return out;
    }
    let fit = (dw as f32 / sw as f32).min(dh as f32 / sh as f32);
    let fw = (sw as f32 * fit).max(1.0);
    let fh = (sh as f32 * fit).max(1.0);
    let ox = ((dw as f32 - fw) * 0.5).round() as i32;
    let oy = ((dh as f32 - fh) * 0.5).round() as i32;
    for y in 0..dh {
        let sy = ((y as i32 - oy) as f32 / fit).floor() as i32;
        if sy < 0 || sy >= sh as i32 {
            continue;
        }
        for x in 0..dw {
            let sx = ((x as i32 - ox) as f32 / fit).floor() as i32;
            if sx < 0 || sx >= sw as i32 {
                continue;
            }
            let si = ((sy as u32 * sw + sx as u32) as usize) * 4;
            let di = ((y * dw + x) as usize) * 3;
            if si + 3 < src.len() {
                out[di] = src[si];
                out[di + 1] = src[si + 1];
                out[di + 2] = src[si + 2];
            }
        }
    }
    out
}

/// Composite `wm` onto `dst` (`channels` 3 = RGB, 4 = RGBA).
/// (`ox`,`oy`,`cw`,`ch`) is the content rectangle the watermark is relative to.
pub fn blit_watermark(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    channels: usize,
    ox: f32,
    oy: f32,
    cw: f32,
    ch: f32,
    wm: &WatermarkBlit,
) {
    if wm.w == 0 || wm.h == 0 || cw <= 1.0 || ch <= 1.0 {
        return;
    }
    let tw = (cw * wm.scale).max(1.0);
    let th = tw * (wm.h as f32 / wm.w as f32);
    let cx = ox + wm.x.clamp(0.0, 1.0) * cw;
    let cy = oy + wm.y.clamp(0.0, 1.0) * ch;
    let ang = wm.angle_deg.to_radians();
    let (sin, cos) = ang.sin_cos();
    let hw = tw * 0.5;
    let hh = th * 0.5;
    let mut minx = f32::MAX;
    let mut miny = f32::MAX;
    let mut maxx = f32::MIN;
    let mut maxy = f32::MIN;
    for (lx, ly) in [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)] {
        let rx = cx + lx * cos - ly * sin;
        let ry = cy + lx * sin + ly * cos;
        minx = minx.min(rx);
        miny = miny.min(ry);
        maxx = maxx.max(rx);
        maxy = maxy.max(ry);
    }
    let x0 = minx.floor().max(0.0) as u32;
    let y0 = miny.floor().max(0.0) as u32;
    let x1 = (maxx.ceil() as u32).min(dw);
    let y1 = (maxy.ceil() as u32).min(dh);
    let opacity = wm.opacity.clamp(0.0, 1.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let lx = dx * cos + dy * sin + hw;
            let ly = -dx * sin + dy * cos + hh;
            if lx < 0.0 || ly < 0.0 || lx >= tw || ly >= th {
                continue;
            }
            let u = lx / tw * (wm.w as f32 - 1.0).max(0.0);
            let v = ly / th * (wm.h as f32 - 1.0).max(0.0);
            let px = sample_bilinear(&wm.rgba, wm.w, wm.h, u, v);
            let a = (px[3] as f32 / 255.0) * opacity;
            if a < 0.002 {
                continue;
            }
            let di = ((y * dw + x) as usize) * channels;
            if di + 2 >= dst.len() {
                continue;
            }
            let mut dst_px = [
                dst[di],
                dst[di + 1],
                dst[di + 2],
                if channels >= 4 { dst[di + 3] } else { 255 },
            ];
            blend_over(&mut dst_px, &px, a, wm.blend);
            dst[di] = dst_px[0];
            dst[di + 1] = dst_px[1];
            dst[di + 2] = dst_px[2];
            if channels >= 4 && di + 3 < dst.len() {
                dst[di + 3] = dst_px[3];
            }
        }
    }
}

fn sample_bilinear(img: &[u8], w: u32, h: u32, x: f32, y: f32) -> [u8; 4] {
    if w == 0 || h == 0 || img.len() < 4 {
        return [0; 4];
    }
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let pix = |xx: u32, yy: u32| {
        let i = ((yy * w + xx) as usize) * 4;
        if i + 3 < img.len() {
            [img[i], img[i + 1], img[i + 2], img[i + 3]]
        } else {
            [0; 4]
        }
    };
    let p00 = pix(x0, y0);
    let p10 = pix(x1, y0);
    let p01 = pix(x0, y1);
    let p11 = pix(x1, y1);
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
        let bot = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
        out[c] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

pub fn ensure_video_extension(path: PathBuf, format: VideoFormat) -> PathBuf {
    let want = format.ext();
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case(want) => path,
        _ => {
            let mut p = path;
            p.set_extension(want);
            p
        }
    }
}
