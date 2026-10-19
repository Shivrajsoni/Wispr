//! Minimal WAV reader (PCM 16-bit and IEEE float 32-bit) for `--transcribe` and `--benchmark`.
//! The daemon itself never reads or writes audio files.

use anyhow::{Result, bail};

pub struct Wav {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved samples in [-1, 1].
    pub samples: Vec<f32>,
}

pub fn parse(bytes: &[u8]) -> Result<Wav> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(len).min(bytes.len());
        let body = &bytes[body_start..body_end];
        match id {
            b"fmt " if body.len() >= 16 => {
                let u16_at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
                let mut format = u16_at(0);
                if format == 0xFFFE && body.len() >= 26 {
                    format = u16_at(24); // WAVE_FORMAT_EXTENSIBLE: sub-format GUID starts with the tag
                }
                let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                fmt = Some((format, u16_at(2), rate, u16_at(14)));
            }
            b"data" => {
                let Some((format, channels, sample_rate, bits)) = fmt else {
                    bail!("data chunk before fmt chunk");
                };
                let samples = match (format, bits) {
                    (1, 16) => body
                        .chunks_exact(2)
                        .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
                        .collect(),
                    (3, 32) => body
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect(),
                    _ => bail!(
                        "unsupported WAV encoding (format {format}, {bits} bits); use 16-bit PCM"
                    ),
                };
                if channels == 0 || sample_rate == 0 {
                    bail!("invalid WAV header");
                }
                return Ok(Wav {
                    sample_rate,
                    channels,
                    samples,
                });
            }
            _ => {}
        }
        pos = body_start + len + (len & 1);
    }
    bail!("WAV file has no data chunk")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav16(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"LIST\x03\x00\x00\x00abc\x00"); // odd-length chunk with pad byte
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    #[test]
    fn parses_pcm16_and_skips_unknown_chunks() {
        let w = parse(&wav16(16_000, 1, &[0, 16384, -32768])).unwrap();
        assert_eq!((w.sample_rate, w.channels), (16_000, 1));
        assert_eq!(w.samples, vec![0.0, 0.5, -1.0]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"hello world, not audio").is_err());
    }
}
