use crate::waveform::{
    HUD_HEIGHT, HUD_WIDTH, SharedLevels, WaveformColor, WaveformMode, WaveformStyle, draw_waveform,
    snapshot,
};
use anyhow::{Context, Result, anyhow};
use std::env;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum HudMode {
    Listening,
    Processing,
}

pub struct LinuxHud {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LinuxHud {
    pub fn spawn_recording(
        levels: SharedLevels,
        gradient_start: WaveformColor,
        gradient_end: WaveformColor,
    ) -> Result<Self> {
        Self::spawn(levels, gradient_start, gradient_end, HudMode::Listening)
    }

    pub fn spawn_processing(
        levels: SharedLevels,
        gradient_start: WaveformColor,
        gradient_end: WaveformColor,
    ) -> Result<Self> {
        Self::spawn(levels, gradient_start, gradient_end, HudMode::Processing)
    }

    fn spawn(
        levels: SharedLevels,
        gradient_start: WaveformColor,
        gradient_end: WaveformColor,
        mode: HudMode,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("xhisperflow-waveform".to_string())
            .spawn(move || {
                let result = run_hud(
                    levels,
                    gradient_start,
                    gradient_end,
                    mode,
                    stop_for_thread,
                    ready_tx.clone(),
                );
                if let Err(err) = result {
                    let _ = ready_tx.send(Err(format!("{err:#}")));
                    eprintln!("waveform HUD unavailable: {err:#}");
                }
            })
            .context("failed to start waveform HUD thread")?;

        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(anyhow!(err))
            }
            Err(err) => {
                stop.store(true, Ordering::Release);
                drop(thread);
                Err(anyhow!("waveform HUD startup timed out: {err}"))
            }
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for LinuxHud {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_hud(
    levels: SharedLevels,
    gradient_start: WaveformColor,
    gradient_end: WaveformColor,
    mode: HudMode,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    let wayland_requested = env::var_os("WAYLAND_DISPLAY").is_some()
        || env::var("XDG_SESSION_TYPE").is_ok_and(|value| value.eq_ignore_ascii_case("wayland"));

    if wayland_requested {
        match wayland::run(
            levels.clone(),
            gradient_start,
            gradient_end,
            mode,
            stop.clone(),
            ready.clone(),
        ) {
            Ok(()) => return Ok(()),
            Err(err) if env::var_os("DISPLAY").is_some() => {
                eprintln!("Wayland waveform HUD unavailable, trying X11: {err:#}");
            }
            Err(err) => return Err(err),
        }
    }

    if env::var_os("DISPLAY").is_some() {
        return x11::run(levels, gradient_start, gradient_end, mode, stop, ready);
    }

    Err(anyhow!("neither a Wayland nor X11 display is available"))
}

mod wayland {
    use super::*;
    use smithay_client_toolkit::{
        compositor::{CompositorHandler, CompositorState, Region},
        delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
        output::{OutputHandler, OutputState},
        registry::{ProvidesRegistryState, RegistryState},
        registry_handlers,
        shell::{
            WaylandSurface,
            wlr_layer::{
                Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
                LayerSurfaceConfigure,
            },
        },
        shm::{Shm, ShmHandler, slot::SlotPool},
    };
    use wayland_client::{
        Connection, QueueHandle,
        globals::registry_queue_init,
        protocol::{wl_output, wl_shm, wl_surface},
    };

    pub fn run(
        levels: SharedLevels,
        gradient_start: WaveformColor,
        gradient_end: WaveformColor,
        mode: HudMode,
        stop: Arc<AtomicBool>,
        ready: mpsc::Sender<Result<(), String>>,
    ) -> Result<()> {
        let connection = Connection::connect_to_env().context("failed to connect to Wayland")?;
        let (globals, mut event_queue) =
            registry_queue_init(&connection).context("failed to read Wayland globals")?;
        let queue_handle = event_queue.handle();
        let compositor = CompositorState::bind(&globals, &queue_handle)
            .context("wl_compositor is unavailable")?;
        let layer_shell =
            LayerShell::bind(&globals, &queue_handle).context("wlr layer-shell is unavailable")?;
        let shm = Shm::bind(&globals, &queue_handle).context("wl_shm is unavailable")?;

        let surface = compositor.create_surface(&queue_handle);
        let empty_input_region =
            Region::new(&compositor).context("failed to create Wayland input region")?;
        surface.set_input_region(Some(empty_input_region.wl_region()));
        let layer = layer_shell.create_layer_surface(
            &queue_handle,
            surface,
            Layer::Overlay,
            Some("xhisperflow-waveform"),
            None,
        );
        layer.set_anchor(Anchor::TOP);
        layer.set_margin(8, 0, 0, 0);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(HUD_WIDTH, HUD_HEIGHT);
        layer.commit();

        let pool = SlotPool::new((HUD_WIDTH * HUD_HEIGHT * 4) as usize, &shm)
            .context("failed to create Wayland shared-memory pool")?;
        let mut state = WaylandHud {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &queue_handle),
            shm,
            pool,
            layer,
            width: HUD_WIDTH,
            height: HUD_HEIGHT,
            first_configure: true,
            exit: false,
            stop,
            levels,
            gradient_start,
            gradient_end,
            mode,
            animation_started: Instant::now(),
            ready: Some(ready),
        };

        while !state.exit && !state.stop.load(Ordering::Acquire) {
            event_queue
                .blocking_dispatch(&mut state)
                .context("Wayland HUD event dispatch failed")?;
        }
        Ok(())
    }

    struct WaylandHud {
        registry_state: RegistryState,
        output_state: OutputState,
        shm: Shm,
        pool: SlotPool,
        layer: LayerSurface,
        width: u32,
        height: u32,
        first_configure: bool,
        exit: bool,
        stop: Arc<AtomicBool>,
        levels: SharedLevels,
        gradient_start: WaveformColor,
        gradient_end: WaveformColor,
        mode: HudMode,
        animation_started: Instant,
        ready: Option<mpsc::Sender<Result<(), String>>>,
    }

    impl WaylandHud {
        fn draw(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
            let width = self.width.max(1);
            let height = self.height.max(1);
            let needed = (width * height * 4) as usize;
            if self.pool.len() < needed {
                self.pool
                    .resize(needed)
                    .context("failed to resize Wayland shared-memory pool")?;
            }
            let stride = width as i32 * 4;
            let (buffer, canvas) = self
                .pool
                .create_buffer(
                    width as i32,
                    height as i32,
                    stride,
                    wl_shm::Format::Argb8888,
                )
                .context("failed to create Wayland waveform buffer")?;
            let mut pixels = vec![0_u32; (width * height) as usize];
            draw_waveform(
                &mut pixels,
                width,
                height,
                &snapshot(&self.levels),
                1.8,
                waveform_mode(self.mode, self.animation_started),
                WaveformStyle {
                    gradient_start: self.gradient_start,
                    gradient_end: self.gradient_end,
                    rounded_background: true,
                },
            );
            for (bytes, pixel) in canvas.chunks_exact_mut(4).zip(pixels) {
                bytes.copy_from_slice(&pixel.to_le_bytes());
            }

            let surface = self.layer.wl_surface();
            surface.damage_buffer(0, 0, width as i32, height as i32);
            surface.frame(queue_handle, surface.clone());
            buffer
                .attach_to(surface)
                .context("failed to attach Wayland waveform buffer")?;
            self.layer.commit();
            Ok(())
        }
    }

    impl CompositorHandler for WaylandHud {
        fn scale_factor_changed(
            &mut self,
            _connection: &Connection,
            _queue_handle: &QueueHandle<Self>,
            _surface: &wl_surface::WlSurface,
            _new_factor: i32,
        ) {
        }

        fn transform_changed(
            &mut self,
            _connection: &Connection,
            _queue_handle: &QueueHandle<Self>,
            _surface: &wl_surface::WlSurface,
            _new_transform: wl_output::Transform,
        ) {
        }

        fn frame(
            &mut self,
            _connection: &Connection,
            queue_handle: &QueueHandle<Self>,
            _surface: &wl_surface::WlSurface,
            _time: u32,
        ) {
            if self.stop.load(Ordering::Acquire) {
                self.exit = true;
            } else if let Err(err) = self.draw(queue_handle) {
                eprintln!("failed to draw Wayland waveform HUD: {err:#}");
                self.exit = true;
            }
        }

        fn surface_enter(
            &mut self,
            _connection: &Connection,
            _queue_handle: &QueueHandle<Self>,
            _surface: &wl_surface::WlSurface,
            _output: &wl_output::WlOutput,
        ) {
        }

        fn surface_leave(
            &mut self,
            _connection: &Connection,
            _queue_handle: &QueueHandle<Self>,
            _surface: &wl_surface::WlSurface,
            _output: &wl_output::WlOutput,
        ) {
        }
    }

    impl OutputHandler for WaylandHud {
        fn output_state(&mut self) -> &mut OutputState {
            &mut self.output_state
        }

        fn new_output(
            &mut self,
            _connection: &Connection,
            _queue_handle: &QueueHandle<Self>,
            _output: wl_output::WlOutput,
        ) {
        }

        fn update_output(
            &mut self,
            _connection: &Connection,
            _queue_handle: &QueueHandle<Self>,
            _output: wl_output::WlOutput,
        ) {
        }

        fn output_destroyed(
            &mut self,
            _connection: &Connection,
            _queue_handle: &QueueHandle<Self>,
            _output: wl_output::WlOutput,
        ) {
        }
    }

    impl LayerShellHandler for WaylandHud {
        fn closed(
            &mut self,
            _connection: &Connection,
            _queue_handle: &QueueHandle<Self>,
            _layer: &LayerSurface,
        ) {
            self.exit = true;
        }

        fn configure(
            &mut self,
            _connection: &Connection,
            queue_handle: &QueueHandle<Self>,
            _layer: &LayerSurface,
            configure: LayerSurfaceConfigure,
            _serial: u32,
        ) {
            if configure.new_size.0 > 0 && configure.new_size.1 > 0 {
                self.width = configure.new_size.0;
                self.height = configure.new_size.1;
            }
            if self.first_configure {
                self.first_configure = false;
                match self.draw(queue_handle) {
                    Ok(()) => {
                        if let Some(ready) = self.ready.take() {
                            let _ = ready.send(Ok(()));
                        }
                    }
                    Err(err) => {
                        if let Some(ready) = self.ready.take() {
                            let _ = ready.send(Err(format!("{err:#}")));
                        }
                        self.exit = true;
                    }
                }
            }
        }
    }

    impl ShmHandler for WaylandHud {
        fn shm_state(&mut self) -> &mut Shm {
            &mut self.shm
        }
    }

    delegate_compositor!(WaylandHud);
    delegate_output!(WaylandHud);
    delegate_shm!(WaylandHud);
    delegate_layer!(WaylandHud);
    delegate_registry!(WaylandHud);

    impl ProvidesRegistryState for WaylandHud {
        fn registry(&mut self) -> &mut RegistryState {
            &mut self.registry_state
        }

        registry_handlers![OutputState];
    }
}

mod x11 {
    use super::*;
    use softbuffer::{Context as SoftContext, Surface};
    use winit::{
        application::ApplicationHandler,
        dpi::{LogicalSize, PhysicalPosition},
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        platform::x11::{EventLoopBuilderExtX11, WindowAttributesExtX11, WindowType},
        window::{Window, WindowAttributes, WindowId, WindowLevel},
    };

