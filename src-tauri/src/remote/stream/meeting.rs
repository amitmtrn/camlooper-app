//! A **meeting session** ([`SessionKind::Meeting`](super::runner::SessionKind)): the desktop
//! sends what this computer plays — the meeting's other participants — and, when the user
//! picked one, a window or screen to the phone, which only receives.
//!
//! ```text
//!  MeetingSource ──audio input──▶ ffmpeg (libopus, Ogg) ──▶ OggPackets ──┐
//!        │                                                              ├─▶ RtcCommand::Media ─▶ str0m ─▶ phone
//!        └───────video input──▶ ffmpeg (libx264, Annex B) ─▶ AccessUnits ┘
//! ```
//!
//! * **str0m does not encode.** Two ffmpeg processes do: one for sound, one for the picture,
//!   so the picture failing (the picker cancelled, the window closed, no libx264) leaves the
//!   sound running. Their stdout is cut into Opus packets and H.264 access units here and
//!   written with str0m's frame-level writer.
//! * **Sound is required, the picture is not.** A session whose sound cannot be captured
//!   ends with an error; one without a picture is a sound-only session.
//! * **Where the media comes from** is a [`MeetingSource`]: on Linux the default sink's
//!   monitor and the xdg-desktop-portal window picker ([`super::capture`]); a
//!   [`FixtureSource`] (ffmpeg's test pattern and a tone) for tests and the dev CLI.
//!   Windows and macOS have none yet, and a desktop without one does not offer `meeting`.
//! * **The camera is not involved.** The video loop keeps playing on the virtual camera.
//!
//! The user's consent is the confirm dialog (and, for the picture, the system's own picker):
//! nothing here starts unless the session was accepted.
use std::any::Any;
use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures_util::future::BoxFuture;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use super::rtc::{self, OutKind, OutMedia, RtcClose, RtcCommand, RtcEvent, RtcMode};
use super::runner::{phone_stop_reason, sockets_and_offer, MediaStats, Outcome, SessionCommand, SessionDeps, SessionEvent};
use super::signal::StopReason;
use crate::pipeline::EncodedCodec;

/// The picture sent to the phone: at most this size (letterboxed into it) and rate.
pub const MEETING_WIDTH: u32 = 1280;
pub const MEETING_HEIGHT: u32 = 720;
pub const MEETING_FPS: u32 = 30;
/// About what a phone's live stream is capped at in the other direction.
pub const MEETING_VIDEO_KBPS: u32 = 2500;
pub const MEETING_AUDIO_KBPS: u32 = 64;
/// The default output device is looked at this often; a changed one restarts the sound.
const AUDIO_DEVICE_POLL: Duration = Duration::from_secs(3);
/// A picture that has been arriving and then brings nothing for this long is over (the
/// window was closed, sharing was stopped in the system's indicator): a live capture never
/// goes quiet, it repeats the last frame of a window that stands still.
pub const VIDEO_STALL: Duration = Duration::from_secs(4);
/// …and one that has not brought its first frame this long after it was picked never will.
pub const VIDEO_START_TIMEOUT: Duration = Duration::from_secs(15);
/// The sound encoder may die and be restarted this often per session.
const MAX_AUDIO_RESTARTS: u32 = 5;
/// Encoded frames waiting for the session beyond this many are dropped (it is behind).
const ENCODED_QUEUE: usize = 64;
/// An access unit or an Ogg packet larger than this is not one: the parser starts over.
const MAX_UNIT_BYTES: usize = 4 * 1024 * 1024;
/// What the phone is told on the transcript channel for now.
pub const CAPTIONS_UNAVAILABLE: &str = r#"{"type":"captions","state":"unavailable"}"#;
pub const NO_SOUND: &str = "This computer's sound could not be captured.";

/// One input for an encoder ffmpeg.
pub struct MediaInput {
    /// The ffmpeg arguments that describe the input, up to and including `-i …`.
    pub args: Vec<String>,
    /// What ffmpeg reads on stdin, when the input is `-i pipe:0`.
    pub stdin: Option<Stdio>,
    /// Kept for as long as the encoder runs: a capture process, a portal session.
    pub keep: Option<Box<dyn Any + Send>>,
    /// The device behind it (see [`MeetingSource::audio_device`]).
    pub device: Option<String>,
}

