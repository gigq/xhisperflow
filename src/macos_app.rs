use crate::app::{LOG_PATH, load_home_env, log_timed_step, post_process, sleep_secs, transcribe};
use crate::config::{Config, config_file_path, home_dir};
use anyhow::{Context, Result, anyhow, bail};
use arboard::Clipboard;
use block::ConcreteBlock;
#[allow(deprecated)]
use cocoa::appkit::{NSBackingStoreBuffered, NSColor, NSWindowStyleMask};
#[allow(deprecated)]
use cocoa::base::{NO, YES, id, nil};
#[allow(deprecated)]
use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
use core_foundation::runloop::CFRunLoop;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CallbackResult, EventField, KeyCode,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings,
};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use softbuffer::Surface;
use std::collections::VecDeque;
use std::env;
use std::ffi::{CStr, c_void};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex, Once,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{
    ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy, OwnedDisplayHandle,
};
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

type Boolean = u8;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> Boolean;
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

const MAC_RECORDING_PATH: &str = "/tmp/xhisperflow-mac.wav";
const HUD_WIDTH: u32 = 360;
const HUD_HEIGHT: u32 = 78;
const HUD_TOP_OFFSET: i32 = 0;
const HUD_BOTTOM_RADIUS: f64 = 18.0;
const HUD_SHOULDER_Y: f64 = 14.0;
const HUD_SHOULDER_INSET: f64 = 8.0;
const WAVEFORM_HEIGHT: u32 = 58;
const WAVEFORM_BOTTOM_PADDING: u32 = 14;
const WAVEFORM_LEVEL_FLOOR: f32 = 0.10;
const WAVEFORM_LEVEL_CEILING: f32 = 0.62;
const HOTKEY_DEBOUNCE: Duration = Duration::from_millis(250);
const LEVEL_HISTORY: usize = 180;
const LOGIN_AGENT_LABEL: &str = "com.gigq.xhisperflow";

type HudSurface = Surface<OwnedDisplayHandle, Rc<Window>>;

pub fn run() -> Result<()> {
    load_home_env();

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("failed to build macOS event loop")?;
    let proxy = event_loop.create_proxy();

    let proxy_for_menu = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy_for_menu.send_event(UserEvent::Menu(event));
    }));

    let proxy_for_hotkey = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event| {
        let _ = proxy_for_hotkey.send_event(UserEvent::HotKey(event));
    }));

    let mut app = MacApp::new(proxy)?;
    event_loop
        .run_app(&mut app)
        .context("macOS app event loop failed")
}

#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
    HotKey(GlobalHotKeyEvent),
    OrderIndependentHotKey,
    OrderIndependentCancelHotKey,
    ModifierOnlyHotKey,
    ModifierOnlyCancelHotKey,
    ModifierTapReady,
    ModifierTapFailed(String),
    Worker(WorkerEvent),
}

#[derive(Debug)]
enum WorkerEvent {
    TranscriptionFinished(Result<String, String>),
}

struct MacApp {
    proxy: EventLoopProxy<UserEvent>,
    config: Config,
    tray: Option<TrayIcon>,
    menu_ids: MenuIds,
    toggle_item: Option<MenuItem>,
    status_item: Option<MenuItem>,
    cancel_item: Option<MenuItem>,
    copy_last_item: Option<MenuItem>,
    login_item: Option<MenuItem>,
    hotkey: MacHotKey,
    cancel_hotkey: Option<MacHotKey>,
    hotkey_manager: GlobalHotKeyManager,
    modifier_tap_hotkeys: ModifierTapHotKeys,
    modifier_tap_started: bool,
    modifier_tap_starting: bool,
    last_permission_poll_at: Instant,
    window: Option<Rc<Window>>,
    window_id: Option<WindowId>,
    surface: Option<HudSurface>,
    levels: Arc<Mutex<VecDeque<f32>>>,
    recorder: Option<Recorder>,
    state: AppState,
    status: String,
    started_at: Option<Instant>,
    last_hotkey_at: Option<Instant>,
    last_transcription: Option<String>,
    accessibility_prompted: bool,
    api_key_prompted: bool,
}

