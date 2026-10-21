use crate::config::{Config, OutputMethod};
use crate::daemon::{ClientCommand, WrapKey, is_running, send_command};
#[cfg(target_os = "linux")]
use crate::linux_hud::LinuxHud;
use crate::waveform::{WaveformColor, WaveformStyle, parse_hex_color, push_level, shared_levels};
use anyhow::{Context, Result, bail};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
const RECORDING_PATH: &str = "/tmp/xhisperflow.wav";
pub(crate) const LOG_PATH: &str = "/tmp/xhisperflow.log";
const RECORDING_PID_PATH: &str = "/tmp/xhisperflow-recording.pid";
const NOTIFICATION_ID_PATH: &str = "/tmp/xhisperflow-notification.id";
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub cancel: bool,
    pub print_log: bool,
    pub wrap_key: Option<WrapKey>,
}

pub fn run(args: &[String]) -> Result<()> {
    if args == ["--cleanup-daemon"] {
        return crate::cleanup::run_daemon(&Config::load());
    }
    if args == ["--cleanup-status"] {
        println!("{}", crate::cleanup::status()?);
        return Ok(());
    }
    if args == ["--stop-cleanup-daemon"] {
        return crate::cleanup::stop();
    }
    if args.first().is_some_and(|a| a == "--cleanup-text") {
        anyhow::ensure!(args.len() == 2, "usage: xhisperflow --cleanup-text <text>");
        println!("{}", crate::cleanup::preview(&Config::load(), &args[1])?);
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--cleanup") {
        anyhow::ensure!(
            args.len() == 2 && matches!(args[1].as_str(), "on" | "off"),
            "usage: xhisperflow --cleanup on|off"
        );
        crate::cleanup::set_enabled(args[1] == "on")?;
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--select-model") {
        anyhow::ensure!(
            args.len() == 2,
            "usage: xhisperflow --select-model whisper|parakeet|qwen"
        );
        crate::local::select_model(&args[1])?;
        println!("{}", crate::local::status()?);
        return Ok(());
    }
    if args == ["--transcription-daemon"] {
        return crate::local::run_daemon(&Config::load());
    }
    if args == ["--transcription-status"] {
        println!("{}", crate::local::status()?);
        return Ok(());
    }
    if args == ["--stop-transcription-daemon"] {
        return crate::local::stop();
    }
    if args.first().is_some_and(|arg| arg == "--transcribe-file") {
        anyhow::ensure!(
            args.len() == 2,
            "usage: xhisperflow --transcribe-file <audio.wav>"
        );
        let path = Path::new(&args[1]);
        anyhow::ensure!(path.is_file(), "Audio file does not exist");
        let config = Config::load();
        let text = transcribe(&config, path)?;
        println!("{}", post_process(&config, &text)?);
        return Ok(());
    }
    let options = parse_args(args)?;
    if options.print_log {
        print_log();
        return Ok(());
    }
    if options.cancel {
        cancel_recording()?;
        return Ok(());
    }

    let config = Config::load();

    if let Some(pid) = active_recording_pid() {
        finish_recording(&config, &options, pid)?;
    } else {
        prewarm(&config);
        start_recording(&config)?;
    }

    Ok(())
}

pub fn parse_args(args: &[String]) -> Result<RunOptions> {
    let mut options = RunOptions::default();

    for arg in args {
        match arg.as_str() {
            "--cancel" => options.cancel = true,
            "--log" => options.print_log = true,
            "--leftalt" | "--rightalt" | "--leftctrl" | "--rightctrl" | "--leftshift"
            | "--rightshift" | "--super" => {
                if options.wrap_key.is_some() {
                    bail!("multiple wrap keys are not supported");
                }
                options.wrap_key = WrapKey::from_flag(arg.trim_start_matches("--"));
            }
            _ => bail!(
                "usage: xhisperflow [--cancel] [--log] [--leftalt|--rightalt|--leftctrl|--rightctrl|--leftshift|--rightshift|--super]"
            ),
        }
    }

    Ok(options)
}

fn print_log() {
    match fs::read_to_string(LOG_PATH) {
        Ok(contents) => {
            print!("{contents}");
        }
        Err(_) => eprintln!("No log file found at {LOG_PATH}"),
    }
}

