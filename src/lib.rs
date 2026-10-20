pub mod app;
pub mod cleanup;
mod cleanup_guard;
pub mod config;
pub mod local;
mod local_engine;
mod local_qwen;
mod paste;
mod voxtype;
pub mod waveform;

#[cfg(target_os = "linux")]
pub mod daemon;

#[cfg(not(target_os = "linux"))]
pub mod daemon {
    use anyhow::{Result, bail};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum WrapKey {
        LeftAlt,
        RightAlt,
        LeftCtrl,
        RightCtrl,
        LeftShift,
        RightShift,
        Super,
    }

    impl WrapKey {
        pub fn from_flag(flag: &str) -> Option<Self> {
            match flag {
                "leftalt" => Some(Self::LeftAlt),
                "rightalt" => Some(Self::RightAlt),
                "leftctrl" => Some(Self::LeftCtrl),
                "rightctrl" => Some(Self::RightCtrl),
                "leftshift" => Some(Self::LeftShift),
                "rightshift" => Some(Self::RightShift),
                "super" => Some(Self::Super),
                _ => None,
            }
        }

        pub fn cli_name(self) -> &'static str {
            match self {
                Self::LeftAlt => "leftalt",
                Self::RightAlt => "rightalt",
                Self::LeftCtrl => "leftctrl",
                Self::RightCtrl => "rightctrl",
                Self::LeftShift => "leftshift",
                Self::RightShift => "rightshift",
                Self::Super => "super",
            }
        }
    }

    pub enum ClientCommand<'a> {
        Paste,
        Type(u8),
        TypeString(&'a str),
        Backspace,
        Key(WrapKey),
    }

    pub fn send_command(_command: ClientCommand<'_>) -> Result<()> {
        bail!("xhisperflow helper daemon is only available on Linux")
    }

    pub fn is_running() -> bool {
        false
    }

    pub fn run_daemon() -> Result<()> {
        bail!("xhisperflowtoold is only available on Linux")
    }
}

#[cfg(target_os = "macos")]
pub mod macos_app;

#[cfg(target_os = "linux")]
pub mod linux_hud;

// Avoid ETXTBSY when concurrent fork/exec tests inherit another fixture's
// briefly-open script writer. Only the subprocess fixtures take this lock.
#[cfg(test)]
static PROCESS_FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
