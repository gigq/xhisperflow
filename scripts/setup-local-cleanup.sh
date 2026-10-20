#!/usr/bin/env bash
# Installs a private llama.cpp runtime + VoiceInk dictation cleanup model.
# Does not enable cleanup or change the selected speech recognizer.
set -euo pipefail
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) platform=ubuntu-vulkan-x64; runtime_sha=536b3b470dc01a76ab7024a18f6a994679f81785d7bc1fa8214c54bf8c6774c6 ;;
  Linux-aarch64) platform=ubuntu-vulkan-arm64; runtime_sha=e877bb316d8e6bc0e2907223cafb00fb0e7734a4a054751f6c42ad79b4f86994 ;;
  Darwin-arm64) platform=macos-arm64; runtime_sha=12cde38a94d13a2ca8bb2e5d61932b8fb8176f72593a38ee32a1139a049d36f9 ;;
  Darwin-x86_64) platform=macos-x64; runtime_sha=741aa995672ffb6347bfec1761c31bc6f85ef11fe6b27e7de8020feecbdd6b5f ;;
  *) echo 'Unsupported local cleanup platform' >&2; exit 1 ;;
esac
if [[ "$(uname -s)" == Darwin ]]; then
  data_dir="$HOME/Library/Application Support/xhisperflow"
else
  data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/xhisperflow"
fi
runtime="$data_dir/runtime/llama-b11755"
mkdir -p "$data_dir/runtime" "$data_dir/models"
digest() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d ' ' -f1
  else shasum -a 256 "$1" | cut -d ' ' -f1; fi
}
temporary="$(mktemp -d "$data_dir/runtime/.cleanup-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT
if [[ ! -x "$runtime/llama-server" ]]; then
  curl --fail --location --retry 3 \
    "https://github.com/ggml-org/llama.cpp/releases/download/b11755/llama-b11755-bin-$platform.tar.gz" \
    --output "$temporary/runtime.tar.gz"
  [[ "$(digest "$temporary/runtime.tar.gz")" == "$runtime_sha" ]] || { echo 'Runtime checksum mismatch' >&2; exit 1; }
  mkdir "$temporary/extracted"
  tar -xzf "$temporary/runtime.tar.gz" -C "$temporary/extracted" --strip-components=1
  mv "$temporary/extracted" "$runtime"
fi
"$runtime/llama-server" --version
model="$data_dir/models/Qwen3.5-2B-VoiceInk-Q4_K_M.gguf"
model_sha=343721d889adcec76725373f51be207e6a980eec8411e4e6c553dd6c8329d175
if [[ ! -f "$model" ]] || [[ "$(digest "$model")" != "$model_sha" ]]; then
  curl --fail --location --retry 3 \
    "https://registry.ollama.ai/v2/hourliert/voiceink-qwen3.5-2b/blobs/sha256:$model_sha" \
    --output "$temporary/model.gguf"
  [[ "$(digest "$temporary/model.gguf")" == "$model_sha" ]] || { echo 'Model checksum mismatch' >&2; exit 1; }
  mv "$temporary/model.gguf" "$model"
fi
printf 'Local cleanup installed, still disabled. Preview: xhisperflow --cleanup-text "Um, keep Qwen enabled."\n'
printf 'Enable: xhisperflow --local-cleanup on\nDisable and unload: xhisperflow --local-cleanup off\n'
