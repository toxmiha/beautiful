//! Host audio API for add-ons: decode + play + seek + volume.
//!
//! Product UI (playlists, Global/Canvas modes, transport chrome) lives in add-ons.
//! This module only provides a playback engine.
//!
//! Decode backends:
//! 1. **Symphonia** (via `rodio`) — mp3 / flac / ogg / wav / aac / m4a in-process
//! 2. **ffmpeg** sidecar — full-file WAV cache (avoids re-transcode on seek)

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};

/// Snapshot pushed into add-on queries each frame / before script calls.
#[derive(Clone, Debug, Default)]
pub struct AudioSnapshot {
    pub path: String,
    pub title: String,
    pub playing: bool,
    pub paused: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub volume: f32,
    pub bar_visible: bool,
    pub ffmpeg_available: bool,
    /// True for one query cycle after the current sink emptied while advancing.
    pub ended: bool,
    pub is_stream: bool,
    /// Amplitude envelope 0..1 for seek waveform UI (empty for live streams).
    pub peaks: Vec<f32>,
}

struct CachedDecode {
    wav_path: PathBuf,
    source_mtime: Option<SystemTime>,
    duration: Option<Duration>,
}

pub struct AudioEngine {
    _stream: Option<OutputStream>,
    handle: Option<OutputStreamHandle>,
    sink: Option<Sink>,
    /// Path actually decoded (may be a cache WAV).
    path: Option<PathBuf>,
    /// Original media path for add-ons / UI labels.
    source_path: Option<PathBuf>,
    title: String,
    duration: Option<Duration>,
    base_position: Duration,
    play_started: Option<Instant>,
    paused: bool,
    volume: f32,
    /// Legacy flag for add-ons that toggle a host scrub (unused by chrome).
    pub bar_visible: bool,
    pub last_error: Option<String>,
    ffmpeg_dir: Option<PathBuf>,
    decode_cache: HashMap<PathBuf, CachedDecode>,
    /// Was playing and expecting natural end → set `ended` when sink empties.
    expect_end: bool,
    ended_latch: bool,
    /// Waveform peaks for UI (0..1), empty for live streams.
    pub peaks: Vec<f32>,
    /// HTTP radio / live stream — seek disabled.
    pub is_stream: bool,
    stream_child: Option<std::process::Child>,
    /// Avoid spamming ffprobe every frame when duration is unknown.
    duration_probe_done: bool,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        let (stream, handle, err) = match OutputStream::try_default() {
            Ok((s, h)) => (Some(s), Some(h), None),
            Err(e) => (None, None, Some(format!("audio device: {e}"))),
        };
        let ffmpeg_dir = locate_ffmpeg_dir();
        Self {
            _stream: stream,
            handle,
            sink: None,
            path: None,
            source_path: None,
            title: String::new(),
            duration: None,
            base_position: Duration::ZERO,
            play_started: None,
            paused: true,
            volume: 1.0,
            bar_visible: false,
            last_error: err,
            ffmpeg_dir,
            decode_cache: HashMap::new(),
            expect_end: false,
            ended_latch: false,
            peaks: Vec::new(),
            is_stream: false,
            stream_child: None,
            duration_probe_done: false,
        }
    }

    pub fn ffmpeg_available(&self) -> bool {
        self.ffmpeg_dir.is_some() || which_bin("ffmpeg").is_some()
    }

    /// True after natural end until the next [`Self::tick`] clears it.
    pub fn ended_pending(&self) -> bool {
        self.ended_latch
    }

    pub fn snapshot(&self) -> AudioSnapshot {
        AudioSnapshot {
            path: self
                .source_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            title: self.title.clone(),
            playing: self.is_playing(),
            paused: self.paused,
            position_secs: self.position().as_secs_f64(),
            duration_secs: self.duration.map(|d| d.as_secs_f64()).unwrap_or(0.0),
            volume: self.volume,
            bar_visible: self.bar_visible,
            ffmpeg_available: self.ffmpeg_available(),
            ended: self.ended_latch,
            is_stream: self.is_stream,
            peaks: self.peaks.clone(),
        }
    }

    /// Call once per frame (after add-on panels poll `audio_ended`).
    pub fn tick(&mut self) {
        // Late duration resolve — at most once per track (ffprobe is expensive).
        if self.duration.is_none() && !self.is_stream && !self.duration_probe_done {
            self.duration_probe_done = true;
            if let Some(path) = self.path.clone() {
                self.duration = probe_duration(&path, self.ffmpeg_dir.as_deref());
                if self.duration.is_none() {
                    if let Ok(file) = File::open(&path) {
                        if let Ok(dec) = Decoder::new(BufReader::new(file)) {
                            self.duration = dec.total_duration();
                        }
                    }
                }
            }
        }
        if self.ended_latch {
            // Cleared after panels/bars had a chance to see it last frame.
            self.ended_latch = false;
        }
        if !self.expect_end || self.paused {
            return;
        }
        if self.sink.as_ref().is_some_and(|s| s.empty()) {
            self.expect_end = false;
            self.paused = true;
            self.play_started = None;
            self.ended_latch = true;
        }
    }

    pub fn is_playing(&self) -> bool {
        if self.paused {
            return false;
        }
        self.sink.as_ref().is_some_and(|s| !s.empty())
    }

    pub fn position(&self) -> Duration {
        let mut pos = self.base_position;
        if !self.paused {
            if let Some(t0) = self.play_started {
                pos += t0.elapsed();
            }
        }
        if let Some(dur) = self.duration {
            pos.min(dur)
        } else {
            pos
        }
    }

    pub fn open_path(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        self.open_path_inner(path.as_ref(), false)
    }

    /// Open and start playback in one sink build (faster track switch).
    pub fn open_path_play(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        self.open_path_inner(path.as_ref(), true)
    }

    fn open_path_inner(&mut self, path: &Path, autoplay: bool) -> Result<(), String> {
        let path = path.to_path_buf();
        if !path.is_file() {
            return Err(format!("audio file not found: {}", path.display()));
        }
        self.kill_stream();
        self.is_stream = false;
        // Same file already loaded — refresh duration if missing, then seek/play.
        if self.source_path.as_ref() == Some(&path) && self.path.is_some() {
            if self.duration.is_none() {
                self.duration_probe_done = false;
                if let Some(play) = self.path.clone() {
                    self.duration = probe_duration(&play, self.ffmpeg_dir.as_deref());
                    self.duration_probe_done = self.duration.is_some();
                }
            }
            self.base_position = Duration::ZERO;
            self.ended_latch = false;
            self.rebuild_sink(Duration::ZERO, autoplay)?;
            self.bar_visible = true;
            return Ok(());
        }
        self.stop_internal();
        self.source_path = Some(path.clone());
        self.title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Audio")
            .to_string();
        self.base_position = Duration::ZERO;
        self.ended_latch = false;
        self.expect_end = false;
        self.last_error = None;

        let (play_path, duration) =
            resolve_playable(&path, self.ffmpeg_dir.as_deref(), &mut self.decode_cache)?;
        self.path = Some(play_path.clone());
        // Always resolve a real duration — without it the seek bar stays stuck at 0%.
        self.duration = duration.or_else(|| probe_duration(&play_path, self.ffmpeg_dir.as_deref()));
        if self.duration.is_none() {
            // Last resort: decode once and ask Symphonia after open.
            if let Ok(file) = File::open(&play_path) {
                if let Ok(dec) = Decoder::new(BufReader::new(file)) {
                    self.duration = dec.total_duration();
                }
            }
        }
        self.duration_probe_done = self.duration.is_some();
        self.peaks = compute_peaks(&play_path, 192);
        self.bar_visible = true;
        self.rebuild_sink(Duration::ZERO, autoplay)?;
        Ok(())
    }

    /// Live radio / HTTP stream via ffmpeg pipe (no seek, no duration).
    pub fn open_url_stream(&mut self, url: &str, title: &str) -> Result<(), String> {
        let url = url.trim();
        if url.is_empty() {
            return Err("empty stream url".into());
        }
        let ffmpeg = resolve_bin("ffmpeg", self.ffmpeg_dir.as_deref())
            .ok_or_else(|| "ffmpeg required for radio streams".to_string())?;
        self.kill_stream();
        self.stop_internal();
        self.is_stream = true;
        self.peaks.clear();
        self.duration = None;
        self.duration_probe_done = true; // streams have no duration
        self.base_position = Duration::ZERO;
        self.ended_latch = false;
        self.expect_end = false; // streams don't "end" cleanly
        self.source_path = Some(PathBuf::from(url));
        self.path = None;
        self.title = if title.is_empty() {
            "Radio".into()
        } else {
            title.to_string()
        };
        self.bar_visible = true;

        let mut child = Command::new(&ffmpeg);
        crate::os_win::hide_console(&mut child);
        let mut child = child
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-reconnect",
                "1",
                "-reconnect_streamed",
                "1",
                "-reconnect_delay_max",
                "5",
                "-i",
                url,
                "-vn",
                "-f",
                "s16le",
                "-acodec",
                "pcm_s16le",
                "-ar",
                "44100",
                "-ac",
                "2",
                "pipe:1",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn ffmpeg stream: {e}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ffmpeg stdout missing".to_string())?;
        let handle = self.handle.clone().ok_or_else(|| {
            self.last_error
                .clone()
                .unwrap_or_else(|| "no audio output device".into())
        })?;
        let source = RawPcmS16leSource::new(BufReader::new(stdout), 2, 44_100);
        let sink = Sink::try_new(&handle).map_err(|e| format!("audio sink: {e}"))?;
        sink.set_volume(self.volume);
        sink.append(source);
        sink.play();
        self.paused = false;
        self.play_started = Some(Instant::now());
        self.sink = Some(sink);
        self.stream_child = Some(child);
        self.last_error = None;
        Ok(())
    }

    fn kill_stream(&mut self) {
        if let Some(mut c) = self.stream_child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Warm decode cache for the next track (non-blocking).
    pub fn prefetch(&self, path: impl AsRef<Path>) {
        let path = path.as_ref().to_path_buf();
        if !path.is_file() {
            return;
        }
        let ffmpeg_dir = self.ffmpeg_dir.clone();
        std::thread::spawn(move || {
            let mut cache = HashMap::new();
            let _ = resolve_playable(&path, ffmpeg_dir.as_deref(), &mut cache);
        });
    }

    pub fn play(&mut self) -> Result<(), String> {
        if self.is_stream {
            if let Some(sink) = self.sink.as_ref() {
                if self.paused {
                    sink.play();
                    self.paused = false;
                    self.play_started = Some(Instant::now());
                }
                return Ok(());
            }
            return Err("no radio stream".into());
        }
        if self.path.is_none() {
            return Err("no audio loaded".into());
        }
        if self.sink.is_none() || self.sink.as_ref().is_some_and(|s| s.empty()) {
            let pos = self.position();
            self.rebuild_sink(pos, true)?;
        } else if self.paused {
            if let Some(sink) = self.sink.as_ref() {
                sink.play();
            }
            self.paused = false;
            self.play_started = Some(Instant::now());
        }
        self.expect_end = true;
        self.ended_latch = false;
        Ok(())
    }

    pub fn pause(&mut self) {
        if let Some(sink) = self.sink.as_ref() {
            let pos = self.position();
            sink.pause();
            self.base_position = pos;
            self.play_started = None;
            self.paused = true;
        }
        self.expect_end = false;
    }

    pub fn seek(&mut self, secs: f64) -> Result<(), String> {
        if self.is_stream {
            return Err("can't seek live radio".into());
        }
        let target = Duration::from_secs_f64(secs.max(0.0));
        let target = if let Some(dur) = self.duration {
            target.min(dur)
        } else {
            target
        };
        // Skip no-op seeks (scrub UI can fire near-identical values).
        let cur = self.position();
        if self.sink.is_some() {
            let diff = if target > cur {
                target - cur
            } else {
                cur - target
            };
            if diff < Duration::from_millis(80) {
                return Ok(());
            }
        }
        let was_playing = self.is_playing();
        self.rebuild_sink(target, was_playing)?;
        Ok(())
    }

    pub fn stop(&mut self) {
        let was_stream = self.is_stream;
        self.kill_stream();
        self.stop_internal();
        self.base_position = Duration::ZERO;
        self.paused = true;
        self.play_started = None;
        self.expect_end = false;
        self.ended_latch = false;
        if was_stream {
            self.is_stream = false;
            self.source_path = None;
            self.title.clear();
            self.peaks.clear();
            self.path = None;
            self.duration = None;
        }
        // Do not rebuild/play — stay silent at start until play().
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if let Some(sink) = self.sink.as_ref() {
            sink.set_volume(self.volume);
        }
    }

    pub fn set_bar_visible(&mut self, on: bool) {
        self.bar_visible = on;
    }

    fn stop_internal(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }

    fn rebuild_sink(&mut self, from: Duration, autoplay: bool) -> Result<(), String> {
        let handle = self.handle.clone().ok_or_else(|| {
            self.last_error
                .clone()
                .unwrap_or_else(|| "no audio output device".into())
        })?;
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| "no audio loaded".to_string())?
            .clone();

        self.stop_internal();

        let source = decode_playable(&path, from)?;
        let sink = Sink::try_new(&handle).map_err(|e| format!("audio sink: {e}"))?;
        sink.set_volume(self.volume);
        sink.append(source);
        if autoplay {
            sink.play();
            self.paused = false;
            self.play_started = Some(Instant::now());
            self.expect_end = true;
        } else {
            sink.pause();
            self.paused = true;
            self.play_started = None;
            self.expect_end = false;
        }
        self.base_position = from;
        self.sink = Some(sink);
        self.last_error = None;
        Ok(())
    }
}

