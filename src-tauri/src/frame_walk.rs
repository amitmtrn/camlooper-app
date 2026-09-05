//! Random-walk frame ordering ("natural motion").
//!
//! A straight loop plays `1 2 3 … N, 1 2 3 …`, which has two tells: a hard cut at the seam
//! on every pass, and a period exactly equal to the clip length, so a viewer watching for
//! half a minute sees the same motion repeat on a metronome.
//!
//! This walks the frame index instead — `1 2 3 2 1 2 1 2 3 4 3 …`. Two properties make it
//! read as live footage rather than as a shuffle:
//!
//! * **Adjacency.** Consecutive output frames are always adjacent in the source, so motion
//!   stays continuous. Picking frames uniformly at random would jump and look broken.
//! * **Run length.** Direction flips are gated by [`MIN_RUN`], so the walk commits to a
//!   direction for a beat before turning. Ungated flips can land back to back, and a
//!   one-frame reversal reads as a glitch rather than as movement.
//!
//! Playing idle webcam footage backwards is visually plausible — this is the same reason
//! "boomerang" clips work — so reversing direction costs nothing in realism while removing
//! the loop seam entirely.

/// Frames that must elapse after a direction change before another random one may occur.
/// At 30 fps this is a third of a second.
pub const MIN_RUN: u32 = 10;

/// Per-frame probability of reversing direction, once [`MIN_RUN`] frames have elapsed.
/// With `MIN_RUN` this gives a mean run of `10 + 1/0.04 = 35` frames, about 1.2 s at 30 fps.
pub const REVERSE_PROB: f64 = 0.04;

/// Walks a frame index back and forth across a clip, reversing at random.
///
/// The generator is seeded and self-contained rather than drawn from a global RNG, so a
/// given seed always produces the same sequence and the walk's invariants can be asserted
/// deterministically in tests.
pub struct FrameWalk {
    len: usize,
    pos: usize,
    dir: i32,
    since_flip: u32,
    min_run: u32,
    reverse_prob: f64,
    rng: u64,
    started: bool,
}

impl FrameWalk {
    /// A walk over `len` frames using the tuned [`MIN_RUN`] / [`REVERSE_PROB`] constants.
    pub fn new(len: usize, seed: u64) -> Self {
        Self::with_params(len, seed, MIN_RUN, REVERSE_PROB)
    }

    pub fn with_params(len: usize, seed: u64, min_run: u32, reverse_prob: f64) -> Self {
        Self {
            len,
            pos: 0,
            dir: 1,
            since_flip: 0,
            min_run,
            reverse_prob,
            // xorshift treats zero as a fixed point and would emit nothing but zeros, which
            // would silently degrade the walk into a plain ping-pong.
            rng: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
            started: false,
        }
    }

    /// Extend the walk over a clip that is still being decoded.
    ///
    /// Playback starts as soon as a short prefix of the clip is buffered and the rest fills
    /// in behind it, so this only ever grows — shrinking would let `pos` fall outside the
    /// buffer. Safe to call every tick.
    ///
    /// The walk cannot outrun the decoder: `next_index` starts at 0 and moves at most one
    /// frame per call, so after *k* ticks the index is at most *k*, while the decoder runs
    /// without `-re` — i.e. faster than realtime on any machine that can also encode the
    /// store in realtime.
    pub fn grow(&mut self, len: usize) {
        if len > self.len {
            self.len = len;
        }
    }

    /// The next frame index to emit. The first call yields 0 so playback opens on the first
    /// frame of the clip; every call after that moves exactly one frame.
    pub fn next_index(&mut self) -> usize {
        if self.len <= 1 {
            return 0;
        }

        if !self.started {
            self.started = true;
            return self.pos;
        }

        self.since_flip = self.since_flip.saturating_add(1);

        if self.since_flip >= self.min_run && self.next_f64() < self.reverse_prob {
            self.dir = -self.dir;
            self.since_flip = 0;
        }

        // Reflect at both ends rather than wrapping or holding: `… 3 2 1 2 3 …`, never
        // `… 2 1 1 2 …`. Repeating the endpoint would show up as a visible hitch every
        // time the walk reaches the start or end of the clip.
        let mut next = self.pos as i64 + self.dir as i64;
        if next < 0 || next >= self.len as i64 {
            self.dir = -self.dir;
            self.since_flip = 0;
            next = self.pos as i64 + self.dir as i64;
        }

        self.pos = next as usize;
        self.pos
    }

