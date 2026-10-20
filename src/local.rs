//! Built-in speech worker shared by the Linux CLI and macOS menu bar app.
//! One per-user process owns the model and inference state; clients send WAV paths
//! over a private Unix socket. It never records, pastes, or calls a cloud service.
use crate::config::{Config, home_dir};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_MESSAGE: u64 = 1024 * 1024;

pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir().join("Library/Application Support/xhisperflow")
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local/share"))
            .join("xhisperflow")
    }
}

pub fn model_path(config: &Config) -> PathBuf {
    let path = PathBuf::from(&config.model);
    if path.is_absolute() {
        path
    } else if config.engine == "parakeet" {
        data_dir().join("models").join(&config.model)
    } else if config.engine == "qwen" {
        data_dir()
            .join("models")
            .join(format!("{}.gguf", config.model))
    } else {
        data_dir()
            .join("models")
            .join(format!("ggml-{}.bin", config.model))
    }
}

pub fn engine_compiled(config: &Config) -> bool {
    (config.engine == "whisper" && cfg!(feature = "local-whisper"))
        || (config.engine == "parakeet" && cfg!(feature = "local-parakeet"))
        || config.engine == "qwen"
}

pub fn model_available(config: &Config) -> bool {
    let path = model_path(config);
    if config.engine == "parakeet" {
        path.join("vocab.txt").is_file()
            && ["encoder-model.int8.onnx", "encoder-model.onnx"]
                .iter()
                .any(|f| path.join(f).is_file())
            && ["decoder_joint-model.int8.onnx", "decoder_joint-model.onnx"]
                .iter()
                .any(|f| path.join(f).is_file())
    } else if config.engine == "qwen" {
        path.is_file() && crate::local_qwen::helper_path().is_file()
    } else {
        path.is_file()
    }
}

pub(crate) fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|p| PathBuf::from(p).join("xhisperflow"))
        // Keep the macOS socket below sockaddr_un's short path limit.
        .unwrap_or_else(|| {
            PathBuf::from(format!("/tmp/xhisperflow-{}", unsafe { libc::geteuid() }))
        })
}

fn socket_path() -> PathBuf {
    runtime_dir().join("transcription.sock")
}