fn resolve_playable(
    path: &Path,
    ffmpeg_dir: Option<&Path>,
    cache: &mut HashMap<PathBuf, CachedDecode>,
) -> Result<(PathBuf, Option<Duration>), String> {
    // Hit RAM cache first (instant track switch after first decode).
    if let Some(c) = cache.get(path) {
        if c.wav_path.is_file() {
            let dur = c.duration.or_else(|| probe_duration(&c.wav_path, ffmpeg_dir));
            return Ok((c.wav_path.clone(), dur));
        }
    }
    if let Some(dur) = try_symphonia_duration(path) {
        return Ok((path.to_path_buf(), Some(dur)));
    }
    if decode_symphonia_ok(path) {
        // Openable natively, but many MP3s have no total_duration — probe (ffprobe).
        let dur = probe_duration(path, ffmpeg_dir);
        return Ok((path.to_path_buf(), dur));
    }
    let wav = ensure_wav_cache(path, ffmpeg_dir, cache)?;
    let dur = cache
        .get(path)
        .and_then(|c| c.duration)
        .or_else(|| probe_duration(&wav, ffmpeg_dir));
    Ok((wav, dur))
}

fn try_symphonia_duration(path: &Path) -> Option<Duration> {
    let file = File::open(path).ok()?;
    let decoder = Decoder::new(BufReader::new(file)).ok()?;
    decoder.total_duration()
}

