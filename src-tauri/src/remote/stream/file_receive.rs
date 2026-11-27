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
//!   implementation is `commands::AppLoop`; tests use a fake so they need no ffmpeg.
//!
//! [`super::runner`] wires them to the data channel.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::signal::{clean_file_name, clean_mime, FileOffer};

/// The label of the data channel the phone opens for the file.
pub const DATA_CHANNEL_LABEL: &str = "camlooper-file";
/// The largest binary message the phone may send.
pub const MAX_CHUNK_BYTES: usize = 65536;
/// The largest text (JSON) message the phone may send.
pub const MAX_TEXT_BYTES: usize = 4096;
/// A transfer with nothing arriving for this long has failed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
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
    Idle,
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
}

impl Transfer {
    /// `expected` is the file from the accepted `stream_request`. The idle clock starts at `now`.
    pub fn new(expected: FileOffer, now: Instant, idle: Duration) -> Self {
        Self { expected, state: TransferState::AwaitMeta, received: 0, last_activity: now, last_progress: None, idle }
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
        if len > MAX_CHUNK_BYTES {
            return Err(TransferError::ChunkTooLarge);
        }
        let received = self.received + len as u64;
        if received > self.expected.size {
            return Err(TransferError::TooMuchData);
        }
        self.received = received;
        self.last_activity = now;
        let due = self.last_progress.is_none_or(|t| now.duration_since(t) >= PROGRESS_INTERVAL);
        if due {
            self.last_progress = Some(now);
        }
        Ok(due.then_some(received))
    }

    /// Fails once nothing has arrived for the idle timeout. Not after `end`: what follows is
    /// the desktop's own work.
    pub fn check_idle(&self, now: Instant) -> Result<(), TransferError> {
        if self.state != TransferState::Complete && now.saturating_duration_since(self.last_activity) >= self.idle {
            return Err(TransferError::Idle);
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

/// The extension the stored file gets: from the MIME type, else from the name, else `mp4`.
/// Always one of [`super::signal::PHONE_VIDEO_EXTENSIONS`], whatever the phone sent.
pub fn extension_for(offer: &FileOffer) -> &'static str {
    match offer.mime_essence().as_str() {
        "video/mp4" => "mp4",
        "video/x-m4v" => "m4v",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "video/x-matroska" | "video/matroska" => "mkv",
        "video/3gpp" | "video/3gpp2" => "3gp",
        _ => offer.name_extension().unwrap_or("mp4"),
    }
}

/// The name the video is loaded under: the phone's display name, with the stored file's
/// extension added when the name does not already end in a video extension (the Windows
/// loader picks its parser by the name's extension).
pub fn load_name(offer: &FileOffer) -> String {
    match offer.name_extension() {
        Some(_) => offer.name.clone(),
        None => format!("{}.{}", offer.name, extension_for(offer)),
    }
}

/// `<app data dir>/phone-videos/`: the only place phone videos are written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneVideoStore {
    dir: PathBuf,
}

impl PhoneVideoStore {
    pub fn in_app_data(app_data_dir: &Path) -> Self {
        Self { dir: app_data_dir.join(PHONE_VIDEO_DIR) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether `path` is a file of this store (so: a video that came from the phone).
    pub fn contains(&self, path: &Path) -> bool {
        path.parent() == Some(self.dir.as_path())
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(FILE_PREFIX))
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.dir)
    }

    /// Start a new file for `offer`: `phone-<16 random hex>.<ext>.part`, created exclusively.
    /// Nothing of the phone's name reaches the path.
    pub async fn create(&self, offer: &FileOffer) -> std::io::Result<PartFile> {
        self.ensure_dir()?;
        let id: u64 = rand::random();
        let dest = self.dir.join(format!("{FILE_PREFIX}{id:016x}.{}", extension_for(offer)));
        let mut part = dest.clone().into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&part).await?;
        Ok(PartFile { file: Some(file), part, dest })
    }