impl MediaInput {
    pub fn args(args: &[&str]) -> MediaInput {
        MediaInput { args: args.iter().map(|a| a.to_string()).collect(), stdin: None, keep: None, device: None }
    }
}

/// Where a meeting session's media comes from.
pub trait MeetingSource: Send + Sync {
    /// What this computer plays. Required: a failure ends the session.
    fn audio(&self) -> BoxFuture<'_, Result<MediaInput>>;
    /// A window or screen the user picks. `Ok(None)`: none (the picker was cancelled, or
    /// this source has no picture). May take as long as the user takes; dropped when the
    /// session ends first, which must close the picker.
    fn video(&self) -> BoxFuture<'_, Result<Option<MediaInput>>>;
    /// The output device [`Self::audio`] would capture right now, if that can change while
    /// a session runs. Asked every few seconds; an answer that differs from the running
    /// input's `device` restarts the sound on the new one.
    fn audio_device(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }
}

/// ffmpeg's own test sources: a moving test pattern and a 440 Hz tone, in real time. For
/// tests and `phone_stream_cli --meeting-fixture`; captures nothing.
pub struct FixtureSource {
    pub video: bool,
}

impl MeetingSource for FixtureSource {
    fn audio(&self) -> BoxFuture<'_, Result<MediaInput>> {
        Box::pin(async { Ok(MediaInput::args(&["-re", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])) })
    }

    fn video(&self) -> BoxFuture<'_, Result<Option<MediaInput>>> {
        let size = format!("testsrc2=size={MEETING_WIDTH}x{MEETING_HEIGHT}:rate={MEETING_FPS}");
        let video = self.video;
        Box::pin(async move { Ok(video.then(|| MediaInput::args(&["-re", "-f", "lavfi", "-i", &size]))) })
    }
}

/// The capture source of this platform, if it has one.
pub fn default_source() -> Option<Arc<dyn MeetingSource>> {
    #[cfg(target_os = "linux")]
    {
        Some(Arc::new(super::capture::linux::LinuxMeetingSource))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

// ---------------------------------------------------------------------------------------
// Encoders
// ---------------------------------------------------------------------------------------

/// The ffmpeg arguments after the input for the sound: Opus, 48 kHz stereo, 20 ms packets,
/// in Ogg with (nearly) one packet per page so nothing waits in the muxer.
pub fn audio_encoder_args() -> Vec<String> {
    let bitrate = format!("{MEETING_AUDIO_KBPS}k");
    ["-vn", "-ac", "2", "-ar", "48000", "-c:a", "libopus", "-b:a", &bitrate, "-frame_duration", "20", "-application", "lowdelay"]
        .iter()
        .chain(&["-page_duration", "1", "-flush_packets", "1", "-f", "ogg", "pipe:1"])
        .map(|a| a.to_string())
        .collect()
}

/// The ffmpeg arguments after the input for the picture: H.264 constrained baseline in
/// Annex B, letterboxed into 1280x720 at 30 fps, a keyframe every second (str0m cannot ask
/// ffmpeg for one, so a phone that lost a packet waits at most that long), no B-frames, an
/// access unit delimiter in front of every picture — that is what [`AccessUnits`] cuts on.
pub fn video_encoder_args() -> Vec<String> {
    let (w, h, fps) = (MEETING_WIDTH, MEETING_HEIGHT, MEETING_FPS);
    let filter = format!(
        "fps={fps},scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,format=yuv420p"
    );
    let rate = format!("{MEETING_VIDEO_KBPS}k");
    let gop = fps.to_string();
    ["-an", "-vf", &filter, "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-profile:v", "baseline", "-level", "3.1"]
        .iter()
        .chain(&["-b:v", &rate, "-maxrate", &rate, "-bufsize", &rate, "-g", &gop, "-keyint_min", &gop, "-sc_threshold", "0"])
        .chain(&["-x264-params", "aud=1:repeat-headers=1", "-flush_packets", "1", "-f", "h264", "pipe:1"])
        .map(|a| a.to_string())
        .collect()
}

/// A running encoder: ffmpeg, and its output cut into frames.
struct Encoder {
    frames: mpsc::Receiver<Vec<u8>>,
    device: Option<String>,
    // Dropped in this order: ffmpeg is killed (`kill_on_drop`), then what fed it.
    _child: tokio::process::Child,
    _keep: Option<Box<dyn Any + Send>>,
}

impl Encoder {
    fn spawn(ffmpeg: &Path, input: MediaInput, kind: OutKind) -> Result<Encoder> {
        let MediaInput { args, stdin, keep, device } = input;
        let mut cmd = tokio::process::Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error"]);
        if stdin.is_none() {
            cmd.arg("-nostdin");
        }
        cmd.args(&args);
        cmd.args(match kind {
            OutKind::Audio => audio_encoder_args(),
            OutKind::Video => video_encoder_args(),
        });
        cmd.stdin(stdin.unwrap_or_else(Stdio::null)).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("could not start {}", ffmpeg.display()))?;
        let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("the encoder has no output"))?;
        let (tx, frames) = mpsc::channel(ENCODED_QUEUE);
        tokio::spawn(async move {
            let mut parser = match kind {
                OutKind::Audio => Parser::Ogg(OggPackets::default()),
                OutKind::Video => Parser::H264(AccessUnits::default()),
            };
            let mut buf = vec![0u8; 64 * 1024];
            let mut out = Vec::new();
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => return, // the encoder is gone; the session sees the channel close
                    Ok(n) => parser.push(&buf[..n], &mut out),
                }
                for frame in out.drain(..) {
                    match tx.try_send(frame) {
                        Ok(()) => {}
                        // The session is behind: this frame is lost, the next keyframe heals it.
                        Err(mpsc::error::TrySendError::Full(_)) => {}
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    }
                }
            }
        });
        Ok(Encoder { frames, device, _child: child, _keep: keep })
    }
}

