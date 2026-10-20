#!/usr/bin/env bash
# Build a private resident Qwen adapter: Vulkan on Linux, Metal on Apple Silicon.
# Requires git, cmake, C++17 compiler, curl; Linux also needs Vulkan headers/glslc.
set -euo pipefail
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" == Darwin ]]; then
  data_dir="$HOME/Library/Application Support/xhisperflow"
  backend=(-DTRANSCRIBE_METAL=ON -DGGML_METAL_EMBED_LIBRARY=ON -DGGML_NATIVE=OFF)
  rpath='@loader_path/lib'
else
  data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/xhisperflow"
  backend=(-DTRANSCRIBE_VULKAN=ON)
  rpath='$ORIGIN/lib'
fi
source_dir="$data_dir/runtime/transcribe.cpp"
destination="$data_dir/runtime/qwen"
revision=db678a031cf4c524bcab2c45a0dc6a7813106e4a
mkdir -p "$destination" "$data_dir/models"
if [[ ! -d "$source_dir/.git" ]]; then
  git clone --no-checkout https://github.com/handy-computer/transcribe.cpp.git "$source_dir"
fi
if [[ "$(git -C "$source_dir" rev-parse HEAD)" != "$revision" ]]; then
  git -C "$source_dir" fetch origin "$revision"
  git -C "$source_dir" checkout --detach "$revision"
fi
cmake -S "$source_dir" -B "$source_dir/build" \
  -DCMAKE_BUILD_TYPE=Release -DTRANSCRIBE_BUILD_SHARED=ON \
  -DTRANSCRIBE_BUILD_TESTS=OFF -DTRANSCRIBE_BUILD_EXAMPLES=ON \
  -DTRANSCRIBE_USE_SYSTEM_BLAS=OFF -DCMAKE_INSTALL_PREFIX="$destination" "${backend[@]}"
cmake --build "$source_dir/build" --target transcribe-cli -j "${XHISPERFLOW_BUILD_JOBS:-8}"
cmake --install "$source_dir/build"
"${CXX:-c++}" -std=c++17 -O2 "$repo_dir/native/qwen-worker.cpp" \
  -I "$destination/include" -L "$destination/lib" -ltranscribe \
  "-Wl,-rpath,$rpath" -o "$destination/xhisperflow-qwen-worker.new"
mv "$destination/xhisperflow-qwen-worker.new" "$destination/xhisperflow-qwen-worker"

model="$data_dir/models/Qwen3-ASR-1.7B-Q8_0.gguf"
sha=9a0d81792dfea2d5f278b8a63deb3ea6e02139ce42c2301f32ea19c4f77526b7
digest() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d ' ' -f1
  else shasum -a 256 "$1" | cut -d ' ' -f1; fi
}
if [[ ! -f "$model" ]] || [[ "$(digest "$model")" != "$sha" ]]; then
  temporary="$(mktemp "$model.download.XXXXXX")"
  trap 'rm -f "$temporary"' EXIT
  curl --fail --location --retry 3 \
    'https://huggingface.co/handy-computer/Qwen3-ASR-1.7B-gguf/resolve/main/Qwen3-ASR-1.7B-Q8_0.gguf' \
    --output "$temporary"
  [[ "$(digest "$temporary")" == "$sha" ]] || { echo 'Model checksum mismatch' >&2; exit 1; }
  mv "$temporary" "$model"
  trap - EXIT
fi
printf 'Installed resident Qwen3-ASR 1.7B (Q8_0). Select: xhisperflow --select-model qwen\n'
