use anyhow::{Context, Result, bail, ensure};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// File transcription does not use Voxtype's recorder, overlay, hotkeys or paste
// integration. xhisperflow continues to own those parts of the dictation flow.
pub(crate) fn transcribe(binary: &Path, recording: &Path, timeout: Duration) -> Result<String> {
    let recording = recording.canonicalize().context("Cannot open recording")?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = Command::new(binary)
        .arg("--quiet")
        .arg("transcribe")
        .arg(recording)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .context("Could not start Voxtype")?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(25)),
            result => {
                // Kill the process group as well: Voxtype can delegate inference.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                result.context("Could not wait for Voxtype")?;
                bail!(
                    "Voxtype transcription timed out after {} seconds",
                    timeout.as_secs()
                );
            }
        }
    };
    if !status.success() {
        let detail = read_output(&mut stderr, 4096)?;
        bail!("Voxtype transcription failed ({status}): {}", detail.trim());
    }
    ensure!(
        stdout.metadata()?.len() <= 1024 * 1024,
        "Voxtype transcript exceeds 1 MiB"
    );
    transcript(&read_output(&mut stdout, 1024 * 1024)?)
}

// Voxtype's file command prints a diagnostic preamble to stdout even with
// --quiet. Only the text after its blank separator is dictated output.
fn transcript(output: &str) -> Result<String> {
    let output = output.replace("\r\n", "\n");
    let output = output.trim();
    if !output.contains("\n\n")
        && output.lines().last() == Some("No speech detected, skipping transcription.")
    {
        return Ok(String::new());
    }
    let (preamble, text) = output
        .split_once("\n\n")
        .context("Unexpected Voxtype output: missing transcript separator; nothing was pasted")?;
    ensure!(
        preamble.starts_with("Loading audio file:"),
        "Unexpected Voxtype output: unrecognized preamble; nothing was pasted"
    );
    Ok(text.trim().to_string())
}

fn read_output(file: &mut File, limit: u64) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    String::from_utf8(bytes).context("Voxtype returned invalid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(script: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("voxtype");
        std::fs::write(&binary, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let recording = dir.path().join("recording with spaces.wav");
        std::fs::write(&recording, "fixture").unwrap();
        (dir, binary, recording)
    }

    #[test]
    fn returns_only_transcript_and_passes_audio_as_one_argument() {
        let (_dir, binary, recording) = fixture(
            r#"
            test "$#" = 3 && test "$1" = --quiet && test "$2" = transcribe && test -f "$3" || exit 2
            echo 'model diagnostics' >&2
            printf 'Loading audio file: %s\nAudio format: 16000 Hz, 1 channel(s), Int\nProcessing 60047 samples (3.75s)...\n\n  Hello, world.\n' "$3"
        "#,
        );
        assert_eq!(
            transcribe(&binary, &recording, Duration::from_secs(2)).unwrap(),
            "Hello, world."
        );
    }

    #[test]
    fn silence_remains_empty() {
        let (_dir, binary, recording) = fixture(
            "echo 'Loading audio file: fixture.wav'; echo 'No speech detected, skipping transcription.'",
        );
        assert_eq!(
            transcribe(&binary, &recording, Duration::from_secs(2)).unwrap(),
            ""
        );
    }

    #[test]
    fn local_failure_is_reported_instead_of_switching_to_cloud() {
        let (_dir, binary, recording) = fixture("echo 'model missing' >&2; exit 7");
        let error = transcribe(&binary, &recording, Duration::from_secs(2))
            .unwrap_err()
            .to_string();
        assert!(error.contains("model missing"));
        assert!(error.contains("7"));
    }

    #[test]
    fn diagnostic_output_cannot_become_dictated_text() {
        assert!(transcript("Loading audio file: fixture.wav\nAudio format: 16000 Hz").is_err());
        assert!(transcript("Unknown diagnostics\n\nmore diagnostics").is_err());
        assert!(transcript("").is_err());
        assert_eq!(transcript("Loading audio file: fixture.wav\r\nProcessing 12 samples\r\n\r\nFirst paragraph.\r\n\r\nSecond paragraph.\r\n").unwrap(), "First paragraph.\n\nSecond paragraph.");
    }

    #[test]
    fn hung_transcription_is_killed() {
        let (_dir, binary, recording) = fixture("exec sleep 5");
        let start = Instant::now();
        assert!(
            transcribe(&binary, &recording, Duration::from_millis(50))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
