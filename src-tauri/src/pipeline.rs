//! ffmpeg command lines for the virtual-camera pipeline.
//!
//! Everything here is pure string building — no spawning, no I/O. That is deliberate: it
//! makes the entire ffmpeg surface for all three platforms assertable in `cargo test --lib`
//! on any host, including the Windows and macOS legs that CI cannot otherwise exercise.
//!
//! # Why one process
//!
//! Frames used to reach the camera through *two* ffmpeg processes with an MJPEG hop in
//! between: the producer encoded `-q:v 5` MJPEG, and the sink decoded it straight back to
//! raw. Measured at 720p, that encode cost ~8.2 ms of CPU per frame and the matching decode
//! ~4.7 ms, against ~2.0 ms to emit raw frames directly — roughly 40% of a core, spent
//! making the picture worse. The hop also silently re-subsampled chroma (see
//! [`mjpeg_store_args`]) and ran two scale passes with mismatched aspect handling.
//!
//! So the straight-loop path is one process that decodes and writes raw frames to the
//! camera. Natural motion still stores MJPEG, because a random-access walk needs random
//! access — but there the MJPEG is a *store*, not a transport hop, and the second process
//! is a decoder rather than a redundant re-encode.

use std::path::Path;

/// Where the finished raw frames go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraSink {
    /// Linux: ffmpeg writes straight into the v4l2loopback device node.
    V4l2 { device: String },
    /// Windows: packed BGR24 on stdout, which Rust forwards into softcam's shared memory.
    RawBgr24Stdout,
    /// macOS, once the CMIO extension is signed: BGRA on stdout
    /// (`kCVPixelFormatType_32BGRA`).
    #[allow(dead_code)]
    RawBgraStdout,
    /// No camera leg at all — preview only. Used on macOS today, and whenever playback runs
    /// with the virtual camera switched off.
    None,
}

impl CameraSink {
    /// Bytes per delivered frame, for sinks that hand raw frames back through a pipe.
    /// `None` when ffmpeg writes to the device itself and Rust never sees the pixels.
    pub fn frame_bytes(&self, geom: &Geometry) -> Option<usize> {
        let px = geom.width as usize * geom.height as usize;
        match self {
            CameraSink::RawBgr24Stdout => Some(px * 3),
            CameraSink::RawBgraStdout => Some(px * 4),
            CameraSink::V4l2 { .. } | CameraSink::None => None,
        }
    }

    fn writes_to_stdout(&self) -> bool {
        matches!(self, CameraSink::RawBgr24Stdout | CameraSink::RawBgraStdout)
    }
}

/// The geometry the camera advertises. Both the source scale and the sink must agree on
/// this, or frames get rescaled twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// The low-resolution MJPEG leg that feeds the UI preview.
///
/// Small and slow on purpose. The preview exists to show the user what the camera is
/// emitting; it does not need to be the camera's resolution or its frame rate, and every
/// byte here crosses the IPC boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewSpec {
    pub width: u32,
    pub fps: u32,
    pub quality: u8,
}

impl Default for PreviewSpec {
    fn default() -> Self {
        Self { width: 480, fps: 12, quality: 7 }
    }
}

/// Scale to fit inside the target, pad the remainder, never distort.
///
/// The old pipeline scaled `{W}:-2` at the source and then `{W}:{H}` at the sink, so any
/// source whose aspect ratio differed from the camera's was stretched to fit. That was not
/// a corner case: the in-app recorder captures 4:3 and the default camera is 16:9, so every
/// clip recorded in CamLooper was being stretched.
///
/// `setsar=1` keeps consumers from re-applying a sample aspect ratio on top.
pub fn fit_filter(geom: &Geometry) -> String {
    format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,\
         pad={w}:{h}:-1:-1:color=black,setsar=1",
        w = geom.width,
        h = geom.height
    )
}

