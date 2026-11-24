//! Turning depacketized WebRTC frames into byte streams ffmpeg can read on stdin:
//! H.264 → Annex-B (SPS/PPS guaranteed before every IDR), VP8 → IVF. Pure, no I/O, so the
//! exact bytes are unit-tested.
use std::time::{Duration, Instant};

use crate::pipeline::EncodedCodec;

/// What to do with one frame.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameOut {
    /// Write these bytes to ffmpeg's stdin.
    Write { bytes: Vec<u8>, keyframe: bool },
    /// Dropped: the decoder cannot use it until a keyframe arrives. Ask the phone for one.
    NeedKeyframe,
}

/// Elementary-stream writer for one ffmpeg process. A new ffmpeg gets a new writer, so the
/// stream it sees always starts at a keyframe (and, for VP8, with an IVF header).
pub enum StreamWriter {
    H264(AnnexBWriter),
    Vp8(IvfWriter),
}

impl StreamWriter {
    pub fn new(codec: EncodedCodec) -> Self {
        match codec {
            EncodedCodec::H264 => StreamWriter::H264(AnnexBWriter::default()),
            EncodedCodec::Vp8 => StreamWriter::Vp8(IvfWriter::default()),
        }
    }

    pub fn codec(&self) -> EncodedCodec {
        match self {
            StreamWriter::H264(_) => EncodedCodec::H264,
            StreamWriter::Vp8(_) => EncodedCodec::Vp8,
        }
    }

    pub fn push(&mut self, frame: &[u8], rtp_ts: u32) -> FrameOut {
        match self {
            StreamWriter::H264(w) => w.push(frame),
            StreamWriter::Vp8(w) => w.push(frame, rtp_ts),
        }
    }

    /// After a gap (lost or dropped frames): drop everything until the next keyframe, so the
    /// camera freezes on the last good picture instead of showing smeared macroblocks.
    pub fn require_keyframe(&mut self) {
        match self {
            StreamWriter::H264(w) => w.synced = false,
            StreamWriter::Vp8(w) => w.synced = false,
        }
    }

    /// Picture size from the most recent SPS / VP8 keyframe header.
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match self {
            StreamWriter::H264(w) => w.dims,
            StreamWriter::Vp8(w) => w.dims,
        }
    }
}

// ---------------------------------------------------------------------------------------
// H.264
// ---------------------------------------------------------------------------------------

pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;

/// NAL unit bodies (without start codes) of an Annex-B buffer; 3- and 4-byte start codes.
pub fn annexb_nals(buf: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut end = if k + 1 < starts.len() { starts[k + 1] - 3 } else { buf.len() };
        // A trailing zero belongs to the next NAL's 4-byte start code.
        while end > s && k + 1 < starts.len() && buf[end - 1] == 0 {
            end -= 1;
        }
        if end > s {
            out.push(&buf[s..end]);
        }
    }
    out
}

pub fn h264_is_keyframe(frame: &[u8]) -> bool {
    annexb_nals(frame).iter().any(|n| n[0] & 0x1f == NAL_IDR)
}

#[derive(Default)]
pub struct AnnexBWriter {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    synced: bool,
    dims: Option<(u32, u32)>,
}

impl AnnexBWriter {
    /// `frame` is one access unit in Annex-B, as str0m's H.264 depacketizer emits it.
    pub fn push(&mut self, frame: &[u8]) -> FrameOut {
        let nals = annexb_nals(frame);
        let (mut has_idr, mut has_sps, mut has_pps) = (false, false, false);
        for n in &nals {
            match n[0] & 0x1f {
                NAL_IDR => has_idr = true,
                NAL_SPS => {
                    has_sps = true;
                    if let Some(d) = sps_dimensions(n) {
                        self.dims = Some(d);
                    }
                    self.sps = Some(n.to_vec());
                }
                NAL_PPS => {
                    has_pps = true;
                    self.pps = Some(n.to_vec());
                }
                _ => {}
            }
        }
        if has_idr && (self.sps.is_none() || self.pps.is_none()) {
            return FrameOut::NeedKeyframe;
        }
        if !self.synced && !has_idr {
            return FrameOut::NeedKeyframe;
        }
        self.synced = true;
        let mut buf = Vec::with_capacity(frame.len() + 64);
        if has_idr && !(has_sps && has_pps) {
            for p in [&self.sps, &self.pps].into_iter().flatten() {
                buf.extend_from_slice(&[0, 0, 0, 1]);
                buf.extend_from_slice(p);
            }
        }
        for n in &nals {
            buf.extend_from_slice(&[0, 0, 0, 1]);
            buf.extend_from_slice(n);
        }
        FrameOut::Write { bytes: buf, keyframe: has_idr }
    }
}

