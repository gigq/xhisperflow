//! Resident subprocess isolates transcribe.cpp's ggml from whisper.cpp's ggml.
use crate::{config::Config, local::data_dir};
use anyhow::{Context, Result, ensure};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub fn helper_path() -> PathBuf {
    data_dir().join("runtime/qwen/xhisperflow-qwen-worker")
}

pub struct Qwen {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    timeout: Duration,
}

impl Drop for Qwen {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Qwen {
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn load(config: &Config, model: &Path) -> Result<Self> {
        let mut child = Command::new(helper_path())
            .arg(model)
            .arg(&config.local_language)
            .arg(config.local_threads.to_string())
            .arg(config.local_gpu_device.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Install Qwen with scripts/setup-local-qwen.sh first")?;
        let input = child.stdin.take().context("Qwen stdin unavailable")?;
        let output = child.stdout.take().context("Qwen stdout unavailable")?;
        let mut this = Self {
            child,
            input,
            output,
            timeout: Duration::from_secs(config.local_timeout_secs),
        };
        ensure!(this.response()? == "ready", "Invalid Qwen startup response");
        Ok(this)
    }

    fn read_exact(&mut self, bytes: &mut [u8], deadline: Instant) -> Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "Qwen worker timed out");
            let mut fd = libc::pollfd {
                fd: self.output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe {
                libc::poll(
                    &mut fd,
                    1,
                    remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
                )
            };
            if result < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
            }
            ensure!(result > 0, "Qwen worker timed out");
            let count = self.output.read(&mut bytes[offset..])?;
            ensure!(count > 0, "Qwen worker exited unexpectedly");
            offset += count;
        }
        Ok(())
    }

    fn response(&mut self) -> Result<String> {
        let deadline = Instant::now() + self.timeout;
        let mut header = [0; 8];
        self.read_exact(&mut header, deadline)?;
        let status = u32::from_le_bytes(header[..4].try_into()?);
        let size = u32::from_le_bytes(header[4..].try_into()?) as usize;
        ensure!(size <= 1024 * 1024, "Oversized Qwen response");
        let mut bytes = vec![0; size];
        self.read_exact(&mut bytes, deadline)?;
        let text = String::from_utf8(bytes)?;
        ensure!(status == 0, "{text} (status {status})");
        Ok(text)
    }

    pub fn transcribe(&mut self, samples: &[f32], prompt: &str) -> Result<String> {
        ensure!(
            !samples.is_empty() && samples.len() <= 16000 * 3600,
            "Qwen recording must be under one hour"
        );
        ensure!(
            prompt.len() <= 65536 && !prompt.contains('\0'),
            "Invalid Qwen context prompt"
        );
        self.input
            .write_all(&(samples.len() as u32).to_le_bytes())?;
        self.input.write_all(&(prompt.len() as u32).to_le_bytes())?;
        let pcm: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.input.write_all(&pcm)?;
        self.input.write_all(prompt.as_bytes())?;
        self.input.flush()?;
        // A broken/timed-out stream cannot be reused: a late response could
        // otherwise be mistaken for the following dictation's transcript.
        match self.response() {
            Ok(text) => Ok(text.trim().to_owned()),
            Err(err) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(script: &str) -> Qwen {
        let mut child = Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Qwen {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            child,
            timeout: Duration::from_millis(100),
        }
    }

    #[test]
    fn protocol_accepts_text_and_rejects_errors_truncation_and_oversized_frames() {
        let mut worker = fixture("printf '\\000\\000\\000\\000\\005\\000\\000\\000ready'");
        assert_eq!(worker.response().unwrap(), "ready");
        for script in [
            "printf '\\001\\000\\000\\000\\004\\000\\000\\000oops'",
            "printf '\\000\\000'",
            "printf '\\000\\000\\000\\000\\377\\377\\377\\177'",
        ] {
            assert!(fixture(script).response().is_err());
        }
    }

    #[test]
    fn stalled_worker_times_out_and_is_killed_before_a_late_response_can_be_reused() {
        let mut worker = fixture("exec cat >/dev/null");
        let started = Instant::now();
        assert!(worker.transcribe(&[0.5; 160], "").is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        worker.child.wait().unwrap();
        assert!(!worker.is_running());
    }
}