fn start_recording(config: &Config) -> Result<()> {
    thread::sleep(Duration::from_millis(200));
    upsert_notification("Recording", "", None, 0)?;

    let child = Command::new("pw-record")
        .args(["--channels=1", "--rate=16000", RECORDING_PATH])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start pw-record")?;

    fs::write(RECORDING_PID_PATH, child.id().to_string())
        .context("failed to write recording pid")?;

    let mut meter = LevelMeter::spawn(child.id(), config);
    let _ = wait_for_recording(child);
    meter.stop();

    if active_recording_pid().is_none() {
        let _ = fs::remove_file(RECORDING_PID_PATH);
    }

    Ok(())
}

fn wait_for_recording(mut child: Child) -> Result<()> {
    let _ = child.wait().context("failed waiting for pw-record")?;
    Ok(())
}

fn finish_recording(config: &Config, options: &RunOptions, pid: u32) -> Result<()> {
    let stop_start = Instant::now();
    terminate_pid(pid).context("failed to stop pw-record")?;
    thread::sleep(Duration::from_millis(200));
    log_timed_step(
        "Stop recording",
        "Recorder stopped and file flush buffer elapsed",
        stop_start.elapsed(),
    )?;

    #[cfg(target_os = "linux")]
    let mut processing_hud = start_processing_hud(config);
    #[cfg(target_os = "linux")]
    if processing_hud.is_none() {
        let _ = upsert_notification("Transcribing", "", None, 0);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = upsert_notification("Transcribing", "", None, 0);

    let result: Result<()> = (|| {
        let transcription = transcribe(config, Path::new(RECORDING_PATH))?;
        let cleaned = post_process(config, &transcription)?;

        if !cleaned.trim().is_empty() {
            let paste_start = Instant::now();
            paste(config, options.wrap_key, &cleaned)?;
            log_timed_step(
                "Final paste",
                &format!("Chars: {}", cleaned.chars().count()),
                paste_start.elapsed(),
            )?;
        }
        Ok(())
    })();

    #[cfg(target_os = "linux")]
    if let Some(hud) = &mut processing_hud {
        hud.stop();
    }
    dismiss_notification();
    let _ = fs::remove_file(RECORDING_PATH);
    let _ = fs::remove_file(RECORDING_PID_PATH);
    result
}

#[cfg(target_os = "linux")]
fn start_processing_hud(config: &Config) -> Option<LinuxHud> {
    if !config.floating_waveform {
        return None;
    }

    match LinuxHud::spawn_processing(shared_levels(), waveform_style(config)) {
        Ok(hud) => {
            dismiss_notification();
            Some(hud)
        }
        Err(err) => {
            eprintln!("falling back to transcription notification: {err:#}");
            None
        }
    }
}

fn cancel_recording() -> Result<()> {
    let Some(pid) = active_recording_pid() else {
        return Ok(());
    };

    terminate_pid(pid).context("failed to cancel pw-record")?;
    let started = Instant::now();
    while pid_is_alive(pid) && started.elapsed() < Duration::from_secs(1) {
        thread::sleep(Duration::from_millis(10));
    }

    dismiss_notification();
    let _ = fs::remove_file(RECORDING_PATH);
    let _ = fs::remove_file(RECORDING_PID_PATH);
    Ok(())
}

fn paste(config: &Config, wrap_key: Option<WrapKey>, text: &str) -> Result<()> {
    if matches!(config.output_method, OutputMethod::Paste) {
        ensure_helper_available(wrap_key.is_some(), wrap_key.is_some())?;
        press_wrap_key(wrap_key)?;
        let result = crate::paste::wayland(text, &config.paste_keys);
        // Release an explicitly requested wrap key even if copying/pasting failed.
        let release = press_wrap_key(wrap_key);
        return result.and(release);
    }
    if matches!(config.output_method, OutputMethod::Clipboard) {
        ensure_helper_available(true, wrap_key.is_some())?;
        press_wrap_key(wrap_key)?;
        paste_via_clipboard(config, text)?;
        press_wrap_key(wrap_key)?;
        return Ok(());
    }

    let has_wtype = command_exists("wtype");
    let needs_helper = !has_wtype || wrap_key.is_some();
    ensure_helper_available(needs_helper, wrap_key.is_some())?;

    press_wrap_key(wrap_key)?;
    paste_by_typing(config, text, has_wtype)?;
    press_wrap_key(wrap_key)?;
    Ok(())
}

fn paste_by_typing(config: &Config, text: &str, has_wtype: bool) -> Result<()> {
    if has_wtype {
        let status = Command::new("wtype")
            .args(["-s", "100", "--"])
            .arg(text)
            .status()
            .context("failed to run wtype")?;
        if !status.success() {
            bail!("wtype exited with status {status}");
        }
        return Ok(());
    }

    if text.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
        send_command(ClientCommand::TypeString(text))?;
        return Ok(());
    }

    let clipboard = Clipboard::detect()?;
    let mut original = clipboard.capture();

    for (index, ch) in text.chars().enumerate() {
        if ch.is_ascii() && (' '..='~').contains(&ch) {
            send_command(ClientCommand::Type(ch as u8))?;
            continue;
        }

        let mut buf = [0_u8; 4];
        let encoded = ch.encode_utf8(&mut buf);
        clipboard.copy_text(encoded)?;
        send_command(ClientCommand::Paste)?;
        let delay_secs = if index == 0 {
            config.non_ascii_initial_delay_secs
        } else {
            config.non_ascii_default_delay_secs
        };
        sleep_secs(delay_secs);
    }

    if let Some(contents) = original.take() {
        restore_clipboard_after_delay(
            clipboard,
            contents,
            config.clipboard_restore_delay_secs,
            true,
        );
    }

    Ok(())
}

