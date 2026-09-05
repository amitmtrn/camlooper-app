//! Helpers for the UI preview leg of the video pipeline.
//!
//! Two jobs, depending on how the platform's camera is fed:
//!
//! * When ffmpeg owns the camera (Linux writes into the device node), stdout is free and
//!   ffmpeg emits a second, small MJPEG stream for the UI. [`JpegStreamReader`] carves
//!   frames out of it.
//! * When Rust owns the camera (Windows softcam), stdout is already carrying raw frames and
//!   ffmpeg has no second stdout to give. [`PreviewEncoder`] derives the preview from the
//!   raw frames we are already holding, at a fraction of the camera's rate.
//!
//! Either way the preview must never be able to slow the camera down. A backgrounded
//! WebView2 throttles its timers, which is exactly what happens when the user switches to
//! Zoom — the moment the camera matters most.

use crate::pipeline::{Geometry, PreviewSpec};

/// Carves JPEG frames out of a byte stream.
///
/// The previous version searched for the SOI and EOI markers with
/// `haystack.windows(2).position(...)` and re-ran that search **from offset 0 of the whole
/// accumulated buffer on every read**, then `drain`ed the front, memmoving the remainder.
/// That is quadratic in frame size. This keeps a scan cursor, uses `memchr` for the 0xFF
/// scan, and compacts the buffer only when the consumed prefix is worth reclaiming.
///
/// Scanning raw markers is safe here only because ffmpeg's mjpeg encoder writes no EXIF
/// thumbnail — an embedded JPEG would contain a nested SOI/EOI pair and this would split
/// frames in the wrong place.
pub struct JpegStreamReader {
    buf: Vec<u8>,
    /// Bytes already handed out, waiting to be reclaimed.
    consumed: usize,
    /// A start-of-image we have found but whose end has not arrived yet. Held separately
    /// from the scan cursor: an earlier version folded the two together, so once the cursor
    /// advanced past a pending SOI the reader could never find it again and the stream
    /// stalled the first time an EOI straddled a read boundary.
    soi: Option<usize>,
    /// Where to resume scanning for whichever marker we are currently looking for.
    scan: usize,
}

impl JpegStreamReader {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(128 * 1024), consumed: 0, soi: None, scan: 0 }
    }

    pub fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete JPEG, if one has arrived.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.soi {
                None => match find_marker(&self.buf, self.scan, 0xD8) {
                    Some(i) => {
                        self.soi = Some(i);
                        self.scan = i + 2;
                    }
                    None => {
                        self.park();
                        return None;
                    }
                },
                Some(soi) => match find_marker(&self.buf, self.scan, 0xD9) {
                    Some(eoi) => {
                        let end = eoi + 2;
                        let frame = self.buf[soi..end].to_vec();
                        self.soi = None;
                        self.scan = end;
                        self.consumed = end;
                        self.compact();
                        return Some(frame);
                    }
                    None => {
                        self.park();
                        return None;
                    }
                },
            }
        }
    }

    /// Remember how far the scan got, stopping one byte short so a `0xFF` sitting on the
    /// read boundary is re-examined once its partner arrives. Never moves backwards.
    fn park(&mut self) {
        self.scan = self.buf.len().saturating_sub(1).max(self.scan);
    }

    /// Reclaim the consumed prefix, once it is big enough to be worth the move. Only safe
    /// between frames — with a pending SOI its index would need rebasing too.
    fn compact(&mut self) {
        const THRESHOLD: usize = 64 * 1024;
        if self.soi.is_none() && self.consumed >= THRESHOLD {
            self.buf.drain(..self.consumed);
            self.scan = self.scan.saturating_sub(self.consumed);
            self.consumed = 0;
        }
    }
}

