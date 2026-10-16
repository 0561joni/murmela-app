#!/usr/bin/env bash
set -euo pipefail

python_bin="${PYTHON312:-python3.12}"
output_dir="${1:-dist}"
build_dir="${2:-build}"
"$python_bin" -m PyInstaller \
  --noconfirm \
  --clean \
  --onedir \
  --name murmela-vibevoice-worker \
  --distpath "$output_dir" \
  --workpath "$build_dir/work" \
  --specpath "$build_dir/spec" \
  --collect-all mlx \
  --collect-all mlx_audio \
  --collect-all mlx_lm \
  --collect-submodules transformers.models.qwen2 \
  --hidden-import miniaudio \
  --collect-all tokenizers \
  --add-data "${MURMELA_VIBEVOICE_TOKENIZER:?set MURMELA_VIBEVOICE_TOKENIZER to the verified tokenizer folder}:qwen2.5-7b-tokenizer" \
  "$(dirname "$0")/murmela_vibevoice_worker.py"

bundle="$output_dir/murmela-vibevoice-worker"
if [[ -n "${MURMELA_CODESIGN_IDENTITY:-}" ]]; then
  codesign --force --deep --options runtime --timestamp \
    --sign "$MURMELA_CODESIGN_IDENTITY" "$bundle"
fi
