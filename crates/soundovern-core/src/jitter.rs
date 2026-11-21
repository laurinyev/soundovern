//! Sequence-ordered jitter buffer operating on whole codec frames.
//!
//! The consumer (audio callback side) calls [`JitterBuffer::pop`] once per frame period.
//! Producers call [`JitterBuffer::push`] when datagrams arrive.

use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Debug, PartialEq, Eq)]
pub enum Pop {
    /// Next in-order frame. `fec_next` is set when the *following* frame is present,
    /// not needed here; kept simple.
    Data(Vec<u8>),
    /// Frame missing: conceal. If `next` is Some, it holds the following packet for FEC recovery.
    Lost { next: Option<Vec<u8>> },
    /// Not (re)primed yet or buffer ran dry: output silence.
    Empty,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct JitterStats {
    pub received: u32,
    pub lost: u32,
    pub late: u32,
    pub duplicate: u32,
    pub underruns: u32,
    pub overflows: u32,
    pub resyncs: u32,
}

pub struct JitterBuffer {
    frame_ms: f32,
    frames: BTreeMap<u32, Vec<u8>>,
    next_seq: Option<u32>,
    primed: bool,
    target: usize,
    min_target: usize,
    max_target: usize,
    max_depth: usize,
    // arrival jitter (RFC3550 style, ms)
    last_arrival: Option<Instant>,
    last_seq: u32,
    jitter_ms: f32,
    calm_frames: u32,
    pub stats: JitterStats,
}

fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

impl JitterBuffer {
    pub fn new(frame_ms: u8, min_ms: u32, max_ms: u32) -> Self {
        let f = frame_ms as u32;
        let min_target = (min_ms / f).max(1) as usize;
        Self {
            frame_ms: frame_ms as f32,
            frames: BTreeMap::new(),
            next_seq: None,
            primed: false,
            target: min_target.max(2),
            min_target,
            max_target: (max_ms / f).max(min_target as u32 + 1) as usize,
            max_depth: (max_ms / f) as usize * 2 + 4,
            last_arrival: None,
            last_seq: 0,
            jitter_ms: 0.0,
            calm_frames: 0,
            stats: JitterStats::default(),
        }
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }
    pub fn target(&self) -> usize {
        self.target
    }
    pub fn jitter_ms(&self) -> f32 {
        self.jitter_ms
    }
    pub fn buffered_ms(&self) -> f32 {
        self.frames.len() as f32 * self.frame_ms
    }

    /// Discard everything and re-prime (e.g. server DISCONT flag, reconnect).
    pub fn reset(&mut self) {
        self.frames.clear();
        self.next_seq = None;
        self.primed = false;
        self.last_arrival = None;
    }

    pub fn push(&mut self, seq: u32, payload: Vec<u8>, now: Instant) {
        self.update_jitter(seq, now);
        if let Some(next) = self.next_seq {
            if seq_lt(seq, next) {
                self.stats.late += 1;
                return;
            }
        } else {
            self.next_seq = Some(seq);
        }
        if self.frames.insert(seq, payload).is_some() {
            self.stats.duplicate += 1;
            return;
        }
        self.stats.received += 1;
        // Overflow: drop oldest frames until we're within bounds (latency bounded).
        while self.frames.len() > self.max_depth {
            let (&k, _) = self.frames.iter().next().unwrap();
            self.frames.remove(&k);
            self.next_seq = Some(k.wrapping_add(1));
            self.stats.overflows += 1;
        }
    }

    fn update_jitter(&mut self, seq: u32, now: Instant) {
        if let Some(prev) = self.last_arrival {
            let dseq = seq.wrapping_sub(self.last_seq) as i32;
            if dseq > 0 && dseq < 50 {
                let expected = self.frame_ms * dseq as f32;
                let actual = now.duration_since(prev).as_secs_f32() * 1000.0;
                let d = (actual - expected).abs();
                self.jitter_ms += (d - self.jitter_ms) / 16.0;
                self.last_arrival = Some(now);
                self.last_seq = seq;
            }
        } else {
            self.last_arrival = Some(now);
            self.last_seq = seq;
        }
    }

    /// Re-evaluate the target depth; grows fast, shrinks slowly. Call once per popped frame.
    fn adapt(&mut self, had_loss: bool) {
        let want = ((self.jitter_ms * 3.0 / self.frame_ms).ceil() as usize + 1).clamp(self.min_target, self.max_target);
        if had_loss && self.target < self.max_target {
            self.target = (self.target + 1).max(want).min(self.max_target);
            self.calm_frames = 0;
        } else if want > self.target {
            self.target = want;
            self.calm_frames = 0;
        } else if want < self.target {
            self.calm_frames += 1;
            if self.calm_frames > (2000.0 / self.frame_ms) as u32 {
                self.target -= 1; // shrink one frame after ~2 s of calm
                self.calm_frames = 0;
            }
        }
    }