/// Remove emulation-prevention bytes (00 00 03 → 00 00).
fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let v = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(v as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u64 << zeros) - 1 + self.bits(zeros)? as u64) as u32)
    }
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) } as i32)
    }
}

/// Display size from an SPS NAL (header byte included), cropping applied. ITU-T H.264 7.3.2.1.
pub fn sps_dimensions(nal: &[u8]) -> Option<(u32, u32)> {
    if nal.first()? & 0x1f != NAL_SPS {
        return None;
    }
    let data = rbsp(&nal[1..]);
    let mut r = Bits { data: &data, pos: 0 };
    let profile_idc = r.bits(8)?;
    r.bits(16)?; // constraint flags + level_idc
    r.ue()?; // seq_parameter_set_id
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane = 0;
    if matches!(profile_idc, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        chroma_format_idc = r.ue()?;
        if chroma_format_idc == 3 {
            separate_colour_plane = r.bit()?;
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.bit()?; // qpprime_y_zero_transform_bypass_flag
        if r.bit()? == 1 {
            let lists = if chroma_format_idc != 3 { 8 } else { 12 };
            for i in 0..lists {
                if r.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + r.se()? + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            let n = r.ue()?;
            if n > 255 {
                return None;
            }
            for _ in 0..n {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_value_allowed_flag
    let w_mbs = r.ue()? + 1;
    let h_map_units = r.ue()? + 1;
    let frame_mbs_only = r.bit()?;
    if frame_mbs_only == 0 {
        r.bit()?; // mb_adaptive_frame_field_flag
    }
    r.bit()?; // direct_8x8_inference_flag
    let mut width = w_mbs.checked_mul(16)?;
    let mut height = (2 - frame_mbs_only).checked_mul(h_map_units)?.checked_mul(16)?;
    if r.bit()? == 1 {
        let (left, right, top, bottom) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
        let chroma_array_type = if separate_colour_plane == 1 { 0 } else { chroma_format_idc };
        let (sub_w, sub_h) = match chroma_array_type {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let (crop_x, crop_y) = if chroma_array_type == 0 {
            (1, 2 - frame_mbs_only)
        } else {
            (sub_w, sub_h * (2 - frame_mbs_only))
        };
        width = width.checked_sub(left.checked_add(right)?.checked_mul(crop_x)?)?;
        height = height.checked_sub(top.checked_add(bottom)?.checked_mul(crop_y)?)?;
    }
    (width > 0 && height > 0 && width <= 16384 && height <= 16384).then_some((width, height))
}

// ---------------------------------------------------------------------------------------
// VP8 / IVF
// ---------------------------------------------------------------------------------------

/// VP8 keyframe: the P bit (bit 0 of the first byte) is 0.
pub fn vp8_is_keyframe(frame: &[u8]) -> bool {
    !frame.is_empty() && frame[0] & 0x01 == 0
}

/// Width/height from a VP8 keyframe header (start code 9d 01 2a at offset 3).
pub fn vp8_dimensions(frame: &[u8]) -> Option<(u32, u32)> {
    if frame.len() >= 10 && vp8_is_keyframe(frame) && frame[3..6] == [0x9d, 0x01, 0x2a] {
        let w = u16::from_le_bytes([frame[6], frame[7]]) & 0x3fff;
        let h = u16::from_le_bytes([frame[8], frame[9]]) & 0x3fff;
        (w > 0 && h > 0).then_some((w as u32, h as u32))
    } else {
        None
    }
}

/// IVF header for a stream (frame count 0: it is a live pipe and the count is never known;
/// ffmpeg's demuxer does not need it). Timebase 1/90000, the RTP clock.
pub fn ivf_header(width: u16, height: u16) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[0..4].copy_from_slice(b"DKIF");
    h[4..6].copy_from_slice(&0u16.to_le_bytes());
    h[6..8].copy_from_slice(&32u16.to_le_bytes());
    h[8..12].copy_from_slice(b"VP80");
    h[12..14].copy_from_slice(&width.to_le_bytes());
    h[14..16].copy_from_slice(&height.to_le_bytes());
    h[16..20].copy_from_slice(&90_000u32.to_le_bytes());
    h[20..24].copy_from_slice(&1u32.to_le_bytes());
    h
}

#[derive(Default)]
pub struct IvfWriter {
    header_written: bool,
    synced: bool,
    dims: Option<(u32, u32)>,
    ts: RtpTsUnwrap,
    first_ts: Option<u64>,
}

impl IvfWriter {
    pub fn push(&mut self, frame: &[u8], rtp_ts: u32) -> FrameOut {
        let key = vp8_is_keyframe(frame);
        let dims = if key { vp8_dimensions(frame) } else { None };
        if let Some(d) = dims {
            self.dims = Some(d);
        }
        if !self.synced && dims.is_none() {
            return FrameOut::NeedKeyframe;
        }
        self.synced = true;
        let mut buf = Vec::with_capacity(frame.len() + 44);
        if !self.header_written {
            let (w, h) = self.dims.unwrap_or((0, 0));
            buf.extend_from_slice(&ivf_header(w as u16, h as u16));
            self.header_written = true;
        }
        let ts = self.ts.unwrap(rtp_ts);
        let pts = ts.saturating_sub(*self.first_ts.get_or_insert(ts));
        buf.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        buf.extend_from_slice(&pts.to_le_bytes());
        buf.extend_from_slice(frame);
        FrameOut::Write { bytes: buf, keyframe: key }
    }
}

/// Unwraps the 32-bit RTP timestamp into a monotonic-ish u64.
#[derive(Default, Debug)]
pub struct RtpTsUnwrap {
    last: Option<u32>,
    acc: u64,
}

impl RtpTsUnwrap {
    pub fn unwrap(&mut self, ts: u32) -> u64 {
        match self.last {
            None => self.acc = ts as u64,
            Some(l) => {
                let d = ts.wrapping_sub(l) as i32;
                self.acc = (self.acc as i64 + d as i64).max(0) as u64;
            }
        }
        self.last = Some(ts);
        self.acc
    }
}

// ---------------------------------------------------------------------------------------
// Keyframe requests
// ---------------------------------------------------------------------------------------

/// At most one PLI per `interval`. `want()` records demand; `ready()` says whether one may go
/// out now; `sent()` records that it did.
#[derive(Debug)]
pub struct PliLimiter {
    last: Option<Instant>,
    pending: bool,
    interval: Duration,
}

impl PliLimiter {
    pub fn new(interval: Duration) -> Self {
        Self { last: None, pending: false, interval }
    }
    pub fn want(&mut self) {
        self.pending = true;
    }
    pub fn pending(&self) -> bool {
        self.pending
    }
    pub fn ready(&self, now: Instant) -> bool {
        self.pending && self.last.is_none_or(|l| now.saturating_duration_since(l) >= self.interval)
    }
    /// When the next PLI could go out, if one is wanted.
    pub fn next_at(&self) -> Option<Instant> {
        if !self.pending {
            return None;
        }
        Some(self.last.map(|l| l + self.interval).unwrap_or_else(Instant::now))
    }
    pub fn sent(&mut self, now: Instant) {
        self.pending = false;
        self.last = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x42, 0xe0, 0x1f, 0xaa];
    const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
    const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x10];
    const P: &[u8] = &[0x41, 0x9a, 0x00, 0x01];

    fn ab(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = vec![];
        for (i, n) in nals.iter().enumerate() {
            v.extend_from_slice(if i % 2 == 0 { &[0, 0, 0, 1][..] } else { &[0, 0, 1][..] });
            v.extend_from_slice(n);
        }
        v
    }

    fn written(o: FrameOut) -> (Vec<u8>, bool) {
        match o {
            FrameOut::Write { bytes, keyframe } => (bytes, keyframe),
            FrameOut::NeedKeyframe => panic!("expected a write"),
        }
    }

    #[test]
    fn nal_split_handles_both_start_code_lengths() {
        assert_eq!(annexb_nals(&ab(&[SPS, PPS, IDR, P])), vec![SPS, PPS, IDR, P]);
        assert!(h264_is_keyframe(&ab(&[SPS, PPS, IDR])));
        assert!(!h264_is_keyframe(&ab(&[P])));
    }

    #[test]
    fn annexb_drops_until_idr_and_prepends_parameter_sets() {
        let mut w = AnnexBWriter::default();
        assert_eq!(w.push(&ab(&[P])), FrameOut::NeedKeyframe);
        // IDR without known SPS/PPS is undecodable.
        assert_eq!(w.push(&ab(&[IDR])), FrameOut::NeedKeyframe);
        // Parameter sets in their own access unit (some encoders), then a bare IDR.
        assert_eq!(w.push(&ab(&[SPS, PPS])), FrameOut::NeedKeyframe);
        let (bytes, key) = written(w.push(&ab(&[IDR])));
        assert!(key);
        assert_eq!(annexb_nals(&bytes), vec![SPS, PPS, IDR]);
        assert!(bytes.starts_with(&[0, 0, 0, 1]));
        let (bytes, key) = written(w.push(&ab(&[P])));
        assert!(!key);
        assert_eq!(annexb_nals(&bytes), vec![P]);
        // An IDR that already carries SPS/PPS is not given a second copy.
        let mut w2 = AnnexBWriter::default();
        let (bytes, _) = written(w2.push(&ab(&[SPS, PPS, IDR])));
        assert_eq!(annexb_nals(&bytes), vec![SPS, PPS, IDR]);
    }

    #[test]
    fn require_keyframe_resyncs_on_the_next_idr() {
        let mut w = StreamWriter::new(EncodedCodec::H264);
        written(w.push(&ab(&[SPS, PPS, IDR]), 0));
        written(w.push(&ab(&[P]), 3000));
        w.require_keyframe();
        assert_eq!(w.push(&ab(&[P]), 6000), FrameOut::NeedKeyframe);
        let (bytes, key) = written(w.push(&ab(&[IDR]), 9000));
        assert!(key);
        assert_eq!(annexb_nals(&bytes), vec![SPS, PPS, IDR], "cached parameter sets are re-sent");
    }

    // SPS of a 1280x720 constrained-baseline stream (x264), and of a 1920x1080 High stream
    // whose height needs cropping (1088 coded).
    const SPS_720P: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0xd9, 0x00, 0x50, 0x05, 0xbb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xc0, 0xf1, 0x83, 0x24, 0x80];
    const SPS_1080P_HIGH: &[u8] = &[0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xf0, 0x3c, 0x60, 0xc6, 0x58];

    #[test]
    fn sps_dimensions_baseline_and_high_with_cropping() {
        assert_eq!(sps_dimensions(SPS_720P), Some((1280, 720)));
        assert_eq!(sps_dimensions(SPS_1080P_HIGH), Some((1920, 1080)));
        assert_eq!(sps_dimensions(PPS), None);
        assert_eq!(sps_dimensions(&[0x67, 0x42]), None, "truncated SPS must not panic");
        let mut w = AnnexBWriter::default();
        written(w.push(&ab(&[SPS_720P, PPS, IDR])));
        assert_eq!(w.dims, Some((1280, 720)));
    }

    #[test]
    fn sps_dimensions_of_the_fixture_stream() {
        // The integration test's fixture (x264, 640x360). Parses its real SPS.
        let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/phone-stream/h264-640x360.h264"))
            .expect("fixture");
        let sps = annexb_nals(&data).into_iter().find(|n| n[0] & 0x1f == NAL_SPS).expect("sps");
        assert_eq!(sps_dimensions(sps), Some((640, 360)));
    }

    #[test]
    fn emulation_prevention_is_removed() {
        assert_eq!(rbsp(&[0, 0, 3, 1, 0, 0, 3, 0, 5]), vec![0, 0, 1, 0, 0, 0, 5]);
    }

    #[test]
    fn exp_golomb() {
        // 1 → 0, 010 → 1, 011 → 2, 00100 → 3 ; se: 1→+1, 2→-1
        let data = [0b1010_0110, 0b0100_0000];
        let mut r = Bits { data: &data, pos: 0 };
        assert_eq!(r.ue(), Some(0));
        assert_eq!(r.ue(), Some(1));
        assert_eq!(r.ue(), Some(2));
        assert_eq!(r.ue(), Some(3));
        let data = [0b0100_1100];
        let mut r = Bits { data: &data, pos: 0 };
        assert_eq!(r.se(), Some(1));
        assert_eq!(r.se(), Some(-1));
    }

    // Minimal fake VP8 frames: keyframe tag + start code + 640x480, and an inter frame.
    const VP8_KEY: [u8; 11] = [0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01, 0xff];
    const VP8_INTER: [u8; 4] = [0x31, 0x00, 0x00, 0xaa];

    #[test]
    fn ivf_stream_starts_at_a_keyframe_with_a_header() {
        let mut w = IvfWriter::default();
        assert_eq!(w.push(&VP8_INTER, 0), FrameOut::NeedKeyframe);
        let (first, key) = written(w.push(&VP8_KEY, 1000));
        assert!(key);
        assert_eq!(&first[0..4], b"DKIF");
        assert_eq!(&first[8..12], b"VP80");
        assert_eq!(u16::from_le_bytes([first[12], first[13]]), 640);
        assert_eq!(u16::from_le_bytes([first[14], first[15]]), 480);
        assert_eq!(u32::from_le_bytes(first[16..20].try_into().unwrap()), 90_000);
        // frame header: size, pts 0
        assert_eq!(u32::from_le_bytes(first[32..36].try_into().unwrap()), VP8_KEY.len() as u32);
        assert_eq!(u64::from_le_bytes(first[36..44].try_into().unwrap()), 0);
        assert_eq!(&first[44..], &VP8_KEY);
        let (second, key) = written(w.push(&VP8_INTER, 4000));
        assert!(!key);
        assert_eq!(second.len(), 12 + VP8_INTER.len(), "header only once");
        assert_eq!(u64::from_le_bytes(second[4..12].try_into().unwrap()), 3000);
        assert_eq!(w.dims, Some((640, 480)));
    }

    #[test]
    fn ivf_timestamps_survive_rtp_wrap() {
        let mut w = IvfWriter::default();
        written(w.push(&VP8_KEY, u32::MAX - 1499));
        let (b, _) = written(w.push(&VP8_INTER, 1500));
        assert_eq!(u64::from_le_bytes(b[4..12].try_into().unwrap()), 3000);
    }

    #[test]
    fn vp8_header_parsing() {
        assert!(vp8_is_keyframe(&VP8_KEY));
        assert!(!vp8_is_keyframe(&VP8_INTER));
        assert_eq!(vp8_dimensions(&VP8_KEY), Some((640, 480)));
        assert_eq!(vp8_dimensions(&VP8_INTER), None);
        assert_eq!(vp8_dimensions(&[]), None);
    }

    #[test]
    fn pli_limiter_is_rate_limited() {
        let t0 = Instant::now();
        let mut l = PliLimiter::new(Duration::from_secs(1));
        assert!(!l.ready(t0));
        assert_eq!(l.next_at(), None);
        l.want();
        assert!(l.ready(t0));
        l.sent(t0);
        l.want();
        assert!(!l.ready(t0 + Duration::from_millis(500)));
        assert_eq!(l.next_at(), Some(t0 + Duration::from_secs(1)));
        assert!(l.ready(t0 + Duration::from_millis(1000)));
    }
}
