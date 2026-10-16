# MOSS worker

`build.sh` checks out `moss-transcribe.cpp` at commit
`190a569c13b4b247450f2fb3b2a431244e84833e` and builds it as an isolated
executable. The port's ggml symbols therefore never enter Murmela's Tauri
process or conflict with whisper.cpp.

The packaged directory contains `murmela-moss-worker` (the NDJSON adapter) and
`moss-transcribe` (the native CPU runtime). The Murmela patch adds `--prompt` so
the official diarization/timestamp prompt can receive `Hotwords: …`.

Build it with `npm run build:moss` (`scripts/build-moss-worker.mjs`; `build.sh`
just calls it). The script caches the patched source in
`src-tauri/target/moss-runtime/`, builds `moss-transcribe` with CMake, freezes the
adapter with PyInstaller so it does not need the Mac's Python, signs both with the
local signing identity, smoke-tests them (`--self-test`) and stages the folder in
`src-tauri/bundle/moss/`, packaged as `Contents/Resources/workers/moss/`.
