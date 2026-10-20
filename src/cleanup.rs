//! Optional, fully local dictation cleanup. Its own worker owns llama-server;
//! private Unix sockets are used for both app RPC and model HTTP requests.
use crate::{config::Config, local};
use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::{
    fs::OpenOptionsExt,
    net::{UnixListener, UnixStream},
    process::CommandExt,
};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PROMPT: &str = include_str!("../prompts/local-cleanup.txt");
const MAX_INPUT: usize = 3000;

fn control_socket() -> PathBuf {
    local::runtime_dir().join("cleanup.sock")
}
fn llm_socket() -> PathBuf {
    local::runtime_dir().join("cleanup-llama.sock")
}
fn server_path() -> PathBuf {
    local::data_dir().join("runtime/llama-b11755/llama-server")
}
fn model_path() -> PathBuf {
    local::data_dir().join("models/Qwen3.5-2B-VoiceInk-Q4_K_M.gguf")
}

pub fn installed() -> bool {
    server_path().is_file() && model_path().is_file()
}

fn connect(timeout: Duration) -> Result<UnixStream> {
    let stream = UnixStream::connect(control_socket())?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

fn request(value: &Value, timeout: Duration) -> Result<Value> {
    let mut stream = connect(timeout)?;
    local::send_message(&mut stream, value)?;
    local::read_message(&mut stream)
}

pub fn start() -> Result<()> {
    if connect(Duration::from_millis(20)).is_ok() {
        return Ok(());
    }
    ensure!(installed(), "Run scripts/setup-local-cleanup.sh first");
    local::private_dir(&local::runtime_dir())?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(local::runtime_dir().join("cleanup.log"))?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--cleanup-daemon")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn ready() -> Result<()> {
    start()?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        if status().is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    anyhow::bail!(
        "Local cleanup did not start; see {}",
        local::runtime_dir().join("cleanup.log").display()
    )
}

pub fn status() -> Result<Value> {
    request(&json!({"status":true}), Duration::from_millis(200))
}
pub fn stop() -> Result<()> {
    request(&json!({"stop":true}), Duration::from_secs(2)).map(|_| ())
}

pub fn preview(config: &Config, text: &str) -> Result<Value> {
    ensure!(
        text.len() <= MAX_INPUT,
        "Cleanup input exceeds {MAX_INPUT} bytes; keep the original text"
    );
    ready()?;
    request(
        &json!({"text":text,"timeout_ms":config.local_cleanup_timeout_ms}),
        Duration::from_millis(config.local_cleanup_timeout_ms + 100),
    )
}

// Dictation never waits for model loading. On startup, timeout, error, excessive
// length or rejected edits, pass the original through; there is no cloud fallback.
pub fn apply(config: &Config, text: &str) -> String {
    if text.trim().is_empty() || text.len() > MAX_INPUT {
        return text.to_owned();
    }
    let response = request(
        &json!({"text":text,"timeout_ms":config.local_cleanup_timeout_ms}),
        Duration::from_millis(config.local_cleanup_timeout_ms),
    );
    match response {
        Ok(value) => value["text"].as_str().unwrap_or(text).to_owned(),
        Err(_) => {
            let _ = start();
            text.to_owned()
        }
    }
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    if enabled {
        ready()?;
    }
    let path = crate::config::config_file_path();
    let source = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut lines: Vec<_> = source
        .lines()
        .filter(|line| {
            !line
                .split_once(':')
                .is_some_and(|(key, _)| key.trim() == "local-cleanup-enabled")
        })
        .collect();
    lines.push(if enabled {
        "local-cleanup-enabled : true"
    } else {
        "local-cleanup-enabled : false"
    });
    local::write_config(&path, &(lines.join("\n") + "\n"))?;
    if !enabled {
        let _ = stop();
    }
    Ok(())
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn model_client(timeout: Duration) -> Result<Client> {
    Ok(Client::builder()
        .unix_socket(llm_socket())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()?)
}

fn generate(client: &Client, text: &str, deadline: Duration) -> Result<String> {
    let response = client
        .post("http://localhost/v1/chat/completions")
        .timeout(deadline)
        .json(&json!({
            "model":"xhisperflow-cleanup", "temperature":0, "max_tokens":1024,
            "cache_prompt":true, "messages":[
                {"role":"system", "content":PROMPT},
                {"role":"user", "content":format!("<TRANSCRIPT>\n{text}\n</TRANSCRIPT>")}
            ]
        }))
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "Oversized cleanup model response");
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        value["choices"][0]["finish_reason"] == "stop",
        "Incomplete cleanup generation"
    );
    value["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_owned())
        .context("Cleanup response missing text")
}

pub fn run_daemon(config: &Config) -> Result<()> {
    local::private_dir(&local::runtime_dir())?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(local::runtime_dir().join("cleanup.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(());
    }
    ensure!(installed(), "Local cleanup runtime/model not installed");
    for path in [control_socket(), llm_socket()] {
        match fs::remove_file(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    let mut command = Command::new(server_path());
    command
        .arg("-m")
        .arg(model_path())
        .arg("--host")
        .arg(llm_socket())
        .args([
            "-c",
            "4096",
            "-np",
            "1",
            "-ngl",
            "99",
            "-t",
            "4",
            "--jinja",
            "--reasoning",
            "off",
            "--reasoning-budget",
            "0",
            "--no-webui",
            "--log-disable",
            "--alias",
            "xhisperflow-cleanup",
        ])
        .arg("--device")
        .arg(format!(
            "{}{}",
            if cfg!(target_os = "macos") {
                "Metal"
            } else {
                "Vulkan"
            },
            config.local_gpu_device
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    // No inherited llama.cpp options may turn on tools, external listeners,
    // remote models or model downloads. Runtime assets are already installed.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("LLAMA_") {
            command.env_remove(key);
        }
    }
    let mut server = Server(
        command
            .spawn()
            .context("Cannot launch local cleanup runtime")?,
    );
    let client = model_client(Duration::from_secs(1))?;
    let started = Instant::now();
    loop {
        ensure!(
            server.0.try_wait()?.is_none(),
            "Local cleanup model exited during startup"
        );
        if client
            .get("http://localhost/health")
            .send()
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(25),
            "Cleanup model startup timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // Populate the fixed prompt cache before exposing the app socket.
    let _ = generate(
        &client,
        "Keep the original meaning.",
        Duration::from_secs(5),
    );
    let listener = UnixListener::bind(control_socket())?;
    listener.set_nonblocking(true)?;
    eprintln!(
        "Local cleanup ready: VoiceInk Qwen3.5 2B Q4_K_M, pid={}",
        std::process::id()
    );
    loop {
        ensure!(server.0.try_wait()?.is_none(), "Cleanup runtime exited");
        let (mut stream, _) = match listener.accept() {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_read_timeout(Some(Duration::from_millis(100)))?;
        stream.set_write_timeout(Some(Duration::from_millis(100)))?;
        let Ok(request) = local::read_message(&mut stream) else {
            continue;
        };
        if request["stop"] == true {
            let _ = local::send_message(&mut stream, &json!({"stopped":true}));
            break;
        }
        if request["status"] == true {
            let _ = local::send_message(
                &mut stream,
                &json!({"ready":true,"model":model_path(),"pid":std::process::id(),"server_pid":server.0.id()}),
            );
            continue;
        }
        let Some(text) = request["text"].as_str().filter(|t| t.len() <= MAX_INPUT) else {
            continue;
        };
        let timeout = Duration::from_millis(
            request["timeout_ms"]
                .as_u64()
                .unwrap_or(750)
                .clamp(50, 3000),
        );
        let started = Instant::now();
        let result =
            generate(&client, text, timeout).and_then(|p| crate::cleanup_guard::validate(text, &p));
        let (output, accepted, reason) = match result {
            Ok(value) => (value, true, String::new()),
            Err(error) => (text.to_owned(), false, error.to_string()),
        };
        let response = json!({"text":output,"accepted":accepted,"reason":reason,"elapsed_ms":started.elapsed().as_millis()});
        let _ = local::send_message(&mut stream, &response);
        // Do not log dictation or model responses.
        eprintln!(
            "Local cleanup completed: {}ms, accepted={accepted}",
            started.elapsed().as_millis()
        );
    }
    let _ = fs::remove_file(control_socket());
    let _ = fs::remove_file(llm_socket());
    Ok(())
}