/// Arguments for the MJPEG *store* used by natural motion.
///
/// Two things matter here and neither was set before:
///
/// * `-pix_fmt` was absent, so the encoder defaulted to `yuvj420p` and re-subsampled chroma
///   onto a shifted grid on every frame. `yuvj422p` removes that resample. 444 would not
///   buy anything further — the source is virtually always 4:2:0 H.264, so 444 stores
///   upsampled chroma at ~1.35x the bytes for zero extra information.
/// * `-q:v 5` was a visible quantisation on top. 2 is near-transparent.
///
/// `width` is the *store* width, which the caller should clamp to the source width: encoding
/// an upscaled frame pays for invented detail at JPEG rates.
pub fn mjpeg_store_args(video: &Path, width: u32, max_frames: usize) -> Vec<String> {
    let mut a = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-i".into(),
        video.to_string_lossy().into_owned(),
        // No audio or subtitle decoding for a frame store.
        "-an".into(),
        "-sn".into(),
        "-map".into(),
        "0:v".into(),
        "-vf".into(),
        format!("scale={width}:-2:flags=lanczos"),
    ];
    // Enforce the window at the source rather than decoding a long file in full and
    // throwing most of it away.
    a.extend(["-frames:v".into(), max_frames.to_string()]);
    a.extend([
        "-c:v".into(),
        "mjpeg".into(),
        "-q:v".into(),
        "2".into(),
        "-pix_fmt".into(),
        "yuvj422p".into(),
        "-f".into(),
        "image2pipe".into(),
        "pipe:1".into(),
    ]);
    a
}

/// What the pipeline reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A file on disk. `stream_loop` is ffmpeg's own `-stream_loop`: `-1` for endless,
    /// `n` to play `n + 1` times. Looping inside ffmpeg avoids respawning the process once
    /// per pass, which is what the old straight-loop path did.
    File { path: String, stream_loop: i32 },
    /// MJPEG frames fed in on stdin — the natural-motion walk, replaying its store.
    MjpegStdin { fps: u32 },
    /// A capture device, already expressed as ffmpeg input arguments.
    Device { args: Vec<String> },
}

/// A complete pipeline description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineSpec {
    pub source: Source,
    pub sink: CameraSink,
    pub geometry: Geometry,
    pub preview: Option<PreviewSpec>,
    /// Pace the input at its native rate (`-re`). Right for playing a file in real time;
    /// wrong when something downstream is already the clock, as the walk ticker is.
    pub realtime: bool,
}

impl PipelineSpec {
    /// The preview cannot share stdout with a raw camera leg, and ffmpeg has no second
    /// stdout to give it. On those platforms the preview is derived in Rust from the raw
    /// frames instead, so the spec must not ask ffmpeg for one.
    fn preview_leg(&self) -> Option<&PreviewSpec> {
        match self.preview {
            Some(ref p) if !self.sink.writes_to_stdout() => Some(p),
            _ => None,
        }
    }
}

/// Build the full argument vector for a pipeline.
pub fn build(spec: &PipelineSpec) -> Vec<String> {
    let mut a = build_input(spec);
    a.extend(build_outputs(spec));
    a
}

/// Global flags plus the input. Split out so a caller that assembles its own input — the
/// live-camera path, which has device flags of its own — can still share the output legs.
fn build_input(spec: &PipelineSpec) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        // Never let ffmpeg consume our stdin; the walk feeds it deliberately via pipe:0.
        "-nostdin".into(),
    ];

    match &spec.source {
        Source::File { path, stream_loop } => {
            // -stream_loop must precede -i: it is an input option.
            a.extend(["-stream_loop".into(), stream_loop.to_string()]);
            if spec.realtime {
                a.push("-re".into());
            }
            a.extend(["-i".into(), path.clone()]);
        }
        Source::MjpegStdin { fps } => {
            a.extend([
                "-analyzeduration".into(),
                "0".into(),
                "-probesize".into(),
                "32768".into(),
                "-f".into(),
                "mjpeg".into(),
                "-framerate".into(),
                fps.to_string(),
                "-i".into(),
                "pipe:0".into(),
            ]);
        }
        Source::Device { args } => {
            a.extend(args.iter().cloned());
        }
    }

    a
}

