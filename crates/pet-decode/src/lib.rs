//! Streaming WebM (VP9 + alpha) decoding.
//!
//! Each clip gets one decoder thread. It converts frames to RGBA at the clip's
//! native resolution and delivers them paced to the clip's presentation
//! timestamps, staying at most 120 ms ahead of the playback clock. The UI holds
//! one frame at a time, so memory scales with resolution rather than duration.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling::{context::Context as Scaler, flag::Flags};
use ffmpeg::util::frame::video::Video;
use ffmpeg_next as ffmpeg;

/// How far ahead of the playback clock decoded frames may be queued.
const PREFETCH: Duration = Duration::from_millis(120);
/// Decoder threads wait this long for the UI's playback origin before giving up.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// One decoded frame in RGBA8, tightly packed.
pub struct RgbaFrame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

struct Decoded {
    frame: RgbaFrame,
    /// Presentation time relative to the start of the clip.
    due: Duration,
    /// Clip duration known from the container (`None` before the header is
    /// read, or when the container does not declare one).
    duration: Option<Duration>,
}

/// A clip being decoded and played.
pub struct Player {
    path: PathBuf,
    rx: mpsc::Receiver<Decoded>,
    pending: VecDeque<Decoded>,
    current: Option<RgbaFrame>,
    duration: Option<Duration>,
    origin: Instant,
    disconnected: bool,
    seq: u64,
}

impl Player {
    pub fn new(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let (start_tx, start_rx) = mpsc::channel::<Instant>();
        let thread_path = path.clone();
        let spawn = thread::Builder::new().name("pet-decode".to_owned()).spawn(move || decode_thread(thread_path, tx, start_rx));
        if let Err(err) = spawn {
            log::error!("cannot spawn decoder thread: {err}");
        }
        let origin = Instant::now();
        let _ = start_tx.send(origin);
        Self { path, rx, pending: VecDeque::new(), current: None, duration: None, origin, disconnected: false, seq: 0 }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Duration declared by the container, once known.
    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Moves due frames into `current` and returns when the next frame is due.
    ///
    /// Returns `None` when no frame is queued (the decoder is still working, or
    /// the clip has ended).
    pub fn advance(&mut self, now: Instant) -> Option<Duration> {
        loop {
            match self.rx.try_recv() {
                Ok(frame) => self.pending.push_back(frame),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    break;
                }
            }
        }

        let elapsed = now.saturating_duration_since(self.origin);
        while self.pending.front().is_some_and(|frame| frame.due <= elapsed) {
            if let Some(frame) = self.pending.pop_front() {
                self.current = Some(frame.frame);
                if frame.duration.is_some() {
                    self.duration = frame.duration;
                }
                self.seq += 1;
            }
        }

        self.pending.front().map(|frame| frame.due.saturating_sub(elapsed).max(Duration::from_millis(1)))
    }

    /// The most recently displayed frame. Kept after the clip ends so the last
    /// frame can stay on screen (dragging holds its final pose).
    pub fn current(&self) -> Option<&RgbaFrame> {
        self.current.as_ref()
    }

    /// Bumped whenever `current` changes; cheap change detection for the UI.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// True once the decoder finished and every queued frame was consumed.
    pub fn ended(&self) -> bool {
        self.disconnected && self.pending.is_empty()
    }
}

fn decode_thread(path: PathBuf, tx: mpsc::Sender<Decoded>, start_rx: mpsc::Receiver<Instant>) {
    let origin = match start_rx.recv_timeout(START_TIMEOUT) {
        Ok(origin) => origin,
        Err(_) => return,
    };
    if let Err(err) = decode_clip(&path, &tx, origin) {
        log::error!("decode {}: {err:#}", path.display());
    }
}

