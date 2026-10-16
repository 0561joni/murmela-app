#!/usr/bin/env bash
# The MOSS worker is built by scripts/build-moss-worker.mjs (also run
# automatically by `npm run tauri dev` and `npm run tauri -- build`).
set -euo pipefail
cd "$(dirname "$0")/../.."
exec node scripts/build-moss-worker.mjs
