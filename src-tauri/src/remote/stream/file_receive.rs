//! Receiving one video file from the phone over the `camlooper-file` data channel.
//!
//! Three independent pieces, none of which knows about WebRTC or Tauri:
//!
//! * [`Transfer`] — the data channel protocol as a pure state machine (`meta`, binary
//!   chunks, `end`), with the size limits, the progress throttle and the idle timeout.
//!   Time is passed in.
//! * [`PhoneVideoStore`] / [`PartFile`] — where the bytes go: `<app data>/phone-videos/`,
//!   a random name the phone has no say in, written as `.part` and renamed once complete.
//! * [`LoopPlayer`] — the desktop's video loop, as far as the phone can act on it. The app's
//!   implementation is [`super::loop_engine::EngineLoop`]; tests use a fake so they need no ffmpeg.
//!
//! [`super::runner`] wires them to the data channel.
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::runner::Cancel;
use super::signal::{clean_file_name, clean_mime, FileOffer, LoopAction};
pub use crate::video_processor::PhoneContainer;

/// The label of the data channel the phone opens for the file.
pub const DATA_CHANNEL_LABEL: &str = "camlooper-file";
/// The largest binary message the phone may send.
pub const MAX_CHUNK_BYTES: usize = 65536;
/// The largest text (JSON) message the phone may send.
pub const MAX_TEXT_BYTES: usize = 4096;
/// A transfer with nothing arriving for this long has failed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// The least a transfer must deliver in every window of [`IDLE_TIMEOUT`] while bytes are
/// still owed: a phone that trickles one byte every 19 s is not idle, and would otherwise
/// hold the one session slot (and the partial file) for as long as it likes.
pub const MIN_WINDOW_BYTES: u64 = 64 * 1024;
/// The whole transfer must also keep up this average (bytes per second), on top of
/// [`DEADLINE_SLACK`]: 300 MiB may take 2 h 42 min, not days of windows that just pass.
pub const MIN_AVERAGE_RATE: u64 = 32 * 1024;
pub const DEADLINE_SLACK: Duration = Duration::from_secs(120);
/// How many of a file's first bytes are looked at to tell what it is ([`sniff`]).
pub const SNIFF_BYTES: usize = 64;
/// Free disk space that must be left over once the announced video is stored.
pub const FREE_SPACE_MARGIN: u64 = 64 * 1024 * 1024;
/// Loading and starting a received video may take this long in all (reading its header,
/// turning the camera on — which can be a system prompt — and starting playback).
pub const PLAY_TIMEOUT: Duration = Duration::from_secs(180);
/// What the phone and the UI are told about a file that is not what was announced.
pub const NOT_A_VIDEO: &str = "This file is not a video the computer can loop.";
pub const NO_SPACE: &str = "There is not enough free disk space on the computer for this video.";
/// `progress` goes to the phone (and the UI) at most this often, and once at the end.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// The directory under the app data dir.
pub const PHONE_VIDEO_DIR: &str = "phone-videos";
const FILE_PREFIX: &str = "phone-";

// ---------------------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------------------

/// Why a transfer failed. [`Self::message`] is the short text the phone and the UI show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferError {
    /// Text that is not a JSON object with a `t`, or is too long.
    BadMessage,
    /// `meta` differs from the file the user was asked about.
    MetaMismatch,
    /// A second `meta`.
    UnexpectedMeta,
    /// Data or `end` before `meta`.
    BeforeMeta,
    ChunkTooLarge,
    /// More bytes than announced.
    TooMuchData,
    /// `end` before all announced bytes arrived.
    Incomplete,
    /// Anything after `end`.
    AfterEnd,
    /// A binary message with nothing in it: no progress, so it must not count as activity.
    EmptyChunk,
    Idle,
    /// Bytes keep arriving, but under [`MIN_WINDOW_BYTES`] per window or past the deadline.
    TooSlow,
}

impl TransferError {
    pub fn message(self) -> &'static str {
        match self {
            TransferError::BadMessage => "The phone sent something unexpected.",
            TransferError::MetaMismatch => "The video is not the one that was accepted.",
            TransferError::UnexpectedMeta | TransferError::BeforeMeta | TransferError::AfterEnd => {
                "The phone sent the video out of order."
            }
            TransferError::ChunkTooLarge => "The phone sent the video in pieces that are too large.",
            TransferError::EmptyChunk => "The phone sent an empty piece of the video.",
            TransferError::TooSlow => "The transfer was too slow.",
            TransferError::TooMuchData => "The video is larger than announced.",
            TransferError::Incomplete => "The video arrived incomplete.",
            TransferError::Idle => "The transfer stalled.",
        }
    }
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// What a text message from the phone did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStep {
    /// `meta` matched: the file may be created and data is expected.
    Started,
    /// `end`, with every announced byte received: commit the file.
    Complete,
    /// A message kind this desktop does not know (a newer phone page): nothing to do.
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferState {
    AwaitMeta,
    Receiving,
    Complete,
}

#[derive(Deserialize)]
struct PhoneMessage {
    t: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    mime: Option<String>,
}

/// The receiving end of the data channel protocol. Feed it every message of the channel in
/// order; it says what to do with each and never touches the disk itself.
#[derive(Debug)]
pub struct Transfer {
    expected: FileOffer,
    state: TransferState,
    received: u64,
    last_activity: Instant,
    last_progress: Option<Instant>,
    idle: Duration,
    /// When `meta` arrived: the overall deadline counts from here.
    started: Option<Instant>,
    /// The current rate window (as long as `idle`) and what arrived in it.
    window_start: Instant,
    window_bytes: u64,
}

impl Transfer {
    /// `expected` is the file from the accepted `stream_request`. The idle clock starts at `now`.
    pub fn new(expected: FileOffer, now: Instant, idle: Duration) -> Self {
        Self {
            expected,
            state: TransferState::AwaitMeta,
            received: 0,
            last_activity: now,
            last_progress: None,
            idle,
            started: None,
            window_start: now,
            window_bytes: 0,
        }
    }

