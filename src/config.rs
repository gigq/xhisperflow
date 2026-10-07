use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputMethod {
    Type,
    Clipboard,
    Paste,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptionBackend {
    Auto,
    Groq,
    Voxtype,
}

impl TranscriptionBackend {
    // The native macOS app retains its existing Groq onboarding and backend.
    pub fn use_voxtype(self, linux: bool, installed: bool) -> bool {
        linux
            && match self {
                Self::Auto => installed,
                Self::Groq => false,
                Self::Voxtype => true,
            }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub transcription_backend: TranscriptionBackend,
    pub voxtype_post_processing_enabled: bool,
    pub voxtype_timeout_secs: u64,
    pub long_recording_threshold: f64,
    pub transcription_prompt: String,
    pub post_processing_enabled: bool,
    pub post_processing_model: String,
    pub post_processing_timeout_secs: f64,
    pub output_method: OutputMethod,
    pub paste_keys: String,
    pub clipboard_restore_delay_secs: f64,
    pub non_ascii_initial_delay_secs: f64,
    pub non_ascii_default_delay_secs: f64,
    pub hallucination_no_speech_threshold: f64,
    pub mac_hotkey: String,
    pub mac_cancel_hotkey: String,
    pub floating_waveform: bool,
    pub waveform_gradient_start: String,
    pub waveform_gradient_end: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            transcription_backend: TranscriptionBackend::Auto,
            voxtype_post_processing_enabled: false,
            voxtype_timeout_secs: 120,
            long_recording_threshold: 1000.0,
            transcription_prompt: String::new(),
            post_processing_enabled: true,
            post_processing_model: "openai/gpt-oss-20b".to_string(),
            post_processing_timeout_secs: 3.0,
            output_method: OutputMethod::Type,
            paste_keys: "ctrl+v".to_string(),
            clipboard_restore_delay_secs: 0.15,
            non_ascii_initial_delay_secs: 0.1,
            non_ascii_default_delay_secs: 0.025,
            hallucination_no_speech_threshold: 0.1,
            mac_hotkey: "alt+space".to_string(),
            mac_cancel_hotkey: "ctrl+shift".to_string(),
            floating_waveform: true,
            waveform_gradient_start: "#b58cff".to_string(),
            waveform_gradient_end: "#d7e6ff".to_string(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_file_path();
        let Ok(contents) = fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::from_contents(&contents)
    }

    fn from_contents(contents: &str) -> Self {
        let mut config = Self::default();
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((raw_key, raw_value)) = line.split_once(':') else {
                continue;
            };

            let key = raw_key.trim();
            let value = strip_inline_comment(raw_value.trim())
                .trim()
                .trim_matches('"')
                .to_string();

            match key {
                "transcription-backend" => match value.to_ascii_lowercase().as_str() {
                    "auto" => config.transcription_backend = TranscriptionBackend::Auto,
                    "groq" => config.transcription_backend = TranscriptionBackend::Groq,
                    "voxtype" => config.transcription_backend = TranscriptionBackend::Voxtype,
                    _ => {
                        eprintln!("Unknown transcription-backend {value:?}; using auto");
                        config.transcription_backend = TranscriptionBackend::Auto;
                    }
                },
                "voxtype-post-processing-enabled" => {
                    if let Some(parsed) = parse_bool(&value) {
                        config.voxtype_post_processing_enabled = parsed;
                    }
                }
                "voxtype-timeout" => {
                    if let Ok(parsed) = value.parse::<u64>()
                        && parsed > 0
                    {
                        config.voxtype_timeout_secs = parsed;
                    }
                }
                "long-recording-threshold" => {
                    if let Ok(parsed) = value.parse::<f64>() {
                        config.long_recording_threshold = parsed;
                    }
                }
                "transcription-prompt" => {
                    config.transcription_prompt = value;
                }
                "post-processing-enabled" => {
                    if let Some(parsed) = parse_bool(&value) {
                        config.post_processing_enabled = parsed;
                    }
                }
                "post-processing-model" if !value.is_empty() => {
                    config.post_processing_model = value;
                }
                "post-processing-timeout" => {
                    if let Ok(parsed) = value.parse::<f64>() {
                        config.post_processing_timeout_secs = parsed;
                    }
                }
                "output-method" => match value.to_ascii_lowercase().as_str() {
                    "type" => config.output_method = OutputMethod::Type,
                    "clipboard" => config.output_method = OutputMethod::Clipboard,
                    "paste" => config.output_method = OutputMethod::Paste,
                    _ => {}
                },
                "paste-keys" => config.paste_keys = value,
                "clipboard-restore-delay" => {
                    if let Ok(parsed) = value.parse::<f64>() {
                        config.clipboard_restore_delay_secs = parsed;
                    }
                }
                "non-ascii-initial-delay" => {
                    if let Ok(parsed) = value.parse::<f64>() {
                        config.non_ascii_initial_delay_secs = parsed;
                    }
                }
                "non-ascii-default-delay" => {
                    if let Ok(parsed) = value.parse::<f64>() {
                        config.non_ascii_default_delay_secs = parsed;
                    }
                }
                "hotkey" if !value.is_empty() => {
                    config.mac_hotkey = value;
                }
                "cancel-hotkey" => {
                    config.mac_cancel_hotkey = value;
                }
                "floating-waveform" | "mac-floating-waveform" => {
                    if let Some(parsed) = parse_bool(&value) {
                        config.floating_waveform = parsed;
                    }
                }
                "waveform-gradient-start" | "mac-waveform-gradient-start" if !value.is_empty() => {
                    config.waveform_gradient_start = value;
                }
                "waveform-gradient-end" | "mac-waveform-gradient-end" if !value.is_empty() => {
                    config.waveform_gradient_end = value;
                }
                _ => {}
            }
        }

        config
    }
}

pub fn config_file_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home_dir()
            .join("Library")
            .join("Application Support")
            .join("xhisperflow")
            .join("xhisperflowrc");
    }

    #[cfg(not(target_os = "macos"))]
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path)
            .join("xhisperflow")
            .join("xhisperflowrc");
    }

    #[cfg(not(target_os = "macos"))]
    home_dir()
        .join(".config")
        .join("xhisperflow")
        .join("xhisperflowrc")
}