enum Parser {
    Ogg(OggPackets),
    H264(AccessUnits),
}

impl Parser {
    fn push(&mut self, data: &[u8], out: &mut Vec<Vec<u8>>) {
        match self {
            Parser::Ogg(p) => p.push(data, out),
            Parser::H264(p) => p.push(data, out),
        }
    }
}

/// Cuts an Ogg Opus stream into its Opus packets. The two header packets (`OpusHead`,
/// `OpusTags`) are left out. Bytes that are not a page are skipped up to the next one.
#[derive(Default)]
pub struct OggPackets {
    buf: Vec<u8>,
    /// The packet being put together; it may span pages.
    packet: Vec<u8>,
}

impl OggPackets {
    pub fn push(&mut self, data: &[u8], out: &mut Vec<Vec<u8>>) {
        self.buf.extend_from_slice(data);
        let mut at = 0;
        loop {
            let rest = &self.buf[at..];
            if rest.len() < 27 {
                break;
            }
            if &rest[..4] != b"OggS" {
                // Not at a page: whatever was being put together is lost with it.
                self.packet.clear();
                match rest[1..].windows(4).position(|w| w == b"OggS") {
                    Some(p) => {
                        at += 1 + p;
                        continue;
                    }
                    None => {
                        at = self.buf.len().saturating_sub(3);
                        break;
                    }
                }
            }
            let segments = rest[26] as usize;
            if rest.len() < 27 + segments {
                break;
            }
            let lacing = &rest[27..27 + segments];
            let body: usize = lacing.iter().map(|l| *l as usize).sum();
            if rest.len() < 27 + segments + body {
                break;
            }
            // A page that does not continue a packet starts a fresh one, whatever was left.
            if rest[5] & 0x01 == 0 {
                self.packet.clear();
            }
            let mut pos = 27 + segments;
            for l in lacing {
                let l = *l as usize;
                self.packet.extend_from_slice(&rest[pos..pos + l]);
                pos += l;
                if self.packet.len() > MAX_UNIT_BYTES {
                    self.packet.clear();
                } else if l < 255 {
                    let packet = std::mem::take(&mut self.packet);
                    if !packet.is_empty() && !packet.starts_with(b"OpusHead") && !packet.starts_with(b"OpusTags") {
                        out.push(packet);
                    }
                }
            }
            at += pos;
        }
        self.buf.drain(..at);
    }
}