    /// How long a transfer of `size` bytes may take from `meta` to the last byte.
    pub fn deadline_for(size: u64) -> Duration {
        DEADLINE_SLACK + Duration::from_secs(size / MIN_AVERAGE_RATE)
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    pub fn size(&self) -> u64 {
        self.expected.size
    }

    pub fn is_complete(&self) -> bool {
        self.state == TransferState::Complete
    }

    /// A text message from the phone.
    pub fn on_text(&mut self, text: &str, now: Instant) -> Result<TextStep, TransferError> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(TransferError::BadMessage);
        }
        let msg: PhoneMessage = serde_json::from_str(text).map_err(|_| TransferError::BadMessage)?;
        if self.state == TransferState::Complete {
            return Err(TransferError::AfterEnd);
        }
        match msg.t.as_str() {
            "meta" => {
                if self.state != TransferState::AwaitMeta {
                    return Err(TransferError::UnexpectedMeta);
                }
                // Compared after the same cleaning the request's file went through.
                let name = msg.name.as_deref().and_then(clean_file_name);
                let mime = clean_mime(msg.mime.as_deref().unwrap_or_default());
                if name.as_deref() != Some(self.expected.name.as_str())
                    || msg.size != Some(self.expected.size)
                    || mime != self.expected.mime
                {
                    return Err(TransferError::MetaMismatch);
                }
                self.state = TransferState::Receiving;
                self.last_activity = now;
                self.started = Some(now);
                self.window_start = now;
                self.window_bytes = 0;
                Ok(TextStep::Started)
            }
            "end" => {
                if self.state != TransferState::Receiving {
                    return Err(TransferError::BeforeMeta);
                }
                if self.received != self.expected.size {
                    return Err(TransferError::Incomplete);
                }
                self.state = TransferState::Complete;
                self.last_activity = now;
                Ok(TextStep::Complete)
            }
            _ => Ok(TextStep::Ignored),
        }
    }

    /// A binary message of `len` bytes. On `Ok` the caller appends the bytes to the file;
    /// `Ok(Some(n))` also asks for a `progress` report of `n` bytes received.
    pub fn on_binary(&mut self, len: usize, now: Instant) -> Result<Option<u64>, TransferError> {
        match self.state {
            TransferState::AwaitMeta => return Err(TransferError::BeforeMeta),
            TransferState::Complete => return Err(TransferError::AfterEnd),
            TransferState::Receiving => {}
        }
        if len == 0 {
            return Err(TransferError::EmptyChunk);
        }
        if len > MAX_CHUNK_BYTES {
            return Err(TransferError::ChunkTooLarge);
        }
        let received = self.received + len as u64;
        if received > self.expected.size {
            return Err(TransferError::TooMuchData);
        }
        self.received = received;
        self.last_activity = now;
        self.window_bytes += len as u64;
        let due = self.last_progress.is_none_or(|t| now.duration_since(t) >= PROGRESS_INTERVAL);
        if due {
            self.last_progress = Some(now);
        }
        Ok(due.then_some(received))
    }

    /// Fails once nothing has arrived for the idle timeout, once a whole window brought less
    /// than [`MIN_WINDOW_BYTES`] of the bytes still owed, or once the transfer has taken
    /// longer than [`Self::deadline_for`] its size. Not after `end`: what follows is the
    /// desktop's own work. Call it regularly (it also rolls the rate window).
    pub fn check_idle(&mut self, now: Instant) -> Result<(), TransferError> {
        if self.state == TransferState::Complete {
            return Ok(());
        }
        if now.saturating_duration_since(self.last_activity) >= self.idle {
            return Err(TransferError::Idle);
        }
        if self.state != TransferState::Receiving {
            return Ok(());
        }
        if self.started.is_some_and(|t| now.saturating_duration_since(t) >= Self::deadline_for(self.expected.size)) {
            return Err(TransferError::TooSlow);
        }
        if now.saturating_duration_since(self.window_start) >= self.idle {
            // With every byte in, only `end` is owed: the idle timeout covers that.
            if self.received < self.expected.size && self.window_bytes < MIN_WINDOW_BYTES {
                return Err(TransferError::TooSlow);
            }
            self.window_start = now;
            self.window_bytes = 0;
        }
        Ok(())
    }
}

/// The desktop → phone messages of the data channel, as JSON text.
pub mod reply {
    use serde_json::json;

    pub fn progress(received: u64) -> String {
        json!({"t":"progress","received":received}).to_string()
    }
    pub fn saved() -> String {
        json!({"t":"saved"}).to_string()
    }
    pub fn processing() -> String {
        json!({"t":"processing"}).to_string()
    }
    pub fn looping(name: &str) -> String {
        json!({"t":"looping","name":name}).to_string()
    }
    pub fn error(message: &str) -> String {
        json!({"t":"error","message":message}).to_string()
    }
}

// ---------------------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------------------

/// What a file's first bytes say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniff {
    /// Too few bytes yet to tell.
    NeedMore,
    /// One of the two containers a phone video may be, and the extension its content
    /// suggests (`mov` for a QuickTime brand, `webm` for a WebM DocType, …).
    Video { container: PhoneContainer, ext: &'static str },
    NotAVideo,
}

/// Recognise a video by content, whatever it is called: an ISO base media file (mp4, m4v,
/// mov, 3gp) starts with an `ftyp` box, Matroska and WebM with the EBML magic
/// `1A 45 DF A3`. Everything else — a playlist, a script, an image, random bytes — is
/// [`Sniff::NotAVideo`] and never reaches the loader. `complete` says that `head` is the
/// whole file (so "too short" is final).
pub fn sniff(head: &[u8], complete: bool) -> Sniff {
    let short = if complete { Sniff::NotAVideo } else { Sniff::NeedMore };
    if head.len() < 4 {
        return short;
    }
    if head[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        // The DocType element (ID 42 82, a one-byte length, then "webm" or "matroska")
        // sits in the EBML header, within the first few dozen bytes.
        if head.len() < SNIFF_BYTES && !complete {
            return Sniff::NeedMore;
        }
        let webm = head.windows(7).any(|w| w[..2] == [0x42, 0x82] && w[2] == 0x84 && &w[3..7] == b"webm");
        return Sniff::Video { container: PhoneContainer::Matroska, ext: if webm { "webm" } else { "mkv" } };
    }
    if head.len() < 12 {
        return short;
    }
    let box_size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    // `ftyp` holds a major brand, a version and a short list of compatible brands.
    if &head[4..8] != b"ftyp" || !(16..=1024).contains(&box_size) || box_size % 4 != 0 {
        return Sniff::NotAVideo;
    }
    let brand = &head[8..12];
    let ext = match brand {
        b"qt  " => "mov",
        b"M4V " | b"M4VH" | b"M4VP" => "m4v",
        _ if brand.starts_with(b"3g") => "3gp",
        _ => "mp4",
    };
    Sniff::Video { container: PhoneContainer::IsoBmff, ext }
}

fn container_of(ext: &str) -> PhoneContainer {
    match ext {
        "webm" | "mkv" => PhoneContainer::Matroska,
        _ => PhoneContainer::IsoBmff,
    }
}

/// The extension the phone's MIME type stands for, if it is one this desktop knows.
fn mime_extension(offer: &FileOffer) -> Option<&'static str> {
    Some(match offer.mime_essence().as_str() {
        "video/mp4" => "mp4",
        "video/x-m4v" => "m4v",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "video/x-matroska" | "video/matroska" => "mkv",
        "video/3gpp" | "video/3gpp2" => "3gp",
        _ => return None,
    })
}

/// The extension the phone *announced*: from the MIME type, else from the name, else `mp4`.
/// Always one of [`super::signal::PHONE_VIDEO_EXTENSIONS`], whatever the phone sent. What
/// the stored file is really called is decided by its content: [`stored_extension`].
pub fn extension_for(offer: &FileOffer) -> &'static str {
    mime_extension(offer).or_else(|| offer.name_extension()).unwrap_or("mp4")
}

/// The extension the stored file gets, or `None` when the content is not the kind of video
/// that was announced (a WebM sent as `video/mp4`): that is refused as unsupported.
///
/// The container always comes from the content (`sniffed`, `content_ext` from [`sniff`]).
/// Within it the phone's own extension is kept when it fits — the name's first, then the
/// MIME type's — so that "clip.m4v" stays `.m4v`; otherwise the content's.
pub fn stored_extension(offer: &FileOffer, sniffed: PhoneContainer, content_ext: &'static str) -> Option<&'static str> {
    let announced = mime_extension(offer).or_else(|| offer.name_extension());
    if announced.is_some_and(|ext| container_of(ext) != sniffed) {
        return None;
    }
    let fits = |ext: &&'static str| container_of(ext) == sniffed;
    Some(offer.name_extension().filter(fits).or_else(|| mime_extension(offer).filter(fits)).unwrap_or(content_ext))
}

/// The name the video is loaded under: the phone's display name, always ending in the
/// stored file's extension `ext` (added when the name ends in anything else), so the name
/// never claims another kind of file than the one that was checked and stored.
pub fn load_name(offer: &FileOffer, ext: &str) -> String {
    let fits = offer.name.rsplit_once('.').is_some_and(|(stem, e)| !stem.is_empty() && e.eq_ignore_ascii_case(ext));
    if fits {
        offer.name.clone()
    } else {
        format!("{}.{ext}", offer.name)
    }
}

/// Whether a video of `size` bytes fits on a disk with `free` bytes left, keeping
/// [`FREE_SPACE_MARGIN`]. A disk that cannot be asked (`None`) is not held against it.
pub fn has_room(free: Option<u64>, size: u64) -> bool {
    free.is_none_or(|free| free >= size.saturating_add(FREE_SPACE_MARGIN))
}

/// `<app data dir>/phone-videos/`: the only place phone videos are written.
#[derive(Debug, Clone)]
pub struct PhoneVideoStore {
    dir: PathBuf,
    /// Names of files a session is working on right now (the `.part` being written, the
    /// committed file being loaded). A sweep leaves them alone — it can come from elsewhere
    /// at any moment (the user loading a desktop video).
    in_flight: Arc<Mutex<HashSet<OsString>>>,
}

impl PartialEq for PhoneVideoStore {
    fn eq(&self, other: &Self) -> bool {
        self.dir == other.dir
    }
}
impl Eq for PhoneVideoStore {}

/// Keeps one file of the store out of the sweeps while it is alive.
#[derive(Debug)]
pub struct Hold {
    set: Arc<Mutex<HashSet<OsString>>>,
    name: OsString,
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.name);
    }
}

