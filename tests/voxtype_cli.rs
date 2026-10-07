#![cfg(target_os = "linux")]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

fn run(backend: &str, script: Option<&str>, cleanup: bool) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("xhisperflow");
    fs::create_dir(&config_dir).unwrap();
    fs::write(
        config_dir.join("xhisperflowrc"),
        format!("transcription-backend : {backend}\nvoxtype-post-processing-enabled : {cleanup}\n"),
    )
    .unwrap();
    if let Some(script) = script {
        let path = dir.path().join("voxtype");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let wav = dir.path().join("test recording.wav");
    fs::write(&wav, "mock audio").unwrap();
    Command::new(env!("CARGO_BIN_EXE_xhisperflow"))
        .arg("--transcribe-file")
        .arg(wav)
        .env("XDG_CONFIG_HOME", dir.path())
        .env("PATH", dir.path())
        // Deliberately empty means unavailable, without consulting the user's .env.
        .env("GROQ_API_KEY", "")
        .output()
        .unwrap()
}

#[test]
fn auto_uses_installed_voxtype_without_a_key_or_cloud_cleanup() {
    let out = run(
        "auto",
        Some(
            "printf 'Loading audio file: fixture.wav\\nAudio format: 16000 Hz, 1 channel(s), Int\\nProcessing 60047 samples (3.75s)...\\n\\nLocal transcript.\\n'",
        ),
        false,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"Local transcript.\n");
}

#[test]
fn groq_override_and_missing_auto_dependency_use_the_cloud_path() {
    for (backend, script) in [("groq", Some("exit 99")), ("auto", None)] {
        let out = run(backend, script, false);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("GROQ_API_KEY is not set"));
    }
}

#[test]
fn local_failure_and_missing_explicit_dependency_do_not_fall_back() {
    let out = run("auto", Some("echo 'model unavailable' >&2; exit 7"), false);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("model unavailable"));
    assert!(out.stdout.is_empty());
    let out = run("voxtype", None, false);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not installed on PATH"));
}

#[test]
fn optional_cloud_cleanup_requires_a_key() {
    let out = run(
        "voxtype",
        Some(
            "printf 'Loading audio file: fixture.wav\\nProcessing 12 samples\\n\\nLocal transcript.\\n'",
        ),
        true,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("GROQ_API_KEY is not set"));
}

#[test]
fn diagnostics_without_a_transcript_are_not_printed() {
    let out = run(
        "voxtype",
        Some("echo 'Loading audio file: fixture.wav'"),
        false,
    );
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing transcript separator"));
}