/// The camera leg and, where stdout is free for it, the preview leg.
pub fn build_outputs(spec: &PipelineSpec) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let fit = fit_filter(&spec.geometry);

    match &spec.sink {
        CameraSink::V4l2 { device } => {
            a.extend(["-map".into(), "0:v".into()]);
            a.extend(["-vf".into(), format!("{fit},fps={}", spec.geometry.fps)]);
            // rgb24 must match the format v4l2loopback locks the device to. It also avoids
            // subsampling chroma again on the way across the kernel boundary.
            a.extend(["-f".into(), "v4l2".into(), "-pix_fmt".into(), "rgb24".into()]);
            a.push(device.clone());
        }
        CameraSink::RawBgr24Stdout | CameraSink::RawBgraStdout => {
            let pix = if matches!(spec.sink, CameraSink::RawBgr24Stdout) { "bgr24" } else { "bgra" };
            a.extend(["-map".into(), "0:v".into()]);
            a.extend(["-vf".into(), format!("{fit},fps={}", spec.geometry.fps)]);
            // rawvideo has no stride or padding, so every frame is exactly
            // width*height*bpp bytes and the reader can use a fixed-size read_exact.
            a.extend(["-f".into(), "rawvideo".into(), "-pix_fmt".into(), pix.into()]);
            a.push("pipe:1".into());
        }
        CameraSink::None => {}
    }

    if let Some(p) = spec.preview_leg() {
        a.extend(["-map".into(), "0:v".into()]);
        a.extend([
            "-vf".into(),
            // bilinear, not lanczos: this is a thumbnail, and it is on the same process as
            // the camera leg.
            format!("scale={}:-2:flags=bilinear,fps={}", p.width, p.fps),
        ]);
        a.extend([
            "-c:v".into(),
            "mjpeg".into(),
            "-q:v".into(),
            p.quality.to_string(),
            "-f".into(),
            "image2pipe".into(),
            "pipe:1".into(),
        ]);
    }

    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> Geometry {
        Geometry { width: 1280, height: 720, fps: 30 }
    }

    fn joined(spec: &PipelineSpec) -> String {
        build(spec).join(" ")
    }

    fn file_spec(sink: CameraSink) -> PipelineSpec {
        PipelineSpec {
            source: Source::File { path: "/clip.mp4".into(), stream_loop: -1 },
            sink,
            geometry: geom(),
            preview: Some(PreviewSpec::default()),
            realtime: true,
        }
    }

    // --- the geometry contract -----------------------------------------------------------
    //
    // The whole point of the fit filter is that nothing is ever stretched. These pin the
    // exact filter text, because a silent change here is invisible until someone looks at
    // a 4:3 clip on a 16:9 camera.

    #[test]
    fn fit_never_distorts_and_pads_instead() {
        let f = fit_filter(&geom());
        assert!(f.contains("force_original_aspect_ratio=decrease"), "{f}");
        assert!(f.contains("pad=1280:720"), "{f}");
        assert!(f.contains("setsar=1"), "{f}");
        // A bare scale=W:H would stretch. It must not appear.
        assert!(!f.contains("scale=1280:720,"), "{f}");
    }

    #[test]
    fn fit_uses_lanczos_not_the_swscale_default() {
        assert!(fit_filter(&geom()).contains("flags=lanczos"));
    }

    #[test]
    fn fit_tracks_the_requested_geometry() {
        let f = fit_filter(&Geometry { width: 1920, height: 1080, fps: 30 });
        assert!(f.contains("scale=1920:1080"), "{f}");
        assert!(f.contains("pad=1920:1080"), "{f}");
    }

    // --- ordering rules ffmpeg actually enforces -----------------------------------------

    #[test]
    fn stream_loop_precedes_the_input() {
        let a = build(&file_spec(CameraSink::RawBgr24Stdout));
        let sl = a.iter().position(|x| x == "-stream_loop").expect("-stream_loop");
        let i = a.iter().position(|x| x == "-i").expect("-i");
        assert!(sl < i, "-stream_loop is an input option and must come first: {a:?}");
    }

    #[test]
    fn realtime_pacing_is_opt_in() {
        let mut s = file_spec(CameraSink::RawBgr24Stdout);
        assert!(build(&s).contains(&"-re".to_string()));
        s.realtime = false;
        assert!(!build(&s).contains(&"-re".to_string()));
    }

    // --- per-sink pixel formats ----------------------------------------------------------

    #[test]
    fn linux_writes_rgb24_to_the_device_node() {
        let s = file_spec(CameraSink::V4l2 { device: "/dev/video0".into() });
        let out = joined(&s);
        assert!(out.contains("-f v4l2"), "{out}");
        assert!(out.contains("-pix_fmt rgb24"), "{out}");
        assert!(out.ends_with("pipe:1"), "preview leg should be last: {out}");
        assert!(out.contains("/dev/video0"), "{out}");
    }

    #[test]
    fn windows_writes_packed_bgr24_to_stdout() {
        let out = joined(&file_spec(CameraSink::RawBgr24Stdout));
        assert!(out.contains("-f rawvideo"), "{out}");
        assert!(out.contains("-pix_fmt bgr24"), "{out}");
    }

    #[test]
    fn macos_writes_bgra_for_the_cmio_extension() {
        let out = joined(&file_spec(CameraSink::RawBgraStdout));
        assert!(out.contains("-pix_fmt bgra"), "{out}");
    }

    #[test]
    fn raw_frame_size_matches_the_declared_pixel_format() {
        let g = geom();
        assert_eq!(CameraSink::RawBgr24Stdout.frame_bytes(&g), Some(1280 * 720 * 3));
        assert_eq!(CameraSink::RawBgraStdout.frame_bytes(&g), Some(1280 * 720 * 4));
        // ffmpeg owns the device; Rust never sees these pixels.
        assert_eq!(CameraSink::V4l2 { device: "/dev/video0".into() }.frame_bytes(&g), None);
        assert_eq!(CameraSink::None.frame_bytes(&g), None);
    }

    // --- the preview leg -----------------------------------------------------------------

    #[test]
    fn a_raw_stdout_sink_gets_no_ffmpeg_preview_leg() {
        // stdout is taken by the camera, and there is no second stdout to give the preview.
        // Those platforms derive it in Rust instead; asking ffmpeg for one here would
        // interleave two streams on one pipe and corrupt both.
        let out = joined(&file_spec(CameraSink::RawBgr24Stdout));
        assert!(!out.contains("mjpeg"), "{out}");
        assert_eq!(out.matches("pipe:1").count(), 1, "{out}");
    }

    #[test]
    fn a_device_sink_does_get_one() {
        let out = joined(&file_spec(CameraSink::V4l2 { device: "/dev/video0".into() }));
        assert!(out.contains("-c:v mjpeg"), "{out}");
        assert!(out.contains("scale=480:-2"), "{out}");
    }

    #[test]
    fn no_preview_requested_means_no_preview_leg() {
        let mut s = file_spec(CameraSink::V4l2 { device: "/dev/video0".into() });
        s.preview = None;
        assert!(!joined(&s).contains("mjpeg"));
    }

    #[test]
    fn preview_only_pipeline_has_no_camera_leg() {
        // Playback with the virtual camera switched off.
        let s = file_spec(CameraSink::None);
        let out = joined(&s);
        assert!(!out.contains("rawvideo"), "{out}");
        assert!(!out.contains("v4l2"), "{out}");
        assert!(out.contains("-c:v mjpeg"), "{out}");
    }

    // --- the MJPEG store -----------------------------------------------------------------

    #[test]
    fn the_store_pins_a_pixel_format() {
        // Regression: with no -pix_fmt the encoder picks yuvj420p and re-subsamples chroma
        // onto a shifted grid on every frame. That was the app's largest self-inflicted
        // quality loss.
        let a = mjpeg_store_args(Path::new("/clip.mp4"), 1280, 1800).join(" ");
        assert!(a.contains("-pix_fmt yuvj422p"), "{a}");
        assert!(a.contains("-q:v 2"), "{a}");
    }

    #[test]
    fn the_store_caps_frames_at_the_source() {
        let a = mjpeg_store_args(Path::new("/clip.mp4"), 640, 900).join(" ");
        assert!(a.contains("-frames:v 900"), "{a}");
        assert!(a.contains("scale=640:-2"), "{a}");
    }

    #[test]
    fn the_store_skips_audio_and_subtitles() {
        let a = mjpeg_store_args(Path::new("/clip.mp4"), 640, 10).join(" ");
        assert!(a.contains("-an"), "{a}");
        assert!(a.contains("-sn"), "{a}");
    }

    // --- the walk's replay pipeline ------------------------------------------------------

    #[test]
    fn mjpeg_stdin_is_not_self_paced() {
        // The Rust ticker is the clock for natural motion. A -re here would fight it.
        let s = PipelineSpec {
            source: Source::MjpegStdin { fps: 30 },
            sink: CameraSink::RawBgr24Stdout,
            geometry: geom(),
            preview: None,
            realtime: false,
        };
        let out = joined(&s);
        assert!(!out.contains("-re "), "{out}");
        assert!(out.contains("-f mjpeg"), "{out}");
        assert!(out.contains("-i pipe:0"), "{out}");
        assert!(out.contains("-framerate 30"), "{out}");
    }

    // --- end to end, against a real ffmpeg ----------------------------------------------
    //
    // The unit tests above pin the argument strings; these prove the strings mean what we
    // think. Skipped when ffmpeg is not on PATH, since the app ships its own as a bundled
    // resource and CI runners vary.

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// A 4:3 clip, so fitting it into a 16:9 camera has to pillarbox.
    fn make_4x3_fixture(dir: &std::path::Path, seconds: u32) -> std::path::PathBuf {
        let path = dir.join("fixture43.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error", "-y",
                "-f", "lavfi",
                "-i", &format!("testsrc2=size=640x480:rate=30:duration={seconds}"),
                "-c:v", "libx264", "-pix_fmt", "yuv420p",
            ])
            .arg(&path)
            .status()
            .expect("failed to run ffmpeg");
        assert!(status.success(), "ffmpeg could not build the fixture");
        path
    }

    #[test]
    fn raw_output_is_exactly_one_frame_size_per_frame() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = make_4x3_fixture(dir.path(), 1);
        let geometry = Geometry { width: 1280, height: 720, fps: 30 };
        let spec = PipelineSpec {
            source: Source::File { path: src.to_string_lossy().into_owned(), stream_loop: 0 },
            sink: CameraSink::RawBgr24Stdout,
            geometry,
            preview: None,
            realtime: false,
        };

        let out = std::process::Command::new("ffmpeg")
            .args(build(&spec))
            .output()
            .expect("failed to run the built pipeline");
        assert!(
            out.status.success(),
            "pipeline failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let frame = CameraSink::RawBgr24Stdout.frame_bytes(&geometry).unwrap();
        assert_eq!(
            out.stdout.len() % frame,
            0,
            "rawvideo must be a whole number of {frame}-byte frames, got {}",
            out.stdout.len()
        );
        assert_eq!(out.stdout.len() / frame, 30, "1s at 30 fps");
    }

    #[test]
    fn a_four_by_three_source_is_pillarboxed_not_stretched() {
        // The regression this filter exists for. A 640x480 source fitted into 1280x720
        // scales to 960x720, leaving a 160px black bar on each side. If the bars are not
        // black — or not there — the image is being stretched.
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = make_4x3_fixture(dir.path(), 1);
        let (w, h) = (1280usize, 720usize);
        let geometry = Geometry { width: w as u32, height: h as u32, fps: 30 };
        let spec = PipelineSpec {
            source: Source::File { path: src.to_string_lossy().into_owned(), stream_loop: 0 },
            sink: CameraSink::RawBgr24Stdout,
            geometry,
            preview: None,
            realtime: false,
        };

        let out = std::process::Command::new("ffmpeg").args(build(&spec)).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let content_w = h * 640 / 480; // 960
        let bar = (w - content_w) / 2; // 160
        let frame = &out.stdout[..w * h * 3];

        let mut bar_max = 0u8;
        let mut content_max = 0u8;
        for y in 0..h {
            let row = &frame[y * w * 3..(y + 1) * w * 3];
            for x in 0..w {
                let px = row[x * 3..x * 3 + 3].iter().copied().max().unwrap();
                if x < bar || x >= w - bar {
                    bar_max = bar_max.max(px);
                } else {
                    content_max = content_max.max(px);
                }
            }
        }
        assert_eq!(bar_max, 0, "pillarbox bars must be pure black, saw {bar_max}");
        assert!(content_max > 50, "the padded region swallowed the picture");
    }

    #[test]
    fn device_input_arguments_are_passed_through_verbatim() {
        let s = PipelineSpec {
            source: Source::Device { args: vec!["-f".into(), "v4l2".into(), "-i".into(), "/dev/video1".into()] },
            sink: CameraSink::V4l2 { device: "/dev/video0".into() },
            geometry: geom(),
            preview: None,
            realtime: false,
        };
        assert!(joined(&s).contains("-f v4l2 -i /dev/video1"));
    }
}