/// How many 48 kHz samples an Opus packet holds (RFC 6716 §3.1), or `None` if it is not one.
pub fn opus_packet_samples(packet: &[u8]) -> Option<u64> {
    let toc = *packet.first()?;
    let config = toc >> 3;
    // Frame size in 48 kHz samples.
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][(config % 4) as usize],
        12..=15 => [480, 960][(config % 2) as usize],
        _ => [120, 240, 480, 960][(config % 4) as usize],
    };
    let frames = match toc & 0x03 {
        0 => 1,
        1 | 2 => 2,
        _ => (*packet.get(1)? & 0x3f) as u64,
    };
    // No packet is longer than 120 ms.
    (frames > 0 && frame * frames <= 5760).then_some(frame * frames)
}

/// Cuts an H.264 Annex B stream into access units at its access unit delimiters (NAL type
/// 9; the encoder is told to write one in front of every picture). Each unit starts with
/// its delimiter. Bytes before the first delimiter are dropped. A unit is complete — and
/// handed out — when the next one's delimiter arrives.
#[derive(Default)]
pub struct AccessUnits {
    buf: Vec<u8>,
    /// `buf` starts at a delimiter.
    synced: bool,
    /// Nothing before this offset can be the next delimiter's start code.
    scan: usize,
}

impl AccessUnits {
    pub fn push(&mut self, data: &[u8], out: &mut Vec<Vec<u8>>) {
        self.buf.extend_from_slice(data);
        loop {
            let found = self.buf[self.scan.min(self.buf.len())..]
                .windows(4)
                .position(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] & 0x1f == 9)
                .map(|p| p + self.scan);
            let Some(pos) = found else {
                // Keep the last three bytes in play: a start code may be cut in two.
                self.scan = self.scan.max(self.buf.len().saturating_sub(3));
                break;
            };
            // A four-byte start code belongs to the delimiter as well.
            let start = if pos > 0 && self.buf[pos - 1] == 0 { pos - 1 } else { pos };
            if self.synced && start > 0 {
                out.push(self.buf[..start].to_vec());
            }
            self.buf.drain(..start);
            self.synced = true;
            self.scan = pos - start + 4;
        }
        if self.buf.len() > MAX_UNIT_BYTES {
            *self = AccessUnits::default();
        }
    }
}

// ---------------------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------------------

/// Aborts the task when dropped, so a picker that is still open closes with the session.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn next_frame(encoder: &mut Option<Encoder>) -> Option<Vec<u8>> {
    match encoder {
        Some(e) => e.frames.recv().await,
        None => std::future::pending().await,
    }
}

async fn start_audio(ffmpeg: &Path, source: &dyn MeetingSource) -> Result<Encoder> {
    let input = source.audio().await?;
    Encoder::spawn(ffmpeg, input, OutKind::Audio)
}