fn decode_clip(path: &Path, tx: &mpsc::Sender<Decoded>, origin: Instant) -> Result<()> {
    ffmpeg::init()?;
    let mut input = ffmpeg::format::input(path).with_context(|| format!("open {}", path.display()))?;
    let stream = input.streams().best(ffmpeg::media::Type::Video).ok_or_else(|| anyhow!("no video stream in {}", path.display()))?;
    let stream_index = stream.index();
    let time_base = stream.time_base();
    // Container duration. WebM stores it in `duration` (time_base units); the
    // upstream client uses it to time a walk's travel window.
    let duration = {
        let ticks = stream.duration();
        if ticks > 0 {
            let secs = ticks as f64 * f64::from(time_base.numerator()) / f64::from(time_base.denominator());
            (secs > 0.0).then(|| Duration::from_secs_f64(secs))
        } else {
            let us = input.duration();
            (us > 0).then(|| Duration::from_micros(us as u64))
        }
    };
    log::debug!("clip {} duration {:?}", path.display(), duration);

    // `from_parameters(...).decoder().video()` resolves the default `vp9`
    // decoder by codec id, which drops the alpha side stream. Only libvpx-vp9
    // decodes the WebM alpha channel into RGBA.
    let codec = ffmpeg::decoder::find_by_name("libvpx-vp9").ok_or_else(|| anyhow!("libvpx-vp9 decoder not available"))?;
    let mut context = ffmpeg::codec::context::Context::new_with_codec(codec);
    context.set_parameters(stream.parameters())?;
    let mut decoder = context.decoder().video()?;
    log::debug!("decoder {}x{} format {:?} for {}", decoder.width(), decoder.height(), decoder.format(), path.display());

    // The pixel format is only trustworthy once the first frame arrives, so the
    // scaler is built lazily. Frames stay at the native resolution: the clips
    // are authored at 640x360 and are never cropped or resized.
    let mut scaler: Option<Scaler> = None;
    let mut rgba = Video::empty();
    let mut frames = 0u64;
    let mut keep_going = true;

    let drain = |decoder: &mut ffmpeg::decoder::Video,
                 scaler: &mut Option<Scaler>,
                 rgba: &mut Video,
                 frames: &mut u64,
                 keep_going: &mut bool|
     -> Result<()> {
        let mut decoded = Video::empty();
        while decoder.receive_frame(&mut decoded).is_ok() {
            if scaler.is_none() {
                *scaler = Some(Scaler::get(
                    decoded.format(),
                    decoded.width(),
                    decoded.height(),
                    Pixel::RGBA,
                    decoded.width(),
                    decoded.height(),
                    Flags::BILINEAR,
                )?);
            }
            let scaler = scaler.as_mut().expect("scaler was just created");
            scaler.run(&decoded, rgba)?;

            let width = rgba.width() as usize;
            let height = rgba.height() as usize;
            let stride = rgba.stride(0);
            let data = rgba.data(0);
            let mut pixels = Vec::with_capacity(width * height * 4);
            if stride == width * 4 {
                pixels.extend_from_slice(&data[..width * height * 4]);
            } else {
                for row in 0..height {
                    let start = row * stride;
                    pixels.extend_from_slice(&data[start..start + width * 4]);
                }
            }

            let pts = decoded.pts().or_else(|| decoded.timestamp()).unwrap_or(0) as f64 * f64::from(time_base.numerator())
                / f64::from(time_base.denominator());
            let due = Duration::from_secs_f64(pts.max(0.0));
            let elapsed = Instant::now().saturating_duration_since(origin);
            if due > elapsed + PREFETCH {
                thread::sleep(due - elapsed - PREFETCH);
            }
            if tx.send(Decoded { frame: RgbaFrame { width, height, pixels }, due, duration }).is_err() {
                // The UI dropped the player (animation switch); stop quietly.
                *keep_going = false;
                return Ok(());
            }
            *frames += 1;
        }
        Ok(())
    };

    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        decoder.send_packet(&packet)?;
        drain(&mut decoder, &mut scaler, &mut rgba, &mut frames, &mut keep_going)?;
        if !keep_going {
            return Ok(());
        }
    }
    decoder.send_eof()?;
    drain(&mut decoder, &mut scaler, &mut rgba, &mut frames, &mut keep_going)?;
    log::debug!("end of stream for {} ({frames} frames)", path.display());
    Ok(())
}