#[derive(Clone, Debug, Default)]
struct MenuIds {
    toggle: MenuId,
    cancel: MenuId,
    copy_last: MenuId,
    login: Option<MenuId>,
    open_config: MenuId,
    api_key_setup: MenuId,
    show_log: MenuId,
    permissions: MenuId,
    quit: MenuId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppState {
    Idle,
    Recording,
    Transcribing,
    Pasting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacHotKey {
    Standard(HotKey),
    ModifierOnly(Modifiers),
}

impl MacHotKey {
    fn standard_hotkey(self) -> Option<HotKey> {
        match self {
            Self::Standard(hotkey) => Some(hotkey),
            Self::ModifierOnly(_) => None,
        }
    }

    fn escape_mods(self) -> Option<Modifiers> {
        match self {
            Self::Standard(hotkey) => order_independent_escape_mods(hotkey),
            Self::ModifierOnly(_) => None,
        }
    }

    fn modifier_only_mods(self) -> Option<Modifiers> {
        match self {
            Self::Standard(_) => None,
            Self::ModifierOnly(mods) => Some(mods),
        }
    }
}

impl MacApp {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Result<Self> {
        let config = Config::load();
        let hotkey = parse_hotkey_binding(&config.mac_hotkey)?;
        let hotkey_manager =
            GlobalHotKeyManager::new().context("failed to create global hotkey manager")?;
        if let Some(standard_hotkey) = hotkey.standard_hotkey() {
            hotkey_manager
                .register(standard_hotkey)
                .with_context(|| format!("failed to register hotkey '{}'", config.mac_hotkey))?;
        }
        let cancel_hotkey = parse_optional_hotkey_binding(&config.mac_cancel_hotkey)?;
        if let Some(standard_cancel_hotkey) = cancel_hotkey.and_then(MacHotKey::standard_hotkey) {
            hotkey_manager.register(standard_cancel_hotkey).with_context(|| {
                format!(
                    "failed to register cancel hotkey '{}'",
                    config.mac_cancel_hotkey
                )
            })?;
        }
        let tap_hotkeys = ModifierTapHotKeys {
            toggle_escape_mods: hotkey.escape_mods(),
            cancel_escape_mods: cancel_hotkey.and_then(MacHotKey::escape_mods),
            toggle_modifier_mods: hotkey.modifier_only_mods(),
            cancel_modifier_mods: cancel_hotkey.and_then(MacHotKey::modifier_only_mods),
        };
        Ok(Self {
            proxy,
            config,
            tray: None,
            menu_ids: MenuIds::default(),
            toggle_item: None,
            status_item: None,
            cancel_item: None,
            copy_last_item: None,
            login_item: None,
            hotkey,
            cancel_hotkey,
            hotkey_manager,
            modifier_tap_hotkeys: tap_hotkeys,
            modifier_tap_started: false,
            modifier_tap_starting: false,
            last_permission_poll_at: Instant::now(),
            window: None,
            window_id: None,
            surface: None,
            levels: Arc::new(Mutex::new(VecDeque::with_capacity(LEVEL_HISTORY))),
            recorder: None,
            state: AppState::Idle,
            status: "Ready".to_string(),
            started_at: None,
            last_hotkey_at: None,
            last_transcription: None,
            accessibility_prompted: false,
            api_key_prompted: false,
        })
    }

    fn build_tray(&mut self) -> Result<()> {
        let menu = Menu::new();
        let toggle = MenuItem::new("Start Recording", true, None);
        let cancel = MenuItem::new("Cancel Recording", false, None);
        let copy_last = MenuItem::new("Copy Last Transcription", false, None);
        let status = MenuItem::new("Ready", false, None);
        let open_config = MenuItem::new("Open Config", true, None);
        let api_key_setup = MenuItem::new("API Key Setup", true, None);
        let show_log = MenuItem::new("Show Log", true, None);
        let login = (!login_agent_enabled()).then(|| MenuItem::new("Start at Login", true, None));
        let permissions = MenuItem::new("Permissions Help", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let separator = PredefinedMenuItem::separator();
        let separator_two = PredefinedMenuItem::separator();

        menu.append_items(&[
            &toggle,
            &cancel,
            &copy_last,
            &status,
            &separator,
            &open_config,
            &api_key_setup,
            &show_log,
        ])
        .context("failed to build tray menu")?;
        if let Some(login) = &login {
            menu.append(login)
                .context("failed to append start at login menu item")?;
        }
        menu.append_items(&[&permissions, &separator_two, &quit])
            .context("failed to finish tray menu")?;

        self.menu_ids = MenuIds {
            toggle: toggle.id().clone(),
            cancel: cancel.id().clone(),
            copy_last: copy_last.id().clone(),
            login: login.as_ref().map(|item| item.id().clone()),
            open_config: open_config.id().clone(),
            api_key_setup: api_key_setup.id().clone(),
            show_log: show_log.id().clone(),
            permissions: permissions.id().clone(),
            quit: quit.id().clone(),
        };
        self.toggle_item = Some(toggle);
        self.cancel_item = Some(cancel);
        self.copy_last_item = Some(copy_last);
        self.status_item = Some(status);
        self.login_item = login;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("xhisperflow")
            .with_icon(tray_icon()?)
            .with_icon_as_template(true)
            .build()
            .context("failed to create menu bar icon")?;
        self.tray = Some(tray);
        Ok(())
    }

    fn create_hud(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if self.window.is_some() {
            return Ok(());
        }

        let attrs = WindowAttributes::default()
            .with_title("xhisperflow")
            .with_inner_size(LogicalSize::new(
                f64::from(HUD_WIDTH),
                f64::from(HUD_HEIGHT),
            ))
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_visible(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_has_shadow(false)
            .with_movable_by_window_background(false);

        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .context("failed to create waveform HUD")?,
        );
        let context = softbuffer::Context::new(event_loop.owned_display_handle())
            .map_err(|err| anyhow!("failed to create waveform drawing context: {err:?}"))?;
        let surface = Surface::new(&context, window.clone())
            .map_err(|err| anyhow!("failed to create waveform drawing surface: {err:?}"))?;
        apply_notch_window_shape(&window);
        self.window_id = Some(window.id());
        self.surface = Some(surface);
        self.window = Some(window);
        Ok(())
    }

    fn handle_menu(&mut self, event_loop: &ActiveEventLoop, event: MenuEvent) {
        let id = event.id();
        if id == &self.menu_ids.toggle {
            self.toggle_recording();
        } else if id == &self.menu_ids.cancel {
            self.cancel_recording();
        } else if id == &self.menu_ids.copy_last {
            self.copy_last_transcription();
        } else if id == &self.menu_ids.open_config {
            self.open_config();
        } else if id == &self.menu_ids.api_key_setup {
            self.open_api_key_setup();
        } else if id == &self.menu_ids.show_log {
            self.open_path(Path::new(LOG_PATH));
        } else if self
            .menu_ids
            .login
            .as_ref()
            .is_some_and(|login| id == login)
        {
            self.toggle_start_at_login();
        } else if id == &self.menu_ids.permissions {
            self.open_permissions_help();
        } else if id == &self.menu_ids.quit {
            event_loop.exit();
        }
    }

    fn toggle_recording(&mut self) {
        match self.state {
            AppState::Idle => self.start_recording(),
            AppState::Recording => self.stop_recording(),
            AppState::Transcribing | AppState::Pasting => {
                self.set_status("Busy");
            }
        }
    }

    fn trigger_hotkey_toggle(&mut self) {
        let now = Instant::now();
        if self
            .last_hotkey_at
            .is_some_and(|last| now.duration_since(last) < HOTKEY_DEBOUNCE)
        {
            return;
        }
        self.last_hotkey_at = Some(now);
        self.toggle_recording();
    }

    fn trigger_cancel_hotkey(&mut self) {
        let now = Instant::now();
        if self
            .last_hotkey_at
            .is_some_and(|last| now.duration_since(last) < HOTKEY_DEBOUNCE)
        {
            return;
        }
        self.last_hotkey_at = Some(now);
        self.cancel_recording();
    }

    fn start_recording(&mut self) {
        if microphone_restart_required() {
            self.set_status("Restart required");
            show_microphone_restart_prompt();
            return;
        }
        if !required_permissions_granted() {
            self.set_status("Permissions required");
            self.open_permissions_help();
            return;
        }
        if !groq_api_key_present() {
            self.set_status("Groq API key required");
            self.open_api_key_setup();
            return;
        }

        self.clear_levels();
        match Recorder::start(PathBuf::from(MAC_RECORDING_PATH), self.levels.clone()) {
            Ok(recorder) => {
                self.recorder = Some(recorder);
                self.state = AppState::Recording;
                self.started_at = Some(Instant::now());
                self.set_status("Recording");
                self.show_hud(true);
            }
            Err(err) => {
                self.state = AppState::Idle;
                if microphone_permission_granted() {
                    MICROPHONE_RESTART_REQUIRED.store(true, Ordering::SeqCst);
                    self.set_status("Restart required");
                    show_microphone_restart_prompt();
                } else {
                    self.set_status(&format!("Mic unavailable: {err:#}"));
                    self.open_permissions_help();
                    self.show_hud(true);
                }
            }
        }
    }

    fn stop_recording(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            self.state = AppState::Idle;
            self.set_status("Ready");
            return;
        };

        self.state = AppState::Transcribing;
        self.set_status("Transcribing");

        match recorder.stop() {
            Ok(path) => {
                let config = self.config.clone();
                let proxy = self.proxy.clone();
                thread::spawn(move || {
                    let started = Instant::now();
                    let result = transcribe(&config, &path)
                        .and_then(|text| post_process(&config, &text))
                        .map_err(|err| format!("{err:#}"));
                    let _ = log_timed_step(
                        "macOS worker",
                        "Transcription worker completed",
                        started.elapsed(),
                    );
                    let _ = fs::remove_file(&path);
                    let _ = proxy.send_event(UserEvent::Worker(
                        WorkerEvent::TranscriptionFinished(result),
                    ));
                });
            }
            Err(err) => {
                self.state = AppState::Idle;
                self.set_status(&format!("Recording failed: {err:#}"));
            }
        }
    }

    fn cancel_recording(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            if matches!(self.state, AppState::Transcribing | AppState::Pasting) {
                self.set_status("Busy");
            }
            return;
        };

        let path = recorder.output_path().to_path_buf();
        drop(recorder);
        let _ = fs::remove_file(&path);
        self.state = AppState::Idle;
        self.started_at = None;
        self.clear_levels();
        self.set_status("Ready");
        self.show_hud(false);
    }

    fn finish_transcription(&mut self, result: Result<String, String>) {
        match result {
            Ok(text) if text.trim().is_empty() => {
                self.state = AppState::Idle;
                self.set_status("No speech");
                self.show_hud(false);
            }
            Ok(text) => {
                self.set_last_transcription(text.clone());
                self.state = AppState::Pasting;
                self.set_status("Pasting");
                match paste_text(&self.config, &text) {
                    Ok(()) => {
                        self.state = AppState::Idle;
                        self.set_status("Ready");
                        self.show_hud(false);
                    }
                    Err(err) => {
                        self.state = AppState::Idle;
                        self.set_status(&format!("Paste failed; copied text: {err:#}"));
                        self.show_hud(true);
                    }
                }
            }
            Err(err) => {
                self.state = AppState::Idle;
                self.set_status(&format!("Transcription failed: {err}"));
                self.show_hud(true);
                if !groq_api_key_present() || err.contains("GROQ_API_KEY") {
                    self.open_api_key_setup();
                }
            }
        }
    }

    fn set_last_transcription(&mut self, text: String) {
        self.last_transcription = Some(text);
        if let Some(item) = &self.copy_last_item {
            item.set_enabled(true);
        }
    }

    fn copy_last_transcription(&mut self) {
        let Some(text) = self.last_transcription.as_ref() else {
            self.set_status("No transcription to copy");
            return;
        };

        match Clipboard::new() {
            Ok(mut clipboard) => match clipboard.set_text(text.clone()) {
                Ok(()) => self.set_status("Copied last transcription"),
                Err(err) => self.set_status(&format!("Copy failed: {err}")),
            },
            Err(err) => self.set_status(&format!("Copy failed: {err}")),
        }
    }

    fn set_status(&mut self, status: &str) {
        self.status = status.to_string();
        if let Some(item) = &self.status_item {
            item.set_text(status);
        }
        if let Some(toggle) = &self.toggle_item {
            let text = match self.state {
                AppState::Recording => "Stop Recording",
                AppState::Idle => "Start Recording",
                AppState::Transcribing => "Transcribing...",
                AppState::Pasting => "Pasting...",
            };
            toggle.set_text(text);
            toggle.set_enabled(matches!(self.state, AppState::Idle | AppState::Recording));
        }
        if let Some(cancel) = &self.cancel_item {
            cancel.set_enabled(matches!(self.state, AppState::Recording));
        }
        if let Some(window) = &self.window {
            window.set_title(&format!("xhisperflow - {status}"));
            window.request_redraw();
        }
    }

    fn clear_levels(&mut self) {
        if let Ok(mut levels) = self.levels.lock() {
            levels.clear();
        }
    }

    fn show_preview_hud(&mut self) {
        if let Ok(mut levels) = self.levels.lock() {
            levels.clear();
            for idx in 0..LEVEL_HISTORY {
                let wave = (idx as f32 * 0.22).sin().abs();
                let contour = 0.35 + 0.65 * (idx as f32 * 0.047).sin().abs();
                levels.push_back((wave * contour).clamp(0.0, 1.0));
            }
        }
        self.state = AppState::Recording;
        self.show_hud(true);
    }

    fn show_hud(&self, visible: bool) {
        if let Some(window) = &self.window {
            if visible {
                position_hud_at_notch(window);
            }
            window.set_visible(visible && self.config.mac_floating_waveform);
            if visible {
                window.request_redraw();
            }
        }
    }

    fn open_config(&self) {
        let path = config_file_path();
        if !path.exists() {
            if let Err(err) = crate::app::install_default_config(&path) {
                eprintln!("failed to create config: {err:#}");
                return;
            }
        }
        self.open_path(&path);
    }

    fn open_path(&self, path: &Path) {
        if let Err(err) = std::process::Command::new("open").arg(path).status() {
            eprintln!("failed to open {}: {err}", path.display());
        }
    }

    fn maybe_prompt_for_accessibility_permission(&mut self) {
        let force_prompt = std::env::var_os("XHISPERFLOW_ACCESSIBILITY_PROMPT_PREVIEW").is_some();
        if self.accessibility_prompted
            || (!force_prompt && accessibility_permission_granted() && microphone_permission_granted())
        {
            return;
        }

        self.accessibility_prompted = true;
        self.set_status("Permissions required");
        if catch_unwind(AssertUnwindSafe(show_accessibility_permission_prompt)).is_err() {
            eprintln!("failed to show Accessibility permission prompt");
            open_system_settings_privacy_pane("Privacy_Accessibility");
        }
    }

    fn maybe_prompt_for_api_key(&mut self) {
        if self.api_key_prompted
            || groq_api_key_present()
            || !required_permissions_granted()
            || permission_flow_needs_polling()
            || microphone_restart_required()
        {
            return;
        }

        self.api_key_prompted = true;
        self.set_status("Groq API key required");
        self.open_api_key_setup();
    }

    fn ensure_modifier_event_tap_started(&mut self) {
        if self.modifier_tap_started
            || self.modifier_tap_starting
            || !self.modifier_tap_hotkeys.has_bindings()
            || !accessibility_permission_granted()
        {
            return;
        }

        append_log_line(&format!(
            "starting macOS modifier hotkey listener: {:?}",
            self.modifier_tap_hotkeys
        ));
        start_modifier_event_tap(self.proxy.clone(), self.modifier_tap_hotkeys);
        self.modifier_tap_starting = true;
    }

    fn poll_permission_status(&mut self) {
        if self.last_permission_poll_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_permission_poll_at = Instant::now();
        if permission_flow_needs_polling() {
            refresh_permission_buttons();
        }
        self.ensure_modifier_event_tap_started();
        self.maybe_prompt_for_api_key();
    }

    fn open_permissions_help(&self) {
        if catch_unwind(AssertUnwindSafe(show_accessibility_permission_prompt)).is_err() {
            eprintln!("failed to show permissions help");
        }
    }

    fn open_api_key_setup(&self) {
        if catch_unwind(AssertUnwindSafe(show_api_key_setup_prompt)).is_err() {
            eprintln!("failed to show API key setup");
        }
    }

    fn toggle_start_at_login(&mut self) {
        let result = if login_agent_enabled() {
            disable_start_at_login()
        } else {
            enable_start_at_login()
        };

        match result {
            Ok(()) => {
                if let Some(login) = &self.login_item {
                    login.set_enabled(false);
                    login.set_text("Start at Login Enabled");
                }
            }
            Err(err) => self.set_status(&format!("Login item failed: {err:#}")),
        }
    }

    fn draw_hud(&mut self) -> Result<()> {
        let Some(window) = &self.window else {
            return Ok(());
        };
        let Some(surface) = self.surface.as_mut() else {
            return Ok(());
        };

        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        let width = NonZeroU32::new(size.width).ok_or_else(|| anyhow!("invalid HUD width"))?;
        let height = NonZeroU32::new(size.height).ok_or_else(|| anyhow!("invalid HUD height"))?;
        surface
            .resize(width, height)
            .map_err(|err| anyhow!("failed to resize waveform surface: {err:?}"))?;

        let levels = self
            .levels
            .lock()
            .map(|levels| levels.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut buffer = surface
            .buffer_mut()
            .map_err(|err| anyhow!("failed to acquire waveform buffer: {err:?}"))?;
        let gradient_start = parse_hex_color(
            &self.config.mac_waveform_gradient_start,
            HudColor::new(181, 140, 255),
        );
        let gradient_end = parse_hex_color(
            &self.config.mac_waveform_gradient_end,
            HudColor::new(215, 230, 255),
        );
        draw_waveform(
            &mut buffer,
            size,
            &levels,
            self.state,
            gradient_start,
            gradient_end,
        );
        buffer
            .present()
            .map_err(|err| anyhow!("failed to present waveform buffer: {err:?}"))?;
        Ok(())
    }
}

impl ApplicationHandler<UserEvent> for MacApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Err(err) = self.create_hud(event_loop) {
            eprintln!("{err:#}");
            event_loop.exit();
            return;
        }
        if self.tray.is_none() {
            if let Err(err) = self.build_tray() {
                eprintln!("{err:#}");
                event_loop.exit();
            }
        }
        if std::env::var_os("XHISPERFLOW_HUD_PREVIEW").is_some() {
            self.show_preview_hud();
        }
        self.ensure_modifier_event_tap_started();
        self.maybe_prompt_for_accessibility_permission();
        self.maybe_prompt_for_api_key();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Menu(event) => self.handle_menu(event_loop, event),
            UserEvent::HotKey(event) => {
                if self
                    .hotkey
                    .standard_hotkey()
                    .is_some_and(|hotkey| event.id == hotkey.id())
                    && matches!(event.state, HotKeyState::Pressed)
                {
                    self.trigger_hotkey_toggle();
                } else if self
                    .cancel_hotkey
                    .and_then(MacHotKey::standard_hotkey)
                    .is_some_and(|hotkey| event.id == hotkey.id())
                    && matches!(event.state, HotKeyState::Pressed)
                {
                    self.trigger_cancel_hotkey();
                }
            }
            UserEvent::OrderIndependentHotKey => self.trigger_hotkey_toggle(),
            UserEvent::OrderIndependentCancelHotKey => self.trigger_cancel_hotkey(),
            UserEvent::ModifierOnlyHotKey => self.trigger_hotkey_toggle(),
            UserEvent::ModifierOnlyCancelHotKey => self.trigger_cancel_hotkey(),
            UserEvent::ModifierTapReady => {
                self.modifier_tap_started = true;
                self.modifier_tap_starting = false;
                append_log_line("macOS modifier hotkey listener ready");
            }
            UserEvent::ModifierTapFailed(err) => {
                self.modifier_tap_started = false;
                self.modifier_tap_starting = false;
                append_log_line(&format!("macOS modifier hotkey listener failed: {err}"));
                self.set_status(&format!("Modifier hotkey unavailable: {err}"));
            }
            UserEvent::Worker(WorkerEvent::TranscriptionFinished(result)) => {
                self.finish_transcription(result);
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if Some(window_id) != self.window_id {
            return;
        }

        match event {
            WindowEvent::RedrawRequested => {
                if let Err(err) = self.draw_hud() {
                    eprintln!("failed to draw waveform HUD: {err:#}");
                }
            }
            WindowEvent::Resized(_) => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::Moved(_) => {
                if matches!(self.state, AppState::Recording) {
                    if let Some(window) = &self.window {
                        position_hud_at_notch(window);
                    }
                }
            }
            WindowEvent::CloseRequested => self.show_hud(false),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_permission_status();
        if matches!(self.state, AppState::Recording) {
            event_loop.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(33)));
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        } else if permission_flow_needs_polling()
            || (self.modifier_tap_hotkeys.has_bindings() && !self.modifier_tap_started)
        {
            event_loop.set_control_flow(ControlFlow::wait_duration(Duration::from_secs(1)));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(hotkey) = self.hotkey.standard_hotkey() {
            let _ = self.hotkey_manager.unregister(hotkey);
        }
        if let Some(cancel_hotkey) = self.cancel_hotkey.and_then(MacHotKey::standard_hotkey) {
            let _ = self.hotkey_manager.unregister(cancel_hotkey);
        }
        if let Some(recorder) = self.recorder.take() {
            let _ = recorder.stop();
        }
    }
}

struct Recorder {
    stream: cpal::Stream,
    writer: Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    output_path: PathBuf,
}

impl Recorder {
    fn start(output_path: PathBuf, levels: Arc<Mutex<VecDeque<f32>>>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device"))?;
        let input_config = device
            .default_input_config()
            .context("failed to read default input config")?;
        let sample_rate = input_config.sample_rate();
        let channels = usize::from(input_config.channels());
        if channels == 0 {
            bail!("input device reports zero channels");
        }

        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: WavSampleFormat::Int,
        };
        let writer = WavWriter::create(&output_path, spec).context("failed to create wav file")?;
        let writer = Arc::new(Mutex::new(Some(writer)));
        let writer_for_callback = writer.clone();
        let err_fn = |err| eprintln!("macOS audio input error: {err}");