/// One meeting session, from "accepted" to its outcome.
pub async fn run_meeting_session(
    deps: Arc<SessionDeps>,
    source: Arc<dyn MeetingSource>,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Outcome {
    // The user has just accepted: the window picker comes up now, while the connection is
    // still being made, and stays up for as long as they need.
    let picker = source.clone();
    let mut pick = Some(AbortOnDrop(tokio::spawn(async move { picker.video().await })));

    // --- 1. sockets and the phone's offer; 2. the answer: the desktop sends -----------------
    let (transport, sdp, candidates) = match sockets_and_offer(&deps, &mut cmd_rx, events).await {
        Ok(ready) => ready,
        Err(outcome) => return outcome,
    };
    let (answer, rtc_tx, mut rtc_rx) = match rtc::accept_offer(transport, &sdp, deps.rtc.clone(), RtcMode::Meeting) {
        Ok(x) => x,
        Err(e) => return (StopReason::Error, Some(format!("{e:#}"))),
    };
    let _ = events.send(SessionEvent::Answer(answer));
    for c in candidates {
        let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
    }

    // --- 3. the sound: required ---------------------------------------------------------------
    let mut audio = match start_audio(&deps.ffmpeg, &*source).await {
        Ok(encoder) => Some(encoder),
        Err(e) => {
            eprintln!("meeting share: no sound: {e:#}");
            let _ = rtc_tx.send(RtcCommand::Stop);
            return (StopReason::Error, Some(NO_SOUND.into()));
        }
    };
    eprintln!("meeting share: capturing this computer's sound");
    let mut audio_restarts = 0u32;
    let mut video: Option<Encoder> = None;
    // When the picture's encoder was started, and when it last produced a frame.
    let mut video_since = Instant::now();
    let mut last_video: Option<Instant> = None;

    let started = Instant::now();
    let mut connected = false;
    let mut live = false;
    // Sound: counted in samples, so it never drifts. Picture: stamped on arrival.
    let mut audio_time = 0u64;
    let mut video_sent = false;
    let mut sent = 0u64;
    let mut video_this_second = 0u32;
    let mut fps = 0f32;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut next_device_poll = tokio::time::Instant::now() + AUDIO_DEVICE_POLL;
    let mut no_commands = VecDeque::new();

    let stats = |video_sent: bool, fps: f32, sent: u64| MediaStats {
        codec: video_sent.then_some(EncodedCodec::H264),
        width: video_sent.then_some(MEETING_WIDTH),
        height: video_sent.then_some(MEETING_HEIGHT),
        fps,
        frames: sent,
        decoder_restarts: 0,
    };

    let outcome = loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(SessionCommand::Stop(reason) | SessionCommand::Finish(reason)) => break (reason, None),
                Some(SessionCommand::RemoteCandidate(c)) => {
                    let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
                }
                Some(SessionCommand::Offer(_)) => {}
                None => break (StopReason::Error, Some("session dropped".into())),
            },
            ev = rtc_rx.recv() => match ev {
                Some(RtcEvent::IceConnected) => {
                    connected = true;
                    let _ = events.send(SessionEvent::IceConnected);
                }
                // Captions are a later update; the phone is told so on its channel.
                Some(RtcEvent::ChannelOpen) => {
                    let _ = rtc_tx.send(RtcCommand::ChannelText(CAPTIONS_UNAVAILABLE.into()));
                }
                Some(RtcEvent::Closed(RtcClose::PeerClosed)) => break (phone_stop_reason(&mut no_commands, &mut cmd_rx).await, None),
                Some(RtcEvent::Closed(RtcClose::Failed { reason, detail })) => break (reason, Some(detail)),
                Some(RtcEvent::Closed(RtcClose::Stopped)) | None => break (StopReason::Error, Some("the connection ended unexpectedly".into())),
                Some(_) => {}
            },
            packet = next_frame(&mut audio) => match packet {
                Some(packet) => {
                    let Some(samples) = opus_packet_samples(&packet) else { continue };
                    if connected {
                        let _ = rtc_tx.send(RtcCommand::Media(OutMedia { kind: OutKind::Audio, time: audio_time, data: packet }));
                        sent += 1;
                        if !live {
                            live = true;
                            let _ = events.send(SessionEvent::Live(stats(video_sent, fps, sent)));
                        }
                    }
                    audio_time += samples;
                }
                // The sound encoder ended (the device went away, ffmpeg died): start it
                // again on whatever is the default now, a few times.
                None => {
                    audio = None;
                    audio_restarts += 1;
                    if audio_restarts > MAX_AUDIO_RESTARTS {
                        break (StopReason::Error, Some(NO_SOUND.into()));
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    match start_audio(&deps.ffmpeg, &*source).await {
                        Ok(encoder) => audio = Some(encoder),
                        Err(e) => {
                            eprintln!("meeting share: the sound could not be restarted: {e:#}");
                            break (StopReason::Error, Some(NO_SOUND.into()));
                        }
                    }
                }
            },
            unit = next_frame(&mut video) => match unit {
                Some(unit) => {
                    last_video = Some(Instant::now());
                    // The sender starts the picture on a keyframe (one comes every second).
                    if !connected {
                        continue;
                    }
                    let time = (started.elapsed().as_micros() * 9 / 100) as u64;
                    let _ = rtc_tx.send(RtcCommand::Media(OutMedia { kind: OutKind::Video, time, data: unit }));
                    sent += 1;
                    video_this_second += 1;
                    video_sent = true;
                }
                // The window was closed, sharing was stopped from the system's own
                // indicator, or the encoder died: the sound goes on.
                None => {
                    eprintln!("meeting share: the picture ended; sound only from here");
                    video = None;
                    video_sent = false;
                }
            },
            picked = async { (&mut pick.as_mut().expect("guarded").0).await }, if pick.is_some() => {
                pick = None;
                match picked {
                    Ok(Ok(Some(input))) => match Encoder::spawn(&deps.ffmpeg, input, OutKind::Video) {
                        Ok(encoder) => {
                            eprintln!("meeting share: sending the picked window");
                            video = Some(encoder);
                            video_since = Instant::now();
                            last_video = None;
                        }
                        Err(e) => eprintln!("meeting share: no picture ({e:#}); sound only"),
                    },
                    Ok(Ok(None)) => eprintln!("meeting share: no window was picked; sound only"),
                    Ok(Err(e)) => eprintln!("meeting share: no picture ({e:#}); sound only"),
                    Err(_) => eprintln!("meeting share: the window picker failed unexpectedly; sound only"),
                }
            },
            _ = tick.tick() => {
                fps = video_this_second as f32;
                video_this_second = 0;
                // The capture can end without its process ending. Dropping the encoder ends
                // the capture behind it and the portal session with it.
                let quiet = match last_video {
                    Some(at) => at.elapsed() >= VIDEO_STALL,
                    None => video_since.elapsed() >= VIDEO_START_TIMEOUT,
                };
                if video.is_some() && quiet {
                    eprintln!("meeting share: the picture stopped arriving; sound only from here");
                    video = None;
                    video_sent = false;
                }
                if live {
                    let _ = events.send(SessionEvent::Stats(stats(video_sent, fps, sent)));
                }
                // Follow the default output device (headphones plugged in, a call moved to
                // another sink): the monitor of the old one would go silent.
                if tokio::time::Instant::now() >= next_device_poll {
                    next_device_poll = tokio::time::Instant::now() + AUDIO_DEVICE_POLL;
                    let current = audio.as_ref().and_then(|a| a.device.clone());
                    if let (Some(current), Some(now)) = (current, source.audio_device().await) {
                        if current != now {
                            eprintln!("meeting share: the default output device changed; following it");
                            match start_audio(&deps.ffmpeg, &*source).await {
                                Ok(encoder) => audio = Some(encoder),
                                Err(e) => eprintln!("meeting share: could not follow the new device: {e:#}"),
                            }
                        }
                    }
                }
            }
        }
    };
    let _ = rtc_tx.send(RtcCommand::Stop);
    // The encoders, the capture behind them and a picker that is still open end here.
    drop((audio, video, pick));
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Ogg page holding `packets` (each under 255 bytes unless `lacing` says otherwise).
    fn page(continued: bool, lacing: &[u8], body: &[u8]) -> Vec<u8> {
        let mut p = b"OggS".to_vec();
        p.push(0);
        p.push(continued as u8);
        p.extend_from_slice(&[0u8; 20]); // granule, serial, sequence, checksum: not looked at
        p.push(lacing.len() as u8);
        p.extend_from_slice(lacing);
        p.extend_from_slice(body);
        p
    }

    fn feed<P>(parser: &mut P, stream: &[u8], chunk: usize, push: impl Fn(&mut P, &[u8], &mut Vec<Vec<u8>>)) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for piece in stream.chunks(chunk) {
            push(parser, piece, &mut out);
        }
        out
    }

    #[test]
    fn ogg_pages_become_opus_packets_whatever_the_read_sizes() {
        let mut stream = page(false, &[19], b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00");
        stream.extend(page(false, &[12], b"OpusTags\x00\x00\x00\x00"));
        stream.extend(page(false, &[3], &[0xfc, 1, 2]));
        // Two packets on one page, the second one empty-terminated at exactly 255 bytes.
        let big = vec![7u8; 255];
        let mut body = vec![0xfc, 9];
        body.extend_from_slice(&big);
        stream.extend(page(false, &[2, 255, 0], &body));
        // A packet that spans two pages.
        stream.extend(page(false, &[255], &vec![5u8; 255]));
        stream.extend(page(true, &[10], &[6u8; 10]));
        let mut expect = vec![vec![0xfc, 1, 2], vec![0xfc, 9], big.clone(), [vec![5u8; 255], vec![6u8; 10]].concat()];
        for chunk in [1, 7, 64, 4096] {
            let got = feed(&mut OggPackets::default(), &stream, chunk, |p, d, o| p.push(d, o));
            assert_eq!(got, expect, "chunk size {chunk}");
        }
        // Junk between pages is skipped; the page after it is read.
        let mut noisy = b"\x00garbage".to_vec();
        noisy.extend(page(false, &[3], &[0xfc, 1, 2]));
        noisy.extend_from_slice(b"OggXnot a page");
        noisy.extend(page(false, &[2], &[0xfc, 9]));
        expect.truncate(2);
        assert_eq!(feed(&mut OggPackets::default(), &noisy, 5, |p, d, o| p.push(d, o)), expect);
    }

    #[test]
    fn opus_packet_durations() {
        // CELT fullband 20 ms, one frame (what libopus writes at -frame_duration 20).
        assert_eq!(opus_packet_samples(&[0xfc, 0]), Some(960));
        // SILK wideband 20 ms (config 9), two frames.
        assert_eq!(opus_packet_samples(&[(9 << 3) | 1, 0]), Some(1920));
        // CELT 2.5 ms, code 3 with 4 frames.
        assert_eq!(opus_packet_samples(&[(16 << 3) | 3, 4]), Some(480));
        // Hybrid 10 ms.
        assert_eq!(opus_packet_samples(&[12 << 3]), Some(480));
        assert_eq!(opus_packet_samples(&[]), None);
        assert_eq!(opus_packet_samples(&[(16 << 3) | 3]), None, "code 3 needs its count byte");
        assert_eq!(opus_packet_samples(&[(16 << 3) | 3, 0]), None);
        assert_eq!(opus_packet_samples(&[(3 << 3) | 3, 3]), None, "over 120 ms is not a packet");
    }

    #[test]
    fn annex_b_is_cut_at_the_access_unit_delimiters() {
        let aud4 = [0, 0, 0, 1, 0x09, 0xf0];
        let aud3 = [0, 0, 1, 0x09, 0xf0];
        let idr = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 4, 5, 6];
        let p = [0, 0, 1, 0x41, 7, 8, 9];
        let mut stream = vec![0xaa, 0xbb, 0, 0, 1, 0x41, 1]; // before the first delimiter: dropped
        let au1 = [&aud4[..], &idr[..]].concat();
        let au2 = [&aud3[..], &p[..]].concat();
        let au3 = [&aud4[..], &p[..]].concat();
        for au in [&au1, &au2, &au3] {
            stream.extend_from_slice(au);
        }
        stream.extend_from_slice(&aud4); // the delimiter that completes the third
        for chunk in [1, 2, 3, 5, 11, 4096] {
            let got = feed(&mut AccessUnits::default(), &stream, chunk, |p, d, o| p.push(d, o));
            assert_eq!(got, vec![au1.clone(), au2.clone(), au3.clone()], "chunk size {chunk}");
        }
        assert!(crate::remote::stream::media::h264_is_keyframe(&au1) && !crate::remote::stream::media::h264_is_keyframe(&au2));
        // Without a following delimiter the last unit is still open.
        let open = feed(&mut AccessUnits::default(), &stream[..stream.len() - aud4.len()], 4096, |p, d, o| p.push(d, o));
        assert_eq!(open, vec![au1, au2]);
    }

    #[test]
    fn the_encoders_are_asked_for_what_the_answer_promises() {
        let v = video_encoder_args().join(" ");
        assert!(v.contains("-c:v libx264") && v.contains("-profile:v baseline") && v.contains("-level 3.1"), "{v}");
        assert!(v.contains("-g 30") && v.contains("aud=1") && v.contains("-f h264 pipe:1"), "{v}");
        assert!(v.contains("fps=30") && v.contains("scale=1280:720") && v.contains("-b:v 2500k"), "{v}");
        let a = audio_encoder_args().join(" ");
        assert!(a.contains("-c:a libopus") && a.contains("-ar 48000") && a.contains("-f ogg pipe:1"), "{a}");
        // The fixture captures nothing: both inputs are ffmpeg's own generators.
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let fixture = FixtureSource { video: true };
        let audio = rt.block_on(fixture.audio()).unwrap();
        let video = rt.block_on(fixture.video()).unwrap().unwrap();
        assert!(audio.args.contains(&"lavfi".to_string()) && video.args.contains(&"lavfi".to_string()));
        assert!(rt.block_on(FixtureSource { video: false }.video()).unwrap().is_none());
    }
}
