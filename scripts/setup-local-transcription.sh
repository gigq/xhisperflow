#!/usr/bin/env bash
# Download the standalone worker's model on Linux or macOS. No Voxtype needed.
set -euo pipefail
model="${1:-large-v3-turbo}"
case "$model" in
  tiny|tiny.en|base|base.en|small|small.en|medium|medium.en|large-v3|large-v3-turbo) ;;
  *) echo "Unsupported Whisper model: $model" >&2; exit 1 ;;
esac
if [[ "$(uname -s)" == Darwin ]]; then
  data_dir="$HOME/Library/Application Support/xhisperflow"
else
  data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/xhisperflow"
fi
mkdir -p "$data_dir/models"
destination="$data_dir/models/ggml-$model.bin"
if [[ ! -s "$destination" ]]; then
  temporary="$(mktemp "$destination.download.XXXXXX")"
  trap 'rm -f "$temporary"' EXIT
  curl --fail --location --retry 3 \
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-$model.bin" \
    --output "$temporary"
  mv "$temporary" "$destination"
  trap - EXIT
fi
printf 'Installed model: %s\n' "$destination"
printf 'Select transcription-backend : "local" and local-model : "%s" in xhisperflowrc.\n' "$model"
printf 'The app starts its resident worker automatically. Restart the worker after changing its model.\n'