fn paste_via_clipboard(config: &Config, text: &str) -> Result<()> {
    let clipboard = Clipboard::detect()?;
    let saved = clipboard.capture();

    match clipboard.kind {
        ClipboardKind::WlClipboard => {
            let delay_secs = config.clipboard_restore_delay_secs;
            let had_clipboard = saved.is_some();
            let saved_contents = saved.unwrap_or_default();
            let mut child = Command::new("wl-copy")
                .args(["--foreground", "--paste-once"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("failed to start wl-copy")?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(text.as_bytes())
                    .context("failed writing clipboard text")?;
            }

            send_command(ClientCommand::Paste)?;
            thread::spawn(move || {
                let _ = child.wait();
                restore_clipboard_after_delay(clipboard, saved_contents, delay_secs, had_clipboard);
            });
        }
        ClipboardKind::Xclip => {
            let had_clipboard = saved.is_some();
            let saved_contents = saved.unwrap_or_default();
            clipboard.copy_text(text)?;
            send_command(ClientCommand::Paste)?;
            restore_clipboard_after_delay(
                clipboard,
                saved_contents,
                config.clipboard_restore_delay_secs,
                had_clipboard,
            );
        }
    }

    Ok(())
}

fn restore_clipboard_after_delay(
    clipboard: Clipboard,
    contents: Vec<u8>,
    delay_secs: f64,
    had_clipboard: bool,
) {
    thread::spawn(move || {
        sleep_secs(delay_secs);
        if had_clipboard {
            let _ = clipboard.copy_bytes(&contents);
        } else {
            let _ = clipboard.clear();
        }
    });
}

fn press_wrap_key(wrap_key: Option<WrapKey>) -> Result<()> {
    if let Some(key) = wrap_key {
        send_command(ClientCommand::Key(key))?;
    }
    Ok(())
}

fn ensure_helper_available(needs_helper: bool, wrap_key_needed: bool) -> Result<()> {
    if !needs_helper && !wrap_key_needed {
        return Ok(());
    }

    if is_running() {
        return Ok(());
    }

    let program = find_helper_binary("xhisperflowtoold")
        .or_else(|| which("xhisperflowtoold"))
        .ok_or_else(|| anyhow::anyhow!("xhisperflowtoold not found"))?;

    Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(open_append("/tmp/xhisperflowtoold.log")?)
        .spawn()
        .context("failed to start xhisperflowtoold")?;

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        if is_running() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    bail!("failed to start xhisperflowtoold");
}

pub(crate) fn prewarm(config: &Config) {
    if config.cleanup_enabled
        && let Err(error) = crate::cleanup::start()
    {
        eprintln!("Cleanup warmup failed: {error}");
    }
    let config = config.clone();
    thread::spawn(move || {
        if let Err(err) = crate::local::ensure_running(&config) {
            eprintln!("Transcription warmup failed: {err:#}");
        }
    });
}

pub(crate) fn transcribe(config: &Config, recording: &Path) -> Result<String> {
    let started = Instant::now();
    let text = crate::local::transcribe(config, recording)?;
    log_timed_step(
        &format!("Transcription ({})", config.engine),
        &text,
        started.elapsed(),
    )?;
    Ok(text)
}

pub(crate) fn post_process(config: &Config, transcript: &str) -> Result<String> {
    if !config.cleanup_enabled || transcript.trim().is_empty() {
        return Ok(transcript.to_owned());
    }
    let started = Instant::now();
    let cleaned = crate::cleanup::apply(config, transcript);
    log_timed_step("Cleanup (VoiceInk)", &cleaned, started.elapsed())?;
    Ok(cleaned)
}

fn active_recording_pid() -> Option<u32> {
    let pid = fs::read_to_string(RECORDING_PID_PATH)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    if pid_is_alive(pid) { Some(pid) } else { None }
}

fn pid_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn terminate_pid(pid: u32) -> Result<()> {
    let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(std::io::Error::last_os_error()).context("kill failed")
}

pub(crate) fn log_timed_step(title: &str, detail: &str, elapsed: Duration) -> Result<()> {
    let mut file = open_append(LOG_PATH)?;
    writeln!(file, "=== {title} ===")?;
    writeln!(file, "Result: [{detail}]")?;
    writeln!(file, "Time: {:.3}s", elapsed.as_secs_f64())?;
    Ok(())
}

fn open_append(path: impl AsRef<Path>) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .context("failed to open append file")
}

