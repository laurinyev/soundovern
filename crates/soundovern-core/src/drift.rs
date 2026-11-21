//! Clock-drift compensation: nudges a resampling ratio so jitter-buffer depth tracks its target.

/// Simple PI-style controller. `ratio` > 1.0 means consume input faster than nominal
/// (buffer too full), < 1.0 slower (buffer too empty).
pub struct DriftController {
    ratio: f64,
    integral: f64,
    max_dev: f64,
    smoothed_err: f64,
}

impl DriftController {
    pub fn new(max_dev: f64) -> Self {
        Self { ratio: 1.0, integral: 0.0, max_dev, smoothed_err: 0.0 }
    }

    /// `depth_frames`/`target_frames` in codec frames (fractional ok). Call ~once per frame.
    pub fn update(&mut self, depth_frames: f64, target_frames: f64) -> f64 {
        let err = depth_frames - target_frames;
        self.smoothed_err += (err - self.smoothed_err) * 0.01;
        self.integral = (self.integral + self.smoothed_err * 1e-6).clamp(-self.max_dev, self.max_dev);
        let dev = (self.smoothed_err * 1.5e-3 + self.integral).clamp(-self.max_dev, self.max_dev);
        self.ratio = 1.0 + dev;
        self.ratio
    }

    pub fn ratio(&self) -> f64 {
        self.ratio
    }
}

/// Linear-interpolating stereo-agnostic resampler for tiny ratio corrections.
pub struct LinearResampler {
    channels: usize,
    pos: f64,
    prev: Vec<f32>,
}

impl LinearResampler {
    pub fn new(channels: usize) -> Self {
        Self { channels, pos: 0.0, prev: vec![0.0; channels] }
    }

    /// Resample `input` (interleaved) by `ratio` (input samples consumed per output sample),
    /// appending to `out`. State carries across calls.
    pub fn process(&mut self, input: &[f32], ratio: f64, out: &mut Vec<f32>) {
        let ch = self.channels;
        let n = input.len() / ch;
        // virtual stream: index -1 is prev frame, 0.. are input frames
        while self.pos < n as f64 - 1.0 + 1e-9 && (self.pos + 1.0) as usize <= n {
            let i = self.pos.floor() as isize; // frame index, -1 allowed
            let frac = (self.pos - i as f64) as f32;
            for c in 0..ch {
                let a = if i < 0 { self.prev[c] } else { input[i as usize * ch + c] };
                let bi = (i + 1) as usize;
                if bi >= n {
                    return self.finish(input, n);
                }
                let b = input[bi * ch + c];
                out.push(a + (b - a) * frac);
            }
            self.pos += ratio;
        }
        self.finish(input, n);
    }

    fn finish(&mut self, input: &[f32], n: usize) {
        if n > 0 {
            self.prev.copy_from_slice(&input[(n - 1) * self.channels..n * self.channels]);
            self.pos -= n as f64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_direction() {
        let mut c = DriftController::new(0.005);
        for _ in 0..1000 {
            c.update(6.0, 3.0);
        }
        assert!(c.ratio() > 1.0 && c.ratio() <= 1.005);
        let mut c = DriftController::new(0.005);
        for _ in 0..1000 {
            c.update(1.0, 3.0);
        }
        assert!(c.ratio() < 1.0 && c.ratio() >= 0.995);
    }

    #[test]
    fn resampler_unity_preserves_signal() {
        let mut r = LinearResampler::new(1);
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut out = Vec::new();
        r.process(&input, 1.0, &mut out);
        r.process(&input, 1.0, &mut out);
        assert!(out.len() >= 198 && out.len() <= 200, "{}", out.len());
    }

    #[test]
    fn resampler_ratio_changes_length() {
        let mut r = LinearResampler::new(2);
        let input = vec![0.5f32; 960 * 2];
        let mut out = Vec::new();
        for _ in 0..10 {
            r.process(&input, 1.01, &mut out);
        }
        let frames = out.len() / 2;
        // consuming 1% faster -> ~1% fewer output frames
        assert!((frames as f64 - 9600.0 / 1.01).abs() < 3.0, "{frames}");
    }
}