    /// Delete every phone video (and leftover `.part`) except `keep`. Returns how many files
    /// were removed. A file that cannot be removed now (still open, on Windows) is left for
    /// the next sweep.
    pub fn sweep(&self, keep: Option<&Path>) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return 0 };
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let is_file = entry.file_type().is_ok_and(|t| t.is_file());
            if !is_file || !self.contains(&path) || keep == Some(path.as_path()) {
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

/// A phone video being written. Dropping it without [`Self::commit`] deletes the partial file.
#[derive(Debug)]
pub struct PartFile {
    file: Option<tokio::fs::File>,
    part: PathBuf,
    dest: PathBuf,
}

impl PartFile {
    pub async fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.write_all(data).await,
            None => Err(std::io::Error::other("file already closed")),
        }
    }

    /// Flush, fsync and rename to the final name. On any error the partial file is deleted.
    pub async fn commit(mut self) -> std::io::Result<PathBuf> {
        let mut file = self.file.take().ok_or_else(|| std::io::Error::other("file already closed"))?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&self.part, &self.dest).await?;
        Ok(self.dest.clone())
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
    /// Start looping the loaded video. An error when none is loaded.
    fn start(&self) -> BoxFuture<'_, Result<()>>;
    /// Stop whatever plays, load `path` under `display_name` and loop it on the virtual
    /// camera, exactly as if the user had picked that file on the desktop.
    fn play_file<'a>(&'a self, path: &'a Path, display_name: &'a str) -> BoxFuture<'a, Result<()>>;
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
    fn play_file<'a>(&'a self, _path: &'a Path, _display_name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Err(anyhow!("This build cannot loop a video.")) })
    }
}

/// What a file session needs besides the connection.
#[derive(Clone)]
pub struct FileDeps {
    pub store: PhoneVideoStore,
    pub player: std::sync::Arc<dyn LoopPlayer>,
    /// [`IDLE_TIMEOUT`], shorter in tests.
    pub idle_timeout: Duration,
}

impl FileDeps {
    pub fn new(store: PhoneVideoStore, player: std::sync::Arc<dyn LoopPlayer>) -> Self {
        Self { store, player, idle_timeout: IDLE_TIMEOUT }
    }

