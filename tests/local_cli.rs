#![cfg(unix)]
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::Command;

fn run(response: Value) -> std::process::Output {
    run_with_cleanup(response, None)
}

fn run_with_cleanup(response: Value, cleanup: Option<Option<Value>>) -> std::process::Output {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let runtime = dir.path().join("runtime");
    fs::create_dir_all(runtime.join("xhisperflow")).unwrap();
    let listener = UnixListener::bind(runtime.join("xhisperflow/transcription.sock")).unwrap();
    let config = if cfg!(target_os = "macos") {
        dir.path().join("Library/Application Support/xhisperflow")
    } else {
        dir.path().join("xhisperflow")
    };
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("xhisperflowrc"), "transcription-backend : local\nlocal-model : /model.bin\nlocal-post-processing-enabled : false\n").unwrap();
    let cleanup_worker = cleanup.map(|response| {
        let listener = UnixListener::bind(runtime.join("xhisperflow/cleanup.sock")).unwrap();
        fs::write(config.join("xhisperflowrc"), "transcription-backend : local\nlocal-model : /model.bin\nlocal-post-processing-enabled : true\nlocal-cleanup-enabled : true\nlocal-cleanup-timeout-ms : 50\n").unwrap();
        std::thread::spawn(move || {
            let (mut socket,_) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&socket).read_line(&mut request).unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["text"], "Um, keep Qwen enabled.");
            if let Some(response) = response {
                writeln!(socket, "{response}").unwrap();
            } else { std::thread::sleep(std::time::Duration::from_millis(150)); }
        })
    });
    let wav = dir.path().join("recording with spaces.wav");
    fs::write(&wav, "The resident worker owns WAV decoding").unwrap();
    let expected = wav.canonicalize().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&socket).read_line(&mut request).unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["file"], expected.to_str().unwrap());
        assert_eq!(request["model"], "/model.bin");
        writeln!(socket, "{response}").unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_xhisperflow"))
        .arg("--transcribe-file")
        .arg(&wav)
        .env("HOME", dir.path())
        .env_remove("XDG_DATA_HOME")
        .env("XDG_CONFIG_HOME", dir.path())
        .env("XDG_RUNTIME_DIR", runtime)
        // No Voxtype executable and no cloud credentials are available.
        .env("PATH", dir.path())
        .env("GROQ_API_KEY", "")
        .output()
        .unwrap();
    worker.join().unwrap();
    if let Some(worker) = cleanup_worker {
        worker.join().unwrap();
    }
    output
}

#[test]
fn optional_local_cleanup_runs_without_cloud_credentials() {
    let output = run_with_cleanup(
        json!({"text":"Um, keep Qwen enabled."}),
        Some(Some(json!({"text":"keep Qwen enabled.","accepted":true}))),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"keep Qwen enabled.\n");
}

#[test]
fn local_cleanup_failure_and_timeout_preserve_the_original_without_cloud_fallback() {
    for response in [Some(json!({"error":"model unavailable"})), None] {
        let output = run_with_cleanup(json!({"text":"Um, keep Qwen enabled."}), Some(response));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"Um, keep Qwen enabled.\n");
    }
}

#[test]
fn reuses_resident_worker_without_voxtype_or_api_key() {
    let output = run(json!({"text":"Resident model transcript."}));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Resident model transcript.\n");
}

#[test]
fn local_worker_errors_and_invalid_responses_never_become_dictated_text() {
    for response in [
        json!({"error":"Invalid WAV"}),
        json!({"diagnostics":"Loading model"}),
    ] {
        let output = run(response);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}