        let stream_config = input_config.config();
        let stream = match input_config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    write_f32_input(data, channels, &writer_for_callback, &levels)
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::F64 => device.build_input_stream(
                &stream_config,
                move |data: &[f64], _| {
                    write_f64_input(data, channels, &writer_for_callback, &levels)
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    write_i16_input(data, channels, &writer_for_callback, &levels)
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I32 => device.build_input_stream(
                &stream_config,
                move |data: &[i32], _| {
                    write_i32_input(data, channels, &writer_for_callback, &levels)
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    write_u16_input(data, channels, &writer_for_callback, &levels)
                },
                err_fn,
                None,
            ),
            other => bail!("unsupported input sample format: {other:?}"),
        }
        .context("failed to build input stream")?;

        stream.play().context("failed to start input stream")?;
        Ok(Self {
            stream,
            writer,
            output_path,
        })
    }

    fn stop(self) -> Result<PathBuf> {
        drop(self.stream);
        if let Some(writer) = self
            .writer
            .lock()
            .map_err(|_| anyhow!("wav writer lock poisoned"))?
            .take()
        {
            writer.finalize().context("failed to finalize wav file")?;
        }
        Ok(self.output_path)
    }

    fn output_path(&self) -> &Path {
        &self.output_path
    }
}

fn write_f32_input(
    data: &[f32],
    channels: usize,
    writer: &Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    levels: &Arc<Mutex<VecDeque<f32>>>,
) {
    write_input(
        data.chunks(channels)
            .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32),
        writer,
        levels,
    );
}

fn write_f64_input(
    data: &[f64],
    channels: usize,
    writer: &Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    levels: &Arc<Mutex<VecDeque<f32>>>,
) {
    write_input(
        data.chunks(channels)
            .map(|frame| (frame.iter().copied().sum::<f64>() / frame.len() as f64) as f32),
        writer,
        levels,
    );
}

fn write_i16_input(
    data: &[i16],
    channels: usize,
    writer: &Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    levels: &Arc<Mutex<VecDeque<f32>>>,
) {
    write_input(
        data.chunks(channels).map(|frame| {
            frame
                .iter()
                .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
                .sum::<f32>()
                / frame.len() as f32
        }),
        writer,
        levels,
    );
}

fn write_i32_input(
    data: &[i32],
    channels: usize,
    writer: &Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    levels: &Arc<Mutex<VecDeque<f32>>>,
) {
    write_input(
        data.chunks(channels).map(|frame| {
            frame
                .iter()
                .map(|sample| *sample as f32 / i32::MAX as f32)
                .sum::<f32>()
                / frame.len() as f32
        }),
        writer,
        levels,
    );
}

fn write_u16_input(
    data: &[u16],
    channels: usize,
    writer: &Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    levels: &Arc<Mutex<VecDeque<f32>>>,
) {
    write_input(
        data.chunks(channels).map(|frame| {
            frame
                .iter()
                .map(|sample| (*sample as f32 - 32768.0) / 32768.0)
                .sum::<f32>()
                / frame.len() as f32
        }),
        writer,
        levels,
    );
}

fn write_input<I>(
    samples: I,
    writer: &Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    levels: &Arc<Mutex<VecDeque<f32>>>,
) where
    I: Iterator<Item = f32>,
{
    let mut sum = 0.0_f32;
    let mut count = 0_usize;
    if let Ok(mut guard) = writer.lock() {
        if let Some(writer) = guard.as_mut() {
            for sample in samples {
                let sample = sample.clamp(-1.0, 1.0);
                let pcm = (sample * f32::from(i16::MAX)).round() as i16;
                let _ = writer.write_sample(pcm);
                sum += sample * sample;
                count += 1;
            }
        }
    }

    if count == 0 {
        return;
    }
    let rms = (sum / count as f32).sqrt().clamp(0.0, 1.0);
    if let Ok(mut levels) = levels.lock() {
        let previous = levels.back().copied().unwrap_or(0.0);
        let smoothed = previous * 0.72 + rms * 0.28;
        if levels.len() >= LEVEL_HISTORY {
            levels.pop_front();
        }
        levels.push_back(smoothed);
    }
}

fn accessibility_permission_granted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

fn required_permissions_granted() -> bool {
    accessibility_permission_granted() && microphone_permission_granted()
}

fn microphone_restart_required() -> bool {
    MICROPHONE_RESTART_REQUIRED.load(Ordering::SeqCst)
}

fn microphone_permission_granted() -> bool {
    microphone_authorization_status() == AV_AUTHORIZATION_STATUS_AUTHORIZED
}

fn microphone_authorization_status() -> i64 {
    unsafe {
        let Some(capture_device) = Class::get("AVCaptureDevice") else {
            return -1;
        };
        let audio = NSString::alloc(nil).init_str("soun");
        msg_send![capture_device, authorizationStatusForMediaType: audio]
    }
}

fn request_microphone_permission() {
    MICROPHONE_REQUEST_FINISHED.store(false, Ordering::SeqCst);
    MICROPHONE_REQUEST_GRANTED.store(false, Ordering::SeqCst);

    unsafe {
        let Some(capture_device) = Class::get("AVCaptureDevice") else {
            MICROPHONE_REQUEST_FINISHED.store(true, Ordering::SeqCst);
            return;
        };
        let audio = NSString::alloc(nil).init_str("soun");
        let completion = ConcreteBlock::new(|granted: Boolean| {
            MICROPHONE_REQUEST_GRANTED.store(granted != 0, Ordering::SeqCst);
            MICROPHONE_REQUEST_FINISHED.store(true, Ordering::SeqCst);
        })
        .copy();
        let _: () = msg_send![
            capture_device,
            requestAccessForMediaType: audio
            completionHandler: &*completion
        ];
        std::mem::forget(completion);
    }
}