impl PhoneVideoStore {
    pub fn in_app_data(app_data_dir: &Path) -> Self {
        Self { dir: app_data_dir.join(PHONE_VIDEO_DIR), in_flight: Arc::default() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn hold(&self, path: &Path) -> Hold {
        let name = path.file_name().unwrap_or_default().to_os_string();
        self.in_flight.lock().unwrap_or_else(|e| e.into_inner()).insert(name.clone());
        Hold { set: self.in_flight.clone(), name }
    }

    /// Whether `path` is a file of this store (so: a video that came from the phone).
    ///
    /// The loop engine may hold the path in another spelling than the one it was given
    /// (canonicalised: `\\?\` on Windows, `/private/var` on macOS, a data directory behind a
    /// symlink), so the directories are compared resolved when they differ as written.
    pub fn contains(&self, path: &Path) -> bool {
        if !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(FILE_PREFIX)) {
            return false;
        }
        let Some(parent) = path.parent() else { return false };
        if parent == self.dir {
            return true;
        }
        match (std::fs::canonicalize(parent), std::fs::canonicalize(&self.dir)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// The directory must be a real directory (not a symlink to somewhere else) that only
    /// this user can enter: on Unix, owned by the user and mode 0700 (an existing one of
    /// ours with a wider mode is narrowed). Anything else — someone else's directory put
    /// there first, a symlink to a place where `phone-*` files would be read, swapped or
    /// deleted — and nothing is written or swept.
    fn check_dir(&self) -> std::io::Result<()> {
        let meta = std::fs::symlink_metadata(&self.dir)?;
        if !meta.file_type().is_dir() {
            return Err(std::io::Error::other("the phone video folder is not a real directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // SAFETY: getuid has no preconditions and cannot fail.
            let me = unsafe { libc::getuid() };
            if meta.uid() != me {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "the phone video folder belongs to another user",
                ));
            }
            if meta.mode() & 0o777 != 0o700 {
                std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        if let Some(parent) = self.dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&self.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        self.check_dir()
    }

    /// Free bytes on the disk that holds the store (`None` when it cannot be asked).
    pub fn free_space(&self) -> Option<u64> {
        free_space_at(&self.dir)
    }

    /// Start a new file for `offer`: `phone-<16 random hex>.part`, created exclusively.
    /// Nothing of the phone's name reaches the path; the extension is added by
    /// [`PartFile::commit`], from the content. Refused with `StorageFull` when the disk
    /// has no room for the announced size.
    pub async fn create(&self, offer: &FileOffer) -> std::io::Result<PartFile> {
        self.ensure_dir()?;
        if !has_room(self.free_space(), offer.size) {
            return Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "not enough free disk space"));
        }
        let id: u64 = rand::random();
        let stem = self.dir.join(format!("{FILE_PREFIX}{id:016x}"));
        let part = stem.with_extension("part");
        let hold = self.hold(&part);
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&part).await?;
        Ok(PartFile { file: Some(file), part, stem, head: Vec::with_capacity(SNIFF_BYTES), store: self.clone(), _hold: hold })
    }

    /// Delete every phone video (and leftover `.part`) except `keep`. Returns how many files
    /// were removed. A file that cannot be removed now (still open, on Windows) is left for
    /// the next sweep.
    pub fn sweep(&self, keep: Option<&Path>) -> usize {
        match keep {
            Some(keep) => self.sweep_keeping(&[keep]),
            None => self.sweep_keeping(&[]),
        }
    }

    /// [`Self::sweep`] with any number of files to keep. A kept path is matched by its file
    /// name once it is known to lie in this store ([`Self::contains`]), not byte for byte:
    /// the loop engine's spelling of the path may differ from this store's.
    pub fn sweep_keeping(&self, keep: &[&Path]) -> usize {
        if self.check_dir().is_err() {
            return 0;
        }
        let kept: Vec<&std::ffi::OsStr> = keep.iter().filter(|p| self.contains(p)).filter_map(|p| p.file_name()).collect();
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return 0 };
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let is_file = entry.file_type().is_ok_and(|t| t.is_file());
            let ours = name.to_str().is_some_and(|n| n.starts_with(FILE_PREFIX));
            if !is_file || !ours || kept.contains(&name.as_os_str()) {
                continue;
            }
            if self.in_flight.lock().unwrap_or_else(|e| e.into_inner()).contains(&name) {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) => log::debug!("phone video: could not remove {}: {e}", path.display()),
            }
        }
        removed
    }
}

#[cfg(unix)]
fn free_space_at(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is a properly sized,
    // writable statvfs that the call fills in.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        stat
    };
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(windows)]
fn free_space_at(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(directory: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free: u64 = 0;
    // SAFETY: `wide` is NUL-terminated, `free` is a valid u64 to write, and the two other
    // out-pointers may be null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(not(any(unix, windows)))]
fn free_space_at(_dir: &Path) -> Option<u64> {
    None
}

/// A phone video being written. Dropping it without [`Self::commit`] deletes the partial file.
#[derive(Debug)]
pub struct PartFile {
    file: Option<tokio::fs::File>,
    part: PathBuf,
    /// The final path without its extension.
    stem: PathBuf,
    /// The file's first [`SNIFF_BYTES`] bytes, as written.
    head: Vec<u8>,
    store: PhoneVideoStore,
    _hold: Hold,
}

impl PartFile {
    /// Append `data`. Returns once the bytes have reached the file, not merely tokio's
    /// buffer: `tokio::fs::File::write_all` hands the write to a blocking task and returns
    /// at once, so without the flush the progress reported for this chunk could run ahead
    /// of the file, and a failed write (disk full) would only show at the next chunk.
    /// (The next write would wait for this one anyway, so the flush costs nothing extra.)
    pub async fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        if self.head.len() < SNIFF_BYTES {
            let take = (SNIFF_BYTES - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..take]);
        }
        match self.file.as_mut() {
            Some(f) => {
                f.write_all(data).await?;
                f.flush().await
            }
            None => Err(std::io::Error::other("file already closed")),
        }
    }

    /// What the bytes written so far say the file is ([`sniff`]). `complete`: no more
    /// bytes will come.
    pub fn sniff(&self, complete: bool) -> Sniff {
        sniff(&self.head, complete)
    }

    /// Flush, fsync and rename to the final name, `phone-<hex>.<ext>`. On any error the
    /// partial file is deleted. The returned [`Hold`] keeps the file out of the sweeps
    /// until the caller has handed it to the loop engine (or given up on it).
    pub async fn commit(mut self, ext: &str) -> std::io::Result<(PathBuf, Hold)> {
        let mut file = self.file.take().ok_or_else(|| std::io::Error::other("file already closed"))?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        let dest = self.stem.with_extension(ext);
        let hold = self.store.hold(&dest);
        tokio::fs::rename(&self.part, &dest).await?;
        Ok((dest, hold))
    }

    /// Give up: close the file and delete it. (Dropping does the same, but cannot wait for a
    /// write that is still in flight, which matters where an open file cannot be deleted.)
    pub async fn discard(mut self) {
        if let Some(mut file) = self.file.take() {
            let _ = file.flush().await;
        }
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        self.file = None;
        // After a commit the `.part` name no longer exists and this is a no-op.
        let _ = std::fs::remove_file(&self.part);
    }
}

