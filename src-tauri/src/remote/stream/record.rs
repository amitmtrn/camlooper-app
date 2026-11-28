//! Record mode: the phone streams its camera as for a live session, the desktop **tees** the
//! frames it feeds the live decoder into a file, and when the take ends that file is remuxed
//! and handed to the desktop's own loop engine — after which the phone is no longer needed.
//!
//! ```text
//!  live (first keyframe at the decoder) ── pre-roll ──▶ take ── stop / 60 s ──▶ remux ──▶ loop
//!                                          [`PreRoll`]   [`Take`]               [`finalize`]
//! ```
//!
//! * [`PreRoll`] — skip the encoder's ramp. A WebRTC sender starts small (180p is common) and
//!   steps the picture up over the first seconds, and one file holds one size: the take
//!   starts once the incoming size has been the same for [`SETTLE`], or [`SETTLE_CAP`] after
//!   the first keyframe, on a keyframe that is asked for then; failing that keyframe, from
//!   the last one seen at that size (the frames since then are held in memory for that).
//!   Nothing of an earlier, smaller size is kept.
//! * [`Take`] — the tee file's bytes. The file is IVF for both codecs (`VP80`, or `H264`
//!   with one Annex-B access unit per record — exactly what went to the decoder): IVF gives
//!   every frame a timestamp, which a bare Annex-B stream cannot. Timestamps are the RTP
//!   clock ([`TakeClock`]), falling back to arrival time where RTP makes no sense. A take
//!   whose picture grows within [`RESTART_WINDOW`] starts over at the larger size.
//! * [`finalize`] — the existing ffmpeg sidecar copies the take (`-c copy`, no re-encode)
//!   into the container the loader already accepts for that codec: MP4 for H.264, WebM for
//!   VP8. The result is sniffed like any phone video, fsynced and renamed into place.
//!
//! Everything lives in [`PhoneVideoStore`] (random names, `.part`, 0700, nothing from the
//! phone in a path), and every file here deletes itself unless it is committed.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use super::file_receive::{self, Hold, PartFile, PhoneContainer, PhoneVideoStore, Sniff, SNIFF_BYTES};
use super::signal::RECORD_CAP_MS;
use crate::pipeline::{self, EncodedCodec};

/// A take with less media than this (after the pre-roll) is not looped.
pub const MIN_TAKE_MS: u64 = 2000;
/// The take is finalized at this length whatever the phone does.
pub const RECORD_CAP: Duration = Duration::from_millis(RECORD_CAP_MS);
/// The take does not start before the incoming picture size has been unchanged this long…
pub const SETTLE: Duration = Duration::from_secs(1);
/// …or the stream has been live this long, whichever comes first.
pub const SETTLE_CAP: Duration = Duration::from_secs(3);
/// Once the size has settled a keyframe is asked for; without one after this long the take
/// starts from the last keyframe of that size.
pub const KEYFRAME_WAIT: Duration = Duration::from_millis(500);
/// A take whose picture grows while it is younger than this starts over at the larger size
/// (the ramp outlasted the pre-roll). Later, the take keeps its size and frames of another
/// are left out: a camera switch must not throw a long take away.
pub const RESTART_WINDOW: Duration = Duration::from_secs(3);
/// Frames held in memory during the pre-roll are bounded by time; this bounds them by size
/// as well.
const PRE_ROLL_MAX_BYTES: usize = 8 * 1024 * 1024;
/// A take is finalized when it reaches this size, like at [`RECORD_CAP`] (60 s at 30 Mbit/s).
pub const MAX_TAKE_BYTES: u64 = 256 * 1024 * 1024;
/// Free disk space asked for before a take starts (beyond the store's own margin).
const TAKE_ROOM: u64 = 64 * 1024 * 1024;
/// ffmpeg copies at most 60 s of video; this is for a disk that crawls.
pub const REMUX_TIMEOUT: Duration = Duration::from_secs(30);
/// The name a recorded take is loaded under (plus its extension).
pub const TAKE_NAME: &str = "Phone recording";

pub const TOO_SHORT: &str = "The recording was too short to loop. Record at least 2 seconds.";
pub const INCOMPLETE: &str = "The recording was too incomplete to loop. Try again.";

/// The RTP video clock.
const TICKS_PER_SECOND: u64 = 90_000;
/// A gap between two frames longer than this is a stall.
const STALL_TICKS: u64 = TICKS_PER_SECOND / 2;
/// A take must average at least this many frames per second.
const MIN_AVERAGE_FPS: u64 = 3;

const fn ticks(d: Duration) -> u64 {
    (d.as_millis() as u64) * TICKS_PER_SECOND / 1000
}

/// One frame as it was written to the live decoder.
#[derive(Debug, Clone)]
pub struct TakeFrame {
    pub codec: EncodedCodec,
    pub keyframe: bool,
    pub rtp_ts: u32,
    pub arrived: Instant,
    /// The picture size the stream had at this frame, where known.
    pub dims: Option<(u32, u32)>,
    /// H.264: the Annex-B access unit as the decoder got it (SPS/PPS before every IDR).
    /// VP8: the frame, without its IVF record header.
    pub data: Arc<[u8]>,
}

// ---------------------------------------------------------------------------------------
// Pre-roll
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum PreRollStep {
    Wait,
    /// Keep waiting, and ask the phone for a keyframe (once).
    WantKeyframe,
    /// The take starts with these frames; the first is a keyframe.
    Start(Vec<TakeFrame>),
}

/// The moment between "live" and the first teed frame.
#[derive(Debug)]
pub struct PreRoll {
    started: Instant,
    /// The picture size of the stream now, and when it became that.
    size: Option<(u32, u32)>,
    size_since: Instant,
    /// The frames since the last keyframe, that keyframe first; all of the current size.
    held: Vec<TakeFrame>,
    held_bytes: usize,
    /// When the keyframe for the settled size was asked for.
    asked: Option<Instant>,
}

impl PreRoll {
    /// `now`: when the stream went live.
    pub fn new(now: Instant) -> Self {
        Self { started: now, size: None, size_since: now, held: Vec::new(), held_bytes: 0, asked: None }
    }