fn groq_api_key_present() -> bool {
    env::var("GROQ_API_KEY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

fn save_groq_api_key(api_key: &str) -> Result<()> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        bail!("API key is empty");
    }
    if api_key.bytes().any(|byte| byte == b'\n' || byte == b'\r') {
        bail!("API key must be a single line");
    }

    let path = home_dir().join(".env");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut lines = Vec::new();
    let mut replaced = false;
    for line in existing.lines() {
        if line
            .trim_start()
            .strip_prefix("GROQ_API_KEY")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
        {
            lines.push(format!("GROQ_API_KEY={api_key}"));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        if !lines.is_empty() && lines.last().is_some_and(|line| !line.is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("GROQ_API_KEY={api_key}"));
    }

    let mut contents = lines.join("\n");
    contents.push('\n');
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    unsafe {
        env::set_var("GROQ_API_KEY", api_key);
    }
    Ok(())
}

#[allow(deprecated)]
fn show_accessibility_permission_prompt() {
    unsafe {
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];

        if PERMISSION_SETUP_WINDOW != nil {
            refresh_permission_buttons();
            let _: () = msg_send![PERMISSION_SETUP_WINDOW, makeKeyAndOrderFront: nil];
            return;
        }

        let window = make_setup_window(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 440.0)),
            "Enable xhisperflow",
        );
        let content: id = msg_send![window, contentView];
        set_view_background(content, ns_color(0.13, 0.13, 0.15, 1.0), 0.0);

        let icon = make_app_icon_view(248.0, 312.0, 64.0, 64.0);
        let title = make_label("Enable xhisperflow", 0.0, 260.0, 560.0, 34.0, 23.0, true);
        let body = make_label(
            "xhisperflow needs these permissions to record and paste transcripts on your Mac.",
            78.0,
            214.0,
            404.0,
            42.0,
            13.5,
            false,
        );
        let _: () = msg_send![body, setAlignment: 1_i64];
        let access_row = make_rounded_box(42.0, 118.0, 476.0, 74.0, 16.0);
        let access_icon = make_permission_icon("accessibility", "A", 62.0, 130.0);
        let access_title = make_label("Accessibility", 130.0, 151.0, 220.0, 22.0, 16.0, true);
        let access_detail = make_label(
            "Allows xhisperflow to paste into the active app",
            130.0,
            130.0,
            270.0,
            20.0,
            12.5,
            false,
        );
        let allow = make_permission_button(
            428.0,
            141.0,
            74.0,
            28.0,
            sel!(openAccessibilityFromPermissionWindow:),
        );
        let mic_row = make_rounded_box(42.0, 34.0, 476.0, 74.0, 16.0);
        let mic_icon = make_permission_icon("mic.fill", "M", 62.0, 46.0);
        let mic_title = make_label("Microphone", 130.0, 67.0, 220.0, 22.0, 16.0, true);
        let mic_detail = make_label(
            "Records audio for transcription",
            130.0,
            46.0,
            270.0,
            20.0,
            12.5,
            false,
        );
        let mic_allow = make_permission_button(
            428.0,
            57.0,
            74.0,
            28.0,
            sel!(openMicrophoneFromPermissionWindow:),
        );
        set_permission_buttons(allow, mic_allow);
        refresh_permission_buttons();
        start_permission_button_refresh_timer();

        add_subviews(
            content,
            &[
                icon,
                title,
                body,
                access_row,
                access_icon,
                access_title,
                access_detail,
                allow,
                mic_row,
                mic_icon,
                mic_title,
                mic_detail,
                mic_allow,
            ],
        );
        let _: () = msg_send![window, center];
        let _: id = msg_send![window, retain];
        PERMISSION_SETUP_WINDOW = window;
        let _: () = msg_send![window, makeKeyAndOrderFront: nil];
    }
}

#[allow(deprecated)]
fn show_api_key_setup_prompt() {
    unsafe {
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];

        if API_KEY_SETUP_WINDOW != nil {
            let _: () = msg_send![API_KEY_SETUP_WINDOW, makeKeyAndOrderFront: nil];
            return;
        }

        let window = make_setup_window(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(740.0, 360.0)),
            "Set Up xhisperflow",
        );
        let content: id = msg_send![window, contentView];
        set_view_background(content, ns_color(0.13, 0.13, 0.15, 1.0), 0.0);

        let icon = make_app_icon_view(338.0, 248.0, 64.0, 64.0);
        let title = make_label("Add your Groq API key", 0.0, 205.0, 740.0, 34.0, 22.0, true);
        let body = make_label(
            "Paste your key below. xhisperflow saves it to ~/.env and uses it for transcription.",
            132.0,
            166.0,
            476.0,
            42.0,
            13.0,
            false,
        );
        let _: () = msg_send![body, setAlignment: 1_i64];

        let input_box = make_rounded_box(62.0, 82.0, 616.0, 64.0, 14.0);
        let input_label = make_label("GROQ_API_KEY", 82.0, 116.0, 180.0, 18.0, 11.0, true);
        let field = make_secure_text_field(82.0, 91.0, 576.0, 24.0);
        let status = make_label("", 82.0, 55.0, 400.0, 20.0, 11.5, false);
        let save = make_controller_button(
            "Save",
            586.0,
            50.0,
            92.0,
            30.0,
            sel!(saveApiKeyFromSetupWindow:),
        );

        API_KEY_FIELD = field;
        API_KEY_STATUS_LABEL = status;
        add_subviews(content, &[icon, title, body, input_box, input_label, field, status, save]);
        let _: () = msg_send![window, center];
        let _: id = msg_send![window, retain];
        API_KEY_SETUP_WINDOW = window;
        let _: () = msg_send![window, makeKeyAndOrderFront: nil];
        let _: () = msg_send![window, makeFirstResponder: field];
    }
}

#[allow(deprecated)]
fn make_setup_window(frame: NSRect, title: &str) -> id {
    unsafe {
        let style = NSWindowStyleMask::NSTitledWindowMask
            | NSWindowStyleMask::NSClosableWindowMask
            | NSWindowStyleMask::NSFullSizeContentViewWindowMask;
        let window: id = msg_send![class!(NSWindow), alloc];
        let window: id = msg_send![
            window,
            initWithContentRect: frame
            styleMask: style
            backing: NSBackingStoreBuffered
            defer: NO
        ];
        let title = NSString::alloc(nil).init_str(title);
        let _: () = msg_send![window, setTitle: title];
        let _: () = msg_send![window, setReleasedWhenClosed: NO];
        let _: () = msg_send![window, setTitlebarAppearsTransparent: YES];
        let _: () = msg_send![window, setMovableByWindowBackground: YES];
        window
    }
}

#[allow(deprecated)]
fn make_label(text: &str, x: f64, y: f64, width: f64, height: f64, size: f64, bold: bool) -> id {
    unsafe {
        let text = NSString::alloc(nil).init_str(text);
        let label: id = msg_send![class!(NSTextField), labelWithString: text];
        let font: id = if bold {
            msg_send![class!(NSFont), boldSystemFontOfSize: size]
        } else {
            msg_send![class!(NSFont), systemFontOfSize: size]
        };
        let _: () = msg_send![
            label,
            setFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
        ];
        let _: () = msg_send![label, setFont: font];
        let _: () = msg_send![label, setAlignment: if x == 0.0 { 1_i64 } else { 0_i64 }];
        let _: () = msg_send![label, setLineBreakMode: 0_u64];
        let _: () = msg_send![label, setTextColor: ns_color(0.90, 0.90, 0.92, 1.0)];
        label
    }
}

#[allow(deprecated)]
fn make_permission_icon(symbol: &str, fallback: &str, x: f64, y: f64) -> id {
    unsafe {
        let view: id = msg_send![class!(NSView), alloc];
        let view: id = msg_send![
            view,
            initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(48.0, 48.0))
        ];
        let color: id = msg_send![class!(NSColor), systemBlueColor];
        set_view_background(view, color, 24.0);

        let symbol_name = NSString::alloc(nil).init_str(symbol);
        let image: id = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: symbol_name
            accessibilityDescription: nil
        ];
        let white: id = msg_send![class!(NSColor), whiteColor];
        if image != nil {
            let image_view: id = msg_send![class!(NSImageView), imageViewWithImage: image];
            let _: () = msg_send![
                image_view,
                setFrame: NSRect::new(NSPoint::new(9.0, 9.0), NSSize::new(30.0, 30.0))
            ];
            let _: () = msg_send![image_view, setContentTintColor: white];
            let _: () = msg_send![view, addSubview: image_view];
        } else {
            let glyph = make_label(fallback, 0.0, 8.0, 48.0, 30.0, 24.0, true);
            let _: () = msg_send![glyph, setTextColor: white];
            let _: () = msg_send![view, addSubview: glyph];
        }
        view
    }
}

#[allow(deprecated)]
fn make_app_icon_view(x: f64, y: f64, width: f64, height: f64) -> id {
    unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let image: id = msg_send![
            workspace,
            iconForFile: NSString::alloc(nil).init_str("/Applications/xhisperflow.app")
        ];
        let image_view: id = msg_send![class!(NSImageView), imageViewWithImage: image];
        let _: () = msg_send![
            image_view,
            setFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
        ];
        image_view
    }
}

#[allow(deprecated)]
fn set_view_background(view: id, color: id, radius: f64) {
    unsafe {
        let _: () = msg_send![view, setWantsLayer: YES];
        let layer: id = msg_send![view, layer];
        let cg_color: *const c_void = msg_send![color, CGColor];
        let _: () = msg_send![layer, setBackgroundColor: cg_color];
        let _: () = msg_send![layer, setCornerRadius: radius];
    }
}

#[allow(deprecated)]
fn ns_color(red: f64, green: f64, blue: f64, alpha: f64) -> id {
    unsafe {
        msg_send![
            class!(NSColor),
            colorWithCalibratedRed: red
            green: green
            blue: blue
            alpha: alpha
        ]
    }
}

#[allow(deprecated)]
fn make_rounded_box(x: f64, y: f64, width: f64, height: f64, radius: f64) -> id {
    unsafe {
        let view: id = msg_send![class!(NSView), alloc];
        let view: id = msg_send![
            view,
            initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
        ];
        set_view_background(view, ns_color(0.18, 0.18, 0.20, 1.0), radius);
        view
    }
}

#[allow(deprecated)]
fn make_permission_button(x: f64, y: f64, width: f64, height: f64, action: Sel) -> id {
    unsafe {
        let title = NSString::alloc(nil).init_str("Allow");
        let key = NSString::alloc(nil).init_str("\r");
        let button: id = msg_send![
            class!(NSButton),
            buttonWithTitle: title
            target: permission_setup_controller()
            action: action
        ];
        let _: () = msg_send![
            button,
            setFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
        ];
        let _: () = msg_send![button, setBezelStyle: 1_u64];
        let _: () = msg_send![button, setKeyEquivalent: key];
        button
    }
}

#[allow(deprecated)]
fn make_controller_button(title: &str, x: f64, y: f64, width: f64, height: f64, action: Sel) -> id {
    unsafe {
        let title = NSString::alloc(nil).init_str(title);
        let key = NSString::alloc(nil).init_str("\r");
        let button: id = msg_send![
            class!(NSButton),
            buttonWithTitle: title
            target: permission_setup_controller()
            action: action
        ];
        let _: () = msg_send![
            button,
            setFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
        ];
        let _: () = msg_send![button, setBezelStyle: 1_u64];
        let _: () = msg_send![button, setKeyEquivalent: key];
        button
    }
}

#[allow(deprecated)]
fn make_secure_text_field(x: f64, y: f64, width: f64, height: f64) -> id {
    unsafe {
        let field: id = msg_send![class!(NSSecureTextField), alloc];
        let field: id = msg_send![
            field,
            initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
        ];
        let placeholder = NSString::alloc(nil).init_str("gsk_...");
        let font: id = msg_send![class!(NSFont), systemFontOfSize: 13.0_f64];
        let _: () = msg_send![field, setPlaceholderString: placeholder];
        let _: () = msg_send![field, setFont: font];
        let _: () = msg_send![field, setBezeled: YES];
        let _: () = msg_send![field, setEditable: YES];
        let _: () = msg_send![field, setSelectable: YES];
        field
    }
}

