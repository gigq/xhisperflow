# xhisperflow

Pure Rust rewrite of `gigq/xhisper`.

## What it keeps

- Toggle recording by invoking `xhisperflow` twice
- Groq Whisper transcription
- Optional Groq post-processing cleanup
- Wayland-first typing via `wtype`
- Clipboard output mode
- `xhisperflowtool` / `xhisperflowtoold` binaries for uinput paste, typing, and wrap keys
- Live recording notification updates through `notify-send`

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
floating waveform HUD, Groq transcription, and paste into the active app.

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
chords like `ctrl+opt`.

Use `cancel-hotkey` to bind a shortcut that discards the current recording
without transcription. Set it to an empty string to disable the cancel shortcut.

The floating HUD can be disabled with `mac-floating-waveform : false`. Its
waveform colors are configured with `mac-waveform-gradient-start` and
`mac-waveform-gradient-end` using quoted `#RRGGBB` values.

Use the menu bar app's Start at Login item to install a per-user LaunchAgent
for the current app.

Required macOS permissions:

- Microphone, for recording.
- Accessibility, for pasting the final transcript with Command+V.

The menu bar app includes a Permissions Help item that opens the relevant
System Settings panes. If Accessibility paste is not allowed, the transcript is
left on the clipboard.

## Config

Copy [default_xhisperflowrc](./default_xhisperflowrc) to:

```sh
~/.config/xhisperflow/xhisperflowrc
```

`GROQ_API_KEY` is read from the environment or `~/.env`.