    /// A frame was written to the decoder (the one that made the stream live included).
    pub fn push(&mut self, frame: TakeFrame) -> PreRollStep {
        let now = frame.arrived;
        if let Some(dims) = frame.dims {
            if self.size.is_some_and(|size| size != dims) {
                // The encoder stepped the picture up (or down): the wait starts again, and
                // nothing of the old size is kept or asked for.
                self.size_since = now;
                self.held.clear();
                self.held_bytes = 0;
                self.asked = None;
            }
            self.size = Some(dims);
        }
        let settled = now.saturating_duration_since(self.size_since) >= SETTLE
            || now.saturating_duration_since(self.started) >= SETTLE_CAP;
        if frame.keyframe {
            if settled {
                self.held.clear();
                return PreRollStep::Start(vec![frame]);
            }
            self.held_bytes = frame.data.len();
            self.held = vec![frame];
            return PreRollStep::Wait;
        }
        // Without a keyframe to start from there is nothing to hold on to (the decoder only
        // takes a stream that starts with one; this is the rest of a GOP of another size).
        match self.held.first() {
            Some(first) if first.codec == frame.codec => {
                self.held_bytes += frame.data.len();
                self.held.push(frame);
            }
            _ => {
                self.held.clear();
                self.held_bytes = 0;
            }
        }
        if self.held_bytes >= PRE_ROLL_MAX_BYTES {
            return PreRollStep::Start(std::mem::take(&mut self.held));
        }
        if !settled {
            return PreRollStep::Wait;
        }
        match self.asked {
            None => {
                self.asked = Some(now);
                PreRollStep::WantKeyframe
            }
            Some(asked) if now.saturating_duration_since(asked) >= KEYFRAME_WAIT && !self.held.is_empty() => {
                PreRollStep::Start(std::mem::take(&mut self.held))
            }
            Some(_) => PreRollStep::Wait,
        }
    }
}

// ---------------------------------------------------------------------------------------
// The take
// ---------------------------------------------------------------------------------------

/// Timestamps for the take, in 90 kHz ticks from its first frame. The RTP timestamp is the
/// capture clock and is used wherever it is plausible; where it is not (it went backwards,
/// leapt, or disagrees with the arrival clock by seconds — an encoder that restarted with a
/// new timestamp base), the arrival time is used for that step instead.
#[derive(Debug, Default)]
pub struct TakeClock {
    last: Option<(u32, Instant)>,
    pts: u64,
}

impl TakeClock {
    /// Largest step taken from RTP alone.
    const MAX_STEP: i64 = 10 * TICKS_PER_SECOND as i64;
    /// RTP and arrival may disagree by this much (jitter, a burst after a hiccup).
    const MAX_DISAGREEMENT: i64 = 2 * TICKS_PER_SECOND as i64;

    pub fn stamp(&mut self, rtp_ts: u32, arrived: Instant) -> u64 {
        if let Some((last_rtp, last_arrived)) = self.last {
            let by_rtp = rtp_ts.wrapping_sub(last_rtp) as i32 as i64;
            let by_arrival = ticks(arrived.saturating_duration_since(last_arrived)) as i64;
            let plausible = by_rtp > 0 && by_rtp <= Self::MAX_STEP && (by_rtp - by_arrival).abs() <= Self::MAX_DISAGREEMENT;
            let step = if plausible { by_rtp } else { by_arrival.max(1) };
            self.pts += step as u64;
        }
        self.last = Some((rtp_ts, arrived));
        self.pts
    }
}

/// What to do with a frame offered to the take.
#[derive(Debug, PartialEq, Eq)]
pub enum TakeStep {
    /// Append these bytes to the tee file.
    Write(Vec<u8>),
    /// The picture grew while the take was young: what was written so far is the encoder's
    /// ramp. Delete it and start the file (and the take's clock) again with these bytes.
    Restart(Vec<u8>),
    /// Not part of the take (it does not start on a keyframe, or the stream changed codec
    /// or size and has not come back).
    Skip,
    /// The take is at its cap (time or size): finalize it. The frame was not taken.
    Full,
}

/// Why a take cannot be looped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeFault {
    /// Less than [`MIN_TAKE_MS`] of media.
    TooShort,
    /// No keyframe ever started it, or it is mostly stalls.
    Incomplete,
}

impl TakeFault {
    pub fn message(self) -> &'static str {
        match self {
            TakeFault::TooShort => TOO_SHORT,
            TakeFault::Incomplete => INCOMPLETE,
        }
    }
}

/// A take that passed [`Take::verdict`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TakeSummary {
    pub codec: EncodedCodec,
    pub frames: u64,
    pub duration_ms: u64,
    pub bytes: u64,
}

impl TakeSummary {
    /// The frame rate that plays `frames` in `duration_ms` (for a remux without timestamps).
    pub fn average_fps(&self) -> f64 {
        // The last frame is shown for one more interval.
        let intervals = self.frames.saturating_sub(1).max(1) as f64;
        (intervals * 1000.0 / self.duration_ms.max(1) as f64).clamp(1.0, 120.0)
    }
}

/// The tee: builds the take file's bytes frame by frame. Pure — the caller appends what
/// [`Take::push`] returns to the file.
#[derive(Debug, Default)]
pub struct Take {
    codec: Option<EncodedCodec>,
    dims: Option<(u32, u32)>,
    clock: TakeClock,
    frames: u64,
    bytes: u64,
    last_pts: u64,
    stalled: u64,
    /// When the take's first frame arrived.
    began: Option<Instant>,
    /// The stream is not the take's any more (codec or size changed): wait for a keyframe
    /// of the original kind.
    skipping: bool,
}

impl Take {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn started(&self) -> bool {
        self.frames > 0
    }