#[allow(deprecated)]
fn set_permission_buttons(accessibility_button: id, microphone_button: id) {
    unsafe {
        ACCESSIBILITY_PERMISSION_BUTTON = accessibility_button;
        MICROPHONE_PERMISSION_BUTTON = microphone_button;
    }
}

#[allow(deprecated)]
fn refresh_permission_buttons() {
    unsafe {
        let accessibility_allowed = accessibility_permission_granted();
        let microphone_allowed = microphone_permission_granted();
        update_permission_button(ACCESSIBILITY_PERMISSION_BUTTON, accessibility_allowed);
        update_permission_button(MICROPHONE_PERMISSION_BUTTON, microphone_allowed);
        update_permission_flow(accessibility_allowed, microphone_allowed);
    }
}

#[allow(deprecated)]
fn update_permission_button(button: id, allowed: bool) {
    unsafe {
        if button == nil {
            return;
        }
        let title = if allowed { "Allowed" } else { "Allow" };
        let title = NSString::alloc(nil).init_str(title);
        let _: () = msg_send![button, setTitle: title];
        let _: () = msg_send![button, setEnabled: if allowed { NO } else { YES }];
    }
}

#[allow(deprecated)]
fn start_permission_button_refresh_timer() {
    static INIT: Once = Once::new();

    unsafe {
        INIT.call_once(|| {
            let _: id = msg_send![
                class!(NSTimer),
                scheduledTimerWithTimeInterval: 1.0_f64
                target: permission_setup_controller()
                selector: sel!(refreshPermissionButtons:)
                userInfo: nil
                repeats: YES
            ];
        });
    }
}

static mut ACCESSIBILITY_PERMISSION_BUTTON: id = nil;
static mut MICROPHONE_PERMISSION_BUTTON: id = nil;
static mut PERMISSION_SETUP_WINDOW: id = nil;
static mut PERMISSION_HELPER_PANEL: id = nil;
static mut ACTIVE_PERMISSION_HELPER: i32 = PERMISSION_NONE;
static mut API_KEY_SETUP_WINDOW: id = nil;
static mut API_KEY_FIELD: id = nil;
static mut API_KEY_STATUS_LABEL: id = nil;
static MICROPHONE_REQUEST_FINISHED: AtomicBool = AtomicBool::new(false);
static MICROPHONE_REQUEST_GRANTED: AtomicBool = AtomicBool::new(false);
static MICROPHONE_RESTART_REQUIRED: AtomicBool = AtomicBool::new(false);
const PERMISSION_NONE: i32 = 0;
const PERMISSION_ACCESSIBILITY: i32 = 1;
const PERMISSION_MICROPHONE: i32 = 2;
const AV_AUTHORIZATION_STATUS_NOT_DETERMINED: i64 = 0;
const AV_AUTHORIZATION_STATUS_AUTHORIZED: i64 = 3;

fn add_subviews(content: id, views: &[id]) {
    unsafe {
        for view in views {
            let _: () = msg_send![content, addSubview: *view];
        }
    }
}

fn permission_setup_controller() -> id {
    static INIT: Once = Once::new();
    static mut CONTROLLER: id = nil;

    unsafe {
        INIT.call_once(|| {
            let class = permission_setup_controller_class();
            CONTROLLER = msg_send![class, new];
        });
        CONTROLLER
    }
}

fn permission_setup_controller_class() -> *const Class {
    static INIT: Once = Once::new();
    static mut CLASS: *const Class = std::ptr::null();

    unsafe {
        INIT.call_once(|| {
            if let Some(existing) = Class::get("XhisperflowPermissionSetupController") {
                CLASS = existing;
                return;
            }

            let superclass = class!(NSObject);
            if let Some(mut decl) =
                ClassDecl::new("XhisperflowPermissionSetupController", superclass)
            {
                decl.add_method(
                    sel!(openAccessibilityFromPermissionWindow:),
                    open_accessibility_from_permission_window as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(openMicrophoneFromPermissionWindow:),
                    open_microphone_from_permission_window as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(refreshPermissionButtons:),
                    refresh_permission_buttons_from_timer as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(saveApiKeyFromSetupWindow:),
                    save_api_key_from_setup_window as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(restartXhisperflow:),
                    restart_xhisperflow_from_helper as extern "C" fn(&Object, Sel, id),
                );
                CLASS = decl.register();
            }
        });
        CLASS
    }
}

extern "C" fn open_accessibility_from_permission_window(_: &Object, _: Sel, sender: id) {
    hide_permission_setup_window(sender);
    open_system_settings_privacy_pane("Privacy_Accessibility");
    set_active_permission_helper(PERMISSION_ACCESSIBILITY);
    show_permission_drag_helper_panel("Accessibility", "Drag xhisperflow.app to the Accessibility list above");
    refresh_permission_buttons();
}

extern "C" fn open_microphone_from_permission_window(_: &Object, _: Sel, sender: id) {
    hide_permission_setup_window(sender);
    set_active_permission_helper(PERMISSION_MICROPHONE);
    if microphone_authorization_status() == AV_AUTHORIZATION_STATUS_NOT_DETERMINED {
        show_microphone_prompt_helper_panel();
        request_microphone_permission();
    } else {
        open_system_settings_privacy_pane("Privacy_Microphone");
        show_microphone_settings_helper_panel();
    }
    refresh_permission_buttons();
}

extern "C" fn refresh_permission_buttons_from_timer(_: &Object, _: Sel, _: id) {
    refresh_permission_buttons();
}

extern "C" fn save_api_key_from_setup_window(_: &Object, _: Sel, _: id) {
    unsafe {
        if API_KEY_FIELD == nil {
            return;
        }

        let value: id = msg_send![API_KEY_FIELD, stringValue];
        match save_groq_api_key(&nsstring_to_string(value)) {
            Ok(()) => {
                if API_KEY_SETUP_WINDOW != nil {
                    let _: () = msg_send![API_KEY_SETUP_WINDOW, close];
                    API_KEY_SETUP_WINDOW = nil;
                }
                API_KEY_FIELD = nil;
                API_KEY_STATUS_LABEL = nil;
            }
            Err(err) => set_api_key_status(&format!("{err:#}")),
        }
    }
}

extern "C" fn restart_xhisperflow_from_helper(_: &Object, _: Sel, _: id) {
    unsafe {
        MICROPHONE_RESTART_REQUIRED.store(true, Ordering::SeqCst);
        relaunch_current_app();
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, terminate: nil];
    }
}

fn nsstring_to_string(value: id) -> String {
    unsafe {
        if value == nil {
            return String::new();
        }
        let bytes: *const std::os::raw::c_char = msg_send![value, UTF8String];
        if bytes.is_null() {
            return String::new();
        }
        CStr::from_ptr(bytes).to_string_lossy().into_owned()
    }
}

#[allow(deprecated)]
fn set_api_key_status(message: &str) {
    unsafe {
        if API_KEY_STATUS_LABEL == nil {
            return;
        }
        let message = NSString::alloc(nil).init_str(message);
        let _: () = msg_send![API_KEY_STATUS_LABEL, setStringValue: message];
        let red: id = msg_send![class!(NSColor), systemRedColor];
        let _: () = msg_send![API_KEY_STATUS_LABEL, setTextColor: red];
    }
}

#[allow(deprecated)]
fn hide_permission_setup_window(sender: id) {
    unsafe {
        let window: id = msg_send![sender, window];
        if window != nil {
            PERMISSION_SETUP_WINDOW = window;
            let _: () = msg_send![window, orderOut: nil];
        }
    }
}

fn set_active_permission_helper(permission: i32) {
    unsafe {
        ACTIVE_PERMISSION_HELPER = permission;
    }
}

fn permission_flow_needs_polling() -> bool {
    unsafe { ACTIVE_PERMISSION_HELPER != PERMISSION_NONE || PERMISSION_SETUP_WINDOW != nil }
}

#[allow(deprecated)]
fn update_permission_flow(accessibility_allowed: bool, microphone_allowed: bool) {
    unsafe {
        if ACTIVE_PERMISSION_HELPER == PERMISSION_MICROPHONE
            && MICROPHONE_REQUEST_FINISHED.swap(false, Ordering::SeqCst)
        {
            if MICROPHONE_REQUEST_GRANTED.load(Ordering::SeqCst) {
                ACTIVE_PERMISSION_HELPER = PERMISSION_NONE;
                close_permission_helper_panel();
                close_system_settings_window();
                close_permission_setup_window();
                MICROPHONE_RESTART_REQUIRED.store(true, Ordering::SeqCst);
                show_microphone_restart_prompt();
            } else {
                open_system_settings_privacy_pane("Privacy_Microphone");
                show_microphone_settings_helper_panel();
            }
            return;
        }

        let active_permission_allowed =
            (ACTIVE_PERMISSION_HELPER == PERMISSION_ACCESSIBILITY && accessibility_allowed)
                || (ACTIVE_PERMISSION_HELPER == PERMISSION_MICROPHONE && microphone_allowed);

        if active_permission_allowed {
            let microphone_was_allowed =
                ACTIVE_PERMISSION_HELPER == PERMISSION_MICROPHONE && microphone_allowed;
            ACTIVE_PERMISSION_HELPER = PERMISSION_NONE;
            close_permission_helper_panel();
            close_system_settings_window();
            if accessibility_allowed && microphone_allowed {
                close_permission_setup_window();
                if microphone_was_allowed {
                    MICROPHONE_RESTART_REQUIRED.store(true, Ordering::SeqCst);
                    show_microphone_restart_prompt();
                }
            } else {
                show_permission_setup_window();
            }
        }
    }
}

#[allow(deprecated)]
fn close_permission_helper_panel() {
    unsafe {
        if PERMISSION_HELPER_PANEL != nil {
            let _: () = msg_send![PERMISSION_HELPER_PANEL, close];
            PERMISSION_HELPER_PANEL = nil;
        }
    }
}

#[allow(deprecated)]
fn show_permission_setup_window() {
    unsafe {
        if PERMISSION_SETUP_WINDOW == nil {
            return;
        }
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];
        let _: () = msg_send![PERMISSION_SETUP_WINDOW, orderFrontRegardless];
    }
}

#[allow(deprecated)]
fn close_permission_setup_window() {
    unsafe {
        if PERMISSION_SETUP_WINDOW != nil {
            let _: () = msg_send![PERMISSION_SETUP_WINDOW, close];
            PERMISSION_SETUP_WINDOW = nil;
        }
    }
}

fn wait_for_system_settings_window() {
    for _ in 0..10 {
        if system_settings_window_frame().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(120));
    }
}

fn system_settings_window_frame() -> Option<(f64, f64, f64, f64)> {
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to tell process \"System Settings\" to get {position, size} of window 1",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut values = stdout
        .split(',')
        .filter_map(|value| value.trim().parse::<f64>().ok());
    Some((values.next()?, values.next()?, values.next()?, values.next()?))
}

fn close_system_settings_window() {
    let _ = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to set settings_running to exists process \"System Settings\"",
            "-e",
            "if settings_running then tell application \"System Settings\" to if (count of windows) > 0 then close window 1",
        ])
        .status();
}