/// First `0xFF <code>` pair at or after `from`.
fn find_marker(buf: &[u8], from: usize, code: u8) -> Option<usize> {
    let mut i = from;
    while i + 1 < buf.len() {
        match memchr::memchr(0xFF, &buf[i..buf.len() - 1]) {
            Some(off) => {
                let at = i + off;
                if buf[at + 1] == code {
                    return Some(at);
                }
                i = at + 1;
            }
            None => return None,
        }
    }
    None
}

/// Derives preview JPEGs from raw BGR24 camera frames.
///
/// Runs at `spec.fps`, not the camera's, and decimates by an integer factor before encoding,
/// so the cost is a fraction of a millisecond per preview frame. Deliberately *not* a
/// resampling filter: this is a thumbnail behind the UI, and the camera path is what matters.
pub struct PreviewEncoder {
    width: usize,
    height: usize,
    factor: usize,
    /// Emit one preview per this many camera frames.
    every: u64,
    seen: u64,
    scratch: Vec<u8>,
}

impl PreviewEncoder {
    pub fn new(geom: &Geometry, spec: PreviewSpec) -> Self {
        let factor = ((geom.width as f64) / (spec.width.max(1) as f64)).ceil().max(1.0) as usize;
        let every = ((geom.fps.max(1) as f64) / (spec.fps.max(1) as f64)).round().max(1.0) as u64;
        Self {
            width: geom.width as usize,
            height: geom.height as usize,
            factor,
            every,
            seen: 0,
            scratch: Vec::new(),
        }
    }

    /// Offer a raw BGR24 frame. Returns a JPEG only on the sampled frames.
    pub fn sample(&mut self, bgr: &[u8]) -> Option<Vec<u8>> {
        let n = self.seen;
        self.seen += 1;
        if n % self.every != 0 {
            return None;
        }
        self.encode(bgr)
    }

    /// Offer a raw BGR24 frame, publishing it to the UI if it is a sampled one.
    pub fn maybe_publish(&mut self, bgr: &[u8]) {
        if let Some(jpeg) = self.sample(bgr) {
            publish(jpeg);
        }
    }

    fn encode(&mut self, bgr: &[u8]) -> Option<Vec<u8>> {
        if bgr.len() < self.width * self.height * 3 {
            return None;
        }
        let f = self.factor;
        let ow = self.width / f;
        let oh = self.height / f;
        if ow == 0 || oh == 0 {
            return None;
        }

        self.scratch.clear();
        self.scratch.reserve(ow * oh * 3);
        let area = (f * f) as u32;
        for oy in 0..oh {
            for ox in 0..ow {
                let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
                for dy in 0..f {
                    let row = (oy * f + dy) * self.width * 3;
                    for dx in 0..f {
                        let p = row + (ox * f + dx) * 3;
                        // Source is BGR; the encoder wants RGB.
                        b += bgr[p] as u32;
                        g += bgr[p + 1] as u32;
                        r += bgr[p + 2] as u32;
                    }
                }
                self.scratch.push((r / area) as u8);
                self.scratch.push((g / area) as u8);
                self.scratch.push((b / area) as u8);
            }
        }

        let img = image::ImageBuffer::<image::Rgb<u8>, &[u8]>::from_raw(
            ow as u32,
            oh as u32,
            self.scratch.as_slice(),
        )?;
        let mut out = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 70)
            .encode_image(&img)
            .ok()?;
        Some(out.into_inner())
    }
}