    /// Media in the take: from its first frame to its last.
    pub fn elapsed_ms(&self) -> u64 {
        self.last_pts * 1000 / TICKS_PER_SECOND
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn has_minimum(&self) -> bool {
        self.elapsed_ms() >= MIN_TAKE_MS
    }

    pub fn push(&mut self, f: &TakeFrame) -> TakeStep {
        let Some(codec) = self.codec else {
            if !f.keyframe {
                return TakeStep::Skip;
            }
            return TakeStep::Write(self.begin(f));
        };
        // One file holds one size.
        let same_size = match (self.dims, f.dims) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        if f.codec != codec || !same_size {
            // Larger, and the take has only just begun: the encoder was still stepping the
            // picture up. The take is the larger picture, from its first keyframe.
            let grew = matches!((self.dims, f.dims), (Some(a), Some(b)) if area(b) > area(a));
            let young = self.began.is_some_and(|t| f.arrived.saturating_duration_since(t) < RESTART_WINDOW);
            if f.codec == codec && grew && young && f.keyframe {
                *self = Take::default();
                return TakeStep::Restart(self.begin(f));
            }
            // Otherwise (a camera switch, an encoder that restarted at another size late in
            // the take) those frames stay out until the original stream is back.
            self.skipping = true;
            return TakeStep::Skip;
        }
        if self.skipping {
            if !f.keyframe {
                return TakeStep::Skip;
            }
            self.skipping = false;
        }
        if self.dims.is_none() {
            self.dims = f.dims;
        }
        let pts = self.clock.stamp(f.rtp_ts, f.arrived);
        if pts > ticks(RECORD_CAP) || self.bytes + f.data.len() as u64 > MAX_TAKE_BYTES {
            return TakeStep::Full;
        }
        let step = pts - self.last_pts;
        if step > STALL_TICKS {
            self.stalled += step;
        }
        TakeStep::Write(self.record(Vec::with_capacity(12 + f.data.len()), pts, &f.data))
    }

    /// The file's header and first record: `f` is the keyframe the take starts on.
    fn begin(&mut self, f: &TakeFrame) -> Vec<u8> {
        self.codec = Some(f.codec);
        self.dims = f.dims;
        self.began = Some(f.arrived);
        let (w, h) = f.dims.unwrap_or((0, 0));
        let mut out = Vec::with_capacity(32 + 12 + f.data.len());
        out.extend_from_slice(&take_header(f.codec, w, h));
        let pts = self.clock.stamp(f.rtp_ts, f.arrived);
        self.record(out, pts, &f.data)
    }

    /// The size the take was started at.
    pub fn dims(&self) -> Option<(u32, u32)> {
        self.dims
    }

    fn record(&mut self, mut out: Vec<u8>, pts: u64, data: &[u8]) -> Vec<u8> {
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&pts.to_le_bytes());
        out.extend_from_slice(data);
        self.frames += 1;
        self.bytes += out.len() as u64;
        self.last_pts = pts;
        out
    }

    /// Whether what was teed is worth looping. A little loss is fine; a take that never
    /// started, is too short, or is mostly frozen picture is not.
    pub fn verdict(&self) -> Result<TakeSummary, TakeFault> {
        let Some(codec) = self.codec else { return Err(TakeFault::Incomplete) };
        if !self.has_minimum() {
            return Err(TakeFault::TooShort);
        }
        let mostly_stalls = self.stalled * 2 > self.last_pts;
        let too_few_frames = self.frames * TICKS_PER_SECOND < MIN_AVERAGE_FPS * self.last_pts;
        if mostly_stalls || too_few_frames {
            return Err(TakeFault::Incomplete);
        }
        Ok(TakeSummary { codec, frames: self.frames, duration_ms: self.elapsed_ms(), bytes: self.bytes })
    }
}

fn area((w, h): (u32, u32)) -> u64 {
    w as u64 * h as u64
}

/// The take file's IVF header: like [`super::media::ivf_header`] (timebase 1/90000), with
/// the codec's FourCC. ffmpeg's IVF demuxer reads `H264` records as Annex-B access units.
pub fn take_header(codec: EncodedCodec, width: u32, height: u32) -> [u8; 32] {
    let mut h = super::media::ivf_header(width.min(u16::MAX as u32) as u16, height.min(u16::MAX as u32) as u16);
    h[8..12].copy_from_slice(match codec {
        EncodedCodec::H264 => b"H264",
        EncodedCodec::Vp8 => b"VP80",
    });
    h
}

/// The frames of a take file (the payloads of its IVF records), for the H.264 fallback and
/// for tests. Stops at the first record that is cut short.
pub fn take_payloads(file: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 32;
    while i + 12 <= file.len() {
        let n = u32::from_le_bytes([file[i], file[i + 1], file[i + 2], file[i + 3]]) as usize;
        let Some(data) = file.get(i + 12..i + 12 + n) else { break };
        out.push(data);
        i += 12 + n;
    }
    out
}

// ---------------------------------------------------------------------------------------
// Remux
// ---------------------------------------------------------------------------------------

/// What a take of `codec` becomes: the container (as the loader names it), ffmpeg's muxer
/// and the file extension.
pub fn target_of(codec: EncodedCodec) -> (PhoneContainer, &'static str, &'static str) {
    match codec {
        EncodedCodec::H264 => (PhoneContainer::IsoBmff, "mp4", "mp4"),
        EncodedCodec::Vp8 => (PhoneContainer::Matroska, "webm", "webm"),
    }
}

/// How the take is read by ffmpeg.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TakeInput {
    /// The tee file itself: IVF, every frame with its timestamp.
    Ivf,
    /// H.264 only, for an ffmpeg whose IVF demuxer does not take H.264: the bare Annex-B
    /// stream at a constant frame rate that gives the take's duration.
    RawH264 { fps: f64 },
}

