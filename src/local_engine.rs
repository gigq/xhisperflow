use crate::config::Config;
use anyhow::{Result, bail};
use std::path::Path;

// Only the selected model is instantiated. Both engines live in the same
// resident worker and use the same recorder, resampler, and paste path.
pub(crate) enum Engine {
    #[cfg(feature = "local-whisper")]
    Whisper(whisper_rs::WhisperState),
    #[cfg(feature = "local-parakeet")]
    Parakeet(Box<parakeet_rs::ParakeetTDT>),
}

impl Engine {
    pub fn load(config: &Config, path: &Path) -> Result<Self> {
        let _ = path;
        match config.local_engine.as_str() {
            #[cfg(feature = "local-whisper")]
            "whisper" => {
                use anyhow::{Context, ensure};
                use whisper_rs::{WhisperContext, WhisperContextParameters};
                ensure!(
                    config.local_language == "auto"
                        || whisper_rs::get_lang_id(&config.local_language).is_some(),
                    "Unknown local-language"
                );
                let mut params = WhisperContextParameters::default();
                params.use_gpu(true).gpu_device(config.local_gpu_device);
                let context = WhisperContext::new_with_params(
                    path.to_str().context("Invalid model path")?,
                    params,
                )?;
                // WhisperState holds an Arc to its context, keeping weights alive.
                Ok(Self::Whisper(context.create_state()?))
            }
            #[cfg(feature = "local-parakeet")]
            "parakeet" => {
                let execution = parakeet_rs::ExecutionConfig::default()
                    .with_intra_threads(config.local_threads as usize);
                let model = parakeet_rs::ParakeetTDT::from_pretrained(path, Some(execution))
                    .map_err(|e| anyhow::anyhow!("Parakeet model load failed: {e}"))?;
                Ok(Self::Parakeet(Box::new(model)))
            }
            engine => {
                bail!("Engine {engine} is not compiled in; enable local-{engine} when building")
            }
        }
    }

    pub fn transcribe(&mut self, config: &Config, samples: &[f32], prompt: &str) -> Result<String> {
        let _ = (config, samples, prompt);
        match self {
            #[cfg(feature = "local-whisper")]
            Self::Whisper(state) => {
                use whisper_rs::{FullParams, SamplingStrategy};
                let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                params.set_n_threads(config.local_threads);
                params.set_language(if config.local_language == "auto" {
                    None
                } else {
                    Some(&config.local_language)
                });
                params.set_translate(false);
                params.set_no_context(true);
                params.set_print_special(false);
                params.set_print_progress(false);
                params.set_print_realtime(false);
                params.set_print_timestamps(false);
                params.set_suppress_blank(true);
                params.set_suppress_nst(true);
                if !prompt.is_empty() {
                    params.set_initial_prompt(prompt);
                }
                let duration = samples.len() as f32 / 16000.0;
                if duration < 30.0 {
                    params.set_single_segment(true);
                    if duration <= 22.5 {
                        params
                            .set_audio_ctx((((duration * 50.0) as i32 + 128).max(384) + 7) / 8 * 8);
                    }
                }
                state.full(params, samples)?;
                let mut text = String::new();
                for segment in state.as_iter() {
                    text.push_str(segment.to_str()?);
                }
                Ok(text.trim().to_owned())
            }
            #[cfg(feature = "local-parakeet")]
            Self::Parakeet(model) => {
                use parakeet_rs::Transcriber;
                // TDT v3 detects its supported languages from the audio and
                // emits punctuation itself; it does not use Whisper prompts.
                let result = model
                    .transcribe_samples(samples.to_vec(), 16000, 1, None)
                    .map_err(|e| anyhow::anyhow!("Parakeet inference failed: {e}"))?;
                Ok(result.text.trim().to_owned())
            }
            #[cfg(not(any(feature = "local-whisper", feature = "local-parakeet")))]
            _ => bail!("No local speech engines compiled in"),
        }
    }
}