    pub fn pop(&mut self) -> Pop {
        let Some(next) = self.next_seq else { return Pop::Empty };
        if !self.primed {
            if self.frames.len() >= self.target {
                self.primed = true;
            } else {
                return Pop::Empty;
            }
        }
        // Far over target (startup burst, stall recovery): trim once instead of waiting for drift
        // correction, which only moves ~0.5 % per second.
        if self.frames.len() > self.target + 6 {
            while self.frames.len() > self.target + 2 {
                let (&k, _) = self.frames.iter().next().unwrap();
                self.frames.remove(&k);
                self.next_seq = Some(k.wrapping_add(1));
            }
            self.stats.resyncs += 1;
            return self.pop();
        }
        let next = self.next_seq.unwrap_or(next);
        if let Some(p) = self.frames.remove(&next) {
            self.next_seq = Some(next.wrapping_add(1));
            self.adapt(false);
            return Pop::Data(p);
        }
        if self.frames.is_empty() {
            // Dry: go back to priming so we don't stutter at 1-frame depth.
            self.primed = false;
            self.stats.underruns += 1;
            self.adapt(true);
            return Pop::Empty;
        }
        // Gap: later frames exist, this one is lost (or reordered too late).
        self.stats.lost += 1;
        self.next_seq = Some(next.wrapping_add(1));
        self.adapt(true);
        let following = self.frames.get(&next.wrapping_add(1)).cloned();
        Pop::Lost { next: following }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn feed(jb: &mut JitterBuffer, seqs: &[u32]) {
        let t = Instant::now();
        for (i, s) in seqs.iter().enumerate() {
            jb.push(*s, vec![*s as u8], t + Duration::from_millis(10 * i as u64));
        }
    }

    #[test]
    fn primes_then_plays_in_order_despite_reorder() {
        let mut jb = JitterBuffer::new(10, 20, 100);
        assert_eq!(jb.pop(), Pop::Empty);
        feed(&mut jb, &[0, 2, 1, 3]);
        for s in 0..4u8 {
            assert_eq!(jb.pop(), Pop::Data(vec![s]));
        }
    }

    #[test]
    fn reports_loss_with_fec_candidate() {
        let mut jb = JitterBuffer::new(10, 20, 100);
        feed(&mut jb, &[0, 2, 3]);
        assert_eq!(jb.pop(), Pop::Data(vec![0]));
        assert_eq!(jb.pop(), Pop::Lost { next: Some(vec![2]) });
        assert_eq!(jb.pop(), Pop::Data(vec![2]));
        assert_eq!(jb.stats.lost, 1);
    }

    #[test]
    fn late_and_duplicate_dropped() {
        let mut jb = JitterBuffer::new(10, 20, 100);
        feed(&mut jb, &[0, 1, 1]);
        assert_eq!(jb.stats.duplicate, 1);
        jb.pop();
        jb.pop();
        feed(&mut jb, &[0]);
        assert_eq!(jb.stats.late, 1);
    }

    #[test]
    fn underrun_reprimes() {
        let mut jb = JitterBuffer::new(10, 20, 100);
        feed(&mut jb, &[0, 1]);
        jb.pop();
        jb.pop();
        assert_eq!(jb.pop(), Pop::Empty);
        assert_eq!(jb.stats.underruns, 1);
        feed(&mut jb, &[2]);
        assert_eq!(jb.pop(), Pop::Empty); // re-priming
    }

    #[test]
    fn wraparound() {
        let mut jb = JitterBuffer::new(10, 20, 100);
        feed(&mut jb, &[u32::MAX, 0, 1]);
        assert_eq!(jb.pop(), Pop::Data(vec![u32::MAX as u8]));
        assert_eq!(jb.pop(), Pop::Data(vec![0]));
    }

    #[test]
    fn trims_excess_backlog() {
        let mut jb = JitterBuffer::new(10, 20, 200);
        let seqs: Vec<u32> = (0..15).collect();
        feed(&mut jb, &seqs);
        let _ = jb.pop();
        assert!(jb.depth() <= jb.target() + 2, "depth {}", jb.depth());
        assert_eq!(jb.stats.resyncs, 1);
    }

    #[test]
    fn overflow_bounds_latency() {
        let mut jb = JitterBuffer::new(10, 20, 40);
        let seqs: Vec<u32> = (0..100).collect();
        feed(&mut jb, &seqs);
        assert!(jb.depth() <= 12);
        assert!(jb.stats.overflows > 0);
    }
}