/// ffmpeg arguments that copy the take at `input` into `output`. The input is held to one
/// demuxer and the `file` protocol like every other file that came from outside
/// ([`pipeline::restricted_input_args`]); nothing is re-encoded.
pub fn remux_args(codec: EncodedCodec, how: TakeInput, input: &Path, output: &Path) -> Vec<String> {
    let (_, muxer, _) = target_of(codec);
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y"].map(String::from).to_vec();
    match how {
        TakeInput::Ivf => a.extend(pipeline::restricted_input_args("ivf")),
        TakeInput::RawH264 { fps } => {
            // `-r`, not `-framerate`: the latter is only a default that timing information
            // in the stream itself (the encoder's nominal rate) overrides.
            a.extend(["-r".into(), format!("{fps:.3}")]);
            a.extend(pipeline::restricted_input_args("h264"));
        }
    }
    a.extend(["-i".into(), input.to_string_lossy().into_owned()]);
    a.extend(["-map", "0:v:0", "-an", "-sn", "-dn", "-c", "copy"].map(String::from));
    if muxer == "mp4" {
        // The index first: the loader reads the header without seeking to the end.
        a.extend(["-movflags".into(), "+faststart".into()]);
    }
    a.extend(["-f".into(), muxer.into(), output.to_string_lossy().into_owned()]);
    a
}

async fn run_ffmpeg(ffmpeg: &Path, args: Vec<String>) -> Result<()> {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let child = cmd.spawn().map_err(|e| anyhow!("could not start ffmpeg ({}): {e}", ffmpeg.display()))?;
    let out = match tokio::time::timeout(REMUX_TIMEOUT, child.wait_with_output()).await {
        Ok(out) => out?,
        Err(_) => return Err(anyhow!("ffmpeg did not finish in {} s", REMUX_TIMEOUT.as_secs())),
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        return Err(anyhow!("ffmpeg ended with {} ({tail})", out.status));
    }
    Ok(())
}

/// A new file in the store with room for a take.
pub async fn create_take_file(store: &PhoneVideoStore) -> std::io::Result<PartFile> {
    store.create_sized(TAKE_ROOM).await
}

/// A finished take, remuxed and in place.
#[derive(Debug)]
pub struct Recorded {
    pub path: PathBuf,
    /// Keeps the file out of the store's sweeps until it is loaded (or given up on).
    pub hold: Hold,
    pub container: PhoneContainer,
    /// What the loader is to call it: [`TAKE_NAME`] plus the extension.
    pub display_name: String,
}

/// Turn the teed `take` into a video the loop engine loads: remux, check that the result is
/// what it should be, fsync, rename. The take file is deleted either way, and so is the
/// result unless it is returned. Dropping the future kills ffmpeg and deletes both.
pub async fn finalize(ffmpeg: &Path, store: &PhoneVideoStore, take: PartFile, summary: TakeSummary) -> Result<Recorded> {
    let (container, _, ext) = target_of(summary.codec);
    let out = store.create_sized(summary.bytes)
        .await
        .map_err(|e| anyhow!("could not create the video file: {e}"))?;
    let copied = run_ffmpeg(ffmpeg, remux_args(summary.codec, TakeInput::Ivf, take.path(), out.path())).await;
    if let Err(e) = copied {
        if summary.codec != EncodedCodec::H264 {
            return Err(e);
        }
        eprintln!("phone recording: remux with timestamps failed ({e:#}); trying a constant frame rate");
        let raw = store.create_sized(summary.bytes).await?;
        let (from, to) = (take.path().to_path_buf(), raw.path().to_path_buf());
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let file = std::fs::read(from)?;
            std::fs::write(to, take_payloads(&file).concat())
        })
        .await??;
        let how = TakeInput::RawH264 { fps: summary.average_fps() };
        run_ffmpeg(ffmpeg, remux_args(summary.codec, how, raw.path(), out.path())).await?;
    }
    drop(take);
    // The same question every phone video is asked: is this, by its content, the container
    // it is about to be loaded as?
    let head = read_head(out.path()).await?;
    match file_receive::sniff(&head, head.len() < SNIFF_BYTES) {
        Sniff::Video { container: found, .. } if found == container => {}
        other => return Err(anyhow!("the remuxed take is not a {ext} file ({other:?})")),
    }
    let (path, hold) = out.commit(ext).await.map_err(|e| anyhow!("could not save the video file: {e}"))?;
    Ok(Recorded { path, hold, container, display_name: format!("{TAKE_NAME}.{ext}") })
}

async fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    tokio::fs::File::open(path).await?.take(SNIFF_BYTES as u64).read_to_end(&mut head).await?;
    Ok(head)
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::remote::stream::media::{annexb_nals, h264_is_keyframe};

    pub(crate) fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// The H.264 fixture of the loopback test, as access units (640x360, 30 fps).
    pub(crate) fn h264_fixture() -> Vec<Vec<u8>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phone-stream/h264-640x360.h264");
        let data = std::fs::read(path).expect("fixture");
        let (mut units, mut current) = (Vec::new(), Vec::new());
        for nal in annexb_nals(&data) {
            current.extend_from_slice(&[0, 0, 0, 1]);
            current.extend_from_slice(nal);
            if matches!(nal[0] & 0x1f, 1 | 5) {
                units.push(std::mem::take(&mut current));
            }
        }
        units
    }

    /// `count` fixture frames as the decoder would have been given them, `interval` apart,
    /// starting at the fixture's first frame (a keyframe).
    pub(crate) fn h264_frames(count: usize, interval: Duration, t0: Instant) -> Vec<TakeFrame> {
        let units = h264_fixture();
        (0..count)
            .map(|i| {
                let data = &units[i % units.len()];
                TakeFrame {
                    codec: EncodedCodec::H264,
                    keyframe: h264_is_keyframe(data),
                    rtp_ts: (ticks(interval) * i as u64) as u32,
                    arrived: t0 + interval * i as u32,
                    dims: Some((640, 360)),
                    data: data.as_slice().into(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    const FRAME: Duration = Duration::from_micros(33_333);

    fn frame(key: bool, at: Instant, rtp_ts: u32) -> TakeFrame {
        TakeFrame {
            codec: EncodedCodec::Vp8,
            keyframe: key,
            rtp_ts,
            arrived: at,
            dims: Some((640, 360)),
            data: vec![if key { 0x10 } else { 0x11 }; 100].into(),
        }
    }

    /// Frames 1/30 s apart from `t0`; a keyframe where `key(i)`.
    fn run(t0: Instant, n: u32, key: impl Fn(u32) -> bool) -> Vec<TakeFrame> {
        (0..n).map(|i| frame(key(i), t0 + FRAME * i, i * 3000)).collect()
    }

    /// Where the take started: the index of the frame that started it, the frames it started
    /// with, and how often a keyframe was asked for.
    fn started_at(frames: Vec<TakeFrame>, t0: Instant) -> (usize, Vec<TakeFrame>, u32) {
        let mut pre = PreRoll::new(t0);
        let mut asked = 0;
        for (i, f) in frames.into_iter().enumerate() {
            match pre.push(f) {
                PreRollStep::Wait => {}
                PreRollStep::WantKeyframe => asked += 1,
                PreRollStep::Start(first) => {
                    assert!(first[0].keyframe, "a take starts on a keyframe");
                    return (i, first, asked);
                }
            }
        }
        panic!("the take never started");
    }

    /// A frame of `dims` at `ms` after `t0`.
    fn sized(key: bool, t0: Instant, ms: u64, dims: (u32, u32)) -> TakeFrame {
        TakeFrame { dims: Some(dims), ..frame(key, t0 + Duration::from_millis(ms), (ms * 90) as u32) }
    }

    /// 30 fps from `from_ms` up to `to_ms`, all of `dims`; keyframes at the listed times
    /// (rounded to a frame).
    fn stretch(t0: Instant, from_ms: u64, to_ms: u64, dims: (u32, u32), keys: &[u64]) -> Vec<TakeFrame> {
        (from_ms / 33..to_ms / 33).map(|i| sized(keys.iter().any(|k| k / 33 == i), t0, i * 33, dims)).collect()
    }

    const P180: (u32, u32) = (320, 180);
    const P270: (u32, u32) = (480, 270);
    const P720: (u32, u32) = (1280, 720);

    #[test]
    fn the_constants_are_the_specified_ones() {
        assert_eq!(MIN_TAKE_MS, 2000);
        assert_eq!(RECORD_CAP, Duration::from_secs(60));
        assert_eq!((SETTLE, SETTLE_CAP, RESTART_WINDOW), (Duration::from_secs(1), Duration::from_secs(3), Duration::from_secs(3)));
        assert_eq!(INCOMPLETE, "The recording was too incomplete to loop. Try again.");
    }

    #[test]
    fn a_steady_stream_starts_on_the_keyframe_asked_for_after_one_second() {
        let t0 = Instant::now();
        // A second keyframe at 200 ms is too early to tell that the size will stay. The one
        // at 1.2 s is the answer to the request made at 1 s.
        let (at, first, asked) = started_at(run(t0, 90, |i| i == 0 || i == 6 || i == 36), t0);
        assert_eq!((at, first.len(), asked), (36, 1, 1));
    }

    #[test]
    fn without_the_keyframe_the_take_starts_from_the_last_one_of_that_size() {
        let t0 = Instant::now();
        let (at, first, asked) = started_at(run(t0, 90, |i| i == 0 || i == 6), t0);
        // Asked at 1 s, given up on half a second later (frame 46 is the first past 1.5 s).
        assert!((45..=47).contains(&at), "{at}");
        assert_eq!(first.len(), at + 1 - 6, "everything since the last keyframe, so the file starts decodable");
        assert_eq!(asked, 1);
    }

    /// The bug this pre-roll is for: a sender that starts at 180p and steps up. The take
    /// must be the picture the stream settles on, with nothing of the ramp in it.
    #[test]
    fn the_take_waits_for_the_resolution_ramp_to_settle() {
        let t0 = Instant::now();
        let mut frames = stretch(t0, 0, 600, P180, &[0]);
        frames.extend(stretch(t0, 600, 1400, P270, &[600]));
        // 720p from 1.4 s; it has been that for a second at 2.4 s, the keyframe comes at 2.5 s.
        frames.extend(stretch(t0, 1400, 6000, P720, &[1400, 2500]));
        let (at, first, asked) = started_at(frames.clone(), t0);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].dims, Some(P720));
        assert_eq!(first[0].arrived, frames[at].arrived);
        let started_ms = first[0].arrived.duration_since(t0).as_millis();
        assert!((2450..2550).contains(&started_ms), "{started_ms}");
        assert_eq!(asked, 1, "one request, once the size had settled; none for the sizes that came and went");
    }

    #[test]
    fn a_ramp_without_the_keyframe_starts_from_the_first_frame_of_the_settled_size() {
        let t0 = Instant::now();
        let mut frames = stretch(t0, 0, 700, P180, &[0, 400]);
        frames.extend(stretch(t0, 700, 6000, P720, &[700]));
        let (_, first, asked) = started_at(frames, t0);
        // Settled at 1.7 s, asked, started at 2.2 s from the 720p keyframe at 0.7 s.
        assert!(first.iter().all(|f| f.dims == Some(P720)), "no frame of the 180p prelude is in the take");
        assert_eq!(first[0].arrived.duration_since(t0).as_millis() / 33, 700 / 33);
        let held_ms = first.last().unwrap().arrived.duration_since(t0).as_millis();
        assert!((2150..2300).contains(&held_ms), "{held_ms}");
        assert_eq!(asked, 1);
    }

    #[test]
    fn a_size_that_never_settles_starts_the_take_three_seconds_in() {
        let t0 = Instant::now();
        // A new size every 800 ms, each with its keyframe.
        let sizes = [P180, P270, (640, 360), (848, 480), (960, 540), P720, (1920, 1080)];
        let frames: Vec<TakeFrame> =
            sizes.iter().enumerate().flat_map(|(n, d)| stretch(t0, n as u64 * 800, (n as u64 + 1) * 800, *d, &[n as u64 * 800])).collect();
        let (_, first, _) = started_at(frames, t0);
        // 3 s falls into the 2.4–3.2 s stretch (848x480): a keyframe is asked for, and the
        // one that comes is the next size's, at 3.2 s.
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].dims, Some((960, 540)));
        assert_eq!(first[0].arrived.duration_since(t0).as_millis() / 33, 3200 / 33);
    }

    #[test]
    fn rtp_time_is_used_where_it_makes_sense_and_arrival_time_where_not() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut c = TakeClock::default();
        assert_eq!(c.stamp(4_000_000_000, t), 0);
        // RTP says 33 ms although it arrived 80 ms later (jitter): RTP is the capture clock.
        assert_eq!(c.stamp(4_000_003_000, t + ms(80)), 3000);
        // Across the 32-bit wrap.
        let mut w = TakeClock::default();
        w.stamp(u32::MAX - 1000, t);
        assert_eq!(w.stamp(2000, t + ms(33)), 3001);
        // Backwards, or a leap the arrival clock knows nothing of: arrival time instead.
        assert_eq!(c.stamp(1_000, t + ms(180)), 3000 + 9000);
        assert_eq!(c.stamp(1_000 + 90_000 * 300, t + ms(280)), 12_000 + 9000);
        // The same instant twice still moves forward.
        assert_eq!(c.stamp(5, t + ms(280)), 21_001);
    }

    fn written(step: TakeStep) -> Vec<u8> {
        match step {
            TakeStep::Write(b) => b,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_take_file_is_ivf_with_take_relative_timestamps() {
        let t0 = Instant::now();
        let mut take = Take::new();
        // A delta frame first: nothing is written until a keyframe.
        assert_eq!(take.push(&frame(false, t0, 9000)), TakeStep::Skip);
        assert!(!take.started());
        let first = written(take.push(&frame(true, t0, 12_000)));
        assert_eq!(&first[..4], b"DKIF");
        assert_eq!(&first[8..12], b"VP80");
        assert_eq!((u16::from_le_bytes([first[12], first[13]]), u16::from_le_bytes([first[14], first[15]])), (640, 360));
        assert_eq!(u32::from_le_bytes(first[16..20].try_into().unwrap()), 90_000);
        assert_eq!(u32::from_le_bytes(first[32..36].try_into().unwrap()), 100);
        assert_eq!(u64::from_le_bytes(first[36..44].try_into().unwrap()), 0, "the take's own zero");
        let second = written(take.push(&frame(false, t0 + FRAME, 15_000)));
        assert_eq!(second.len(), 12 + 100, "no second header");
        assert_eq!(u64::from_le_bytes(second[4..12].try_into().unwrap()), 3000);
        assert_eq!((take.frames(), take.elapsed_ms()), (2, 33));
        let file = [first, second].concat();
        assert_eq!(take_payloads(&file).len(), 2);
        assert_eq!(&take_header(EncodedCodec::H264, 1280, 720)[8..12], b"H264");
    }

    fn take_of(frames: &[TakeFrame]) -> Take {
        let mut take = Take::new();
        for f in frames {
            take.push(f);
        }
        take
    }

    #[test]
    fn a_take_under_two_seconds_is_too_short_and_one_without_a_keyframe_is_incomplete() {
        let t0 = Instant::now();
        assert_eq!(take_of(&run(t0, 59, |i| i == 0)).verdict(), Err(TakeFault::TooShort));
        assert_eq!(TakeFault::TooShort.message(), TOO_SHORT);
        let ok = take_of(&run(t0, 62, |i| i == 0)).verdict().expect("2 s of media");
        assert_eq!((ok.frames, ok.codec), (62, EncodedCodec::Vp8));
        assert!((2000..2100).contains(&ok.duration_ms), "{ok:?}");
        assert!((ok.average_fps() - 30.0).abs() < 0.5, "{ok:?}");
        // Never a keyframe: nothing was teed, and that is not "too short".
        let none = take_of(&run(t0, 300, |_| false));
        assert!(!none.started());
        assert_eq!(none.verdict(), Err(TakeFault::Incomplete));
        assert_eq!(TakeFault::Incomplete.message(), INCOMPLETE);
    }

    #[test]
    fn a_little_loss_is_fine_but_a_take_that_is_mostly_stalls_is_not() {
        let t0 = Instant::now();
        let at = |ms: u64, key: bool| frame(key, t0 + Duration::from_millis(ms), (ms * 90) as u32);
        // 4 s at 30 fps with one 700 ms hole (lost frames, then the keyframe that was asked for).
        let mut frames: Vec<TakeFrame> = (0..60).map(|i| at(i * 33, i == 0)).collect();
        frames.extend((0..40).map(|i| at(2700 + i * 33, i == 0)));
        assert!(take_of(&frames).verdict().is_ok());
        // 6 s in which the picture moved for well under half the time.
        let mut frames: Vec<TakeFrame> = (0..10).map(|i| at(i * 33, i == 0)).collect();
        frames.extend((0..10).map(|i| at(3000 + i * 33, i == 0)));
        frames.extend((0..10).map(|i| at(6000 + i * 33, i == 0)));
        assert_eq!(take_of(&frames).verdict(), Err(TakeFault::Incomplete));
        // A slide show.
        let frames: Vec<TakeFrame> = (0..8).map(|i| at(i * 450, i == 0)).collect();
        assert_eq!(take_of(&frames).verdict(), Err(TakeFault::Incomplete));
    }

    #[test]
    fn the_take_is_full_at_sixty_seconds() {
        let t0 = Instant::now();
        let mut take = Take::new();
        let at = |ms: u64, key: bool| frame(key, t0 + Duration::from_millis(ms), (ms * 90) as u32);
        // One frame a 100 ms, to keep the test small.
        for i in 0..=600u64 {
            assert!(matches!(take.push(&at(i * 100, i == 0)), TakeStep::Write(_)), "frame {i}");
        }
        assert_eq!(take.elapsed_ms(), 60_000);
        assert_eq!(take.push(&at(60_100, false)), TakeStep::Full);
        assert_eq!(take.push(&at(60_200, true)), TakeStep::Full);
        let summary = take.verdict().expect("a full take is a good take");
        assert_eq!((summary.frames, summary.duration_ms), (601, 60_000));
    }

    #[test]
    fn frames_of_another_size_or_codec_stay_out_until_the_stream_is_back() {
        let t0 = Instant::now();
        let mut take = Take::new();
        let mut i = 0u32;
        let mut next = |key: bool, dims: (u32, u32), codec: EncodedCodec| {
            i += 1;
            TakeFrame { dims: Some(dims), codec, ..frame(key, t0 + FRAME * i, i * 3000) }
        };
        let (big, small) = ((640, 360), (320, 180));
        assert!(matches!(take.push(&next(true, big, EncodedCodec::Vp8)), TakeStep::Write(_)));
        assert!(matches!(take.push(&next(false, big, EncodedCodec::Vp8)), TakeStep::Write(_)));
        // The camera was switched: a keyframe at another size, and what follows it.
        assert_eq!(take.push(&next(true, small, EncodedCodec::Vp8)), TakeStep::Skip);
        assert_eq!(take.push(&next(false, small, EncodedCodec::Vp8)), TakeStep::Skip);
        // Back at the original size, but mid-GOP: not decodable after the hole.
        assert_eq!(take.push(&next(false, big, EncodedCodec::Vp8)), TakeStep::Skip);
        assert_eq!(take.push(&next(true, big, EncodedCodec::H264)), TakeStep::Skip);
        assert!(matches!(take.push(&next(true, big, EncodedCodec::Vp8)), TakeStep::Write(_)));
        assert!(matches!(take.push(&next(false, big, EncodedCodec::Vp8)), TakeStep::Write(_)));
        assert_eq!(take.frames(), 4);
    }

    fn header_dims(bytes: &[u8]) -> (u32, u32) {
        assert_eq!(&bytes[..4], b"DKIF", "a new file starts with a header");
        (u16::from_le_bytes([bytes[12], bytes[13]]) as u32, u16::from_le_bytes([bytes[14], bytes[15]]) as u32)
    }

    /// The ramp outlasted the pre-roll: the take had started at 180p when 270p came. What
    /// was teed is thrown away and the take — file, clock, frame count — starts again.
    #[test]
    fn a_take_whose_picture_grows_in_its_first_three_seconds_starts_over() {
        let t0 = Instant::now();
        let mut take = Take::new();
        let small: Vec<TakeFrame> = stretch(t0, 0, 2000, P180, &[0]);
        for f in &small {
            assert!(matches!(take.push(f), TakeStep::Write(_)));
        }
        assert_eq!((take.frames(), take.dims()), (small.len() as u64, Some(P180)));
        assert!(take.elapsed_ms() > 1900);
        // The rest of a larger GOP is of no use; its keyframe is where the take restarts.
        assert_eq!(take.push(&sized(false, t0, 2000, P270)), TakeStep::Skip);
        let restarted = match take.push(&sized(true, t0, 2033, P270)) {
            TakeStep::Restart(bytes) => bytes,
            other => panic!("{other:?}"),
        };
        assert_eq!(header_dims(&restarted), P270);
        assert_eq!(u64::from_le_bytes(restarted[36..44].try_into().unwrap()), 0, "the new take's own zero");
        assert_eq!((take.frames(), take.elapsed_ms(), take.dims()), (1, 0, Some(P270)));
        assert_eq!(take.verdict(), Err(TakeFault::TooShort), "the two seconds of prelude do not count");
        // And once more, to 720p, one second into the second attempt.
        assert!(matches!(take.push(&sized(false, t0, 2066, P270)), TakeStep::Write(_)));
        let again = match take.push(&sized(true, t0, 3033, P720)) {
            TakeStep::Restart(bytes) => bytes,
            other => panic!("{other:?}"),
        };
        assert_eq!(header_dims(&again), P720);
        // From here it is an ordinary take of 61 s: full at 60 s of its own clock.
        let mut file = again;
        let mut full_at = None;
        for i in 1..=610u64 {
            match take.push(&sized(false, t0, 3033 + i * 100, P720)) {
                TakeStep::Write(bytes) => file.extend(bytes),
                TakeStep::Full => {
                    full_at = Some(i);
                    break;
                }
                other => panic!("frame {i}: {other:?}"),
            }
        }
        assert_eq!(full_at, Some(601), "60 s from the restart, not from the first attempt");
        assert_eq!(take_payloads(&file).len(), 601);
        assert_eq!(take.verdict().expect("a full take").duration_ms, 60_000);
    }

    #[test]
    fn after_three_seconds_a_larger_picture_does_not_restart_the_take() {
        let t0 = Instant::now();
        let mut take = Take::new();
        for f in &stretch(t0, 0, 3100, P270, &[0]) {
            assert!(matches!(take.push(f), TakeStep::Write(_)));
        }
        let teed = take.frames();
        // A camera switch to a sharper camera, 3.1 s in: today's behaviour, the take is kept.
        assert_eq!(take.push(&sized(true, t0, 3100, P720)), TakeStep::Skip);
        assert_eq!(take.push(&sized(false, t0, 3133, P720)), TakeStep::Skip);
        assert_eq!((take.frames(), take.dims()), (teed, Some(P270)));
        // Back at its own size it goes on, from a keyframe.
        assert_eq!(take.push(&sized(false, t0, 3166, P270)), TakeStep::Skip);
        assert!(matches!(take.push(&sized(true, t0, 3200, P270)), TakeStep::Write(_)));
        assert_eq!(take.frames(), teed + 1);
    }

    #[test]
    fn a_smaller_picture_or_another_codec_never_restarts_the_take() {
        let t0 = Instant::now();
        let mut take = Take::new();
        assert!(matches!(take.push(&sized(true, t0, 0, P720)), TakeStep::Write(_)));
        assert!(matches!(take.push(&sized(false, t0, 33, P720)), TakeStep::Write(_)));
        // The encoder steps down for a moment (congestion): the 720p take is not traded in.
        assert_eq!(take.push(&sized(true, t0, 500, P270)), TakeStep::Skip);
        // A larger picture in another codec is another stream altogether.
        let other = TakeFrame { codec: EncodedCodec::H264, ..sized(true, t0, 600, (1920, 1080)) };
        assert_eq!(take.push(&other), TakeStep::Skip);
        assert!(matches!(take.push(&sized(true, t0, 700, P720)), TakeStep::Write(_)));
        assert_eq!((take.frames(), take.dims()), (3, Some(P720)));
    }

    #[test]
    fn the_remux_copies_from_a_named_demuxer_and_the_file_protocol_only() {
        let (i, o) = (Path::new("/store/phone-1.part"), Path::new("/store/phone-2.part"));
        let h264 = remux_args(EncodedCodec::H264, TakeInput::Ivf, i, o).join(" ");
        assert!(h264.contains("-protocol_whitelist file -f ivf -i /store/phone-1.part"), "{h264}");
        assert!(h264.contains("-c copy"), "{h264}");
        assert!(h264.ends_with("-movflags +faststart -f mp4 /store/phone-2.part"), "{h264}");
        let vp8 = remux_args(EncodedCodec::Vp8, TakeInput::Ivf, i, o).join(" ");
        assert!(vp8.ends_with("-c copy -f webm /store/phone-2.part"), "{vp8}");
        let raw = remux_args(EncodedCodec::H264, TakeInput::RawH264 { fps: 29.97 }, i, o).join(" ");
        assert!(raw.contains("-r 29.970 -protocol_whitelist file -f h264 -i /store/phone-1.part"), "{raw}");
        assert_eq!(target_of(EncodedCodec::H264), (PhoneContainer::IsoBmff, "mp4", "mp4"));
        assert_eq!(target_of(EncodedCodec::Vp8), (PhoneContainer::Matroska, "webm", "webm"));
    }

    /// Tee `frames` into a take file in `store`, as the session does.
    async fn teed(store: &PhoneVideoStore, frames: &[TakeFrame]) -> (PartFile, Take) {
        let mut part = create_take_file(store).await.unwrap();
        let mut take = Take::new();
        for f in frames {
            if let TakeStep::Write(bytes) = take.push(f) {
                part.write(&bytes).await.unwrap();
            }
        }
        (part, take)
    }

    fn names(store: &PhoneVideoStore) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(store.dir())
            .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Real H.264 through the tee and the real ffmpeg: the result is an MP4 by content, it
    /// is what the loader's own probe accepts, and it lasts as long as the take did.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_teed_h264_take_becomes_an_mp4_the_loader_accepts() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        // 3 s at an uneven pace (every tenth frame late), so only real timestamps give 3.4 s.
        let t0 = Instant::now();
        let mut frames = h264_frames(90, FRAME, t0);
        let mut late = 0u32;
        for (i, f) in frames.iter_mut().enumerate() {
            if i > 0 && i % 10 == 0 {
                late += 1;
            }
            f.rtp_ts += late * 4000;
            f.arrived += Duration::from_micros(44_444) * late;
        }
        let (part, take) = teed(&store, &frames).await;
        let summary = take.verdict().expect("a good take");
        assert_eq!(summary.frames, 90);
        let teed_ms = summary.duration_ms;
        // 89 steps of 33 ms (the fixture clock's whole milliseconds) and 8 late ones.
        assert!((3250..3350).contains(&teed_ms), "{teed_ms}");

        let done = finalize(Path::new("ffmpeg"), &store, part, summary).await.expect("remux");
        assert_eq!(done.container, PhoneContainer::IsoBmff);
        assert_eq!(done.display_name, "Phone recording.mp4");
        assert!(store.contains(&done.path) && done.path.extension().is_some_and(|e| e == "mp4"), "{:?}", done.path);
        let bytes = std::fs::read(&done.path).unwrap();
        assert!(matches!(file_receive::sniff(&bytes[..64], false), Sniff::Video { container: PhoneContainer::IsoBmff, ext: "mp4" }));
        assert_eq!(names(&store).len(), 1, "the take file is gone, only the video is left: {:?}", names(&store));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&done.path).unwrap().permissions().mode() & 0o777, 0o600);
        }

        let info = crate::video_processor::probe_phone_video(done.path.clone(), done.display_name.clone(), done.container, Duration::from_secs(20))
            .await
            .expect("the loader's probe accepts it");
        assert_eq!((info.width, info.height), (640, 360));
        let probed_ms = (info.duration * 1000.0) as i64;
        assert!((probed_ms - teed_ms as i64).abs() < 150, "probed {probed_ms} ms, teed {teed_ms} ms");
        assert!(probed_ms as u64 >= MIN_TAKE_MS);
        // Held until the session lets go; then an ordinary phone video for the sweeps.
        assert_eq!(store.sweep(None), 0);
        drop(done.hold);
        assert_eq!(store.sweep(None), 1);
    }

    /// The fallback for an ffmpeg that does not read H.264 from IVF: same file, constant rate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_constant_rate_fallback_gives_the_same_duration() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        let (part, take) = teed(&store, &h264_frames(75, Duration::from_millis(40), Instant::now())).await;
        let summary = take.verdict().unwrap();
        assert!((summary.average_fps() - 25.0).abs() < 0.01, "{summary:?}");
        let raw = tmp.path().join("raw.h264");
        std::fs::write(&raw, take_payloads(&std::fs::read(part.path()).unwrap()).concat()).unwrap();
        let out = tmp.path().join("out.mp4");
        let how = TakeInput::RawH264 { fps: summary.average_fps() };
        run_ffmpeg(Path::new("ffmpeg"), remux_args(EncodedCodec::H264, how, &raw, &out)).await.expect("remux");
        let info = crate::video_processor::probe_phone_video(out, "x.mp4".into(), PhoneContainer::IsoBmff, Duration::from_secs(20)).await.unwrap();
        assert!((info.duration * 1000.0 - 3000.0).abs() < 100.0, "{info:?}");
    }

    /// A take ffmpeg cannot make a video of (here: noise where the frames should be) leaves
    /// nothing behind — not the take, not a half-written result.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_take_that_cannot_be_remuxed_is_deleted() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        let mut part = create_take_file(&store).await.unwrap();
        part.write(b"this is not an IVF file at all, whatever its name says").await.unwrap();
        let summary = TakeSummary { codec: EncodedCodec::Vp8, frames: 90, duration_ms: 3000, bytes: 54 };
        let e = finalize(Path::new("ffmpeg"), &store, part, summary).await.expect_err("not a take");
        assert!(format!("{e:#}").contains("ffmpeg"), "{e:#}");
        assert!(names(&store).is_empty(), "{:?}", names(&store));
        assert_eq!(store.sweep(None), 0);
    }
}
