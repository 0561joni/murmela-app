# Release notes

## Murmela 1.1.1: dictate into nine languages, and Pro for $19

- Translate while you dictate now covers all nine languages: English, German, French, Spanish, Argentinian Spanish, Italian, Brazilian Portuguese, Dutch and Polish. Choose the ones the language shortcut switches between in the new Settings → Languages.
- Hold 🌐 and press → or ← to switch language, at any pace. Keep holding and speak: the dictation comes out in the new language. If you already speak the chosen language, your words are pasted unchanged.
- The welcome setup asks which languages you want to translate into, downloads the recommended engine by itself and lets you practise the 🌐 key. Run it again any time from Settings → General.
- Live dictation is no longer experimental.
- The network activity log stays compact and can be exported as a text file.
- New: Murmela Pro for $19, with a year of updates. The version you have keeps Pro for good after that year. Lifetime stays $49 for the first 500 founders, and existing keys stay Lifetime.
- Fixed: the language shortcut did nothing while "Translate while you dictate" was off, without saying so. Settings now says so.

## Murmela 1.1: calls and meetings

- A new look for macOS 26: Liquid Glass sidebar, buttons, menus and dialogs, with Apple's system colours and San Francisco type. With Reduce Transparency on, Murmela uses solid surfaces.
- Record a call or a meeting in the room and get a local transcript with speakers. On a call, your microphone is "You" and the call's sound is "Other side"; nothing leaves your Mac. Recording a call's sound needs macOS 14.4 or later.
- The optional browser helper reads who is speaking in Google Meet, Teams and Zoom in the browser, so the other side is named. With Pro, several people on the other side are told apart and named.
- Settings → General → Network activity lists the only three places Murmela can contact (Hugging Face, GitHub, Lemon Squeezy) and logs every connection it made. There is still no telemetry.
- Murmela is now built for the Mac only, which makes the app smaller and simpler to maintain. Unused code and settings from the pre-release builds were removed.

## Murmela 1.0

The first public release of Murmela: private dictation, translation and transcription that run entirely on your Mac.

- Hold the fn key in any app, talk, and your words appear where you type. No cloud, no account, no telemetry.
- Translate as you speak, or translate typed text, in 9 languages.
- Transcribe audio and video files up to six hours, with a searchable library you can switch off.
- Murmela Pro adds speaker labels, document translation and word alternatives. Activate it with one click from your receipt email, or paste your license key in Settings → License. After activation, Pro works offline.
- Requires a Mac with Apple silicon (M1 or newer) and macOS 11 or later.

## Signed by Apple, with automatic updates

- Murmela is signed with an Apple Developer ID and notarized, so macOS opens it without security warnings.
- New versions download in the background and install when you restart or quit Murmela. You can turn this off under Settings → About.
- Before macOS asks for Accessibility access, Murmela now explains what the permission is used for.
- Existing history, settings and models move over automatically. macOS asks once more for microphone and Accessibility access.

## VibeVoice is back

- VibeVoice-ASR returns as the most accurate file engine, labelled Experimental. It is available on Apple Silicon Macs with at least 18 GB of memory, and a loaded translation model is released before it runs.
- New installs and replaced selections now prefer VibeVoice, then Qwen3-ASR and MOSS, then Whisper for files.
- The licence notices now include the Python packages bundled in the model workers.

## Licence cleanup before selling Murmela

- Removed R2T2 (NetEase model licence under Chinese law with downstream contract duties). Murmela deletes its downloaded files on the next start; saved selections fall back to an installed engine.
- TranslateGemma now asks you to accept the Gemma Terms of Use and Prohibited Use Policy before it downloads; the backend refuses the download without that agreement.
- Every model download links to its licence, and THIRD_PARTY_NOTICES.md describes the speaker-identification models' licences accurately.

## Native long-form transcription models

- Added MOSS Transcribe + Diarize 0.9B F16 for dictation and files up to 90 minutes.
- Added VibeVoice-ASR 8-bit MLX for file transcription on Apple Silicon with macOS 14 or newer; 32 GB unified memory is recommended.
- Native timestamps and speaker labels are now preserved automatically and identified as built-in in History. Explicit speaker retry still replaces them with standalone post-processing.
- Murmela vocabulary is passed as MOSS hotwords and VibeVoice context. Fixed-language settings remain informational for these automatic, code-switching models.
- Retired Whisper Tiny. Existing managed weights and partials are removed once, saved selections migrate to an installed fallback, and later manually added Tiny files are ignored.
