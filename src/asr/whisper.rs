//! Local transcription with whisper.cpp on the Metal GPU.
//!
//! The model is loaded once at startup and warmed up with a silent pass, which compiles the
//! Metal kernels, so the first real dictation pays no load cost. Audio is VAD-trimmed first,
//! and recordings with no speech never reach the model.

use crate::vad;
use anyhow::{Result, anyhow};
use std::path::Path;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

pub struct LocalWhisper {
    state: WhisperState,
    threads: i32,
    language: String,
}

impl LocalWhisper {
    pub fn load(model: &Path, threads: u8, language: &str) -> Result<Self> {
        if !model.is_file() {
            return Err(anyhow!(
                "Whisper model not found at {} (run `wispr --download-models`)",
                model.display()
            ));
        }
        silence_whisper_logs();
        let params = WhisperContextParameters {
            use_gpu: true,
            flash_attn: true,
            ..Default::default()
        };
        let path = model
            .to_str()
            .ok_or_else(|| anyhow!("model path is not UTF-8"))?;
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| anyhow!("loading {}: {e:?}", model.display()))?;
        let state = ctx
            .create_state()
            .map_err(|e| anyhow!("creating Whisper state: {e:?}"))?;
        let mut whisper = Self {
            state,
            threads: i32::from(threads),
            language: language.to_string(),
        };
        whisper.infer(&vec![0.0; 16_000], "")?;
        Ok(whisper)
    }

    /// Transcribes 16 kHz mono audio. Returns an empty string when no speech is detected.
    pub fn transcribe(&mut self, samples: &[f32], prompt: &str) -> Result<String> {
        let Some((start, end)) = vad::speech_bounds(samples) else {
            return Ok(String::new());
        };
        let mut speech = samples[start..end].to_vec();
        // whisper.cpp needs at least one second of input to decode reliably.
        if speech.len() < 16_000 {
            speech.resize(16_000, 0.0);
        }
        let text = self.infer(&speech, prompt)?;
        Ok(clean(&text))
    }

    fn infer(&mut self, samples: &[f32], prompt: &str) -> Result<String> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(Some(&self.language));
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_non_speech_tokens(true);
        params.set_no_speech_thold(0.6);
        params.set_temperature(0.0);
        if !prompt.is_empty() {
            params.set_initial_prompt(prompt);
        }

        self.state
            .full(params, samples)
            .map_err(|e| anyhow!("Whisper inference failed: {e:?}"))?;
        let segments = self
            .state
            .full_n_segments()
            .map_err(|e| anyhow!("reading Whisper output: {e:?}"))?;
        let mut text = String::new();
        for i in 0..segments {
            if let Ok(s) = self.state.full_get_segment_text(i) {
                text.push_str(&s);
            }
        }
        Ok(text)
    }
}

fn silence_whisper_logs() {
    unsafe extern "C" fn discard(_: u32, _: *const std::ffi::c_char, _: *mut std::ffi::c_void) {}
    // SAFETY: the callback is a no-op that never touches its arguments, and a null user-data
    // pointer is permitted by whisper.cpp.
    unsafe { whisper_rs::set_log_callback(Some(discard), std::ptr::null_mut()) };
}

/// Removes non-speech annotations and decoder repetition loops.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    collapse_repetition(out.split_whitespace().collect::<Vec<_>>().join(" ").trim())
}

/// Whisper sometimes loops ("I'm going to go. I'm going to go. I'm going to go."). If the text
/// opens with a 3-6 word phrase repeated at least 3 times back to back, keep one copy.
fn collapse_repetition(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    for len in 3..=6 {
        if words.len() < len * 3 {
            break;
        }
        let pattern = &words[..len];
        let repeats = words.chunks(len).take_while(|c| *c == pattern).count();
        if repeats >= 3 {
            let mut rest = vec![pattern.join(" ")];
            rest.extend(words[repeats * len..].iter().map(|w| w.to_string()));
            return rest.join(" ");
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_annotations() {
        assert_eq!(clean(" [BLANK_AUDIO] "), "");
        assert_eq!(clean(" Hello (coughs) world."), "Hello world.");
    }

    #[test]
    fn collapses_decoder_loops() {
        assert_eq!(
            collapse_repetition("I am not sure I am not sure I am not sure about this"),
            "I am not sure about this"
        );
        let normal = "We should ship it on Monday and review it on Tuesday";
        assert_eq!(collapse_repetition(normal), normal);
    }
}