    type HudSurface = Surface<winit::event_loop::OwnedDisplayHandle, Rc<Window>>;

    pub fn run(
        levels: SharedLevels,
        gradient_start: WaveformColor,
        gradient_end: WaveformColor,
        mode: HudMode,
        stop: Arc<AtomicBool>,
        ready: mpsc::Sender<Result<(), String>>,
    ) -> Result<()> {
        let mut builder = EventLoop::<()>::with_user_event();
        builder.with_x11().with_any_thread(true);
        let event_loop = builder.build().context("failed to connect to X11")?;
        let mut app = X11Hud {
            window: None,
            window_id: None,
            surface: None,
            stop,
            levels,
            gradient_start,
            gradient_end,
            mode,
            animation_started: Instant::now(),
            ready: Some(ready),
        };
        event_loop
            .run_app(&mut app)
            .context("X11 waveform event loop failed")
    }

    struct X11Hud {
        window: Option<Rc<Window>>,
        window_id: Option<WindowId>,
        surface: Option<HudSurface>,
        stop: Arc<AtomicBool>,
        levels: SharedLevels,
        gradient_start: WaveformColor,
        gradient_end: WaveformColor,
        mode: HudMode,
        animation_started: Instant,
        ready: Option<mpsc::Sender<Result<(), String>>>,
    }

