//! Energy-based voice activity detection on 16 kHz mono audio.
//!
//! Used to trim silence before local Whisper inference and to skip it entirely when nothing was
//! said: Whisper invents text ("Thank you.") when fed silence. Deepgram does its own endpointing,
//! so this module is not on the streaming path. The threshold adapts to the recording's own noise
//! floor, clamped so that both a silent room and a continuous monologue are classified correctly.

const RATE: usize = 16_000;
/// 30 ms analysis frames.
pub const FRAME: usize = RATE * 30 / 1000;
/// Never treat anything quieter than this RMS as speech.
const MIN_THRESHOLD: f32 = 0.006;
/// Never require more than this RMS, even when the "noise floor" is actually speech.
const MAX_THRESHOLD: f32 = 0.02;
const FLOOR_MULTIPLIER: f32 = 3.0;
/// At least 150 ms of voiced frames before we call it speech.
const MIN_SPEECH_FRAMES: usize = 5;
/// Keep 200 ms of context around detected speech so word onsets are not clipped.
const PAD_FRAMES: usize = 7;

/// Sample range `[start, end)` containing speech, or `None` if the audio is effectively silent.
pub fn speech_bounds(samples: &[f32]) -> Option<(usize, usize)> {
    let energies: Vec<f32> = samples.chunks(FRAME).map(rms).collect();
    if energies.is_empty() {
        return None;
    }

    let mut sorted = energies.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 10];
    let threshold = (floor * FLOOR_MULTIPLIER).clamp(MIN_THRESHOLD, MAX_THRESHOLD);

    let voiced = energies.iter().filter(|&&e| e > threshold).count();
    if voiced < MIN_SPEECH_FRAMES {
        return None;
    }
    let first = energies.iter().position(|&e| e > threshold)?;
    let last = energies.iter().rposition(|&e| e > threshold)?;
    let start = first.saturating_sub(PAD_FRAMES) * FRAME;
    let end = ((last + 1 + PAD_FRAMES) * FRAME).min(samples.len());
    Some((start, end))
}

/// Incremental speech detector for a live recording. Tracks the quietest frame seen so far as
/// the noise floor and reports once enough voiced frames have been heard.
pub struct SpeechWatch {
    pending: Vec<f32>,
    floor: f32,
    voiced: usize,
}

impl SpeechWatch {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            floor: f32::MAX,
            voiced: 0,
        }
    }

    pub fn feed(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
        let whole = self.pending.len() / FRAME * FRAME;
        for frame in self.pending[..whole].chunks(FRAME) {
            let energy = rms(frame);
            self.floor = self.floor.min(energy);
            let threshold = (self.floor * FLOOR_MULTIPLIER).clamp(MIN_THRESHOLD, MAX_THRESHOLD);
            if energy > threshold {
                self.voiced += 1;
            }
        }
        self.pending.drain(..whole);
    }

    pub fn heard(&self) -> bool {
        self.voiced >= MIN_SPEECH_FRAMES
    }
}

impl Default for SpeechWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// True when at least one second of audio is exactly zero: a muted input or a denied mic
/// permission (macOS delivers silence rather than an error in that case).
pub fn is_digital_silence(samples: &[f32]) -> bool {
    samples.len() >= RATE && samples.iter().all(|&s| s == 0.0)
}

fn rms(frame: &[f32]) -> f32 {
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-noise so tests do not need a rand dependency.
    fn noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    fn voice(n: usize) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * 0.07).sin() * 0.2).collect()
    }

    #[test]
    fn silence_and_room_noise_have_no_speech() {
        assert_eq!(speech_bounds(&vec![0.0; RATE * 2]), None);
        assert_eq!(speech_bounds(&noise(RATE * 2, 0.004, 1)), None);
        assert_eq!(speech_bounds(&[]), None);
    }

    #[test]
    fn trims_leading_and_trailing_silence() {
        let mut audio = noise(RATE, 0.002, 2);
        audio.extend(voice(RATE));
        audio.extend(noise(RATE, 0.002, 3));
        let (start, end) = speech_bounds(&audio).unwrap();
        let pad = PAD_FRAMES * FRAME;
        assert!(
            start >= RATE - pad - FRAME && start <= RATE,
            "start {start}"
        );
        assert!(
            end >= 2 * RATE && end <= 2 * RATE + pad + FRAME,
            "end {end}"
        );
    }

    #[test]
    fn continuous_speech_is_kept_whole() {
        let audio = voice(RATE * 3);
        assert_eq!(speech_bounds(&audio), Some((0, audio.len())));
    }

    #[test]
    fn a_click_is_not_speech() {
        let mut audio = vec![0.0; RATE];
        audio[8_000..8_100].fill(0.5);
        assert_eq!(speech_bounds(&audio), None);
    }

    #[test]
    fn watch_ignores_silence_and_hears_speech() {
        let mut w = SpeechWatch::new();
        w.feed(&noise(RATE * 3, 0.004, 5));
        assert!(!w.heard());
        let mut w = SpeechWatch::new();
        w.feed(&noise(RATE, 0.002, 6));
        w.feed(&voice(RATE / 2));
        assert!(w.heard());
    }

    #[test]
    fn digital_silence_detection() {
        assert!(is_digital_silence(&vec![0.0; RATE]));
        assert!(!is_digital_silence(&vec![0.0; RATE / 2]));
        assert!(!is_digital_silence(&noise(RATE, 0.001, 4)));
    }
}
