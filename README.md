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

## Runtime dependencies

- `pw-record`
- `notify-send` for notifications
- `arecord` for the live level meter
- `wtype` for direct Wayland typing
- `wl-copy` / `wl-paste` or `xclip` for clipboard support
- access to `/dev/uinput` when the helper daemon is used

## Config

Copy [default_xhisperflowrc](./default_xhisperflowrc) to:

```sh
~/.config/xhisperflow/xhisperflowrc
```

`GROQ_API_KEY` is read from the environment or `~/.env`.