pub(crate) fn sleep_secs(secs: f64) {
    if secs > 0.0 {
        thread::sleep(Duration::from_secs_f64(secs));
    }
}

fn command_exists(name: &str) -> bool {
    which(name).is_some()
}

fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file() && is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn find_helper_binary(name: &str) -> Option<PathBuf> {
    let current_exe = env::current_exe().ok()?;
    Some(current_exe.parent()?.join(name)).filter(|path| path.is_file())
}

#[derive(Clone)]
struct Clipboard {
    kind: ClipboardKind,
}

#[derive(Clone, Copy)]
enum ClipboardKind {
    WlClipboard,
    Xclip,
}

impl Clipboard {
    fn detect() -> Result<Self> {
        if command_exists("wl-copy") && command_exists("wl-paste") {
            return Ok(Self {
                kind: ClipboardKind::WlClipboard,
            });
        }
        if command_exists("xclip") {
            return Ok(Self {
                kind: ClipboardKind::Xclip,
            });
        }
        bail!("no clipboard tool found; install wl-clipboard or xclip");
    }

    fn capture(&self) -> Option<Vec<u8>> {
        let output = match self.kind {
            ClipboardKind::WlClipboard => Command::new("wl-paste").output().ok()?,
            ClipboardKind::Xclip => Command::new("xclip")
                .args(["-o", "-selection", "clipboard"])
                .output()
                .ok()?,
        };

        if output.status.success() {
            Some(output.stdout)
        } else {
            None
        }
    }

    fn copy_text(&self, text: &str) -> Result<()> {
        self.copy_bytes(text.as_bytes())
    }

    fn copy_bytes(&self, bytes: &[u8]) -> Result<()> {
        let mut child = match self.kind {
            ClipboardKind::WlClipboard => Command::new("wl-copy")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("failed to start wl-copy")?,
            ClipboardKind::Xclip => Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("failed to start xclip")?,
        };

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(bytes)
                .context("failed to write clipboard contents")?;
        }

        let status = child.wait().context("failed waiting for clipboard tool")?;
        if !status.success() {
            bail!("clipboard copy command exited with status {status}");
        }

        Ok(())
    }

    fn clear(&self) -> Result<()> {
        match self.kind {
            ClipboardKind::WlClipboard => {
                let status = Command::new("wl-copy")
                    .arg("--clear")
                    .status()
                    .context("failed to clear wl clipboard")?;
                if !status.success() {
                    bail!("wl-copy --clear exited with status {status}");
                }
            }
            ClipboardKind::Xclip => {
                self.copy_bytes(b"")?;
            }
        }
        Ok(())
    }
}

struct LevelMeter {
    child: Option<Child>,
    #[cfg(target_os = "linux")]
    hud: Option<LinuxHud>,
}