pub fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn parse_bool(input: &str) -> Option<bool> {
    match input.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn strip_inline_comment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_quotes = false;
    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                out.push(ch);
            }
            '#' if !in_quotes => {
                if out.ends_with(char::is_whitespace) || out.is_empty() {
                    break;
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{Config, TranscriptionBackend, strip_inline_comment};

    #[test]
    fn backend_selection_preserves_platform_defaults_and_explicit_choice() {
        use TranscriptionBackend::*;
        assert!(Auto.use_voxtype(true, true));
        assert!(!Auto.use_voxtype(true, false));
        assert!(!Groq.use_voxtype(true, true));
        assert!(Voxtype.use_voxtype(true, false));
        for backend in [Auto, Groq, Voxtype] {
            assert!(!backend.use_voxtype(false, true));
        }
    }

    #[test]
    fn parses_backend_configuration_without_changing_waveform() {
        let config = Config::from_contents(
            "transcription-backend : voxtype # local\nvoxtype-post-processing-enabled : true\nvoxtype-timeout : 25\nwaveform-gradient-start : \"#abcdef\"",
        );
        assert_eq!(config.transcription_backend, TranscriptionBackend::Voxtype);
        assert!(config.voxtype_post_processing_enabled);
        assert_eq!(config.voxtype_timeout_secs, 25);
        assert_eq!(config.waveform_gradient_start, "#abcdef");
        assert!(config.floating_waveform);
        let defaults = Config::from_contents("voxtype-timeout : 0");
        assert_eq!(defaults.voxtype_timeout_secs, 120);
        assert!(!defaults.voxtype_post_processing_enabled);
    }

    #[test]
    fn strips_trailing_comments() {
        assert_eq!(strip_inline_comment("0.15 # comment"), "0.15 ");
        assert_eq!(strip_inline_comment("\"#kept\" # comment"), "\"#kept\" ");
    }

    #[test]
    fn app_defaults_are_present() {
        let config = Config::default();
        assert_eq!(config.mac_hotkey, "alt+space");
        assert_eq!(config.mac_cancel_hotkey, "ctrl+shift");
        assert!(config.floating_waveform);
        assert_eq!(config.waveform_gradient_start, "#b58cff");
        assert_eq!(config.waveform_gradient_end, "#d7e6ff");
    }
}