#[allow(deprecated)]
fn show_microphone_restart_prompt() {
    unsafe {
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];

        let alert: id = msg_send![class!(NSAlert), alloc];
        let alert: id = msg_send![alert, init];
        let title = NSString::alloc(nil).init_str("Restart xhisperflow");
        let detail = NSString::alloc(nil).init_str(
            "Microphone access is enabled. Restart xhisperflow now so recording can use the new permission.",
        );
        let restart = NSString::alloc(nil).init_str("Restart Now");
        let later = NSString::alloc(nil).init_str("Later");
        let _: () = msg_send![alert, setMessageText: title];
        let _: () = msg_send![alert, setInformativeText: detail];
        let _: id = msg_send![alert, addButtonWithTitle: restart];
        let _: id = msg_send![alert, addButtonWithTitle: later];
        let response: i64 = msg_send![alert, runModal];
        let _: () = msg_send![alert, release];

        if response == 1000 {
            relaunch_current_app();
            let _: () = msg_send![app, terminate: nil];
        }
    }
}

fn relaunch_current_app() {
    let Ok(exe) = env::current_exe() else {
        return;
    };
    if let Some(app_bundle) = exe.ancestors().find(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    }) {
        let _ = std::process::Command::new("open")
            .arg("-n")
            .arg(app_bundle)
            .status();
        return;
    }

    let _ = std::process::Command::new(exe).spawn();
}

#[allow(deprecated)]
fn position_permission_helper_panel(panel: id) {
    unsafe {
        let frame: NSRect = msg_send![panel, frame];
        let screen: id = msg_send![class!(NSScreen), mainScreen];
        let screen_frame: NSRect = msg_send![screen, frame];
        let visible_frame: NSRect = msg_send![screen, visibleFrame];

        if let Some((settings_x, settings_y, settings_width, settings_height)) =
            system_settings_window_frame()
        {
            let x = settings_x + (settings_width - frame.size.width) / 2.0;
            let y = screen_frame.size.height - settings_y - settings_height + 12.0;
            let x = x.clamp(
                visible_frame.origin.x,
                visible_frame.origin.x + visible_frame.size.width - frame.size.width,
            );
            let y = y.clamp(
                visible_frame.origin.y,
                visible_frame.origin.y + visible_frame.size.height - frame.size.height,
            );
            let _: () = msg_send![panel, setFrameOrigin: NSPoint::new(x, y)];
            return;
        }

        let x = visible_frame.origin.x + (visible_frame.size.width - frame.size.width) / 2.0;
        let y = visible_frame.origin.y + 20.0;
        let _: () = msg_send![panel, setFrameOrigin: NSPoint::new(x, y)];
    }
}

#[allow(deprecated)]
fn show_permission_drag_helper_panel(permission: &str, instruction: &str) {
    unsafe {
        wait_for_system_settings_window();
        close_permission_helper_panel();

        let title = format!("Add xhisperflow to {permission}");
        let panel = make_setup_window(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(620.0, 128.0)),
            &title,
        );
        let _: () = msg_send![panel, setMovableByWindowBackground: NO];
        let _: () = msg_send![panel, setLevel: 25_i64];
        let content: id = msg_send![panel, contentView];
        set_view_background(content, ns_color(0.13, 0.13, 0.15, 0.96), 0.0);

        let arrow = make_arrow_icon(62.0, 66.0);
        let instruction = make_label(
            instruction,
            116.0,
            72.0,
            420.0,
            24.0,
            14.0,
            true,
        );
        let fallback = make_label(
            "If dragging is blocked, click + and choose /Applications/xhisperflow.app.",
            116.0,
            52.0,
            430.0,
            20.0,
            11.5,
            false,
        );
        let drag_tile = make_draggable_app_tile(88.0, 12.0, 444.0, 34.0);

        add_subviews(content, &[arrow, instruction, fallback, drag_tile]);
        position_permission_helper_panel(panel);
        let _: id = msg_send![panel, retain];
        PERMISSION_HELPER_PANEL = panel;
        let _: () = msg_send![panel, orderFrontRegardless];
    }
}

#[allow(deprecated)]
fn show_microphone_prompt_helper_panel() {
    show_permission_instruction_helper_panel(
        "Allow Microphone Access",
        "Use the macOS prompt to allow microphone access",
        "If you choose Don't Allow, you can turn it on in System Settings.",
        false,
    );
}

#[allow(deprecated)]
fn show_microphone_settings_helper_panel() {
    wait_for_system_settings_window();
    unsafe {
        close_permission_helper_panel();

        let panel = make_setup_window(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(620.0, 134.0)),
            "Enable Microphone Access",
        );
        let _: () = msg_send![panel, setMovableByWindowBackground: NO];
        let _: () = msg_send![panel, setLevel: 25_i64];
        let content: id = msg_send![panel, contentView];
        set_view_background(content, ns_color(0.13, 0.13, 0.15, 0.96), 0.0);

        let arrow = make_arrow_icon(62.0, 64.0);
        let instruction = make_label(
            "Turn on xhisperflow in the Microphone list above",
            116.0,
            80.0,
            430.0,
            24.0,
            14.0,
            true,
        );
        let detail = make_label(
            "After enabling it, restart xhisperflow so recording can use the new permission.",
            116.0,
            58.0,
            430.0,
            20.0,
            11.5,
            false,
        );
        let restart = make_controller_button(
            "Restart xhisperflow",
            233.0,
            18.0,
            154.0,
            30.0,
            sel!(restartXhisperflow:),
        );

        add_subviews(content, &[arrow, instruction, detail, restart]);
        position_permission_helper_panel(panel);
        let _: id = msg_send![panel, retain];
        PERMISSION_HELPER_PANEL = panel;
        let _: () = msg_send![panel, orderFrontRegardless];
    }
}

#[allow(deprecated)]
fn show_permission_instruction_helper_panel(
    title: &str,
    instruction: &str,
    detail: &str,
    align_to_settings: bool,
) {
    unsafe {
        close_permission_helper_panel();

        let panel = make_setup_window(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(620.0, 112.0)),
            title,
        );
        let _: () = msg_send![panel, setMovableByWindowBackground: NO];
        let _: () = msg_send![panel, setLevel: 25_i64];
        let content: id = msg_send![panel, contentView];
        set_view_background(content, ns_color(0.13, 0.13, 0.15, 0.96), 0.0);

        let arrow = make_arrow_icon(62.0, 42.0);
        let instruction = make_label(instruction, 116.0, 58.0, 430.0, 24.0, 14.0, true);
        let detail = make_label(detail, 116.0, 36.0, 430.0, 20.0, 11.5, false);

        add_subviews(content, &[arrow, instruction, detail]);
        if align_to_settings {
            position_permission_helper_panel(panel);
        } else {
            let _: () = msg_send![panel, center];
        }
        let _: id = msg_send![panel, retain];
        PERMISSION_HELPER_PANEL = panel;
        let _: () = msg_send![panel, orderFrontRegardless];
    }
}

#[allow(deprecated)]
fn make_arrow_icon(x: f64, y: f64) -> id {
    unsafe {
        let image: id = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: NSString::alloc(nil).init_str("arrow.up")
            accessibilityDescription: nil
        ];
        if image != nil {
            let image_view: id = msg_send![class!(NSImageView), imageViewWithImage: image];
            let _: () = msg_send![
                image_view,
                setFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(34.0, 34.0))
            ];
            let blue: id = msg_send![class!(NSColor), systemBlueColor];
            let _: () = msg_send![image_view, setContentTintColor: blue];
            image_view
        } else {
            let label = make_label("^", x, y, 34.0, 34.0, 26.0, true);
            let blue: id = msg_send![class!(NSColor), systemBlueColor];
            let _: () = msg_send![label, setTextColor: blue];
            label
        }
    }
}

#[allow(deprecated)]
fn make_draggable_app_tile(x: f64, y: f64, width: f64, height: f64) -> id {
    unsafe {
        let class = draggable_app_view_class();
        let tile: id = msg_send![class, alloc];
        let tile: id = msg_send![
            tile,
            initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
        ];
        set_view_background(tile, ns_color(0.16, 0.16, 0.18, 1.0), 6.0);

        let icon = make_app_icon_view(10.0, 4.0, 26.0, 26.0);
        let label = make_label("xhisperflow.app", 48.0, 7.0, width - 60.0, 20.0, 13.0, false);
        add_subviews(tile, &[icon, label]);
        tile
    }
}

fn draggable_app_view_class() -> *const Class {
    static INIT: Once = Once::new();
    static mut CLASS: *const Class = std::ptr::null();

    unsafe {
        INIT.call_once(|| {
            if let Some(existing) = Class::get("XhisperflowDraggableAppView") {
                CLASS = existing;
                return;
            }

            let superclass = class!(NSView);
            if let Some(mut decl) = ClassDecl::new("XhisperflowDraggableAppView", superclass) {
                decl.add_method(
                    sel!(hitTest:),
                    draggable_app_hit_test as extern "C" fn(&Object, Sel, NSPoint) -> id,
                );
                decl.add_method(
                    sel!(acceptsFirstMouse:),
                    draggable_app_accepts_first_mouse as extern "C" fn(&Object, Sel, id) -> Boolean,
                );
                decl.add_method(
                    sel!(mouseDown:),
                    drag_xhisperflow_app as extern "C" fn(&Object, Sel, id),
                );
                CLASS = decl.register();
            }
        });
        CLASS
    }
}

extern "C" fn draggable_app_hit_test(view: &Object, _: Sel, point: NSPoint) -> id {
    unsafe {
        let bounds: NSRect = msg_send![view, bounds];
        let inside = point.x >= bounds.origin.x
            && point.x <= bounds.origin.x + bounds.size.width
            && point.y >= bounds.origin.y
            && point.y <= bounds.origin.y + bounds.size.height;
        if inside { view as *const Object as id } else { nil }
    }
}

extern "C" fn draggable_app_accepts_first_mouse(_: &Object, _: Sel, _: id) -> Boolean {
    1
}

extern "C" fn drag_xhisperflow_app(view: &Object, _: Sel, event: id) {
    unsafe {
        let path = NSString::alloc(nil).init_str("/Applications/xhisperflow.app");
        let bounds: NSRect = msg_send![view, bounds];
        let _: Boolean = msg_send![
            view,
            dragFile: path
            fromRect: bounds
            slideBack: YES
            event: event
        ];
    }
}

fn open_system_settings_privacy_pane(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    let _ = std::process::Command::new("open").arg(url).status();
}

fn append_log_line(message: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(LOG_PATH) {
        let _ = writeln!(file, "{message}");
    }
}

