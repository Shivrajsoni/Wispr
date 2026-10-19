//! Microphone capture.
//!
//! The stream is built once (stopped) and then only started and stopped per dictation: starting a
//! pre-built stream delivers audio in ~40 ms versus ~200 ms for opening the device from scratch,
//! which would clip the first word. While stopped, CoreAudio is not capturing.
//!
//! The CoreAudio callback converts samples to `f32` and pushes them into a lock-free SPSC ring
//! buffer. It never blocks, locks, or allocates; if the control thread falls behind, samples are
//! dropped and counted rather than stalling the audio thread. Device errors (for example the mic
//! being unplugged) set a flag that the control thread polls.

pub mod resample;
pub mod wav;

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Seconds of device audio the ring can hold before the callback starts dropping samples.
const RING_SECONDS: usize = 2;

pub struct Microphone {
    stream: cpal::Stream,
    running: bool,
    consumer: HeapCons<f32>,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    failed: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
}

impl Microphone {
    /// Prepares the named input device (or the system default) without starting capture.
    pub fn open(device_name: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(wanted) => host
                .input_devices()?
                .find(|d| d.description().is_ok_and(|desc| desc.name() == wanted))
                .ok_or_else(|| anyhow!("input device {wanted:?} not found (see --list-devices)"))?,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no microphone found"))?,
        };
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".into());

        let supported = device
            .default_input_config()
            .context("querying the microphone format")?;
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let ring = HeapRb::<f32>::new(sample_rate as usize * usize::from(channels) * RING_SECONDS);
        let (producer, consumer) = ring.split();
        let failed = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicUsize::new(0));

        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, config, producer, &failed, &dropped),
            cpal::SampleFormat::I16 => build::<i16>(&device, config, producer, &failed, &dropped),
            cpal::SampleFormat::I32 => build::<i32>(&device, config, producer, &failed, &dropped),
            cpal::SampleFormat::U16 => build::<u16>(&device, config, producer, &failed, &dropped),
            other => Err(anyhow!("unsupported microphone sample format {other:?}")),
        }?;

        Ok(Self {
            stream,
            running: false,
            consumer,
            name,
            sample_rate,
            channels,
            failed,
            dropped,
        })
    }

    /// Starts capturing, discarding anything left over from a previous run.
    pub fn start(&mut self) -> Result<()> {
        self.consumer.clear();
        if !self.running {
            self.stream.play().context("starting the microphone")?;
            self.running = true;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.running {
            let _ = self.stream.pause();
            self.running = false;
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether this is still the system default input (it changes when e.g. a headset is plugged in).
    pub fn is_system_default(&self) -> bool {
        cpal::default_host()
            .default_input_device()
            .and_then(|d| d.description().ok())
            .is_some_and(|d| d.name() == self.name)
    }

    /// Moves everything captured so far into `out` (interleaved, device rate).
    pub fn drain(&mut self, out: &mut Vec<f32>) {
        out.extend(self.consumer.pop_iter());
    }

    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Samples dropped because the ring was full; reset on read.
    pub fn take_dropped(&self) -> usize {
        self.dropped.swap(0, Ordering::Relaxed)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut producer: HeapProd<f32>,
    failed: &Arc<AtomicBool>,
    dropped: &Arc<AtomicUsize>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let dropped = Arc::clone(dropped);
    let failed = Arc::clone(failed);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mut lost = 0;
            for &s in data {
                if producer
                    .try_push(cpal::Sample::to_sample::<f32>(s))
                    .is_err()
                {
                    lost += 1;
                }
            }
            if lost > 0 {
                dropped.fetch_add(lost, Ordering::Relaxed);
            }
        },
        move |_err| failed.store(true, Ordering::Relaxed),
        None,
    )?;
    Ok(stream)
}

pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    pub format: String,
}

pub fn list_input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.description().ok())
        .map(|d| d.name().to_string());
    let mut out = Vec::new();
    for device in host.input_devices()? {
        let Ok(desc) = device.description() else {
            continue;
        };
        let format = device
            .default_input_config()
            .map(|c| {
                format!(
                    "{} ch @ {} Hz, {:?}",
                    c.channels(),
                    c.sample_rate(),
                    c.sample_format()
                )
            })
            .unwrap_or_else(|e| format!("unavailable ({e})"));
        out.push(DeviceInfo {
            is_default: default_name.as_deref() == Some(desc.name()),
            name: desc.name().to_string(),
            format,
        });
    }
    Ok(out)
}

/// Converts 16 kHz `f32` samples to little-endian signed 16-bit PCM.
pub fn f32_to_pcm16(samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * 2);
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_conversion_clamps_and_is_little_endian() {
        let mut out = Vec::new();
        f32_to_pcm16(&[0.0, 1.0, -1.0, 2.0], &mut out);
        assert_eq!(out, [0, 0, 0xff, 0x7f, 0x01, 0x80, 0xff, 0x7f]);
    }
}