    /// Delete every phone video except the one that is loaded right now.
    pub async fn sweep_unused(&self) -> usize {
        let loaded = self.player.state().await.path;
        self.store.sweep(loaded.as_deref())
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
        /// `play_file` fails (an unreadable video).
        pub fail_play: bool,
        /// `play_file` takes this long (a slow probe).
        pub play_delay: Duration,
        /// What the file held when it was handed over.
        pub played_bytes: Mutex<Option<Vec<u8>>>,
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
        fn play_file<'a>(&'a self, path: &'a Path, display_name: &'a str) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("play_file {display_name}"));
                tokio::time::sleep(self.play_delay).await;
                // Like the real engine: whatever played before is stopped first, and a
                // video that cannot be read leaves the old one loaded.
                self.state.lock().unwrap().playing = false;
                if self.fail_play {
                    return Err(anyhow!("No video stream found"));
                }
                *self.played_bytes.lock().unwrap() = Some(std::fs::read(path)?);
                *self.state.lock().unwrap() =
                    LoopState { playing: true, name: Some(display_name.into()), path: Some(path.into()) };
                Ok(())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(name: &str, size: u64, mime: &str) -> FileOffer {
        FileOffer { name: name.into(), size, mime: mime.into() }
    }
    fn meta(name: &str, size: u64, mime: &str) -> String {
        serde_json::json!({"t":"meta","name":name,"size":size,"mime":mime}).to_string()
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
        let tr = Transfer::new(offer("clip.mp4", 1000, "video/mp4"), t, IDLE_TIMEOUT);
        assert_eq!(tr.check_idle(t + secs(19)), Ok(()));
        assert_eq!(tr.check_idle(t + secs(20)), Err(TransferError::Idle));
        // Every message restarts the clock.
        let mut tr = started(1000, t + secs(15));
        assert_eq!(tr.check_idle(t + secs(30)), Ok(()));
        tr.on_binary(500, t + secs(30)).unwrap();
        assert_eq!(tr.check_idle(t + secs(49)), Ok(()));
        assert_eq!(tr.check_idle(t + secs(50)), Err(TransferError::Idle));
        // After `end` the phone has nothing left to send.
        tr.on_binary(500, t + secs(31)).unwrap();
        tr.on_text(r#"{"t":"end"}"#, t + secs(31)).unwrap();
        assert_eq!(tr.check_idle(t + secs(500)), Ok(()));
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
            ("../../evil.mp4/../x.3gp", "", "3gp"),
        ] {
            let o = offer(name, 1, mime);
            assert_eq!(extension_for(&o), ext, "{name} {mime}");
            assert!(crate::remote::stream::signal::PHONE_VIDEO_EXTENSIONS.contains(&extension_for(&o)));
        }
        // The loader gets a name that ends in a video extension.
        assert_eq!(load_name(&offer("clip.MOV", 1, "video/quicktime")), "clip.MOV");
        assert_eq!(load_name(&offer("VID_2024", 1, "video/mp4")), "VID_2024.mp4");
        assert_eq!(load_name(&offer("notes.txt", 1, "video/webm")), "notes.txt.webm");
    }

    #[tokio::test]
    async fn a_file_is_written_as_part_and_renamed_on_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        assert_eq!(store.dir(), tmp.path().join("phone-videos"));
        // A name full of path tricks: none of it reaches the disk.
        let mut part = store.create(&offer("../../../etc/passwd\\..\\x.mov", 6, "video/quicktime")).await.unwrap();
        let names = |dir: &Path| -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
            v.sort();
            v
        };
        let during = names(store.dir());
        assert_eq!(during.len(), 1);
        let n = &during[0];
        assert!(n.starts_with("phone-") && n.ends_with(".mov.part"), "{n}");
        let hex = &n["phone-".len()..n.len() - ".mov.part".len()];
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
        let path = part.commit().await.unwrap();
        assert_eq!(names(store.dir()), vec![format!("phone-{hex}.mov")]);
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        assert!(store.contains(&path));
        assert!(!store.contains(&tmp.path().join("phone-x.mp4")));
        assert!(!store.contains(&store.dir().join("other.mp4")));
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
        let a = store.create(&offer("a.mp4", 10, "video/mp4")).await.unwrap().commit().await.unwrap();
        let b = store.create(&offer("a.mp4", 10, "video/mp4")).await.unwrap().commit().await.unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn the_sweep_keeps_only_the_current_video() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        assert_eq!(store.sweep(None), 0, "no directory yet is fine");
        let old = store.create(&offer("a.mp4", 1, "video/mp4")).await.unwrap().commit().await.unwrap();
        let current = store.create(&offer("b.webm", 1, "video/webm")).await.unwrap().commit().await.unwrap();
        // A `.part` left behind by a crash, a file that is not ours, and a subdirectory.
        let stale = store.dir().join("phone-0123456789abcdef.mp4.part");
        std::fs::write(&stale, b"x").unwrap();
        let foreign = store.dir().join("notes.txt");
        std::fs::write(&foreign, b"x").unwrap();
        std::fs::create_dir(store.dir().join("phone-dir")).unwrap();

        assert_eq!(store.sweep(Some(&current)), 2);
        assert!(!old.exists() && !stale.exists());
        assert!(current.exists() && foreign.exists() && store.dir().join("phone-dir").exists());
        // A kept path elsewhere (a desktop-picked video) protects nothing here.
        assert_eq!(store.sweep(Some(Path::new("/somewhere/else.mp4"))), 1);
        assert!(!current.exists() && foreign.exists());
    }
}