/// Hand a finished preview JPEG to the UI.
pub fn publish(jpeg: Vec<u8>) {
    crate::video_processor::publish_preview_frame(jpeg);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(payload);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn carves_whole_frames_from_one_chunk() {
        let mut r = JpegStreamReader::new();
        r.extend(&jpeg(b"abc"));
        assert_eq!(r.next_frame(), Some(jpeg(b"abc")));
        assert_eq!(r.next_frame(), None);
    }

    #[test]
    fn carves_several_frames_in_sequence() {
        let mut r = JpegStreamReader::new();
        let mut s = jpeg(b"one");
        s.extend(jpeg(b"two"));
        s.extend(jpeg(b"three"));
        r.extend(&s);
        assert_eq!(r.next_frame(), Some(jpeg(b"one")));
        assert_eq!(r.next_frame(), Some(jpeg(b"two")));
        assert_eq!(r.next_frame(), Some(jpeg(b"three")));
        assert_eq!(r.next_frame(), None);
    }

    #[test]
    fn tolerates_a_byte_at_a_time() {
        // The pathological feed for a scanner that restarts from zero.
        let mut r = JpegStreamReader::new();
        let src = jpeg(b"payload");
        let mut got = None;
        for b in &src {
            r.extend(&[*b]);
            if let Some(f) = r.next_frame() {
                got = Some(f);
            }
        }
        assert_eq!(got, Some(src));
    }

    #[test]
    fn tolerates_a_marker_split_across_a_read() {
        let mut r = JpegStreamReader::new();
        let src = jpeg(b"xy");
        let (a, b) = src.split_at(src.len() - 1); // EOI's second byte arrives alone
        r.extend(a);
        assert_eq!(r.next_frame(), None);
        r.extend(b);
        assert_eq!(r.next_frame(), Some(src));
    }

    #[test]
    fn skips_leading_garbage_before_the_first_soi() {
        let mut r = JpegStreamReader::new();
        r.extend(&[0x00, 0x11, 0x22]);
        r.extend(&jpeg(b"z"));
        assert_eq!(r.next_frame(), Some(jpeg(b"z")));
    }

    #[test]
    fn an_ff_that_is_not_a_marker_does_not_derail_the_scan() {
        let mut r = JpegStreamReader::new();
        // 0xFF 0x00 is a stuffed byte inside entropy-coded data, not a marker.
        r.extend(&jpeg(&[0xFF, 0x00, 0x41]));
        assert_eq!(r.next_frame(), Some(jpeg(&[0xFF, 0x00, 0x41])));
    }

    #[test]
    fn compaction_does_not_lose_a_pending_frame() {
        // Push enough traffic to cross the compaction threshold, then check the reader is
        // still framing correctly rather than having shifted its cursors out from under
        // itself.
        let mut r = JpegStreamReader::new();
        let big = jpeg(&vec![0x41; 40 * 1024]);
        for _ in 0..4 {
            r.extend(&big);
            assert_eq!(r.next_frame().as_deref(), Some(big.as_slice()));
        }
        assert_eq!(r.next_frame(), None);
    }

    #[test]
    fn preview_encoder_samples_at_the_requested_rate() {
        let geom = Geometry { width: 64, height: 64, fps: 30 };
        let enc = PreviewEncoder::new(&geom, PreviewSpec { width: 16, fps: 10, quality: 7 });
        assert_eq!(enc.every, 3, "30 fps camera, 10 fps preview");
        assert_eq!(enc.factor, 4, "64 -> 16");
    }

    #[test]
    fn preview_encoder_produces_a_decodable_jpeg() {
        let geom = Geometry { width: 32, height: 32, fps: 30 };
        let mut enc = PreviewEncoder::new(&geom, PreviewSpec { width: 8, fps: 30, quality: 7 });
        // Solid red in BGR byte order.
        let frame: Vec<u8> = std::iter::repeat([0u8, 0, 255]).take(32 * 32).flatten().collect();
        let jpeg = enc.encode(&frame).expect("encode");
        let img = image::load_from_memory(&jpeg).expect("decode").to_rgb8();
        assert_eq!(img.dimensions(), (8, 8));
        let px = img.get_pixel(4, 4);
        assert!(px[0] > 200 && px[1] < 60 && px[2] < 60, "BGR->RGB swap wrong: {px:?}");
    }

    #[test]
    fn preview_encoder_rejects_a_short_frame() {
        let geom = Geometry { width: 32, height: 32, fps: 30 };
        let mut enc = PreviewEncoder::new(&geom, PreviewSpec::default());
        assert!(enc.encode(&[0u8; 10]).is_none());
    }
}
