# xhisperflow

xhisperflow is a lightweight dictation app that records your voice, transcribes
it locally with Voxtype on Linux when installed (otherwise with
`whisper-large-v3-turbo` on Groq), optionally cleans up the text with
`openai/gpt-oss-20b`, and inserts the final transcript into the app you were
using.

On macOS, xhisperflow runs as a menu bar app with a global hotkey, onboarding
for required permissions, and direct paste into the active app. On Linux, it
provides command-line tools that can be bound to your window manager or desktop
shortcuts for push-to-toggle dictation. Both platforms display the same floating
waveform HUD while recording.

![xhisperflow recording from the macOS menu bar](assets/docs/xhisperflow-in-use.png)

## Features

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

This produces:

- `target/debug/xhisperflow`
- `target/debug/xhisperflowtool`
- `target/debug/xhisperflowtoold`

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
floating waveform HUD, Groq-hosted transcription, and paste into the active app.

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

On Linux, `transcription-backend : "auto"` prefers `voxtype` when it is executable
on `PATH`; otherwise it uses Groq. Set it to `"groq"` to keep the original backend,
or `"voxtype"` to require the local CLI. macOS retains the existing Groq backend
and onboarding; these backend-selection settings currently apply to Linux only.

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
