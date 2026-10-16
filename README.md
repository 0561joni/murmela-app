# Murmela

Private dictation, translation and transcription that run entirely on your Mac.
Murmela is a Tauri 2 app (Rust backend, React + TypeScript frontend) for Macs with
Apple silicon and macOS 11 or later.

This is the source code of the app and of its browser helper. The signed and
notarized app, with automatic updates, is at [murmela.com](https://murmela.com/).
Buying Murmela Pro there is how the project is paid for.

- Dictate into any app with the 🌐 fn key, a key combination or Right ⌥.
- Translate while you dictate, or translate typed, pasted or opened text
  (.txt, .md) in nine languages, offline.
- Transcribe audio and video files and record calls and meetings, with speakers.
- Settings and transcript history live in a local SQLite database.

## Translate screen

**Translate** in the sidebar translates text you type, paste or open (.txt and .md,
or drop the file on the window) between English, German, French, Spanish (Spain and
Argentina), Italian, Portuguese (Brazil), Dutch and Polish. It translates while you
type, paragraph by paragraph, and only re-translates paragraphs you change. It needs
TranslateGemma 12B (best quality) or TranslateGemma 4B (compact, for Macs with less
free memory), downloaded from the screen or from Settings → Dictation. Finished
translations go to the Library (*Translations*) while history is on.

## Dictation translation

In **Settings → Models → Local translation**, download TranslateGemma 12B Q6_K
(9.66 GB). The verified download enables the language shortcut. Dictate in German
or English and press **⌃⌥L** between dictations (or hold the dictation key and
press → / ←) to cycle through **Original →
Français → Español (AR)**. The shortcut is configurable and reserved globally.
Each app start resets to Original; switching is locked while a dictation is active.

Global dictation pastes the finished translation; the Dictate workspace offers
separate original/translation views and copy actions. Failed translations preserve
the source and never paste. History follows the existing preference, and retrying
a failed translation never pastes into a potentially different application.
Imported audio remains unchanged. Inference works offline after model setup.

The native helper is built automatically for Apple Silicon by `npm run tauri dev`
and `npm run tauri -- build`. Use `npm run build:translation` to build it separately.
It requires CMake and a working Apple developer toolchain at build time; the packaged
application needs no separately installed translation runtime.

## File transcription: audio and video

Drop files on **Files** (or use **Choose files**). Supported:

- **Audio:** WAV, MP3, M4A, OPUS (up to 2 GB)
- **Video:** MP4, MOV, M4V, MKV, WEBM, AVI (up to 20 GB). Only the first audio
  track is extracted and transcribed; the picture is skipped. The 6-hour
  duration cap applies to both.

MP4/MOV/M4V, MKV (AAC/FLAC/Vorbis/PCM) and Vorbis WebM decode fully offline with
the built-in decoder, with macOS AVFoundation (`avconvert`) as fallback. Opus
WebM and AVI need `ffmpeg` (`brew install ffmpeg`); without it Murmela shows a
clear error. Review playback streams MP4/MOV/M4V directly and plays extracted
audio for the other video formats. Videos without sound are rejected with
"has no audio track".

## Meetings: calls and in-person meetings

**Meetings** in the sidebar records a call (the microphone and the call's sound)
or a meeting in the room (the microphone only), and transcribes it locally with
speakers:
- On a call, everything on the microphone is **You** and everything in the call's
  sound is the **Other side**.
- With Pro, the people on the other side are told apart.
- The optional browser helper (`browser-helper/`) reads who is speaking in Google
  Meet, Teams and Zoom in the browser, so the transcript carries their names.

Call sound uses a Core Audio process tap on macOS 14.4+.

## Network activity

Murmela works offline. It can only contact three places, and refuses any other
host: Hugging Face (model downloads), GitHub (the optional update check and
update download) and Lemon Squeezy (activating or deactivating Murmela Pro).
**Settings → General → Network activity** lists them and logs every connection
Murmela made, with date, destination, purpose and size. There is no analytics,
crash reporting or telemetry. While recording a call, Murmela also listens on
127.0.0.1 for the browser helper; that connection stays on the computer. All
requests go through `src-tauri/src/network_activity.rs`; a unit test fails if
another file talks to the network without it.

## Local development

Use the repo's supported Node version before installing dependencies:

```bash
node -v
cat .nvmrc
```

Recommended runtime: `Node 22 LTS` (`22.19.0` in `.nvmrc`).

Frontend only:

```bash
npm install
npm run dev
```

Full Tauri app:

```bash
npm install
npm run tauri dev
```

### Closing and quitting

Murmela runs one desktop instance per user. Opening it again restores the existing window (or the splash screen while starting), including when it is hidden or minimized. Simultaneous launches share an OS lock that is automatically released after a crash; transcription workers continue to run separately.

The red close button hides the workspace. Clicking the Dock icon or the menu-bar icon restores it. Cmd-Q, Dock Quit, the application menu, and the tray's Quit entry share one shutdown path.

Idle quit requires no confirmation. If recording, transcription, or speaker processing is active, Murmela offers **Keep working** or **Stop and quit**. Accepted quit blocks new work, stops capture and workers, restores audio volume, drains active operations, and releases cached model/GPU resources before terminating. The window shows cleanup status while this completes. Engines without a safe mid-call abort finish their current native call before their memory is released. Partial model downloads retain the existing resume behavior.

Native regression probes (macOS, without workspace windows or user data):

```bash
cargo run --manifest-path src-tauri/Cargo.toml --example macos_quit_smoke -- --native
cargo run --manifest-path src-tauri/Cargo.toml --example macos_quit_smoke -- --tauri
```

The ignored `asr::tests::metal_cache_release_exits_cleanly` test can additionally be run in a dedicated test process with `MURMELA_WHISPER_SMOKE_MODEL` set to an installed Whisper `.bin` file. It verifies that a cached Metal model is released before process exit, reproducing the managed-state lifetime involved in the former quit crash.

The decoder regression test also exercises first and repeated GPU transcription, CPU transcription, and native shutdown cancellation. Supply an installed Whisper model and a speech recording with at least one second of audio:

```bash
MURMELA_WHISPER_SMOKE_MODEL=/path/to/ggml-small.bin \
MURMELA_WHISPER_SMOKE_AUDIO=/path/to/speech.wav \
cargo test --manifest-path src-tauri/Cargo.toml --lib \
  asr::tests::native_abort_callback_decodes_and_cancels_cleanly -- --ignored --exact --nocapture
```

Run it alone because it sets the process-wide shutdown flag. This covers the encoder callback that model-loading tests cannot reach: Murmela registers a static C callback directly to avoid the incorrect closure pointer cast in `whisper-rs 0.16.0`'s safe abort wrapper.

Prerequisites: a Mac with Apple silicon, a working Rust toolchain and the Xcode
command line tools (license accepted). The app targets `macOS 11.0+` because the
native Whisper/ggml toolchain for Apple silicon release builds needs that
deployment target.

## Feature requirements

| Feature | Requires |
| --- | --- |
| Dictation, files, Whisper, Qwen3-ASR and MOSS | macOS 11+ |
| Live dictation (live pair) | macOS 14+ |
| Recording a call's sound | macOS 14.4+ |
| VibeVoice-ASR 8-bit MLX (experimental) | macOS 14+, 18 GB+ memory |
| GPU acceleration for Whisper | Metal |

## Models

Models are downloaded from Settings → Models. Whisper `ggml-*.bin` files placed in
the models folder by hand are listed as custom models, for example:

- `ggml-small.bin` -> `balanced`
- `ggml-medium.bin` or `ggml-large-v3.bin` -> `accurate`

MOSS Transcribe + Diarize is a 1.83 GB CPU model available for shortcut dictation,
Quick Dictate, and files up to 90 minutes. VibeVoice-ASR is a 9.52 GB file-only model
and the most accurate in our tests; it is offered on Apple Silicon Macs with macOS 14
or newer and at least 18 GB of memory (it uses about 14 GB while transcribing). Both use
their own timestamps, speakers, automatic language detection, and Murmela vocabulary
context. Their native speaker labels are preserved even when standalone speaker
post-processing is off.

VibeVoice and MOSS run in bundled, self-contained workers (VibeVoice: `mlx-audio`
frozen with PyInstaller; MOSS: the native `moss-transcribe` runtime plus a frozen
adapter). `npm run tauri dev` / `npm run tauri -- build` (and the Deploy script) build
them automatically; `npm run build:vibevoice` and `npm run build:moss` build them on
their own. Building needs an arm64 Python 3.10 or newer (Homebrew, python.org, pyenv
or `uv`; set `MURMELA_PYTHON` to pick one), CMake, and network access the first time;
the packaged app never uses the system `python3`.

Settings → Models shows the models folder and opens it in Finder.

## Building from source

Follow *Local development* above, then build a release app with
`npm run tauri -- build`. Without a Developer ID certificate the build is signed
ad hoc and runs on your own Mac. `Setup local signing.command` creates a stable
local signing identity, so macOS keeps the microphone and accessibility
permissions between rebuilds.

A build from this source is named Murmela, uses the bundle ID `com.murmela.app`
and checks the official update feed. If you leave the update check on, it offers
the official signed release. Switch it off in Settings if you want to keep your
own build. If you distribute a modified build, change the name, icons, bundle ID,
URL scheme and the updater endpoint and key (`src-tauri/tauri.conf.json`); see
[TRADEMARKS.md](TRADEMARKS.md).

The Pro features are part of this source. Builds you make yourself are covered by
the GPL like everything else here. If Murmela is useful to you, buying Pro is what
keeps it maintained.

## Licence

Murmela is free software: you can redistribute it and/or modify it under the terms
of the GNU General Public License, version 3 only (`GPL-3.0-only`). See
[LICENSE](LICENSE).

The licence covers the code, not the Murmela name, logo and icons: see
[TRADEMARKS.md](TRADEMARKS.md). Third-party code and the speech and translation
models keep their own licences: see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)
and `src-tauri/licenses/`. TranslateGemma is provided under the Gemma Terms of Use,
which the app shows and asks the user to accept before it downloads the model. Forks
that offer the model must keep that step.

Copyright (C) 2026 Jonathan Zuecker
