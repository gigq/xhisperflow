# xhisperflow

xhisperflow is a lightweight dictation app that records your voice, transcribes
it locally with a built-in Whisper worker on Linux and macOS (or with
Voxtype on Linux / Groq in the cloud), optionally cleans up the text with
`openai/gpt-oss-20b`, and inserts the final transcript into the app you were
using.

On macOS, xhisperflow runs as a menu bar app with a global hotkey, onboarding
for required permissions, and direct paste into the active app. On Linux, it
provides command-line tools that can be bound to your window manager or desktop
shortcuts for push-to-toggle dictation. Both platforms display the same floating
waveform HUD while recording.

![xhisperflow recording from the macOS menu bar](assets/docs/xhisperflow-in-use.png)

## Features

- Built-in resident Whisper worker: keeps the model and inference state loaded between recordings, with Vulkan on Linux and Metal on macOS. No Voxtype install or cloud key required.
- Optional local Voxtype transcription on Linux with the existing waveform, processing animation, hotkeys, and paste behavior.
- Cloud voice transcription using `whisper-large-v3-turbo`, with
  `whisper-large-v3` used for long recordings.
- Optional post-processing cleanup with `openai/gpt-oss-20b` for punctuation,
  capitalization, and wording.
- Global recording hotkeys
- Menu bar controls for recording, canceling, permissions, API key setup, and
  copying the last transcript.
- Floating waveform HUD while recording.
- Native Wayland and X11 waveform HUDs on Linux, with notification updates as a
  fallback.
- `--cancel` support for discarding a Linux recording without transcription.
- Wayland-first typing via `wtype`, with clipboard output mode available.
- `xhisperflowtool` and `xhisperflowtoold` helpers for uinput paste, typing,
  and wrap-key workflows.

## Build

```sh
cargo build --bins
```

This produces `target/debug/xhisperflow`, `target/debug/xhisperflowtool`, and
`target/debug/xhisperflowtoold`.

Native transcription requires CMake, a C/C++ compiler, and libclang at build
time. For Linux GPU acceleration, install Vulkan headers and `glslc`, then
build with `cargo build --release --features vulkan --bin xhisperflow`.
CPU transcription is included by default; `--no-default-features` omits Whisper.

## Built-in local transcription (Linux and macOS)

The resident worker supports Whisper, Parakeet TDT v3, and Qwen3-ASR 1.7B. Whisper can use Vulkan
or Metal; Parakeet currently runs on CPU and stays loaded between takes. Both
are compiled into the default build, including the same worker used by the Mac
menu bar app. No Voxtype executable or ONNX Runtime system install is needed.

To compare Parakeet with Whisper, install both models once and select one:

```sh
scripts/setup-local-transcription.sh large-v3-turbo
scripts/setup-local-transcription.sh parakeet
xhisperflow --select-model parakeet
xhisperflow --select-model whisper
```

`parakeet` selects the quantized `parakeet-tdt-0.6b-v3-int8` model. The full
`parakeet-tdt-0.6b-v3` export can also be installed and selected by that exact
name. Parakeet emits punctuation and capitalization, detects the spoken language
within its supported set, and transcribes speech in that language; it does not
translate it or use Whisper's text prompt. Model source:
[NVIDIA Parakeet TDT v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3).
The Parakeet installer uses Python 3.8+ and curl to download and verify the model
files against their size and SHA-256 manifest; neither is needed for inference.

The selector preserves other configuration, checks that the target model is
installed, and reloads the worker. On Linux it restarts the active user service;
otherwise it restarts the app's own worker. A failed load restores the previous
configuration. The Mac app reads the selected backend at the start of each take.
Status output includes `engine` as well as the model and resident PID.

Equivalent settings for Parakeet are:

```text
transcription-backend : "local"
local-engine : "parakeet"
local-model : "parakeet-tdt-0.6b-v3-int8"
```

Use `local-engine : "whisper"` with a Whisper model. `local-threads` controls
CPU threads; `local-gpu-device` applies to Whisper and Qwen.

