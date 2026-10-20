#!/usr/bin/env bash
# Download the standalone worker's model on Linux or macOS. No Voxtype needed.
set -euo pipefail
model="${1:-large-v3-turbo}"
engine=whisper
case "$model" in
  tiny|tiny.en|base|base.en|small|small.en|medium|medium.en|large-v3|large-v3-turbo) ;;
  parakeet) model=parakeet-tdt-0.6b-v3-int8; engine=parakeet ;;
  parakeet-tdt-0.6b-v3|parakeet-tdt-0.6b-v3-int8) engine=parakeet ;;
  *) echo "Unsupported Whisper model: $model" >&2; exit 1 ;;
esac
if [[ "$(uname -s)" == Darwin ]]; then
  data_dir="$HOME/Library/Application Support/xhisperflow"
else
  data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/xhisperflow"
fi
mkdir -p "$data_dir/models"
if [[ "$engine" == parakeet ]]; then
  # Python is only an installer dependency; inference is compiled into the app.
  python3 - "$model" "$data_dir/models/$model" <<'PY'
import hashlib, json, os, pathlib, subprocess, sys, tempfile
model, destination = sys.argv[1:]
destination = pathlib.Path(destination)
destination.mkdir(parents=True, exist_ok=True)
base = f"https://models.voxtype.io/parakeet/{model}/"
manifest = json.loads(subprocess.check_output(['curl', '--fail', '--silent', '--show-error', '--location', '--retry', '3', base + 'manifest.json']))
assert manifest['model'] == model and manifest['engine'] == 'parakeet'
for item in manifest['files']:
    assert pathlib.Path(item['path']).name == item['path'], 'Invalid manifest path'
    target = destination / item['path']
    def valid(path):
        if not path.is_file() or path.stat().st_size != item['size']: return False
        with path.open('rb') as handle:
            digest = hashlib.sha256()
            while chunk := handle.read(1024 * 1024): digest.update(chunk)
            return digest.hexdigest() == item['sha256']
    if valid(target): continue
    print('Downloading ' + item['path'], flush=True)
    descriptor, temporary = tempfile.mkstemp(dir=destination, prefix='.download-')
    os.close(descriptor)
    try:
        subprocess.run(['curl', '--fail', '--location', '--retry', '3', base + item['path'], '--output', temporary], check=True)
        assert valid(pathlib.Path(temporary)), 'Model checksum mismatch'
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary): os.unlink(temporary)
print('Installed model: ' + str(destination))
PY
  printf 'Select it with: xhisperflow --select-model %s\n' "$model"
  exit 0
fi
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
printf 'Select local-engine : "whisper" and local-model : "%s" in xhisperflowrc.\n' "$model"
printf 'The app starts its resident worker automatically. Restart the worker after changing its model.\n'
