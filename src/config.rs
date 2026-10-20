use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputMethod {
    Type,
    Clipboard,
    Paste,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub model: String,
    pub engine: String,
    pub language: String,
    pub threads: i32,
    pub gpu_device: i32,
    pub transcription_timeout_secs: u64,
    pub cleanup_enabled: bool,
    pub cleanup_timeout_ms: u64,
    pub transcription_prompt: String,
    pub output_method: OutputMethod,
    pub paste_keys: String,
    pub clipboard_restore_delay_secs: f64,
    pub non_ascii_initial_delay_secs: f64,
    pub non_ascii_default_delay_secs: f64,
    pub mac_hotkey: String,
    pub mac_cancel_hotkey: String,
    pub floating_waveform: bool,
    pub waveform_gradient_start: String,
    pub waveform_gradient_end: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "large-v3-turbo".into(),
            engine: "whisper".into(),
            language: "en".into(),
            threads: 4,
            gpu_device: 0,
            transcription_timeout_secs: 120,
            cleanup_enabled: false,
            cleanup_timeout_ms: 3000,
            transcription_prompt: String::new(),
            output_method: OutputMethod::Type,
            paste_keys: "ctrl+v".to_string(),
            clipboard_restore_delay_secs: 0.15,
            non_ascii_initial_delay_secs: 0.1,
            non_ascii_default_delay_secs: 0.025,
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
                "model" | "local-model" if !value.is_empty() => config.model = value,
                "engine" | "local-engine"
                    if matches!(value.as_str(), "whisper" | "parakeet" | "qwen") =>
                {
                    config.engine = value
                }
                "language" | "local-language" if !value.is_empty() => config.language = value,
                "threads" | "local-threads" => {
                    if let Ok(n @ 1..=256) = value.parse() {
                        config.threads = n;
                    }
                }
                "gpu-device" | "local-gpu-device" => {
                    if let Ok(n @ 0..=15) = value.parse() {
                        config.gpu_device = n;
                    }
                }
                "transcription-timeout" | "local-timeout" => {
                    if let Ok(n @ 1..=3600) = value.parse() {
                        config.transcription_timeout_secs = n;
                    }
                }
                "cleanup-enabled" | "local-cleanup-enabled" => {
                    if let Some(enabled) = parse_bool(&value) {
                        config.cleanup_enabled = enabled;
                    }
                }
                "cleanup-timeout-ms" | "local-cleanup-timeout-ms" => {
                    if let Ok(value @ 50..=3000) = value.parse() {
                        config.cleanup_timeout_ms = value;
                    }
                }
                "transcription-prompt" => config.transcription_prompt = value,
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
    use super::{Config, strip_inline_comment};

    #[test]
    fn parses_engines_cleanup_and_bounds_worker_settings() {
        let config = Config::from_contents(
            "engine : parakeet\nmodel : \"/tmp/a model.bin\"\nlanguage : auto\nthreads : 8\ngpu-device : 1\ntranscription-timeout : 30\ncleanup-enabled : true\ncleanup-timeout-ms : 900",
        );
        assert_eq!(config.model, "/tmp/a model.bin");
        assert_eq!(config.engine, "parakeet");
        assert_eq!(config.language, "auto");
        assert_eq!(config.threads, 8);
        assert_eq!(config.gpu_device, 1);
        assert_eq!(config.transcription_timeout_secs, 30);
        assert!(config.cleanup_enabled);
        assert_eq!(config.cleanup_timeout_ms, 900);
        let invalid = Config::from_contents(
            "threads : 0\ntranscription-timeout : 0\ngpu-device : -1\ncleanup-timeout-ms : 0",
        );
        assert_eq!(invalid.threads, 4);
        assert_eq!(invalid.transcription_timeout_secs, 120);
        assert_eq!(invalid.gpu_device, 0);
        assert_eq!(invalid.cleanup_timeout_ms, 3000);
    }

    #[test]
    fn accepts_existing_model_settings_without_backend_selection() {
        let config = Config::from_contents(
            "local-engine : qwen\nlocal-model : Qwen3-ASR-1.7B-Q8_0\nlocal-cleanup-enabled : true\nwaveform-gradient-start : \"#abcdef\"",
        );
        assert_eq!(config.engine, "qwen");
        assert!(config.cleanup_enabled);
        assert_eq!(config.waveform_gradient_start, "#abcdef");
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