// ---------------------------------------------------------------------------------------
// The desktop's loop
// ---------------------------------------------------------------------------------------

/// The desktop's video loop as the phone sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoopState {
    pub playing: bool,
    /// Display name of the loaded video.
    pub name: Option<String>,
    /// Where the loaded video is on disk.
    pub path: Option<PathBuf>,
}

/// The existing local-video loop engine, reduced to what the phone can ask of it.
pub trait LoopPlayer: Send + Sync {
    fn state(&self) -> BoxFuture<'_, LoopState>;
    /// Stop the loop. Not an error when nothing is playing.
    fn stop(&self) -> BoxFuture<'_, Result<()>>;
    /// Start looping the loaded video. An error when none is loaded. Never sets up a camera
    /// that is off (see [`Self::camera_ready`]).
    fn start(&self) -> BoxFuture<'_, Result<()>>;
    /// Load the received video and loop it on the virtual camera, as if the user had picked
    /// that file on the desktop — but only once it is known to be loadable: what was playing
    /// keeps playing until then, and comes back if the new video cannot be started.
    ///
    /// `request.cancel` is honoured up to the point where the new video starts; after that
    /// the answer is [`Played::Looping`], cancelled or not.
    fn play_file(&self, request: PlayRequest) -> BoxFuture<'_, Result<Played, PlayError>>;
    /// Whether [`Self::start`] can run without asking the user for anything: the virtual
    /// camera is already on. A `loop_control` from the phone must never raise a system
    /// prompt (polkit, UAC) on the computer.
    fn camera_ready(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }
    /// A `loop_control` from the phone: `stop` or `start` (`status` does nothing).
    ///
    /// `refused` is asked immediately before acting, and — in an implementation that
    /// serialises loop operations, as the app's does — under the same lock the camera
    /// handoff and `play_file` take: it says whether a phone session has become active (or
    /// is still shutting down) since the request was queued. Errors are the short codes
    /// that go into `loop_status.error`: [`LOOP_BUSY`], [`LOOP_CAMERA_OFF`], or the
    /// engine's own text.
    fn control<'a>(&'a self, action: LoopAction, refused: &'a (dyn Fn() -> bool + Send + Sync)) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if refused() {
                return Err(LOOP_BUSY.to_string());
            }
            match action {
                LoopAction::Status => Ok(()),
                LoopAction::Stop => self.stop().await.map_err(|e| e.to_string()),
                LoopAction::Start => {
                    if !self.camera_ready().await {
                        return Err(LOOP_CAMERA_OFF.to_string());
                    }
                    self.start().await.map_err(|e| e.to_string())
                }
            }
        })
    }
}

/// `loop_status.error` when a `loop_control` came while a phone session was active.
pub const LOOP_BUSY: &str = "busy";
/// `loop_status.error` for a `start` while the virtual camera is off: the phone may start
/// the loop on a camera that is running, but not make the computer set one up.
pub const LOOP_CAMERA_OFF: &str = "camera_off";
/// `loop_status.error` for a `stop` / `start` that came too soon after the last one.
pub const LOOP_RATE_LIMITED: &str = "rate_limited";

/// A received video for [`LoopPlayer::play_file`].
#[derive(Debug, Clone)]
pub struct PlayRequest {
    /// The committed file in the store.
    pub path: PathBuf,
    /// [`load_name`]: the phone's name for it, ending in the stored extension.
    pub display_name: String,
    /// What its first bytes said it is; the loader is held to that.
    pub container: PhoneContainer,
    pub cancel: Cancel,
}

/// How [`LoopPlayer::play_file`] ended when the video was loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Played {
    /// Looping on the virtual camera. `path` is the file as the engine holds it.
    Looping { path: PathBuf },
    /// Loaded, but playback could not be started and there was no earlier video to go
    /// back to. The UI has been told the same.
    LoadedNotPlaying { path: PathBuf },
}

/// How [`LoopPlayer::play_file`] ended when the video was *not* loaded. In both cases the
/// loop is as it was before (the earlier video loaded, and playing if it was).
#[derive(Debug)]
pub enum PlayError {
    /// `cancel` fired in time.
    Cancelled,
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for PlayError {
    fn from(e: anyhow::Error) -> Self {
        PlayError::Failed(e)
    }
}

/// No loop engine (headless tools, tests that do not care): nothing is ever loaded.
pub struct NoLoop;

impl LoopPlayer for NoLoop {
    fn state(&self) -> BoxFuture<'_, LoopState> {
        Box::pin(async { LoopState::default() })
    }
    fn stop(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn start(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(anyhow!("No video is loaded.")) })
    }
    fn play_file(&self, _request: PlayRequest) -> BoxFuture<'_, Result<Played, PlayError>> {
        Box::pin(async { Err(PlayError::Failed(anyhow!("This build cannot loop a video."))) })
    }
}

/// What a file session needs besides the connection.
#[derive(Clone)]
pub struct FileDeps {
    pub store: PhoneVideoStore,
    pub player: std::sync::Arc<dyn LoopPlayer>,
    /// [`IDLE_TIMEOUT`], shorter in tests.
    pub idle_timeout: Duration,
    /// [`PLAY_TIMEOUT`], shorter in tests.
    pub play_timeout: Duration,
}

impl FileDeps {
    pub fn new(store: PhoneVideoStore, player: std::sync::Arc<dyn LoopPlayer>) -> Self {
        Self { store, player, idle_timeout: IDLE_TIMEOUT, play_timeout: PLAY_TIMEOUT }
    }

    /// Delete every phone video except the one that is loaded right now.
    pub async fn sweep_unused(&self) -> usize {
        self.sweep_unused_keeping(None).await
    }

    /// [`Self::sweep_unused`], also keeping `also` (the path `play_file` said it loaded).
    pub async fn sweep_unused_keeping(&self, also: Option<&Path>) -> usize {
        let loaded = self.player.state().await.path;
        let keep: Vec<&Path> = loaded.as_deref().into_iter().chain(also).collect();
        self.store.sweep_keeping(&keep)
    }
}