impl LevelMeter {
    fn spawn(recording_pid: u32, config: &Config) -> Self {
        let can_notify = command_exists("notify-send");
        if !command_exists("arecord") || (!config.floating_waveform && !can_notify) {
            return Self {
                child: None,
                #[cfg(target_os = "linux")]
                hud: None,
            };
        }

        let Ok(mut child) = Command::new("arecord")
            .args([
                "-D",
                "pulse",
                "-f",
                "S16_LE",
                "-r",
                "16000",
                "-c1",
                "--period-time=33333",
                "--buffer-time=133332",
                "-vvv",
                "/dev/null",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        else {
            return Self {
                child: None,
                #[cfg(target_os = "linux")]
                hud: None,
            };
        };

        let levels = shared_levels();
        #[cfg(target_os = "linux")]
        let hud = config
            .floating_waveform
            .then(|| LinuxHud::spawn_recording(levels.clone(), waveform_style(config)));
        #[cfg(target_os = "linux")]
        let hud = hud.and_then(|result| match result {
            Ok(hud) => Some(hud),
            Err(err) => {
                eprintln!("falling back to notification level meter: {err:#}");
                None
            }
        });
        #[cfg(target_os = "linux")]
        let hud_active = hud.is_some();
        #[cfg(not(target_os = "linux"))]
        let hud_active = false;
        if hud_active {
            dismiss_notification();
        }

        if let Some(stderr) = child.stderr.take() {
            let levels = levels.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                let mut last_value = None;
                for line in reader.lines().map_while(Result::ok) {
                    if !pid_is_alive(recording_pid) {
                        break;
                    }
                    if line.contains("Recording WAVE") {
                        continue;
                    }
                    let Some(raw) = extract_peak_hex(&line) else {
                        continue;
                    };
                    let level = raw_peak_to_level(raw);
                    let value = raw_peak_percent(raw);
                    push_level(&levels, level);
                    if !hud_active && can_notify && last_value != Some(value) {
                        let _ = upsert_notification("Recording", "", Some(value), 0);
                        last_value = Some(value);
                    }
                }
            });
        }

        Self {
            child: Some(child),
            #[cfg(target_os = "linux")]
            hud,
        }
    }

    fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(target_os = "linux")]
        if let Some(hud) = &mut self.hud {
            hud.stop();
        }
    }
}

impl Drop for LevelMeter {
    fn drop(&mut self) {
        self.stop();
    }
}