fn paste_text(config: &Config, text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new().context("failed to access clipboard")?;
    let saved = clipboard.get_text().ok();
    clipboard
        .set_text(text.to_string())
        .context("failed to copy transcript")?;

    let mut enigo = Enigo::new(&Settings::default()).context("failed to create input simulator")?;
    enigo
        .key(Key::Meta, Press)
        .context("failed to press Command key")?;
    let paste_result = enigo
        .key(Key::Unicode('v'), Click)
        .context("failed to press V key");
    let release_result = enigo
        .key(Key::Meta, Release)
        .context("failed to release Command key");
    paste_result.and(release_result)?;

    if let Some(saved) = saved {
        let delay = config.clipboard_restore_delay_secs;
        thread::spawn(move || {
            sleep_secs(delay);
            if let Ok(mut clipboard) = Clipboard::new() {
                let _ = clipboard.set_text(saved);
            }
        });
    }

    Ok(())
}

fn login_agent_enabled() -> bool {
    login_agent_path().exists()
}

fn enable_start_at_login() -> Result<()> {
    let path = login_agent_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create LaunchAgents directory")?;
    }
    fs::write(&path, login_agent_plist()?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn disable_start_at_login() -> Result<()> {
    let path = login_agent_path();
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn login_agent_path() -> PathBuf {
    home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LOGIN_AGENT_LABEL}.plist"))
}

fn login_agent_plist() -> Result<String> {
    let args = login_program_arguments()?;
    let args = args
        .iter()
        .map(|arg| format!("    <string>{}</string>", xml_escape(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{}</string>
  <key>ProgramArguments</key>
  <array>
{}
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#,
        LOGIN_AGENT_LABEL, args
    ))
}

fn login_program_arguments() -> Result<Vec<String>> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    if let Some(app_bundle) = exe.ancestors().find(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    }) {
        return Ok(vec![
            "/usr/bin/open".to_string(),
            "-a".to_string(),
            app_bundle.display().to_string(),
        ]);
    }
    Ok(vec![login_launcher_path(&exe)?.display().to_string()])
}

fn login_launcher_path(exe: &Path) -> Result<PathBuf> {
    let launcher = home_dir()
        .join("Library")
        .join("Application Support")
        .join("xhisperflow")
        .join("xhisperflow");
    if let Some(parent) = launcher.parent() {
        fs::create_dir_all(parent)
            .context("failed to create xhisperflow application support directory")?;
    }

    if launcher.exists() {
        fs::remove_file(&launcher)
            .with_context(|| format!("failed to replace {}", launcher.display()))?;
    }
    std::os::unix::fs::symlink(exe, &launcher)
        .with_context(|| format!("failed to create {}", launcher.display()))?;
    Ok(launcher)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn parse_hotkey_binding(value: &str) -> Result<MacHotKey> {
    parse_standard_hotkey(value)
        .map(MacHotKey::Standard)
        .or_else(|standard_err| {
            parse_modifier_only_hotkey(value)
                .map(MacHotKey::ModifierOnly)
                .ok_or(standard_err)
        })
        .with_context(|| format!("invalid hotkey '{value}'"))
}

fn parse_standard_hotkey(value: &str) -> Result<HotKey> {
    let normalized = value
        .split('+')
        .map(normalize_hotkey_token)
        .collect::<Vec<_>>()
        .join("+");

    normalized
        .parse::<HotKey>()
        .or_else(|_| {
            if normalized == "alt+space" {
                Ok(HotKey::new(Some(Modifiers::ALT), Code::Space))
            } else {
                normalized.parse::<HotKey>()
            }
        })
        .with_context(|| format!("invalid standard hotkey '{value}'"))
}

fn parse_modifier_only_hotkey(value: &str) -> Option<Modifiers> {
    let mut mods = Modifiers::empty();
    let mut found_modifier = false;

    for token in value.split('+').map(normalize_hotkey_token) {
        let modifier = match token.as_str() {
            "alt" => Modifiers::ALT,
            "ctrl" => Modifiers::CONTROL,
            "shift" => Modifiers::SHIFT,
            "super" => Modifiers::SUPER,
            _ => return None,
        };
        mods |= modifier;
        found_modifier = true;
    }

    found_modifier.then_some(mods)
}

fn normalize_hotkey_token(part: &str) -> String {
    match part.trim().to_ascii_lowercase().as_str() {
        "option" | "opt" | "alt" => "alt".to_string(),
        "cmd" | "command" | "meta" | "super" => "super".to_string(),
        "ctrl" | "control" => "ctrl".to_string(),
        "shift" => "shift".to_string(),
        "space" => "space".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_hotkey_binding() {
        assert!(matches!(
            parse_hotkey_binding("ctrl+opt+space").unwrap(),
            MacHotKey::Standard(_)
        ));
    }

    #[test]
    fn parses_modifier_only_hotkey_binding() {
        assert_eq!(
            parse_hotkey_binding("ctrl+opt").unwrap(),
            MacHotKey::ModifierOnly(Modifiers::CONTROL | Modifiers::ALT)
        );
        assert_eq!(
            parse_hotkey_binding("ctrl+shift").unwrap(),
            MacHotKey::ModifierOnly(Modifiers::CONTROL | Modifiers::SHIFT)
        );
    }
}

fn parse_optional_hotkey_binding(value: &str) -> Result<Option<MacHotKey>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    parse_hotkey_binding(value).map(Some)
}

fn order_independent_escape_mods(hotkey: HotKey) -> Option<Modifiers> {
    (hotkey.key == Code::Escape && !hotkey.mods.is_empty()).then_some(hotkey.mods)
}

#[derive(Default)]
struct ModifierTapState {
    escape_down: bool,
    last_modifier_mods: Option<Modifiers>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ModifierTapHotKeys {
    toggle_escape_mods: Option<Modifiers>,
    cancel_escape_mods: Option<Modifiers>,
    toggle_modifier_mods: Option<Modifiers>,
    cancel_modifier_mods: Option<Modifiers>,
}

impl ModifierTapHotKeys {
    fn has_bindings(self) -> bool {
        self.toggle_escape_mods.is_some()
            || self.cancel_escape_mods.is_some()
            || self.toggle_modifier_mods.is_some()
            || self.cancel_modifier_mods.is_some()
    }
}

fn start_modifier_event_tap(proxy: EventLoopProxy<UserEvent>, hotkeys: ModifierTapHotKeys) {
    thread::spawn(move || {
        let state = Arc::new(Mutex::new(ModifierTapState::default()));
        let tap_state = state.clone();
        let callback_proxy = proxy.clone();
        let event_tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            vec![
                CGEventType::KeyDown,
                CGEventType::KeyUp,
                CGEventType::FlagsChanged,
            ],
            move |proxy_ref, event_type, event| {
                handle_escape_modifier_tap_event(
                    proxy_ref,
                    event_type,
                    event,
                    &callback_proxy,
                    &tap_state,
                    hotkeys,
                )
            },
        );

        let Ok(event_tap) = event_tap else {
            append_log_line("macOS modifier hotkey listener could not install event tap");
            let _ = proxy.send_event(UserEvent::ModifierTapFailed(
                "could not install macOS event tap".to_string(),
            ));
            return;
        };

        let loop_source = match event_tap.mach_port().create_runloop_source(0) {
            Ok(loop_source) => loop_source,
            Err(_) => {
                append_log_line("macOS modifier hotkey listener could not attach run loop source");
                let _ = proxy.send_event(UserEvent::ModifierTapFailed(
                    "could not attach event tap to run loop".to_string(),
                ));
                return;
            }
        };
        CFRunLoop::get_current().add_source(&loop_source, unsafe {
            core_foundation::runloop::kCFRunLoopCommonModes
        });
        event_tap.enable();
        append_log_line("macOS modifier hotkey listener installed");
        let _ = proxy.send_event(UserEvent::ModifierTapReady);
        CFRunLoop::run_current();
        append_log_line("macOS modifier hotkey listener run loop stopped");
        let _ = proxy.send_event(UserEvent::ModifierTapFailed(
            "macOS event tap stopped".to_string(),
        ));
    });
}

fn handle_escape_modifier_tap_event(
    _proxy_ref: CGEventTapProxy,
    event_type: CGEventType,
    event: &CGEvent,
    proxy: &EventLoopProxy<UserEvent>,
    state: &Arc<Mutex<ModifierTapState>>,
    hotkeys: ModifierTapHotKeys,
) -> CallbackResult {
    let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
    let mods = modifiers_from_event_flags(event.get_flags());

    match event_type {
        CGEventType::KeyDown if keycode == KeyCode::ESCAPE => {
            if event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) == 0 {
                if let Ok(mut state) = state.lock() {
                    state.escape_down = true;
                }
                send_escape_modifier_event(proxy, mods, hotkeys);
            }
        }
        CGEventType::KeyUp if keycode == KeyCode::ESCAPE => {
            if let Ok(mut state) = state.lock() {
                state.escape_down = false;
            }
        }
        CGEventType::FlagsChanged => {
            if let Ok(mut state) = state.lock() {
                if state.escape_down && !mods.is_empty() {
                    send_escape_modifier_event(proxy, mods, hotkeys);
                }
                send_modifier_only_event(proxy, mods, hotkeys, &mut state);
            }
        }
        _ => {}
    }

    CallbackResult::Keep
}

fn send_escape_modifier_event(
    proxy: &EventLoopProxy<UserEvent>,
    mods: Modifiers,
    hotkeys: ModifierTapHotKeys,
) {
    if Some(mods) == hotkeys.cancel_escape_mods {
        let _ = proxy.send_event(UserEvent::OrderIndependentCancelHotKey);
    } else if Some(mods) == hotkeys.toggle_escape_mods {
        let _ = proxy.send_event(UserEvent::OrderIndependentHotKey);
    }
}

fn send_modifier_only_event(
    proxy: &EventLoopProxy<UserEvent>,
    mods: Modifiers,
    hotkeys: ModifierTapHotKeys,
    state: &mut ModifierTapState,
) {
    if state.last_modifier_mods == Some(mods) {
        return;
    }
    state.last_modifier_mods = (!mods.is_empty()).then_some(mods);

    if Some(mods) == hotkeys.cancel_modifier_mods {
        append_log_line(&format!("matched modifier-only cancel hotkey: {mods:?}"));
        let _ = proxy.send_event(UserEvent::ModifierOnlyCancelHotKey);
    } else if Some(mods) == hotkeys.toggle_modifier_mods {
        append_log_line(&format!("matched modifier-only toggle hotkey: {mods:?}"));
        let _ = proxy.send_event(UserEvent::ModifierOnlyHotKey);
    }
}

fn modifiers_from_event_flags(flags: CGEventFlags) -> Modifiers {
    let mut mods = Modifiers::empty();
    if flags.contains(CGEventFlags::CGEventFlagShift) {
        mods |= Modifiers::SHIFT;
    }
    if flags.contains(CGEventFlags::CGEventFlagControl) {
        mods |= Modifiers::CONTROL;
    }
    if flags.contains(CGEventFlags::CGEventFlagAlternate) {
        mods |= Modifiers::ALT;
    }
    if flags.contains(CGEventFlags::CGEventFlagCommand) {
        mods |= Modifiers::SUPER;
    }
    mods
}