    /// xorshift64*. Small, fast, and good enough to decide a coin flip once a frame.
    fn next_u64(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, 1)`, from the top 53 bits so the mantissa is filled exactly.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(walk: &mut FrameWalk, n: usize) -> Vec<usize> {
        (0..n).map(|_| walk.next_index()).collect()
    }

    // --- the two invariants that decide whether it looks real ---------------------------

    #[test]
    fn consecutive_frames_are_always_adjacent() {
        // The whole premise. If the walk ever jumps by more than one frame the footage
        // tears, which is exactly the artefact that makes a shuffled feed obviously fake.
        for seed in [1u64, 42, 7777, u64::MAX] {
            let mut walk = FrameWalk::new(300, seed);
            let seq = collect(&mut walk, 10_000);
            for pair in seq.windows(2) {
                let step = (pair[1] as i64 - pair[0] as i64).abs();
                assert_eq!(step, 1, "seed {seed}: jumped from {} to {}", pair[0], pair[1]);
            }
        }
    }

    #[test]
    fn random_flips_never_land_inside_the_minimum_run() {
        // Back-to-back reversals produce a one-frame stutter that reads as a glitch. Only
        // a reflection off the ends of the clip is allowed to cut a run short.
        let len = 300;
        let mut walk = FrameWalk::new(len, 12345);
        let seq = collect(&mut walk, 20_000);

        let mut run = 1u32;
        for window in seq.windows(3) {
            let before = window[1] as i64 - window[0] as i64;
            let after = window[2] as i64 - window[1] as i64;
            if before == after {
                run += 1;
                continue;
            }
            // Direction changed at window[1]. A reflection is legal at any run length.
            let at_boundary = window[1] == 0 || window[1] == len - 1;
            if !at_boundary {
                assert!(run >= MIN_RUN, "random flip after a run of only {run} frames");
            }
            run = 1;
        }
    }

    // --- boundaries ---------------------------------------------------------------------

    #[test]
    fn reflects_at_both_ends_without_repeating_the_endpoint() {
        // With reversal disabled the walk is a pure ping-pong, so the exact sequence can be
        // pinned down -- including that the turn at each end does not emit a frame twice.
        let mut walk = FrameWalk::with_params(5, 1, MIN_RUN, 0.0);
        assert_eq!(
            collect(&mut walk, 13),
            vec![0, 1, 2, 3, 4, 3, 2, 1, 0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn stays_in_bounds() {
        // An out-of-range index would panic on the frame lookup and kill the stream.
        for len in [2usize, 3, 17, 900] {
            let mut walk = FrameWalk::new(len, 99);
            for idx in collect(&mut walk, 5_000) {
                assert!(idx < len, "index {idx} out of range for a {len}-frame clip");
            }
        }
    }

    #[test]
    fn degenerate_clips_do_not_panic() {
        // A clip that decoded to nothing, or to a single frame, must hold that frame rather
        // than divide by zero or oscillate.
        let mut empty = FrameWalk::new(0, 5);
        assert_eq!(collect(&mut empty, 20), vec![0; 20]);

        let mut single = FrameWalk::new(1, 5);
        assert_eq!(collect(&mut single, 20), vec![0; 20]);
    }

    // --- it is actually a walk, not a loop ----------------------------------------------

    #[test]
    fn starts_on_the_first_frame_and_moves_forward() {
        // Playback should open on frame 0, not on frame 1 -- the first tick emits the
        // current position rather than stepping off it.
        let mut walk = FrameWalk::new(100, 3);
        assert_eq!(collect(&mut walk, 4), vec![0, 1, 2, 3]);
    }

    #[test]
    fn reverses_direction_away_from_the_clip_ends() {
        // If REVERSE_PROB were ever tuned to zero this degrades to a boomerang, which still
        // has a fixed period. Prove the randomness is doing something in mid-clip.
        let mut walk = FrameWalk::new(2_000, 2024);
        let seq = collect(&mut walk, 20_000);

        let interior_flips = seq
            .windows(3)
            .filter(|w| {
                let turned = (w[1] as i64 - w[0] as i64) != (w[2] as i64 - w[1] as i64);
                turned && w[1] != 0 && w[1] != 1_999
            })
            .count();

        assert!(interior_flips > 100, "only {interior_flips} interior reversals in 20k frames");
    }

    #[test]
    fn does_not_replay_the_straight_loop_order() {
        // The bug this feature exists to fix: emitting 0,1,2,…,N-1,0,1,2,… again.
        let len = 60;
        let mut walk = FrameWalk::new(len, 808);
        let seq = collect(&mut walk, len * 4);
        let straight: Vec<usize> = (0..len * 4).map(|i| i % len).collect();
        assert_ne!(seq, straight);
    }

    #[test]
    fn covers_the_clip_rather_than_hovering_in_one_spot() {
        // A walk that never wanders far would show the same second of footage forever.
        let len = 400;
        let mut walk = FrameWalk::new(len, 555);
        let seq = collect(&mut walk, 200_000);
        let lo = *seq.iter().min().unwrap();
        let hi = *seq.iter().max().unwrap();
        assert_eq!(lo, 0);
        assert_eq!(hi, len - 1, "walk never reached the end of the clip");
    }

    // --- growing while the clip is still decoding -----------------------------------------

    #[test]
    fn growing_never_breaks_adjacency() {
        // The invariant that matters most, now under a buffer that changes size mid-walk.
        let mut walk = FrameWalk::new(30, 4242);
        let mut seq = Vec::new();
        let mut len = 30;
        for i in 0..5_000 {
            if i % 7 == 0 && len < 900 {
                len += 3;
                walk.grow(len);
            }
            seq.push(walk.next_index());
        }
        for pair in seq.windows(2) {
            assert_eq!((pair[1] as i64 - pair[0] as i64).abs(), 1, "{pair:?}");
        }
        assert!(*seq.iter().max().unwrap() < len);
    }

    #[test]
    fn growing_is_monotonic() {
        // A shrink would let pos fall outside the buffer and panic on the frame lookup.
        let mut walk = FrameWalk::new(100, 1);
        walk.grow(50);
        assert_eq!(walk.len, 100, "grow must never shrink the walk");
        walk.grow(400);
        assert_eq!(walk.len, 400);
    }

    #[test]
    fn a_walk_that_starts_on_one_frame_recovers_once_more_arrive() {
        // Playback opens on a prefix that may be a single frame. Before grow() that was a
        // permanent fixed point, because len <= 1 short-circuits next_index.
        let mut walk = FrameWalk::new(1, 7);
        assert_eq!(collect(&mut walk, 3), vec![0, 0, 0]);
        walk.grow(200);
        let seq = collect(&mut walk, 50);
        assert!(seq.iter().any(|&i| i > 0), "walk never left frame 0 after growing");
        for pair in seq.windows(2) {
            assert_eq!((pair[1] as i64 - pair[0] as i64).abs(), 1, "{pair:?}");
        }
    }

    #[test]
    fn a_seed_reproduces_its_sequence() {
        // Tests above depend on this, and so does any future attempt to debug a walk that
        // looked wrong on screen.
        let a = collect(&mut FrameWalk::new(250, 31337), 1_000);
        let b = collect(&mut FrameWalk::new(250, 31337), 1_000);
        let c = collect(&mut FrameWalk::new(250, 31338), 1_000);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