    impl X11Hud {
        fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
            let attributes = WindowAttributes::default()
                .with_title("xhisperflow waveform")
                .with_inner_size(LogicalSize::new(
                    f64::from(HUD_WIDTH),
                    f64::from(HUD_HEIGHT),
                ))
                .with_resizable(false)
                .with_decorations(false)
                .with_transparent(true)
                .with_visible(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_override_redirect(true)
                .with_x11_window_type(vec![WindowType::Notification])
                .with_name("xhisperflow", "xhisperflow-waveform");
            let window = Rc::new(
                event_loop
                    .create_window(attributes)
                    .context("failed to create X11 waveform window")?,
            );
            let _ = window.set_cursor_hittest(false);
            if let Some(monitor) = event_loop.primary_monitor() {
                let monitor_size = monitor.size();
                let monitor_position = monitor.position();
                let x = monitor_position.x
                    + ((monitor_size.width as i32 - HUD_WIDTH as i32) / 2).max(0);
                window.set_outer_position(PhysicalPosition::new(x, monitor_position.y + 8));
            }

            let context = SoftContext::new(event_loop.owned_display_handle())
                .map_err(|err| anyhow!("failed to create X11 drawing context: {err:?}"))?;
            let surface = Surface::new(&context, window.clone())
                .map_err(|err| anyhow!("failed to create X11 drawing surface: {err:?}"))?;
            self.window_id = Some(window.id());
            self.surface = Some(surface);
            self.window = Some(window.clone());
            window.set_visible(true);
            window.request_redraw();
            Ok(())
        }