To trial Qwen3-ASR locally without cleanup:

```sh
scripts/setup-local-transcription.sh qwen
xhisperflow --select-model qwen
# Return to the previous model:
xhisperflow --select-model whisper
```

Qwen uses the [transcribe.cpp runtime](https://github.com/handy-computer/transcribe.cpp)
and its [Q8_0 conversion of Qwen3-ASR-1.7B](https://huggingface.co/handy-computer/Qwen3-ASR-1.7B-gguf).
This is an 8-bit conversion of the original Qwen checkpoint, rather than running
the Transformers `-hf` package directly. The model is 2.19 GB on disk, verified
by SHA-256. The installer builds a pinned runtime with Vulkan on Linux or Metal
on Apple Silicon and installs it privately in the app's data directory. It needs
git, CMake, a C++17 compiler, curl, and Linux Vulkan headers/glslc at setup time;
there is no Python inference runtime or Voxtype dependency. macOS runtime
verification is still pending. Qwen uses `local-gpu-device` and `local-threads`,
accepts `local-language` and the context prompt, and stays loaded between takes.
Its resident child process isolates its ggml version from Whisper's. Selecting
another engine releases it. A failed or timed-out response produces an error;
the next take can restart the helper. No cloud fallback or cleanup is added.

### Optional local LLM cleanup

Qwen, Whisper large-v3-turbo, and Parakeet remain independently selectable.
Cleanup is a separate option, disabled by default. Install it, then preview a
piece of text without recording, pasting, or changing the speech model:

```sh
scripts/setup-local-cleanup.sh
xhisperflow --cleanup-text 'Um, I would, uh, like to keep Qwen enabled.'
xhisperflow --cleanup-status
# Enable for subsequent dictation:
xhisperflow --local-cleanup on
# Disable and unload the cleanup model:
xhisperflow --local-cleanup off
```

The preview returns JSON with `text`, `accepted`, `reason`, and `elapsed_ms`.
`accepted` means validation passed; an unchanged proposal may also pass. Rejected
proposals return the original text. The preview starts the resident cleanup
worker even when automatic cleanup is off; the `off` command unloads it.

This uses [VoiceInk's Qwen3.5-2B fine-tune](https://github.com/hourliert/VoiceInk-Qwen3.5-2B-FT)
at Q4_K_M (1.27 GB on disk) through a pinned, checksum-verified llama.cpp
runtime. There is no Ollama installation or cloud fallback. Both the app RPC and
model HTTP traffic use private Unix sockets, with reasoning, web UI, and tools
disabled. No clipboard or active-window context is collected. Model input and
output are not written to the cleanup worker log; the existing app transcript
logging remains as before.

The LLM proposes deletions. The app aligns that proposal to the source and
rebuilds from original words, retaining technical spelling, case, contractions,
and numeric formats. Added/substituted/reordered words, lost negations or
conditions, and uncorrected removals of protected terms/qualifiers are rejected.
Clear nearby spoken corrections and repeated starts have limited exceptions.
These checks cannot prove semantic fidelity: this is a conservative experimental
option, and ambiguous backtracking can remain unchanged. It does not polish
grammar, guess transcription mistakes, or convert dictated formatting commands.

Automatic cleanup has a default 750 ms deadline (`local-cleanup-timeout-ms`,
50–3000), and skips input over 3000 UTF-8 bytes. Cold startup, unavailable models,
errors, timeouts, and invalid edits pass through the original transcript.
`local-cleanup-enabled : true` selects this local path ahead of the legacy
cloud cleanup flags; it still respects `post-processing-enabled : false`.
On this machine, short warmed requests measured roughly 0.08–0.18 seconds,
with about 1.34 GiB extra VRAM and 0.69 GiB process RSS. Long/ambiguous examples
can be slower and rejected. Shared Mac code uses Metal; Mac runtime validation
remains pending.

Download a model once:

```sh
scripts/setup-local-transcription.sh large-v3-turbo
```

Set these in xhisperflowrc (or leave `auto`, which selects the built-in worker
when its model is installed):

```text
transcription-backend : "local"
local-engine : "whisper"
local-model : "large-v3-turbo"
local-language : "en"
```

Models live in `$XDG_DATA_HOME/xhisperflow/models` (normally
`~/.local/share/xhisperflow/models`) on Linux and
`~/Library/Application Support/xhisperflow/models` on macOS. `local-model` can
also be an absolute GGML `.bin` path. No model is bundled in the release archive.

The app starts a per-user worker from its own executable and warms it while you
record (at app launch on macOS). Recordings travel over a private Unix socket;
the worker keeps the model and inference state loaded across requests and
resamples native Mac microphone rates to 16 kHz. xhisperflow owns recording,
the waveform, cancellation, optional cleanup, and paste on both platforms.
Local failures are reported without silently uploading audio to a cloud service.
`local-post-processing-enabled : true` explicitly enables Groq cleanup if
`post-processing-enabled` is also true; the default is false, with no API-key
onboarding for local transcription on macOS.

Inspect or stop the worker:

```sh
xhisperflow --transcription-status
xhisperflow --stop-transcription-daemon
```

On macOS these flags also work on
`xhisperflow.app/Contents/MacOS/xhisperflow`, before the menu bar UI is started.
Stop/restart the worker after changing its model, language, GPU, or thread count.
On Linux, an optional login service preloads it before the first hotkey:

```sh
install -Dm644 scripts/xhisperflow-transcription.service ~/.config/systemd/user/xhisperflow-transcription.service
systemctl --user daemon-reload
systemctl --user enable --now xhisperflow-transcription.service
```

With that service installed, restart it with
`systemctl --user restart xhisperflow-transcription.service`. Stop any auto-started worker
before first enabling the service. macOS uses the same executable/worker
protocol; `scripts/build-macos-app.sh` enables Metal and embeds its shader library.

## Release builds

GitHub release builds run when a `v*` tag is pushed:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow publishes a Linux tarball and a universal macOS DMG.

## Runtime dependencies

- `pw-record`
- `notify-send` for notifications
- `arecord` for the live level meter
- `wtype` for direct Wayland typing
- `wl-copy` / `wl-paste` or `xclip` for clipboard support
- access to `/dev/uinput` when the helper daemon is used

## macOS

The macOS app is a native Rust menu bar app with a global recording hotkey,
floating waveform HUD, built-in local or Groq-hosted transcription, and paste into the active app.

Build the app bundle:

```sh
scripts/build-macos-app.sh
open target/xhisperflow.app
```

The app uses `Option+Space` by default. Configure it in:

```sh
~/Library/Application Support/xhisperflow/xhisperflowrc
```

macOS hotkeys can be standard key chords like `option+space` or modifier-only
chords like `ctrl+shift`. The default cancel hotkey is `Control+Shift`.

Use `cancel-hotkey` to bind a shortcut that discards the current recording
without transcription. Set it to an empty string to disable the cancel shortcut.

The floating HUD can be disabled with `floating-waveform : false`. Its colors
are configured with `waveform-gradient-start` and `waveform-gradient-end` using
quoted `#RRGGBB` values. The previous `mac-*` names remain accepted for existing
configurations.

Use the menu bar app's Start at Login item to install a per-user LaunchAgent
for the current app.

Required macOS permissions:

- Microphone, for recording.
- Accessibility, for pasting the final transcript with Command+V.

On launch, the app checks Accessibility access. If it is missing, xhisperflow
shows a setup prompt. Clicking Allow opens the Accessibility page in System
Settings and keeps a helper window visible with the app path to drag into the
permission list.

The menu bar app includes a Permissions Help item that opens the relevant
System Settings panes. If Accessibility paste is not allowed, the transcript is
left on the clipboard.

## Config

Copy [default_xhisperflowrc](./default_xhisperflowrc) to:

```sh
~/.config/xhisperflow/xhisperflowrc
```

`transcription-backend : "auto"` first prefers the built-in worker when its
model is installed. Otherwise, on Linux it prefers `voxtype` when executable
on `PATH`, then Groq. Set it to `"groq"` to keep the original cloud backend,
or `"voxtype"` to require the local CLI. macOS retains the existing Groq backend
and onboarding when the built-in model is not installed. Explicit `local` works
on both platforms; the external Voxtype adapter remains Linux-only.

Voxtype receives the completed WAV via `voxtype --quiet transcribe <file>`.
Its loading/audio-format/progress preamble is removed before cleanup or paste;
unrecognized output produces an error rather than inserting diagnostics.
xhisperflow still owns recording, its waveform and processing animation, cancel,
and pasting. It does not start Voxtype's daemon or its overlay. Voxtype uses its
own configured model and engine. File transcription loads the model per invocation,
so it may take longer than Voxtype's already-running daemon; benchmark on your host.
In auto mode only a missing executable selects Groq: a local transcription error
is reported, never silently retried against the cloud.

Groq still honors `post-processing-enabled`. The Voxtype path skips that cloud
cleanup by default; opt in with `voxtype-post-processing-enabled : true` while
`post-processing-enabled` is also true. `voxtype-timeout : 120` bounds the local
transcription process in seconds. A hung process is terminated and no text pasted.
Voxtype can itself be configured for a remote engine; this adapter does not override
its configuration.

`GROQ_API_KEY` is read from the environment or `~/.env` and is required only for
Groq transcription or the optional Groq cleanup. Existing Linux hotkey wrappers
that unconditionally require this key must be updated: `scripts/xhisperflow-toggle`
loads the existing environment and keeps the release-event debounce, then lets
xhisperflow validate the selected backend. Install it with:

```sh
install -Dm755 scripts/xhisperflow-toggle ~/.local/bin/xhisperflow-toggle
```

Control+Option and Control+Shift can keep their existing toggle/cancel bindings to
xhisperflow. The new backend does not require changing the bindings or deleting
any existing Groq credentials.

On Linux, bind `xhisperflow` to start or finish dictation and bind
`xhisperflow --cancel` to discard the current recording without transcription.
Global shortcuts are configured by the desktop environment or compositor.

### Fast text insertion on Linux

Set `output-method : "paste"` to insert the entire transcript in one clipboard
paste instead of simulating a keypress for every character. This Wayland mode
requires `wl-copy` and `wtype`, waits for clipboard ownership, and allows 100 ms for
the virtual keyboard to initialize before sending the shortcut. It avoids the
legacy helper daemon and `--paste-once`, which clipboard managers can consume.

`paste-keys : "ctrl+v"` works in normal text fields. Use `"ctrl+shift+v"` for a
terminal emulator or `"shift+insert"` where appropriate. This mode leaves the
transcript on the clipboard so a slow clipboard read cannot race restoration;
it never automatically submits your message. Simulated typing remains available
with `output-method : "type"` (also given a short device-initialization pause).
The existing `"clipboard"` helper mode remains available for compatibility.
These modes affect insertion, not transcription accuracy.

### Linux waveform HUD

On Wayland, xhisperflow uses the `wlr-layer-shell` protocol to place the HUD at
the top center of the current display. Compositors without that protocol fall
back to X11 through XWayland when `DISPLAY` is available. Native X11 sessions
use an always-on-top notification window. If neither window backend can start,
the existing `notify-send` level meter is used automatically.

While recording, the waveform responds to the microphone level. During
transcription, the same HUD switches to a repeating left-to-right ripple so the
processing state is visually distinct from listening on both Linux and macOS.

## File transcription for remote clients

`xhisperflow --transcribe-file /path/to/audio.wav` transcribes an existing recording
and writes only the final text to stdout (errors go to stderr). It uses the same
selected backend and optional cleanup configuration as desktop dictation. It does not start recording, show a HUD, or change the clipboard
or focused application. The Groq backend sends audio to Groq; the Voxtype backend
uses the host's Voxtype engine configuration.

For Omarchy Remote, set its host environment override:

```sh
OMARCHY_DICTATION_COMMAND='["xhisperflow","--transcribe-file","{audio}"]'
```