fn worker_lock_available() -> bool {
    let Ok(lock) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(runtime_dir().join("transcription.lock"))
    else {
        return true;
    };
    // The fd closes immediately after this probe and releases the test lock.
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "Unsafe worker directory: {}",
        path.display()
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub(crate) fn read_message(stream: &mut UnixStream) -> Result<Value> {
    let mut bytes = Vec::new();
    BufReader::new(stream.take(MAX_MESSAGE + 1)).read_until(b'\n', &mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= MAX_MESSAGE && bytes.last() == Some(&b'\n'),
        "Invalid or oversized transcription message"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn send_message(stream: &mut UnixStream, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() < MAX_MESSAGE as usize,
        "Transcription response too large"
    );
    stream.write_all(&bytes)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn connect(timeout: Duration) -> Result<UnixStream> {
    private_dir(&runtime_dir())?;
    let stream = UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

pub fn ensure_running(config: &Config) -> Result<UnixStream> {
    let timeout = Duration::from_secs(config.transcription_timeout_secs);
    match connect(timeout) {
        Ok(stream) => return Ok(stream),
        Err(err)
            if err.downcast_ref::<std::io::Error>().is_some_and(|e| {
                matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                )
            }) => {}
        Err(err) => return Err(err).context("Cannot connect to local transcription worker"),
    }
    ensure!(
        engine_compiled(config),
        "Rebuild xhisperflow with the local-{} feature",
        config.engine
    );
    ensure!(
        model_available(config),
        "Local model not found: {}. Run scripts/setup-local-transcription.sh first.",
        model_path(config).display()
    );
    private_dir(&runtime_dir())?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(runtime_dir().join("transcription.log"))?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--transcription-daemon")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .context("Cannot start built-in transcription worker")?;
    // Reap when used by a long-lived menu bar app. A short-lived CLI can exit
    // without killing the worker, which is adopted by the user's init process.
    let started = Instant::now();
    while started.elapsed() < timeout {
        if let Ok(stream) = connect(timeout) {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(stream);
        }
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "Local worker exited ({status}); see {}",
                runtime_dir().join("transcription.log").display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!(
        "Local worker did not become ready within {}s; see {}",
        timeout.as_secs(),
        runtime_dir().join("transcription.log").display()
    )
}

pub fn transcribe(config: &Config, recording: &Path) -> Result<String> {
    let path = recording.canonicalize().context("Cannot open recording")?;
    let mut stream = ensure_running(config)?;
    send_message(
        &mut stream,
        &json!({"file": path, "engine": config.engine, "model": model_path(config), "language": config.language,
        "prompt": config.transcription_prompt}),
    )?;
    transcript_response(read_message(&mut stream)?)
}

fn transcript_response(response: Value) -> Result<String> {
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        bail!("Local transcription failed: {error}");
    }
    response
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Invalid response from local transcription worker; nothing was pasted")
}

pub fn status() -> Result<Value> {
    let mut stream =
        connect(Duration::from_secs(2)).context("Local transcription worker is not running")?;
    send_message(&mut stream, &json!({"status": true}))?;
    read_message(&mut stream)
}

pub fn stop() -> Result<()> {
    let mut stream =
        connect(Duration::from_secs(2)).context("Local transcription worker is not running")?;
    send_message(&mut stream, &json!({"stop": true}))?;
    let response = read_message(&mut stream)?;
    ensure!(
        response.get("stopped").and_then(Value::as_bool) == Some(true),
        "Worker did not acknowledge shutdown"
    );
    Ok(())
}

fn selected_config(contents: &str, engine: &str, model: &str) -> String {
    let mut result = contents
        .lines()
        .filter(|line| {
            !line.split_once(':').is_some_and(|(key, _)| {
                matches!(
                    key.trim(),
                    "engine" | "model" | "local-engine" | "local-model" | "transcription-backend"
                )
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    result.push_str(&format!("\nengine : \"{engine}\"\nmodel : \"{model}\"\n"));
    result
}

pub(crate) fn write_config(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().context("Config needs a parent directory")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(meta) = fs::metadata(path) {
        temporary.as_file().set_permissions(meta.permissions())?;
    }
    temporary.write_all(contents.as_bytes())?;
    temporary.persist(path)?;
    Ok(())
}

pub fn select_model(name: &str) -> Result<()> {
    let (engine, model) = match name {
        "whisper" | "large-v3-turbo" => ("whisper", "large-v3-turbo"),
        "parakeet" | "parakeet-tdt-0.6b-v3-int8" => ("parakeet", "parakeet-tdt-0.6b-v3-int8"),
        "parakeet-tdt-0.6b-v3" => ("parakeet", "parakeet-tdt-0.6b-v3"),
        "qwen" | "Qwen3-ASR-1.7B-Q8_0" => ("qwen", "Qwen3-ASR-1.7B-Q8_0"),
        _ => bail!("Choose whisper, parakeet, or qwen"),
    };
    let mut config = Config::load();
    config.engine = engine.into();
    config.model = model.into();
    ensure!(
        engine_compiled(&config),
        "This build does not include local-{engine}"
    );
    ensure!(
        model_available(&config),
        "Model not installed: {}. Run setup-local-transcription.sh {model} first.",
        model_path(&config).display()
    );
    ensure!(
        !Path::new("/tmp/xhisperflow-recording.pid").exists(),
        "Finish or cancel the current recording before switching models"
    );
    let path = crate::config::config_file_path();
    let original = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    // Preserve login preloading when the Linux worker is service-managed.
    let managed = cfg!(target_os = "linux")
        && Command::new("systemctl")
            .args([
                "--user",
                "is-active",
                "--quiet",
                "xhisperflow-transcription.service",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    write_config(&path, &selected_config(&original, engine, model))?;
    let result = (|| -> Result<()> {
        if managed {
            ensure!(
                Command::new("systemctl")
                    .args(["--user", "restart", "xhisperflow-transcription.service"])
                    .status()?
                    .success(),
                "Worker service restart failed"
            );
        } else {
            if status().is_ok() {
                stop()?;
                let start = Instant::now();
                while !worker_lock_available() && start.elapsed() < Duration::from_secs(10) {
                    std::thread::sleep(Duration::from_millis(25));
                }
                ensure!(worker_lock_available(), "Previous worker did not shut down");
            }
            drop(ensure_running(&config)?);
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(config.transcription_timeout_secs) {
            if let Ok(ready) = status() {
                ensure!(
                    ready
                        .get("engine")
                        .and_then(Value::as_str)
                        .unwrap_or("whisper")
                        == engine
                        && ready.get("model") == Some(&json!(model_path(&config))),
                    "Worker loaded the wrong model"
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        bail!("Selected model did not become ready")
    })();
    if let Err(error) = result {
        write_config(&path, &original)
            .context("Could not restore previous config after model load failed")?;
        if managed {
            let _ = Command::new("systemctl")
                .args(["--user", "restart", "xhisperflow-transcription.service"])
                .status();
        } else {
            let _ = stop();
        }
        return Err(error).context("Model switch failed; previous configuration restored");
    }
    Ok(())
}

pub fn run_daemon(config: &Config) -> Result<()> {
    private_dir(&runtime_dir())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(runtime_dir().join("transcription.lock"))?;
    // Lock before touching a socket. Simultaneous first-use clients must never
    // replace a live worker's socket or load a second GPU model.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        // Another first-use client won startup. The caller will connect to it.
        return Ok(());
    }
    let path = model_path(config);
    ensure!(
        model_available(config),
        "Local model incomplete or missing: {}",
        path.display()
    );
    let started = Instant::now();
    let mut engine = crate::local_engine::Engine::load(config, &path)?;
    let socket = socket_path();
    match fs::remove_file(&socket) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    eprintln!(
        "Local {} ready: model={}, pid={}, loaded in {:.3}s",
        config.engine,
        path.display(),
        std::process::id(),
        started.elapsed().as_secs_f64()
    );
    for stream in listener.incoming() {
        let mut stream = stream?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let request = match read_message(&mut stream) {
            Ok(value) => value,
            // Warmup probes connect and close without sending a request.
            Err(_) => continue,
        };
        if request.get("stop").and_then(Value::as_bool) == Some(true) {
            let _ = send_message(&mut stream, &json!({"stopped":true}));
            break;
        }
        if request.get("status").and_then(Value::as_bool) == Some(true) {
            let _ = send_message(
                &mut stream,
                &json!({"ready":true, "engine":config.engine, "model":path, "pid":std::process::id(), "language":config.language}),
            );
            continue;
        }
        let started = Instant::now();
        let result: Result<String> = (|| {
            ensure!(
                request.get("model") == Some(&json!(path))
                    && request
                        .get("engine")
                        .and_then(Value::as_str)
                        .unwrap_or("whisper")
                        == config.engine
                    && request.get("language") == Some(&json!(config.language)),
                "Worker engine/model/language differs from config; restart the transcription worker"
            );
            let file = request
                .get("file")
                .and_then(Value::as_str)
                .context("Missing WAV path")?;
            let samples = read_audio(Path::new(file))?;
            if samples.is_empty() || samples.iter().all(|sample| sample.abs() < 0.00001) {
                return Ok(String::new());
            }
            let prompt = request.get("prompt").and_then(Value::as_str).unwrap_or("");
            engine.transcribe(config, &samples, prompt)
        })();
        let response = match result {
            Ok(text) => json!({"text":text}),
            Err(error) => json!({"error":format!("{error:#}")}),
        };
        // A timed-out/disconnected client must not crash the resident worker.
        let _ = send_message(&mut stream, &response);
        eprintln!(
            "Local transcription completed in {:.3}s (ok={})",
            started.elapsed().as_secs_f64(),
            response.get("error").is_none()
        );
    }
    fs::remove_file(socket)?;
    Ok(())
}

fn read_audio(path: &Path) -> Result<Vec<f32>> {
    ensure!(
        fs::metadata(path)?.len() <= 128 * 1024 * 1024,
        "Recording exceeds 128 MiB"
    );
    let mut reader = hound::WavReader::open(path).context("Cannot read recording WAV")?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1 && (8000..=192000).contains(&spec.sample_rate),
        "Local transcription requires mono WAV audio at 8–192 kHz"
    );
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Int => {
            ensure!(
                (1..=32).contains(&spec.bits_per_sample),
                "Unsupported WAV bit depth"
            );
            let scale = 2_f32.powi(spec.bits_per_sample as i32 - 1);
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<std::result::Result<_, _>>()?
        }
    };
    ensure!(
        samples.iter().all(|s| s.is_finite()),
        "Recording contains invalid audio samples"
    );
    resample(samples, spec.sample_rate)
}

fn resample(samples: Vec<f32>, rate: u32) -> Result<Vec<f32>> {
    use rubato::{FftFixedIn, Resampler};
    if rate == 16000 || samples.is_empty() {
        return Ok(samples);
    }
    // The Mac recorder uses the microphone's native rate (often 44.1/48 kHz).
    // Band-limited conversion avoids folding ultrasonic noise into speech.
    let mut resampler = FftFixedIn::<f32>::new(rate as usize, 16000, 1024, 1, 1)?;
    let delay = resampler.output_delay();
    let wanted = (samples.len() as u64 * 16000 / rate as u64) as usize;
    let mut output = Vec::new();
    let mut position = 0;
    while output.len() < wanted + delay {
        let frames = resampler.input_frames_next();
        let mut chunk = vec![0.0; frames];
        let available = samples.len().saturating_sub(position).min(frames);
        chunk[..available].copy_from_slice(&samples[position..position + available]);
        position += available;
        output.extend_from_slice(&resampler.process(&[chunk], None)?[0]);
    }
    Ok(output[delay..delay + wanted].to_vec())
}

// OpenOptionsExt is also needed by the client when opening its worker log.
use std::os::unix::fs::OpenOptionsExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_preserves_unrelated_settings_and_removes_duplicate_overrides() {
        let original = "# my dictation\nmodel : old\noutput-method : paste\nlocal-model : duplicate\nengine : whisper\ntranscription-backend : auto\n";
        let updated = selected_config(original, "parakeet", "parakeet-tdt-0.6b-v3-int8");
        assert!(updated.contains("# my dictation\noutput-method : paste"));
        assert_eq!(updated.matches("model :").count(), 1);
        assert_eq!(updated.matches("engine :").count(), 1);
        assert!(!updated.contains("local-model"));
        assert!(updated.contains("engine : \"parakeet\""));
        assert!(!updated.contains("transcription-backend"));
    }

    #[test]
    fn parakeet_requires_both_networks_and_vocabulary() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            engine: "parakeet".into(),
            model: dir.path().to_str().unwrap().into(),
            ..Default::default()
        };
        assert!(!model_available(&config));
        for name in [
            "encoder-model.int8.onnx",
            "decoder_joint-model.int8.onnx",
            "vocab.txt",
        ] {
            fs::write(dir.path().join(name), "fixture").unwrap();
        }
        assert!(model_available(&config));
        fs::remove_file(dir.path().join("decoder_joint-model.int8.onnx")).unwrap();
        assert!(!model_available(&config));
    }

    #[test]
    fn protocol_preserves_paths_spaces_and_multiline_text() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        send_message(&mut client, &json!({"file":"/tmp/voice take.wav"})).unwrap();
        assert_eq!(
            read_message(&mut server).unwrap()["file"],
            "/tmp/voice take.wav"
        );
        send_message(&mut server, &json!({"text":"First line.\nSecond line."})).unwrap();
        assert_eq!(
            transcript_response(read_message(&mut client).unwrap()).unwrap(),
            "First line.\nSecond line."
        );
        assert!(transcript_response(json!({"error":"Invalid WAV"})).is_err());
        assert!(transcript_response(json!({"diagnostics":"not text"})).is_err());
    }

    #[test]
    fn rejects_oversized_protocol_messages() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = client.write_all(&vec![b'x'; MAX_MESSAGE as usize + 1]);
        });
        assert!(read_message(&mut server).is_err());
        drop(server);
        writer.join().unwrap();
    }

    #[test]
    fn reads_pipewire_integer_and_macos_float_wav_and_rejects_wrong_rate() {
        let dir = tempfile::tempdir().unwrap();
        for float in [false, true] {
            let path = dir.path().join(if float { "float.wav" } else { "int.wav" });
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: if float { 32 } else { 16 },
                sample_format: if float {
                    hound::SampleFormat::Float
                } else {
                    hound::SampleFormat::Int
                },
            };
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            if float {
                writer.write_sample(0.5_f32).unwrap();
            } else {
                writer.write_sample(16384_i16).unwrap();
            }
            writer.finalize().unwrap();
            assert_eq!(read_audio(&path).unwrap(), vec![0.5]);
        }
        let path = dir.path().join("wrong-rate.wav");
        hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 2,
                sample_rate: 44100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap()
        .finalize()
        .unwrap();
        assert!(read_audio(&path).is_err());
    }

    #[test]
    fn resamples_native_mac_rates_without_losing_the_tail_or_aliasing() {
        for rate in [44100, 48000] {
            let tone = |hz: f32| {
                (0..rate)
                    .map(|i| (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin())
                    .collect()
            };
            let speech = resample(tone(1000.0), rate).unwrap();
            assert_eq!(speech.len(), 16000);
            let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
            assert!(rms(&speech[15000..15900]) > 0.6);
            let high = resample(tone(12000.0), rate).unwrap();
            assert!(rms(&high[1000..15000]) < 0.05);
        }
    }
}