fn decode_symphonia_ok(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    Decoder::new(BufReader::new(file)).is_ok()
}

fn ensure_wav_cache(
    path: &Path,
    ffmpeg_dir: Option<&Path>,
    cache: &mut HashMap<PathBuf, CachedDecode>,
) -> Result<PathBuf, String> {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Some(c) = cache.get(path) {
        if c.wav_path.is_file() && c.source_mtime == mtime {
            return Ok(c.wav_path.clone());
        }
    }
    let ffmpeg = resolve_bin("ffmpeg", ffmpeg_dir)
        .ok_or_else(|| "ffmpeg not found (install ffmpeg or copy to dist/ffmpeg)".to_string())?;
    let dir = std::env::temp_dir().join("beautiful_audio_cache");
    let _ = std::fs::create_dir_all(&dir);
    let hash = simple_path_hash(path);
    let wav = dir.join(format!("{hash}.wav"));
    if wav.is_file() {
        if let Some(mt) = mtime {
            if let Ok(meta) = std::fs::metadata(&wav) {
                if let Ok(wt) = meta.modified() {
                    if wt >= mt {
                        let dur = probe_duration(&wav, None);
                        cache.insert(
                            path.to_path_buf(),
                            CachedDecode {
                                wav_path: wav.clone(),
                                source_mtime: mtime,
                                duration: dur,
                            },
                        );
                        return Ok(wav);
                    }
                }
            }
        }
    }
    let mut cmd = Command::new(&ffmpeg);
    crate::os_win::hide_console(&mut cmd);
    let status = cmd
        .args([
            "-y",
            "-i",
            &path.to_string_lossy(),
            "-vn",
            "-acodec",
            "pcm_s16le",
            "-ar",
            "44100",
            "-ac",
            "2",
            &wav.to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;
    if !status.status.success() {
        let err = String::from_utf8_lossy(&status.stderr);
        let _ = std::fs::remove_file(&wav);
        return Err(format!(
            "ffmpeg: {}",
            err.chars().take(400).collect::<String>()
        ));
    }
    let dur = probe_duration(&wav, None);
    cache.insert(
        path.to_path_buf(),
        CachedDecode {
            wav_path: wav.clone(),
            source_mtime: mtime,
            duration: dur,
        },
    );
    Ok(wav)
}

fn simple_path_hash(path: &Path) -> u64 {
    let s = path.to_string_lossy();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Live radio PCM from ffmpeg pipe (no Seek — rodio Decoder can't take ChildStdout).
struct RawPcmS16leSource<R: Read> {
    reader: R,
    channels: u16,
    sample_rate: u32,
}

impl<R: Read> RawPcmS16leSource<R> {
    fn new(reader: R, channels: u16, sample_rate: u32) -> Self {
        Self {
            reader,
            channels,
            sample_rate,
        }
    }
}

impl<R: Read> Iterator for RawPcmS16leSource<R> {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let mut buf = [0u8; 2];
        self.reader.read_exact(&mut buf).ok()?;
        let s = i16::from_le_bytes(buf);
        Some(s as f32 / 32768.0)
    }
}

impl<R: Read + Send> Source for RawPcmS16leSource<R> {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// Downsampled amplitude envelope for the streaming seek bar (0..1).
fn compute_peaks(path: &Path, bins: usize) -> Vec<f32> {
    let bins = bins.max(8);
    let Ok(file) = File::open(path) else {
        return vec![0.18; bins];
    };
    let Ok(decoder) = Decoder::new(BufReader::new(file)) else {
        return vec![0.18; bins];
    };
    let mut buckets: Vec<f32> = Vec::new();
    let mut cur = 0.0f32;
    let mut in_bucket = 0usize;
    let mut n = 0usize;
    const STRIDE: usize = 32;
    const PER_BUCKET: usize = 512;
    for s in decoder.convert_samples::<f32>() {
        if n % STRIDE == 0 {
            cur = cur.max(s.abs());
            in_bucket += 1;
            if in_bucket >= PER_BUCKET {
                buckets.push(cur);
                cur = 0.0;
                in_bucket = 0;
            }
        }
        n += 1;
        if n > 80_000_000 {
            break;
        }
    }
    if in_bucket > 0 {
        buckets.push(cur);
    }
    if buckets.is_empty() {
        return vec![0.15; bins];
    }
    let mut peaks = Vec::with_capacity(bins);
    for i in 0..bins {
        let t0 = (i as f32) / bins as f32;
        let t1 = ((i + 1) as f32) / bins as f32;
        let a = (t0 * buckets.len() as f32) as usize;
        let b = ((t1 * buckets.len() as f32) as usize).max(a + 1).min(buckets.len());
        let mut m = 0.0f32;
        for &v in &buckets[a..b] {
            m = m.max(v);
        }
        peaks.push(m);
    }
    let max = peaks.iter().copied().fold(0.0f32, f32::max).max(0.02);
    for p in &mut peaks {
        *p = (*p / max).sqrt().clamp(0.04, 1.0);
    }
    peaks
}

fn decode_playable(
    path: &Path,
    from: Duration,
) -> Result<Box<dyn Source<Item = f32> + Send>, String> {
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let mut decoder = Decoder::new(BufReader::new(file)).map_err(|e| format!("decode: {e}"))?;
    if from > Duration::ZERO {
        decoder
            .try_seek(from)
            .map_err(|e| format!("seek: {e}"))?;
    }
    Ok(Box::new(decoder.convert_samples::<f32>()))
}

fn probe_duration(path: &Path, ffmpeg_dir: Option<&Path>) -> Option<Duration> {
    // Prefer in-process (fast). ffprobe is a process spawn — last resort only.
    if let Some(d) = try_symphonia_duration(path) {
        return Some(d);
    }
    probe_duration_ffprobe(path, ffmpeg_dir)
}

fn probe_duration_ffprobe(path: &Path, ffmpeg_dir: Option<&Path>) -> Option<Duration> {
    let ffprobe = resolve_bin("ffprobe", ffmpeg_dir)?;
    let mut cmd = Command::new(ffprobe);
    crate::os_win::hide_console(&mut cmd);
    let out = cmd
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            &path.to_string_lossy(),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let secs: f64 = s.trim().parse().ok()?;
    if secs.is_finite() && secs > 0.0 {
        Some(Duration::from_secs_f64(secs))
    } else {
        None
    }
}

/// Path to `ffmpeg` (dist/ffmpeg, PATH, vendor, C:\ffmpeg\bin).
pub fn ffmpeg_path() -> Option<PathBuf> {
    resolve_bin("ffmpeg", locate_ffmpeg_dir().as_deref())
}

fn locate_ffmpeg_dir() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("ffmpeg");
            if cand.join("ffmpeg.exe").is_file() || cand.join("ffmpeg").is_file() {
                return Some(cand);
            }
            if dir.join("ffmpeg.exe").is_file() || dir.join("ffmpeg").is_file() {
                return Some(dir.to_path_buf());
            }
        }
    }
    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/ffmpeg");
    if vendor.join("ffmpeg.exe").is_file() || vendor.join("ffmpeg").is_file() {
        return Some(vendor);
    }
    let common = PathBuf::from(r"C:\ffmpeg\bin");
    if common.join("ffmpeg.exe").is_file() {
        return Some(common);
    }
    None
}

fn resolve_bin(name: &str, ffmpeg_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = ffmpeg_dir {
        let win = dir.join(format!("{name}.exe"));
        if win.is_file() {
            return Some(win);
        }
        let unix = dir.join(name);
        if unix.is_file() {
            return Some(unix);
        }
    }
    which_bin(name)
}

fn which_bin(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let win = dir.join(format!("{name}.exe"));
        if win.is_file() {
            return Some(win);
        }
        let unix = dir.join(name);
        if unix.is_file() {
            return Some(unix);
        }
    }
    None
}

/// Copy ffmpeg/ffprobe into `dist/ffmpeg/` from known locations (release packaging).
pub fn copy_ffmpeg_to_dist(dist: &Path) -> Result<(), String> {
    let src = locate_ffmpeg_dir().ok_or_else(|| "no ffmpeg found to copy".to_string())?;
    let dst = dist.join("ffmpeg");
    std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
    for name in ["ffmpeg.exe", "ffprobe.exe", "ffmpeg", "ffprobe"] {
        let from = src.join(name);
        if from.is_file() {
            std::fs::copy(&from, dst.join(name)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