/// A loop engine for tests: it keeps state and a call log, and needs no ffmpeg.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct FakeLoop {
        pub state: Mutex<LoopState>,
        pub calls: Mutex<Vec<String>>,
        /// `play_file` fails (an unreadable video). As in the real engine, that is found
        /// out before anything is stopped: the state stays as it was.
        pub fail_play: bool,
        /// `play_file` takes this long (a slow probe, a system prompt), unless cancelled.
        pub play_delay: Duration,
        /// The load can no longer be prevented: `cancel` is not looked at.
        pub ignore_cancel: bool,
        /// The video loads but playback does not start, and nothing was loaded before.
        pub not_startable: bool,
        /// Report the loaded path canonicalised, as a loader may.
        pub canonical_paths: bool,
        /// The virtual camera is off.
        pub camera_off: bool,
        /// What the file held when it was handed over.
        pub played_bytes: Mutex<Option<Vec<u8>>>,
        pub played_container: Mutex<Option<PhoneContainer>>,
    }

    impl FakeLoop {
        pub(crate) fn loaded(name: &str, path: &Path, playing: bool) -> std::sync::Arc<Self> {
            let l = FakeLoop::default();
            *l.state.lock().unwrap() = LoopState { playing, name: Some(name.into()), path: Some(path.into()) };
            std::sync::Arc::new(l)
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl LoopPlayer for FakeLoop {
        fn state(&self) -> BoxFuture<'_, LoopState> {
            Box::pin(async { self.state.lock().unwrap().clone() })
        }
        fn stop(&self) -> BoxFuture<'_, Result<()>> {
            Box::pin(async {
                self.calls.lock().unwrap().push("stop".into());
                self.state.lock().unwrap().playing = false;
                Ok(())
            })
        }
        fn start(&self) -> BoxFuture<'_, Result<()>> {
            Box::pin(async {
                self.calls.lock().unwrap().push("start".into());
                let mut st = self.state.lock().unwrap();
                if st.path.is_none() {
                    return Err(anyhow!("No video is loaded."));
                }
                st.playing = true;
                Ok(())
            })
        }
        fn camera_ready(&self) -> BoxFuture<'_, bool> {
            Box::pin(async { !self.camera_off })
        }
        fn play_file(&self, request: PlayRequest) -> BoxFuture<'_, Result<Played, PlayError>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("play_file {}", request.display_name));
                if self.ignore_cancel {
                    tokio::time::sleep(self.play_delay).await;
                } else {
                    tokio::select! {
                        _ = tokio::time::sleep(self.play_delay) => {}
                        _ = request.cancel.cancelled() => return Err(PlayError::Cancelled),
                    }
                    if request.cancel.is_cancelled() {
                        return Err(PlayError::Cancelled);
                    }
                }
                if self.fail_play {
                    return Err(PlayError::Failed(anyhow!("No video stream found")));
                }
                *self.played_bytes.lock().unwrap() = Some(std::fs::read(&request.path).map_err(anyhow::Error::from)?);
                *self.played_container.lock().unwrap() = Some(request.container);
                let path = if self.canonical_paths { std::fs::canonicalize(&request.path).map_err(anyhow::Error::from)? } else { request.path.clone() };
                let playing = !self.not_startable;
                *self.state.lock().unwrap() = LoopState { playing, name: Some(request.display_name.clone()), path: Some(path.clone()) };
                Ok(if playing { Played::Looping { path } } else { Played::LoadedNotPlaying { path } })
            })
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn offer(name: &str, size: u64, mime: &str) -> FileOffer {
        FileOffer { name: name.into(), size, mime: mime.into() }
    }
    fn meta(name: &str, size: u64, mime: &str) -> String {
        serde_json::json!({"t":"meta","name":name,"size":size,"mime":mime}).to_string()
    }
    const MAX: u64 = crate::remote::stream::signal::MAX_PHONE_VIDEO_BYTES;

    /// The first bytes of an MP4 (`ftyp`, major brand `brand`), padded to `len`.
    pub(crate) fn mp4_head(brand: &[u8; 4], len: usize) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 24];
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(brand);
        v.extend_from_slice(&[0, 0, 2, 0]);
        v.extend_from_slice(b"isommp41");
        v.resize(len.max(24), 0);
        v
    }

    /// The first bytes of a Matroska / WebM file with the given DocType.
    fn ebml_head(doctype: &str) -> Vec<u8> {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x86, 0x81, 0x01, 0x42, 0xF7, 0x81, 0x01, 0x42, 0xF2, 0x81, 0x04];
        v.extend_from_slice(&[0x42, 0xF3, 0x81, 0x08, 0x42, 0x82, 0x80 | doctype.len() as u8]);
        v.extend_from_slice(doctype.as_bytes());
        v.resize(SNIFF_BYTES, 0);
        v
    }

    fn started(size: u64, t: Instant) -> Transfer {
        let mut tr = Transfer::new(offer("clip.mp4", size, "video/mp4"), t, IDLE_TIMEOUT);
        assert_eq!(tr.on_text(&meta("clip.mp4", size, "video/mp4"), t), Ok(TextStep::Started));
        tr
    }

    #[test]
    fn meta_chunks_end() {
        let t = Instant::now();
        let mut tr = started(150_000, t);
        assert_eq!(tr.on_binary(65536, t), Ok(Some(65536)), "the first chunk reports progress");
        assert_eq!(tr.on_binary(65536, t + Duration::from_millis(10)), Ok(None), "throttled");
        assert_eq!(tr.on_binary(18928, t + Duration::from_millis(20)), Ok(None));
        assert_eq!((tr.received(), tr.size()), (150_000, 150_000));
        assert!(!tr.is_complete());
        assert_eq!(tr.on_text(r#"{"t":"end"}"#, t), Ok(TextStep::Complete));
        assert!(tr.is_complete());
    }

    #[test]
    fn progress_is_reported_at_most_every_250_ms() {
        let t = Instant::now();
        let mut tr = started(10_000_000, t);
        let mut reports = vec![];
        for i in 0..100u64 {
            // One 1000-byte chunk every 10 ms for a second.
            if let Some(n) = tr.on_binary(1000, t + Duration::from_millis(i * 10)).unwrap() {
                reports.push((i * 10, n));
            }
        }
        assert_eq!(reports, vec![(0, 1000), (250, 26_000), (500, 51_000), (750, 76_000)]);
        assert_eq!(PROGRESS_INTERVAL, Duration::from_millis(250));
    }

    #[test]
    fn meta_must_equal_the_accepted_file() {
        let t = Instant::now();
        let new = || Transfer::new(offer("clip.mp4", 1000, "video/mp4"), t, IDLE_TIMEOUT);
        for bad in [
            meta("other.mp4", 1000, "video/mp4"),
            meta("clip.mp4", 1001, "video/mp4"),
            meta("clip.mp4", 1000, "video/webm"),
            r#"{"t":"meta","name":"clip.mp4","mime":"video/mp4"}"#.to_string(),
            r#"{"t":"meta","size":1000,"mime":"video/mp4"}"#.to_string(),
        ] {
            assert_eq!(new().on_text(&bad, t), Err(TransferError::MetaMismatch), "{bad}");
        }
        // The request's name was cleaned for display; the same name, uncleaned, still matches.
        let mut tr = Transfer::new(offer("my clip.mp4", 1000, "video/mp4"), t, IDLE_TIMEOUT);
        assert_eq!(tr.on_text(&meta("  my\u{200b} clip.mp4\n", 1000, "video/mp4"), t), Ok(TextStep::Started));
        // Twice is an error.
        assert_eq!(tr.on_text(&meta("my clip.mp4", 1000, "video/mp4"), t), Err(TransferError::UnexpectedMeta));
    }

    #[test]
    fn sizes_are_enforced() {
        let t = Instant::now();
        // One byte over, at any point.
        let mut tr = started(100_000, t);
        assert!(tr.on_binary(65536, t).is_ok());
        assert_eq!(tr.on_binary(34_465, t), Err(TransferError::TooMuchData));
        // A chunk over 64 KiB.
        let mut tr = started(10_000_000, t);
        assert!(tr.on_binary(MAX_CHUNK_BYTES, t).is_ok());
        assert_eq!(tr.on_binary(MAX_CHUNK_BYTES + 1, t), Err(TransferError::ChunkTooLarge));
        // `end` too early.
        let mut tr = started(100_000, t);
        assert!(tr.on_binary(50_000, t).is_ok());
        assert_eq!(tr.on_text(r#"{"t":"end"}"#, t), Err(TransferError::Incomplete));
    }

    #[test]
    fn messages_out_of_order_or_unreadable() {
        let t = Instant::now();
        let new = || Transfer::new(offer("clip.mp4", 10, "video/mp4"), t, IDLE_TIMEOUT);
        assert_eq!(new().on_binary(10, t), Err(TransferError::BeforeMeta));
        assert_eq!(new().on_text(r#"{"t":"end"}"#, t), Err(TransferError::BeforeMeta));
        for junk in ["", "not json", "[1,2]", r#"{"name":"x"}"#, r#"{"t":7}"#] {
            assert_eq!(new().on_text(junk, t), Err(TransferError::BadMessage), "{junk:?}");
        }
        let long = format!(r#"{{"t":"meta","name":"{}"}}"#, "x".repeat(MAX_TEXT_BYTES));
        assert_eq!(new().on_text(&long, t), Err(TransferError::BadMessage));
        // A message kind from a newer phone page is skipped, before and during the transfer.
        let mut tr = new();
        assert_eq!(tr.on_text(r#"{"t":"hello","v":2}"#, t), Ok(TextStep::Ignored));
        assert_eq!(tr.on_text(&meta("clip.mp4", 10, "video/mp4"), t), Ok(TextStep::Started));
        assert_eq!(tr.on_text(r#"{"t":"ping"}"#, t), Ok(TextStep::Ignored));
        assert!(tr.on_binary(10, t).is_ok());
        assert_eq!(tr.on_text(r#"{"t":"end"}"#, t), Ok(TextStep::Complete));
        // Nothing may follow `end`.
        assert_eq!(tr.on_binary(1, t), Err(TransferError::AfterEnd));
        assert_eq!(tr.on_text(r#"{"t":"end"}"#, t), Err(TransferError::AfterEnd));
    }

    #[test]
    fn a_stalled_transfer_fails_after_20_seconds() {
        let t = Instant::now();
        let secs = Duration::from_secs;
        assert_eq!(IDLE_TIMEOUT, secs(20));
        // No `meta` at all.
        let mut tr = Transfer::new(offer("clip.mp4", 1000, "video/mp4"), t, IDLE_TIMEOUT);
        assert_eq!(tr.check_idle(t + secs(19)), Ok(()));
        assert_eq!(tr.check_idle(t + secs(20)), Err(TransferError::Idle));
        // Every message restarts the clock.
        let chunk = MAX_CHUNK_BYTES;
        let mut tr = started(2 * chunk as u64, t + secs(15));
        assert_eq!(tr.check_idle(t + secs(30)), Ok(()));
        tr.on_binary(chunk, t + secs(30)).unwrap();
        assert_eq!(tr.check_idle(t + secs(49)), Ok(()));
        assert_eq!(tr.check_idle(t + secs(50)), Err(TransferError::Idle));
        // After `end` the phone has nothing left to send.
        tr.on_binary(chunk, t + secs(31)).unwrap();
        tr.on_text(r#"{"t":"end"}"#, t + secs(31)).unwrap();
        assert_eq!(tr.check_idle(t + secs(500)), Ok(()));
    }

    #[test]
    fn an_empty_chunk_is_refused_and_does_not_count_as_activity() {
        let t = Instant::now();
        let mut tr = started(1000, t);
        assert_eq!(tr.on_binary(0, t + Duration::from_secs(19)), Err(TransferError::EmptyChunk));
        // It did not restart the idle clock either.
        assert_eq!(tr.check_idle(t + Duration::from_secs(20)), Err(TransferError::Idle));
    }

    #[test]
    fn a_trickle_that_is_never_idle_still_fails() {
        let t = Instant::now();
        let secs = Duration::from_secs;
        assert_eq!(MIN_WINDOW_BYTES, 64 * 1024);
        // One byte every 19 s: never idle, but the first 20 s window brought 2 bytes.
        let mut tr = started(MAX, t);
        tr.on_binary(1, t + secs(1)).unwrap();
        assert_eq!(tr.check_idle(t + secs(19)), Ok(()));
        tr.on_binary(1, t + secs(19)).unwrap();
        assert_eq!(tr.check_idle(t + secs(20)), Err(TransferError::TooSlow));

        // 64 KiB in every window is enough, window after window.
        let mut tr = started(MAX, t);
        for window in 0..50u64 {
            tr.on_binary(MAX_CHUNK_BYTES, t + secs(window * 20 + 10)).unwrap();
            assert_eq!(tr.check_idle(t + secs(window * 20 + 20)), Ok(()), "window {window}");
        }
        // One window with less, and it is over.
        tr.on_binary(MAX_CHUNK_BYTES - 1, t + secs(1010)).unwrap();
        assert_eq!(tr.check_idle(t + secs(1020)), Err(TransferError::TooSlow));

        // The last bytes of a file are not held to the window minimum: with everything in,
        // only `end` is owed, and the idle timeout covers that.
        let mut tr = started(1000, t);
        tr.on_binary(1000, t + secs(5)).unwrap();
        assert_eq!(tr.check_idle(t + secs(20)), Ok(()));
        assert_eq!(tr.check_idle(t + secs(25)), Err(TransferError::Idle));
    }

    #[test]
    fn a_transfer_has_a_deadline_that_grows_with_its_size() {
        let t = Instant::now();
        assert_eq!(Transfer::deadline_for(0), Duration::from_secs(120));
        assert_eq!(Transfer::deadline_for(MAX), Duration::from_secs(120 + 9600), "300 MiB: 2 h 42 min");
        // 1 MiB may take 120 + 32 s. Fed just enough to pass every window, it still ends.
        let size = 1024 * 1024;
        let mut tr = started(size, t);
        let mut at = t;
        let mut outcome = Ok(());
        for _ in 0..8 {
            at += Duration::from_secs(19);
            let _ = tr.on_binary(MAX_CHUNK_BYTES, at);
            outcome = tr.check_idle(at + Duration::from_secs(1));
            at += Duration::from_secs(1);
            if outcome.is_err() {
                break;
            }
        }
        assert_eq!(outcome, Err(TransferError::TooSlow));
        assert_eq!(at.duration_since(t), Duration::from_secs(160), "the first check past 152 s");
    }

    #[test]
    fn replies_have_the_wire_shape() {
        assert_eq!(reply::progress(42), r#"{"received":42,"t":"progress"}"#);
        assert_eq!(reply::saved(), r#"{"t":"saved"}"#);
        assert_eq!(reply::processing(), r#"{"t":"processing"}"#);
        assert_eq!(reply::looping("a \"b\".mp4"), r#"{"name":"a \"b\".mp4","t":"looping"}"#);
        assert_eq!(reply::error("x"), r#"{"message":"x","t":"error"}"#);
    }

    #[test]
    fn the_extension_comes_from_an_allowlist() {
        for (name, mime, ext) in [
            ("clip.mp4", "video/mp4", "mp4"),
            ("clip", "video/mp4", "mp4"),
            ("clip.MOV", "video/quicktime", "mov"),
            ("clip", "video/webm;codecs=vp9", "webm"),
            ("clip", "video/x-matroska", "mkv"),
            ("clip", "video/3gpp", "3gp"),
            ("clip", "video/x-m4v", "m4v"),
            // The type wins over the name…
            ("clip.webm", "video/mp4", "mp4"),
            // …an unknown video type falls back to the name, then to mp4.
            ("clip.mkv", "video/x-unknown", "mkv"),
            ("clip.exe", "video/x-unknown", "mp4"),
            ("clip.sh", "video/mp4", "mp4"),
            ("clip.WebM", "application/octet-stream", "webm"),
            ("x.3gp", "", "3gp"),
        ] {
            let o = offer(name, 1, mime);
            assert_eq!(extension_for(&o), ext, "{name} {mime}");
            assert!(crate::remote::stream::signal::PHONE_VIDEO_EXTENSIONS.contains(&extension_for(&o)));
        }
    }

    #[test]
    fn a_video_is_recognised_by_its_first_bytes() {
        use PhoneContainer::{IsoBmff, Matroska};
        let video = |container, ext| Sniff::Video { container, ext };
        assert_eq!(sniff(&mp4_head(b"isom", 64), false), video(IsoBmff, "mp4"));
        assert_eq!(sniff(&mp4_head(b"mp42", 64), false), video(IsoBmff, "mp4"));
        assert_eq!(sniff(&mp4_head(b"qt  ", 64), false), video(IsoBmff, "mov"));
        assert_eq!(sniff(&mp4_head(b"3gp5", 64), false), video(IsoBmff, "3gp"));
        assert_eq!(sniff(&mp4_head(b"M4V ", 64), false), video(IsoBmff, "m4v"));
        assert_eq!(sniff(&ebml_head("webm"), false), video(Matroska, "webm"));
        assert_eq!(sniff(&ebml_head("matroska"), false), video(Matroska, "mkv"));

        // Things that are not videos, whatever they are called. The first three are what
        // makes ffmpeg open other files and URLs when it is left to guess.
        for junk in [
            &b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nfile:///etc/passwd\n#EXT-X-ENDLIST\n"[..],
            b"ffconcat version 1.0\nfile '/etc/passwd'\n                                        ",
            b"v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=x\r\nc=IN IP4 10.0.0.1\r\nm=video 5004 RTP/AVP 96\r\n",
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0\x90wS\xde\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
            b"RIFF\x24\0\0\0AVI LIST                                                    ",
            &[0u8; 64][..],
            // `ftyp` in the right place, with a box size no `ftyp` has.
            b"\0\0\0\x08ftypisom\0\0\0\0                                                ",
            b"\x7f\xff\xff\xfcftypisom\0\0\0\0                                                ",
        ] {
            assert_eq!(sniff(junk, false), Sniff::NotAVideo, "{:?}", String::from_utf8_lossy(&junk[..12]));
        }

        // Too little to tell — unless that is all there is.
        assert_eq!(sniff(b"", false), Sniff::NeedMore);
        assert_eq!(sniff(&mp4_head(b"isom", 24)[..11], false), Sniff::NeedMore);
        assert_eq!(sniff(&mp4_head(b"isom", 24)[..11], true), Sniff::NotAVideo);
        assert_eq!(sniff(&ebml_head("webm")[..20], false), Sniff::NeedMore);
        assert_eq!(sniff(&ebml_head("webm")[..20], true), video(Matroska, "mkv"), "a truncated header is still Matroska");
        assert_eq!(sniff(&mp4_head(b"isom", 24)[..12], false), video(IsoBmff, "mp4"));
    }

    #[test]
    fn the_stored_extension_follows_the_content_and_a_mismatch_is_refused() {
        use PhoneContainer::{IsoBmff, Matroska};
        for (name, mime, sniffed, content_ext, stored) in [
            ("clip.mp4", "video/mp4", IsoBmff, "mp4", Some("mp4")),
            // Within the container the phone's own extension is kept…
            ("clip.m4v", "video/mp4", IsoBmff, "mp4", Some("m4v")),
            ("IMG_1.MOV", "video/quicktime", IsoBmff, "mov", Some("mov")),
            ("clip.mkv", "video/x-matroska", Matroska, "webm", Some("mkv")),
            // …the type's when the name has none (or one of the other container)…
            ("clip", "video/quicktime", IsoBmff, "mp4", Some("mov")),
            ("clip.webm", "video/mp4", IsoBmff, "mp4", Some("mp4")),
            // …and the content's when the phone named nothing usable.
            ("clip", "video/x-unknown", Matroska, "webm", Some("webm")),
            ("clip.bin", "video/x-unknown", IsoBmff, "mov", Some("mov")),
            // Announced as one container, sent as the other: refused.
            ("clip.mp4", "video/mp4", Matroska, "webm", None),
            ("clip.webm", "video/webm", IsoBmff, "mp4", None),
            ("clip.mkv", "", IsoBmff, "mp4", None),
            ("clip.webm", "video/mp4", Matroska, "webm", None),
        ] {
            let o = offer(name, 1, mime);
            assert_eq!(stored_extension(&o, sniffed, content_ext), stored, "{name} {mime} {sniffed:?}");
            if let Some(ext) = stored {
                assert!(crate::remote::stream::signal::PHONE_VIDEO_EXTENSIONS.contains(&ext));
            }
        }
    }

    #[test]
    fn the_load_name_ends_in_the_stored_extension() {
        let name = |n: &str, ext: &str| load_name(&offer(n, 1, "video/mp4"), ext);
        assert_eq!(name("clip.MOV", "mov"), "clip.MOV");
        assert_eq!(name("VID_2024", "mp4"), "VID_2024.mp4");
        assert_eq!(name("notes.txt", "webm"), "notes.txt.webm");
        // Stored as .mp4 (its content and type), named .webm by the phone: the loader must
        // not pick a parser from that name.
        assert_eq!(name("clip.webm", "mp4"), "clip.webm.mp4");
        assert_eq!(name(".mp4", "mp4"), ".mp4.mp4");
    }

    #[test]
    fn room_on_the_disk_is_checked_against_the_announced_size() {
        assert_eq!(FREE_SPACE_MARGIN, 64 * 1024 * 1024);
        assert!(has_room(None, MAX), "a disk that cannot be asked is not held against the video");
        assert!(has_room(Some(MAX + FREE_SPACE_MARGIN), MAX));
        assert!(!has_room(Some(MAX + FREE_SPACE_MARGIN - 1), MAX));
        assert!(!has_room(Some(0), 1));
        assert!(has_room(Some(u64::MAX), u64::MAX), "no overflow");
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            assert!(free_space_at(tmp.path()).is_some_and(|free| free > 0));
            assert_eq!(free_space_at(&tmp.path().join("missing")), None);
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn a_file_is_written_as_part_and_renamed_on_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        assert_eq!(store.dir(), tmp.path().join("phone-videos"));
        // A name full of path tricks: none of it reaches the disk.
        let mut part = store.create(&offer("../../../etc/passwd\\..\\x.mov", 6, "video/quicktime")).await.unwrap();
        let during = names(store.dir());
        assert_eq!(during.len(), 1);
        let n = &during[0];
        assert!(n.starts_with("phone-") && n.ends_with(".part"), "{n}");
        let hex = &n["phone-".len()..n.len() - ".part".len()];
        assert!(hex.len() == 16 && hex.chars().all(|c| c.is_ascii_hexdigit()), "{n}");
        assert_eq!(names(tmp.path()), vec!["phone-videos"], "nothing outside the directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(store.dir().join(n)).unwrap().permissions().mode() & 0o777, 0o600);
        }

        part.write(b"abc").await.unwrap();
        part.write(b"def").await.unwrap();
        // Written means on disk: what is reported as received can be read back at once.
        assert_eq!(std::fs::read(store.dir().join(n)).unwrap(), b"abcdef");
        assert_eq!(part.sniff(false), Sniff::NeedMore);
        assert_eq!(part.sniff(true), Sniff::NotAVideo);
        let (path, _hold) = part.commit("mov").await.unwrap();
        assert_eq!(names(store.dir()), vec![format!("phone-{hex}.mov")]);
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        assert!(store.contains(&path));
        assert!(!store.contains(&tmp.path().join("phone-x.mp4")));
        assert!(!store.contains(&store.dir().join("other.mp4")));
    }

    #[tokio::test]
    async fn the_part_file_remembers_its_first_bytes_across_small_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        let head = mp4_head(b"qt  ", 200);
        let mut part = store.create(&offer("a.mov", 200, "video/quicktime")).await.unwrap();
        part.write(&head[..5]).await.unwrap();
        assert_eq!(part.sniff(false), Sniff::NeedMore);
        part.write(&head[5..9]).await.unwrap();
        part.write(&head[9..]).await.unwrap();
        assert_eq!(part.sniff(false), Sniff::Video { container: PhoneContainer::IsoBmff, ext: "mov" });
    }

    #[tokio::test]
    async fn an_abandoned_file_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        let count = || std::fs::read_dir(store.dir()).unwrap().count();
        // Dropped mid-transfer.
        let mut part = store.create(&offer("a.mp4", 10, "video/mp4")).await.unwrap();
        part.write(b"partial").await.unwrap();
        assert_eq!(count(), 1);
        drop(part);
        assert_eq!(count(), 0);
        // Discarded.
        let mut part = store.create(&offer("a.mp4", 10, "video/mp4")).await.unwrap();
        part.write(b"partial").await.unwrap();
        part.discard().await;
        assert_eq!(count(), 0);
        // Two transfers never share a name.
        let (a, _) = store.create(&offer("a.mp4", 10, "video/mp4")).await.unwrap().commit("mp4").await.unwrap();
        let (b, _) = store.create(&offer("a.mp4", 10, "video/mp4")).await.unwrap().commit("mp4").await.unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn the_sweep_keeps_only_the_current_video() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        assert_eq!(store.sweep(None), 0, "no directory yet is fine");
        let (old, _) = store.create(&offer("a.mp4", 1, "video/mp4")).await.unwrap().commit("mp4").await.unwrap();
        let (current, _) = store.create(&offer("b.webm", 1, "video/webm")).await.unwrap().commit("webm").await.unwrap();
        // A `.part` left behind by a crash, a file that is not ours, and a subdirectory.
        let stale = store.dir().join("phone-0123456789abcdef.part");
        std::fs::write(&stale, b"x").unwrap();
        let foreign = store.dir().join("notes.txt");
        std::fs::write(&foreign, b"x").unwrap();
        std::fs::create_dir(store.dir().join("phone-dir")).unwrap();

        assert_eq!(store.sweep(Some(&current)), 2);
        assert!(!old.exists() && !stale.exists());
        assert!(current.exists() && foreign.exists() && store.dir().join("phone-dir").exists());
        // A kept path elsewhere (a desktop-picked video) protects nothing here — not even
        // one that happens to carry the same file name.
        let same_name_elsewhere = tmp.path().join(current.file_name().unwrap());
        assert_eq!(store.sweep(Some(&same_name_elsewhere)), 1);
        assert!(!current.exists() && foreign.exists());
    }

    #[tokio::test]
    async fn a_sweep_leaves_the_files_a_transfer_is_working_on() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        // A sweep from elsewhere (the user loads a desktop video) while a file arrives…
        let mut part = store.create(&offer("a.mp4", 10, "video/mp4")).await.unwrap();
        part.write(b"partial").await.unwrap();
        assert_eq!(store.sweep(None), 0);
        assert_eq!(names(store.dir()).len(), 1);
        // …and after it was committed, while the loop engine is loading it.
        let (path, hold) = part.commit("mp4").await.unwrap();
        assert_eq!(store.clone().sweep(None), 0, "a clone of the store sees the same holds");
        assert!(path.exists());
        // Once the session lets go, it is a file like any other.
        drop(hold);
        assert_eq!(store.sweep(None), 1);
        assert!(!path.exists());
    }

    /// The loop engine may report the loaded path in another spelling than the store's own.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_canonicalised_path_is_still_the_stores_file() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-data");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("data");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // The app knows its data directory through the symlink.
        let store = PhoneVideoStore::in_app_data(&link);
        let (current, hold) = store.create(&offer("a.mp4", 1, "video/mp4")).await.unwrap().commit("mp4").await.unwrap();
        let (older, hold2) = store.create(&offer("b.mp4", 1, "video/mp4")).await.unwrap().commit("mp4").await.unwrap();
        drop((hold, hold2));
        let canonical = std::fs::canonicalize(&current).unwrap();
        assert_ne!(canonical, current, "the two spellings differ");
        assert!(canonical.starts_with(std::fs::canonicalize(&real).unwrap()));

        assert!(store.contains(&canonical), "fromPhone must not depend on the spelling");
        assert!(store.contains(&current));
        // Same name, another real directory: not ours.
        assert!(!store.contains(&tmp.path().join(current.file_name().unwrap())));
        // The sweep after a load keeps the looping video, under either spelling.
        assert_eq!(store.sweep(Some(&canonical)), 1);
        assert!(current.exists() && !older.exists());
        assert_eq!(store.sweep(Some(&current)), 0);
        assert!(current.exists());
    }

    /// Whoever can put something at the store's path before the app does must not get the
    /// videos, nor have the sweep delete their own `phone-*` files.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_directory_must_be_a_real_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        // A symlink where the directory should be.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let bait = elsewhere.join("phone-0000000000000001.mp4");
        std::fs::write(&bait, b"someone else's").unwrap();
        let app = tmp.path().join("app");
        std::fs::create_dir(&app).unwrap();
        std::os::unix::fs::symlink(&elsewhere, app.join(PHONE_VIDEO_DIR)).unwrap();
        let store = PhoneVideoStore::in_app_data(&app);
        assert!(store.create(&offer("a.mp4", 1, "video/mp4")).await.is_err(), "nothing is written through a symlink");
        assert_eq!(store.sweep(None), 0, "and nothing is deleted through one");
        assert!(bait.exists());
        assert_eq!(names(&elsewhere), vec!["phone-0000000000000001.mp4"]);

        // A plain file there.
        let app = tmp.path().join("app2");
        std::fs::create_dir(&app).unwrap();
        std::fs::write(app.join(PHONE_VIDEO_DIR), b"x").unwrap();
        assert!(PhoneVideoStore::in_app_data(&app).create(&offer("a.mp4", 1, "video/mp4")).await.is_err());

        // Our own directory with a mode that lets others in: narrowed, then used.
        let app = tmp.path().join("app3");
        std::fs::create_dir_all(app.join(PHONE_VIDEO_DIR)).unwrap();
        std::fs::set_permissions(app.join(PHONE_VIDEO_DIR), std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = PhoneVideoStore::in_app_data(&app);
        let part = store.create(&offer("a.mp4", 1, "video/mp4")).await.unwrap();
        assert_eq!(std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777, 0o700);
        drop(part);

        // The app data directory itself does not exist yet (first run): created.
        let app = tmp.path().join("fresh").join("com.camlooper.app");
        let store = PhoneVideoStore::in_app_data(&app);
        drop(store.create(&offer("a.mp4", 1, "video/mp4")).await.unwrap());
        assert_eq!(std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[tokio::test]
    async fn the_default_loop_control_never_starts_on_a_camera_that_is_off() {
        use testing::FakeLoop;
        let path = Path::new("/videos/desk.mp4");
        let no = || false;
        let yes = || true;
        let player = FakeLoop { camera_off: true, ..FakeLoop::default() };
        *player.state.lock().unwrap() = LoopState { playing: false, name: Some("desk.mp4".into()), path: Some(path.into()) };
        assert_eq!(player.control(LoopAction::Start, &no).await, Err(LOOP_CAMERA_OFF.to_string()));
        assert!(player.calls().is_empty(), "start was not even tried");
        // Stopping needs no camera.
        assert_eq!(player.control(LoopAction::Stop, &no).await, Ok(()));
        // A session that became active since the request was queued: nothing is done.
        let player = FakeLoop::loaded("desk.mp4", path, true);
        assert_eq!(player.control(LoopAction::Stop, &yes).await, Err(LOOP_BUSY.to_string()));
        assert_eq!(player.control(LoopAction::Start, &yes).await, Err(LOOP_BUSY.to_string()));
        assert!(player.calls().is_empty());
        assert_eq!(player.control(LoopAction::Start, &no).await, Ok(()));
        assert_eq!(player.calls(), vec!["start"]);
    }
}