        fn draw(&mut self) -> Result<()> {
            let Some(window) = &self.window else {
                return Ok(());
            };
            let Some(surface) = self.surface.as_mut() else {
                return Ok(());
            };
            let size = window.inner_size();
            let width = NonZeroU32::new(size.width).ok_or_else(|| anyhow!("invalid HUD width"))?;
            let height =
                NonZeroU32::new(size.height).ok_or_else(|| anyhow!("invalid HUD height"))?;
            surface
                .resize(width, height)
                .map_err(|err| anyhow!("failed to resize X11 waveform surface: {err:?}"))?;
            let mut buffer = surface
                .buffer_mut()
                .map_err(|err| anyhow!("failed to acquire X11 waveform buffer: {err:?}"))?;
            draw_waveform(
                &mut buffer,
                size.width,
                size.height,
                &snapshot(&self.levels),
                1.8,
                waveform_mode(self.mode, self.animation_started),
                WaveformStyle {
                    gradient_start: self.gradient_start,
                    gradient_end: self.gradient_end,
                    rounded_background: true,
                },
            );
            buffer
                .present()
                .map_err(|err| anyhow!("failed to present X11 waveform buffer: {err:?}"))?;
            Ok(())
        }
    }

    impl ApplicationHandler<()> for X11Hud {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            match self.create_window(event_loop) {
                Ok(()) => {
                    if let Some(ready) = self.ready.take() {
                        let _ = ready.send(Ok(()));
                    }
                }
                Err(err) => {
                    if let Some(ready) = self.ready.take() {
                        let _ = ready.send(Err(format!("{err:#}")));
                    }
                    event_loop.exit();
                }
            }
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            window_id: WindowId,
            event: WindowEvent,
        ) {
            if Some(window_id) != self.window_id {
                return;
            }
            match event {
                WindowEvent::RedrawRequested => {
                    if let Err(err) = self.draw() {
                        eprintln!("failed to draw X11 waveform HUD: {err:#}");
                        event_loop.exit();
                    }
                }
                WindowEvent::CloseRequested => event_loop.exit(),
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.stop.load(Ordering::Acquire) {
                event_loop.exit();
                return;
            }
            event_loop.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(33)));
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }
}

fn waveform_mode(mode: HudMode, animation_started: Instant) -> WaveformMode {
    match mode {
        HudMode::Listening => WaveformMode::Listening,
        HudMode::Processing => WaveformMode::Processing {
            elapsed_seconds: animation_started.elapsed().as_secs_f32(),
        },
    }
}