fn extract_peak_hex(line: &str) -> Option<u32> {
    let marker = "0x";
    let start = line.find(marker)? + marker.len();
    let hex: String = line[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .collect();
    if hex.is_empty() {
        return None;
    }
    u32::from_str_radix(&hex, 16).ok()
}

fn raw_peak_to_level(raw: u32) -> f32 {
    if raw <= 64 {
        return 0.0;
    }

    const S16_PEAK: f32 = i16::MAX as f32;
    const SPEECH_PEAK_TO_RMS_RATIO: f32 = 4.5;
    (raw.min(i16::MAX as u32) as f32 / S16_PEAK / SPEECH_PEAK_TO_RMS_RATIO).clamp(0.0, 1.0)
}

fn raw_peak_percent(raw: u32) -> u8 {
    (raw.min(i16::MAX as u32) as f32 / i16::MAX as f32 * 100.0).round() as u8
}

fn waveform_style(config: &Config) -> WaveformStyle {
    WaveformStyle {
        gradient_start: parse_hex_color(
            &config.waveform_gradient_start,
            WaveformColor::new(181, 140, 255),
        ),
        gradient_end: parse_hex_color(
            &config.waveform_gradient_end,
            WaveformColor::new(215, 230, 255),
        ),
        rounded_background: true,
        speech_gain: config.waveform_speech_gain,
    }
}

fn get_notification_id() -> Option<String> {
    fs::read_to_string(NOTIFICATION_ID_PATH)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn upsert_notification(
    summary: &str,
    body: &str,
    value: Option<u8>,
    expire_time: i32,
) -> Result<()> {
    if !command_exists("notify-send") {
        return Ok(());
    }

    let mut command = Command::new("notify-send");
    command
        .args([
            "--app-name=xhisperflow",
            "--print-id",
            "--transient",
            &format!("--expire-time={expire_time}"),
            "--icon=audio-input-microphone",
        ])
        .arg(summary)
        .arg(body)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    if let Some(replace_id) = get_notification_id()
        && replace_id.chars().all(|ch| ch.is_ascii_digit())
    {
        command.arg(format!("--replace-id={replace_id}"));
    }

    if let Some(value) = value {
        command.arg(format!("--hint=int:value:{value}"));
    }

    let output = command.output().context("failed to run notify-send")?;
    if output.status.success() {
        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if id.chars().all(|ch| ch.is_ascii_digit()) {
            fs::write(NOTIFICATION_ID_PATH, id).context("failed to persist notification id")?;
        }
    }

    Ok(())
}

fn dismiss_notification() {
    let Some(id) = get_notification_id() else {
        let _ = fs::remove_file(NOTIFICATION_ID_PATH);
        return;
    };

    if command_exists("makoctl") {
        let _ = Command::new("makoctl")
            .args(["dismiss", "-n", &id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let _ = fs::remove_file(NOTIFICATION_ID_PATH);
}

pub fn run_tool(args: &[String]) -> Result<()> {
    if args.is_empty() {
        print_tool_usage();
        bail!("missing xhisperflowtool command");
    }

    match args[0].as_str() {
        "paste" => send_command(ClientCommand::Paste),
        "backspace" => send_command(ClientCommand::Backspace),
        "type" => {
            if args.len() != 2 || args[1].len() != 1 {
                bail!("'type' requires exactly one character argument");
            }
            send_command(ClientCommand::Type(args[1].as_bytes()[0]))
        }
        "typestring" => {
            if args.len() != 2 || args[1].is_empty() {
                bail!("'typestring' requires a non-empty string argument");
            }
            send_command(ClientCommand::TypeString(&args[1]))
        }
        other => {
            if let Some(key) = WrapKey::from_flag(other) {
                send_command(ClientCommand::Key(key))
            } else if other == "--daemon" {
                crate::daemon::run_daemon()
            } else {
                print_tool_usage();
                bail!("unknown command '{other}'");
            }
        }
    }
}

pub fn print_tool_usage() {
    eprintln!(
        "Usage:\n  xhisperflowtool paste\n  xhisperflowtool type <char>\n  xhisperflowtool typestring <txt>\n  xhisperflowtool backspace\n\nInput switching keys:\n  xhisperflowtool leftalt\n  xhisperflowtool rightalt\n  xhisperflowtool leftctrl\n  xhisperflowtool rightctrl\n  xhisperflowtool leftshift\n  xhisperflowtool rightshift\n  xhisperflowtool super\n\nDaemon:\n  xhisperflowtoold\n  xhisperflowtool --daemon"
    );
}

pub fn install_default_config(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create config directory")?;
    }
    fs::write(path, include_bytes!("../default_xhisperflowrc"))
        .context("failed to write default config")
}

fn args_from_os() -> Vec<String> {
    env::args_os().skip(1).map(os_to_string_lossy).collect()
}

pub fn run_xhisperflow_main() -> Result<()> {
    run(&args_from_os())
}

pub fn run_xhisperflowtool_main() -> Result<()> {
    run_tool(&args_from_os())
}

pub fn run_xhisperflowtoold_main() -> Result<()> {
    crate::daemon::run_daemon()
}

fn os_to_string_lossy(value: OsString) -> String {
    value.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::{parse_args, raw_peak_percent, raw_peak_to_level};

    #[test]
    fn transcription_is_always_builtin_and_cleanup_is_optional() {
        let config = crate::config::Config::default();
        assert!(!config.cleanup_enabled);
        assert_eq!(
            super::post_process(&config, "Local text.").unwrap(),
            "Local text."
        );
    }

    #[test]
    fn file_mode_rejects_missing_arguments_without_recording() {
        assert!(super::run(&["--transcribe-file".into()]).is_err());
        assert!(
            super::run(&[
                "--transcribe-file".into(),
                "/does-not-exist/recording.wav".into()
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_cancel_option() {
        let options = parse_args(&["--cancel".to_string()]).unwrap();
        assert!(options.cancel);
        assert!(!options.print_log);
        assert!(options.wrap_key.is_none());
    }

    #[test]
    fn linux_peak_meter_preserves_speech_headroom() {
        assert_eq!(raw_peak_to_level(64), 0.0);
        assert!((0.02..0.04).contains(&raw_peak_to_level(0x1000)));
        assert!((0.20..0.24).contains(&raw_peak_to_level(i16::MAX as u32)));
        assert_eq!(raw_peak_percent(i16::MAX as u32), 100);
    }
}
