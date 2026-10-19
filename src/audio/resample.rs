//! Streaming conversion from the device format to 16 kHz mono `f32`.
//!
//! Stages: downmix -> 80 Hz high-pass (rumble, hum, breath pops) -> anti-alias FIR low-pass ->
//! fractional-step linear interpolation. All state (partial frames, filter history, resampling
//! phase) carries across calls, so feeding audio in arbitrary chunk sizes produces exactly the
//! same output as feeding it all at once. No allocation happens beyond growing the caller's buffer.

use std::f32::consts::PI;

pub const TARGET_RATE: u32 = 16_000;

/// FIR length. 63 taps at 48 kHz gives ~60 dB of rejection above 8 kHz at trivial cost.
const FIR_TAPS: usize = 63;
const PASSBAND_HZ: f32 = 7_200.0;

pub struct Resampler {
    channels: usize,
    frame: Vec<f32>,
    hpf: Biquad,
    lpf: Option<Fir>,
    step: f64,
    /// Position of the next output sample relative to the current input sample, in input samples.
    phase: f64,
    prev: f32,
}

impl Resampler {
    pub fn new(src_rate: u32, channels: u16) -> Self {
        let channels = usize::from(channels.max(1));
        let src = src_rate.max(1) as f32;
        Self {
            channels,
            frame: Vec::with_capacity(channels),
            hpf: Biquad::high_pass(80.0, src),
            lpf: (src_rate > TARGET_RATE).then(|| Fir::low_pass(PASSBAND_HZ / src)),
            step: f64::from(src_rate.max(1)) / f64::from(TARGET_RATE),
            phase: 0.0,
            prev: 0.0,
        }
    }

    /// Converts interleaved device samples and appends 16 kHz mono samples to `out`.
    pub fn process(&mut self, interleaved: &[f32], out: &mut Vec<f32>) {
        for &s in interleaved {
            self.frame.push(s);
            if self.frame.len() < self.channels {
                continue;
            }
            let mono = self.frame.iter().sum::<f32>() / self.channels as f32;
            self.frame.clear();

            let x = self.hpf.process(mono);
            let cur = match self.lpf.as_mut() {
                Some(f) => f.process(x),
                None => x,
            };
            while self.phase <= 0.0 {
                let t = (1.0 + self.phase) as f32;
                out.push(self.prev + t * (cur - self.prev));
                self.phase += self.step;
            }
            self.phase -= 1.0;
            self.prev = cur;
        }
    }
}

/// Second-order Butterworth high-pass (RBJ cookbook).
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn high_pass(cutoff: f32, sample_rate: f32) -> Self {
        let w = 2.0 * PI * cutoff / sample_rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cos) / 2.0) / a0,
            b1: (-(1.0 + cos)) / a0,
            b2: ((1.0 + cos) / 2.0) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Blackman-windowed sinc low-pass with a circular history buffer.
struct Fir {
    taps: [f32; FIR_TAPS],
    hist: [f32; FIR_TAPS],
    pos: usize,
}

impl Fir {
    /// `cutoff` is normalized to the input sample rate (cycles per sample, < 0.5).
    fn low_pass(cutoff: f32) -> Self {
        let mut taps = [0.0f32; FIR_TAPS];
        let m = (FIR_TAPS - 1) as f32;
        for (n, tap) in taps.iter_mut().enumerate() {
            let k = n as f32 - m / 2.0;
            let sinc = if k == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * k).sin() / (PI * k)
            };
            let w = 0.42 - 0.5 * (2.0 * PI * n as f32 / m).cos()
                + 0.08 * (4.0 * PI * n as f32 / m).cos();
            *tap = sinc * w;
        }
        let sum: f32 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Self {
            taps,
            hist: [0.0; FIR_TAPS],
            pos: 0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.pos = (self.pos + 1) % FIR_TAPS;
        self.hist[self.pos] = x;
        let (newer, older) = self.hist.split_at(self.pos + 1);
        newer
            .iter()
            .rev()
            .chain(older.iter().rev())
            .zip(self.taps.iter())
            .map(|(s, t)| s * t)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, rate: u32, secs: f32, channels: usize) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (2.0 * PI * freq * i as f32 / rate as f32).sin() * 0.5;
                std::iter::repeat_n(v, channels)
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Amplitude of `freq` in `x` via a single-bin DFT.
    fn magnitude(x: &[f32], freq: f32, rate: u32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * f64::from(freq) * i as f64 / f64::from(rate);
            re += f64::from(v) * ph.cos();
            im += f64::from(v) * ph.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    fn run(rate: u32, ch: u16, input: &[f32]) -> Vec<f32> {
        let mut r = Resampler::new(rate, ch);
        let mut out = Vec::new();
        r.process(input, &mut out);
        out
    }

    #[test]
    fn output_length_matches_ratio() {
        for rate in [48_000, 44_100, 16_000, 8_000] {
            let out = run(rate, 1, &vec![0.0; rate as usize]);
            assert!(
                (out.len() as i64 - 16_000).abs() <= 1,
                "{rate}: {}",
                out.len()
            );
        }
    }

    #[test]
    fn chunked_processing_is_identical_to_one_shot() {
        let input = tone(440.0, 48_000, 0.5, 2);
        let whole = run(48_000, 2, &input);

        let mut r = Resampler::new(48_000, 2);
        let mut chunked = Vec::new();
        // Odd chunk sizes split stereo frames and FIR windows at awkward boundaries.
        for chunk in input.chunks(7) {
            r.process(chunk, &mut chunked);
        }
        assert_eq!(whole, chunked);
    }

    #[test]
    fn speech_band_tone_survives() {
        let out = run(48_000, 1, &tone(1_000.0, 48_000, 1.0, 1));
        let settled = &out[1_000..];
        let m = magnitude(settled, 1_000.0, TARGET_RATE);
        assert!((m - 0.5).abs() < 0.03, "1 kHz magnitude {m}");
    }

    #[test]
    fn content_above_nyquist_is_not_aliased() {
        // 10 kHz at 48 kHz would fold to 6 kHz after naive decimation.
        let out = run(48_000, 1, &tone(10_000.0, 48_000, 1.0, 1));
        let settled = &out[1_000..];
        assert!(rms(settled) < 0.01, "aliased energy {}", rms(settled));
    }

    #[test]
    fn stereo_is_downmixed() {
        let left = tone(1_000.0, 48_000, 0.5, 1);
        let interleaved: Vec<f32> = left.iter().flat_map(|&l| [l, -l]).collect();
        let out = run(48_000, 2, &interleaved);
        assert!(rms(&out) < 1e-4);
    }

    #[test]
    fn rumble_is_removed() {
        let out = run(16_000, 1, &tone(30.0, 16_000, 2.0, 1));
        assert!(
            rms(&out[8_000..]) < 0.08,
            "30 Hz rms {}",
            rms(&out[8_000..])
        );
    }
}