fn draw_waveform(
    buffer: &mut softbuffer::Buffer<'_, OwnedDisplayHandle, Rc<Window>>,
    size: PhysicalSize<u32>,
    levels: &[f32],
    state: AppState,
    gradient_start: HudColor,
    gradient_end: HudColor,
) {
    let width = size.width.max(1);
    let height = size.height.max(1);
    for pixel in buffer.iter_mut() {
        *pixel = rgb(0, 0, 0);
    }

    let waveform_bottom = height.saturating_sub(WAVEFORM_BOTTOM_PADDING).max(1);
    let waveform_top = waveform_bottom.saturating_sub(WAVEFORM_HEIGHT);
    let center = waveform_top + (waveform_bottom.saturating_sub(waveform_top) / 2);

    let left = 42_u32;
    let right = width.saturating_sub(42);
    let bar_width = 3_u32;
    let gap = 5_u32;
    let stride = bar_width + gap;
    let drawable_width = right.saturating_sub(left).max(1);
    let bar_count = (drawable_width / stride).max(1);

    for bar_index in 0..bar_count {
        let x = left + bar_index * stride;
        let progress = bar_index as f32 / bar_count.saturating_sub(1).max(1) as f32;
        let color = gradient_start.mix(gradient_end, progress).to_pixel();
        let raw_level = if levels.is_empty() {
            0.18
        } else {
            let idx = (bar_index as usize * levels.len() / bar_count as usize)
                .min(levels.len().saturating_sub(1));
            let response = match state {
                AppState::Recording => 1.8,
                AppState::Transcribing => 0.9,
                AppState::Pasting => 0.55,
                AppState::Idle => 0.35,
            };
            (levels[idx].sqrt() * response).clamp(0.0, 1.0)
        };
        let level = shape_waveform_level(raw_level);
        let distance_from_center = ((progress - 0.5).abs() * 2.0).clamp(0.0, 1.0);
        let taper = 1.0 - distance_from_center * 0.62;
        let bar_height = (4.0 + level * taper * WAVEFORM_HEIGHT as f32).round() as u32;
        let y = center.saturating_sub(bar_height / 2);
        draw_waveform_bar(buffer, width, x, y, bar_width, bar_height, color);
    }
}

fn shape_waveform_level(level: f32) -> f32 {
    let normalized = ((level - WAVEFORM_LEVEL_FLOOR)
        / (WAVEFORM_LEVEL_CEILING - WAVEFORM_LEVEL_FLOOR))
        .clamp(0.0, 1.0);
    normalized * normalized * (3.0 - 2.0 * normalized)
}

fn draw_waveform_bar(
    buffer: &mut softbuffer::Buffer<'_, OwnedDisplayHandle, Rc<Window>>,
    width: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: u32,
) {
    if h <= 2 {
        draw_rect(buffer, width, x, y, w, h, color);
        return;
    }

    draw_rect(
        buffer,
        width,
        x + 1,
        y,
        w.saturating_sub(2).max(1),
        1,
        color,
    );
    draw_rect(buffer, width, x, y + 1, w, h.saturating_sub(2), color);
    draw_rect(
        buffer,
        width,
        x + 1,
        y + h.saturating_sub(1),
        w.saturating_sub(2).max(1),
        1,
        color,
    );
}

fn position_hud_at_notch(window: &Window) {
    let Ok(handle) = window.window_handle() else {
        return;
    };

    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };

    unsafe {
        let Some(screen) = screen_containing_mouse() else {
            return;
        };
        let view = handle.ns_view.as_ptr() as id;
        let ns_window: id = msg_send![view, window];
        if ns_window == nil {
            return;
        }

        let screen_frame: NSRect = msg_send![screen, frame];
        let window_frame: NSRect = msg_send![ns_window, frame];
        let window_width = if window_frame.size.width > 0.0 {
            window_frame.size.width
        } else {
            f64::from(HUD_WIDTH)
        };
        let x = screen_frame.origin.x + ((screen_frame.size.width - window_width) / 2.0).max(0.0);
        let y = screen_frame.origin.y + screen_frame.size.height - f64::from(HUD_TOP_OFFSET);
        let _: () = msg_send![ns_window, setFrameTopLeftPoint: NSPoint::new(x, y)];
    }
}

#[allow(deprecated, unexpected_cfgs)]
unsafe fn screen_containing_mouse() -> Option<id> {
    let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    let screens: id = msg_send![class!(NSScreen), screens];
    if screens != nil {
        let count: usize = msg_send![screens, count];
        for idx in 0..count {
            let screen: id = msg_send![screens, objectAtIndex: idx];
            if screen == nil {
                continue;
            }
            let frame: NSRect = msg_send![screen, frame];
            if point_in_rect(mouse, frame) {
                return Some(screen);
            }
        }
    }

    let main_screen: id = msg_send![class!(NSScreen), mainScreen];
    if main_screen == nil {
        None
    } else {
        Some(main_screen)
    }
}

#[allow(deprecated)]
fn point_in_rect(point: NSPoint, rect: NSRect) -> bool {
    point.x >= rect.origin.x
        && point.x <= rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y <= rect.origin.y + rect.size.height
}

#[allow(deprecated, unexpected_cfgs)]
fn apply_notch_window_shape(window: &Window) {
    let Ok(handle) = window.window_handle() else {
        return;
    };

    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };

    unsafe {
        let view = handle.ns_view.as_ptr() as id;
        let ns_window: id = msg_send![view, window];
        if ns_window != nil {
            let clear = NSColor::clearColor(nil);
            let level: isize = 25;
            let _: () = msg_send![ns_window, setOpaque: NO];
            let _: () = msg_send![ns_window, setBackgroundColor: clear];
            let _: () = msg_send![ns_window, setHasShadow: NO];
            let _: () = msg_send![ns_window, setLevel: level];
        }

        let _: () = msg_send![view, setWantsLayer: YES];
        let layer: id = msg_send![view, layer];
        if layer != nil {
            let _: () = msg_send![layer, setMasksToBounds: YES];
            let mask = create_notch_mask_layer(f64::from(HUD_WIDTH), f64::from(HUD_HEIGHT));
            if mask != nil {
                let _: () = msg_send![layer, setMask: mask];
            }
        }
    }
}

#[allow(deprecated, unexpected_cfgs)]
unsafe fn create_notch_mask_layer(width: f64, height: f64) -> id {
    let bottom_radius = HUD_BOTTOM_RADIUS;
    let shoulder_y = HUD_SHOULDER_Y;
    let shoulder_inset = HUD_SHOULDER_INSET;
    let path: id = msg_send![class!(NSBezierPath), bezierPath];
    if path == nil {
        return nil;
    }

    let _: () = msg_send![path, moveToPoint: NSPoint::new(0.0, 0.0)];
    let _: () = msg_send![path, lineToPoint: NSPoint::new(width, 0.0)];
    let _: () = msg_send![
        path,
        curveToPoint: NSPoint::new(width - shoulder_inset, shoulder_y)
        controlPoint1: NSPoint::new(width - shoulder_inset * 0.18, 0.0)
        controlPoint2: NSPoint::new(width - shoulder_inset, shoulder_y * 0.42)
    ];
    let _: () = msg_send![
        path,
        lineToPoint: NSPoint::new(width - shoulder_inset, height - bottom_radius)
    ];
    let _: () = msg_send![
        path,
        curveToPoint: NSPoint::new(width - shoulder_inset - bottom_radius, height)
        controlPoint1: NSPoint::new(width - shoulder_inset, height - bottom_radius * 0.45)
        controlPoint2: NSPoint::new(width - shoulder_inset - bottom_radius * 0.45, height)
    ];
    let _: () = msg_send![
        path,
        lineToPoint: NSPoint::new(shoulder_inset + bottom_radius, height)
    ];
    let _: () = msg_send![
        path,
        curveToPoint: NSPoint::new(shoulder_inset, height - bottom_radius)
        controlPoint1: NSPoint::new(shoulder_inset + bottom_radius * 0.45, height)
        controlPoint2: NSPoint::new(shoulder_inset, height - bottom_radius * 0.45)
    ];
    let _: () = msg_send![
        path,
        lineToPoint: NSPoint::new(shoulder_inset, shoulder_y)
    ];
    let _: () = msg_send![
        path,
        curveToPoint: NSPoint::new(0.0, 0.0)
        controlPoint1: NSPoint::new(shoulder_inset, shoulder_y * 0.42)
        controlPoint2: NSPoint::new(shoulder_inset * 0.18, 0.0)
    ];
    let _: () = msg_send![path, closePath];

    let cg_path: *const c_void = msg_send![path, CGPath];
    if cg_path.is_null() {
        return nil;
    }

    let mask: id = msg_send![class!(CAShapeLayer), layer];
    if mask != nil {
        let _: () = msg_send![mask, setPath: cg_path];
    }
    mask
}

fn draw_rect(
    buffer: &mut softbuffer::Buffer<'_, OwnedDisplayHandle, Rc<Window>>,
    width: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: u32,
) {
    let buffer_width = width as usize;
    let buffer_len = buffer.len();
    for row in y..y.saturating_add(h) {
        let start = row as usize * buffer_width + x as usize;
        if start >= buffer_len {
            break;
        }
        let end = (start + w as usize).min(buffer_len);
        for pixel in &mut buffer[start..end] {
            *pixel = color;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct HudColor {
    r: u8,
    g: u8,
    b: u8,
}

impl HudColor {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    fn mix(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self {
            r: mix_channel(self.r, other.r, amount),
            g: mix_channel(self.g, other.g, amount),
            b: mix_channel(self.b, other.b, amount),
        }
    }

    fn to_pixel(self) -> u32 {
        rgb(u32::from(self.r), u32::from(self.g), u32::from(self.b))
    }
}

fn parse_hex_color(value: &str, fallback: HudColor) -> HudColor {
    let value = value.trim().trim_matches('"').trim_start_matches('#');
    if value.len() != 6 {
        return fallback;
    }

    let Ok(parsed) = u32::from_str_radix(value, 16) else {
        return fallback;
    };

    HudColor::new(
        ((parsed >> 16) & 0xff) as u8,
        ((parsed >> 8) & 0xff) as u8,
        (parsed & 0xff) as u8,
    )
}

fn mix_channel(start: u8, end: u8, amount: f32) -> u8 {
    (start as f32 + (end as f32 - start as f32) * amount).round() as u8
}

fn rgb(r: u32, g: u32, b: u32) -> u32 {
    b | (g << 8) | (r << 16)
}

fn tray_icon() -> Result<Icon> {
    let width = 22_usize;
    let height = 18_usize;
    let mut rgba = vec![0_u8; width * height * 4];
    let bars = [2, 4, 6, 11, 14, 11, 8, 5, 7, 5, 3];

    for (bar_index, bar_height) in bars.iter().enumerate() {
        let x = 1 + bar_index * 2;
        let top = (height - *bar_height) / 2;
        for y in top..top + *bar_height {
            set_icon_pixel(&mut rgba, width, x, y);
        }
    }

    Icon::from_rgba(rgba, width as u32, height as u32).context("failed to build tray icon")
}

fn set_icon_pixel(rgba: &mut [u8], width: usize, x: usize, y: usize) {
    let idx = (y * width + x) * 4;
    if idx + 3 >= rgba.len() {
        return;
    }
    rgba[idx] = 255;
    rgba[idx + 1] = 255;
    rgba[idx + 2] = 255;
    rgba[idx + 3] = 255;
}
