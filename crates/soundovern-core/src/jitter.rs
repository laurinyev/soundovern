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
    // Delay estimate: x = arrival - seq*frame; delay = x - running-min baseline.
    epoch: Option<(Instant, u32)>,
    base_ms: f32,
    jitter_ms: f32, // smoothed mean delay (for display)
    peak_ms: f32,   // slowly-decaying peak delay (drives the target)
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
            target: min_target.max(3),
            min_target,
            max_target: (max_ms / f).max(min_target as u32 + 1) as usize,
            max_depth: (max_ms / f) as usize * 2 + 4,
            epoch: None,
            base_ms: 0.0,
            jitter_ms: 0.0,
            peak_ms: 0.0,
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
        self.epoch = None;
        self.peak_ms = 0.0;
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
        let (t0, s0) = *self.epoch.get_or_insert((now, seq));
        let arr = now.duration_since(t0).as_secs_f32() * 1000.0;
        let seq_ms = seq.wrapping_sub(s0) as i32 as f32 * self.frame_ms;
        let x = arr - seq_ms;
        if self.stats.received == 0 && self.base_ms == 0.0 {
            self.base_ms = x;
        }
        // Baseline = lowest transit seen, creeping up 0.2 ms/s so sender/receiver clock drift
        // can't pin it forever.
        self.base_ms = (self.base_ms + 0.002).min(x);
        let d = (x - self.base_ms).max(0.0);
        self.jitter_ms += (d - self.jitter_ms) / 16.0;
        self.peak_ms = (self.peak_ms * 0.999).max(d);
    }

    /// Re-evaluate the target depth; grows fast, shrinks slowly. Call once per popped frame.
    fn adapt(&mut self, underrun: bool) {
        let want = ((self.peak_ms * 1.15 / self.frame_ms).ceil() as usize + 1).clamp(self.min_target, self.max_target);
        if underrun {
            self.target = (self.target + 2).max(want).min(self.max_target);
            self.calm_frames = 0;
        } else if want > self.target {
            self.target = want;
            self.calm_frames = 0;
        } else if want < self.target {
            self.calm_frames += 1;
            if self.calm_frames > (10_000.0 / self.frame_ms) as u32 {
                self.target -= 1; // shrink one frame after ~10 s without need
                self.calm_frames = 0;
            }
        } else {
            self.calm_frames = 0;
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
        self.adapt(false);
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
        feed(&mut jb, &[0, 1, 2, 2]);
        assert_eq!(jb.stats.duplicate, 1);
        jb.pop();
        jb.pop();
        feed(&mut jb, &[0]);
        assert_eq!(jb.stats.late, 1);
    }

    #[test]
    fn underrun_reprimes() {
        let mut jb = JitterBuffer::new(10, 20, 100);
        feed(&mut jb, &[0, 1, 2]);
        jb.pop();
        jb.pop();
        jb.pop();
        assert_eq!(jb.pop(), Pop::Empty);
        assert_eq!(jb.stats.underruns, 1);
        feed(&mut jb, &[3]);
        assert_eq!(jb.pop(), Pop::Empty); // re-priming
    }

    #[test]
    fn wraparound() {
        let mut jb = JitterBuffer::new(10, 20, 100);
        feed(&mut jb, &[u32::MAX, 0, 1]);
        assert_eq!(jb.pop(), Pop::Data(vec![u32::MAX as u8]));
        assert_eq!(jb.pop(), Pop::Data(vec![0]));
    }

    /// Virtual-time simulation: 10 ms packets; every 3 s the link stalls for 45 ms and everything
    /// queued is released as one burst (head-of-line blocking, like Wi-Fi power-save/roaming).
    #[test]
    fn periodic_stalls_stop_causing_underruns() {
        let mut jb = JitterBuffer::new(10, 20, 150);
        let t0 = Instant::now();
        let mut pending: Vec<(u64, u32)> = Vec::new(); // (arrival_ms, seq)
        let mut seq = 0u32;
        let mut late_underruns = 0;
        for t in 0..60_000u64 {
            if t % 10 == 0 {
                let phase = t % 3000;
                let arrive = if phase < 45 { t - phase + 45 + 2 } else { t + 2 };
                pending.push((arrive, seq));
                seq += 1;
            }
            pending.sort();
            while pending.first().is_some_and(|p| p.0 <= t) {
                let (at, s) = pending.remove(0);
                jb.push(s, vec![1], t0 + Duration::from_millis(at));
            }
            if t % 10 == 5 {
                let before = jb.stats.underruns;
                jb.pop();
                if t > 20_000 && jb.stats.underruns > before {
                    late_underruns += 1;
                }
            }
        }
        assert_eq!(late_underruns, 0, "still underrunning after adaptation (target {})", jb.target());
        assert!(jb.target() <= 8, "target ballooned: {}", jb.target());
    }

    /// Closed loop: jitter buffer + drift controller, 0.2 % faster device clock, device pulling
    /// 5 ms chunks (a frame is popped whenever a full frame of input has been consumed), plus a
    /// 40 ms link stall every 5 s. After warm-up the buffer must not keep underrunning.
    #[test]
    fn closed_loop_with_drift_and_stalls_is_stable() {
        use crate::drift::DriftController;
        let mut jb = JitterBuffer::new(10, 20, 150);
        let mut dc = DriftController::new(0.005);
        let t0 = Instant::now();
        let mut pending: Vec<(u64, u32)> = Vec::new();
        let (mut seq, mut acc) = (0u32, 0.0f64);
        let mut late_underruns = 0;
        for t in 0..120_000u64 {
            if t % 10 == 0 {
                let phase = t % 5000;
                let arrive = if phase < 40 { t - phase + 40 + 2 } else { t + 2 };
                pending.push((arrive, seq));
                seq += 1;
            }
            pending.sort();
            while pending.first().is_some_and(|p| p.0 <= t) {
                let (at, s) = pending.remove(0);
                jb.push(s, vec![1], t0 + Duration::from_millis(at));
            }
            if t % 5 == 4 {
                // 5 ms device chunk consumes 0.5 frames of input, scaled by the drift ratio
                acc += 0.5 * 1.002 * dc.ratio();
                while acc >= 1.0 {
                    acc -= 1.0;
                    let before = jb.stats.underruns;
                    let p = jb.pop();
                    if t > 40_000 && jb.stats.underruns > before {
                        late_underruns += 1;
                    }
                    if p != Pop::Empty {
                        dc.update(jb.depth() as f64, jb.target() as f64);
                    }
                }
            }
        }
        assert!(late_underruns <= 1, "{late_underruns} underruns after warm-up (target {}, total {})", jb.target(), jb.stats.underruns);
        assert!(jb.target() <= 10, "target ballooned: {}", jb.target());
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
