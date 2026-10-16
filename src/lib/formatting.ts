/** Settings values for the one-key shortcuts (see key_gesture.rs). */
export const FN_KEY_SHORTCUT = "Fn";
export const RIGHT_OPTION_SHORTCUT = "RightAlt";

/** Hold to talk, double-tap for hands-free: no key combination involved. */
export function isOneKeyShortcut(shortcut: string | null | undefined) {
  return shortcut === FN_KEY_SHORTCUT || shortcut === RIGHT_OPTION_SHORTCUT;
}

const KEY_SYMBOLS: Record<string, string> = {
  CmdOrCtrl: "\u2318",
  Cmd: "\u2318",
  Shift: "\u21E7",
  Alt: "\u2325",
  Ctrl: "\u2303",
};

/** Shows a stored shortcut with the Mac's key symbols, e.g. "⌃+⌥+L". */
export function formatShortcutForDisplay(shortcut: string) {
  if (shortcut === FN_KEY_SHORTCUT) return "🌐 fn";
  if (shortcut === RIGHT_OPTION_SHORTCUT) return "Right ⌥";
  return shortcut
    .split("+")
    .map((part) => KEY_SYMBOLS[part] ?? part)
    .join("+");
}

export const PASTE_SHORTCUT = "\u2318+V";

/** Whole-second durations: "45s", "8m 12s", "1h 5m 3s". */
export function formatDuration(seconds: number) {
  const total = Math.max(0, Math.round(seconds));
  if (total < 60) {
    return `${total}s`;
  }
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const remainder = total % 60;
  const parts = hours > 0 ? [`${hours}h`, `${minutes}m`] : [`${minutes}m`];
  if (remainder > 0) parts.push(`${remainder}s`);
  return parts.join(" ");
}

/** Playback/segment clock: "08:12" below an hour, "1:15:03" from an hour on. */
export function formatClock(ms: number) {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  const tail = `${String(Math.floor(seconds / 60) % 60).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
  return seconds >= 3600 ? `${Math.floor(seconds / 3600)}:${tail}` : tail;
}

/** Decimal units, as in Finder: 4,221,849 bytes → "4.2 MB". */
export function formatBytes(bytes: number) {
  const value = Math.max(0, bytes);
  if (value < 1000) return `${Math.round(value)} B`;
  if (value < 1_000_000) return `${Math.round(value / 1000)} KB`;
  if (value < 1_000_000_000) {
    const megabytes = value / 1_000_000;
    return megabytes < 10 ? `${megabytes.toFixed(1)} MB` : `${Math.round(megabytes)} MB`;
  }
  return `${(value / 1_000_000_000).toFixed(1)} GB`;
}

// Dates follow the system's language and region (e.g. "25. Sept." on a German
// Mac, "Sep 25" on an English one). Tests pass a locale explicitly.

/** "25. Sept." / "Sep 25" */
export function formatShortDate(iso: string, locale?: string) {
  return new Date(iso).toLocaleDateString(locale, { day: "numeric", month: "short" });
}

/** "25.09.2026, 23:04" / "Sep 25, 2026, 11:04 PM" */
export function formatDateTime(iso: string, locale?: string) {
  return new Date(iso).toLocaleString(locale, { dateStyle: "medium", timeStyle: "short" });
}

/** "23:04" / "11:04 PM" */
export function formatTime(iso: string, locale?: string) {
  return new Date(iso).toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" });
}

/** Clock-style duration for lists: "0:32", "8:13", "1:15:03". */
export function formatListDuration(ms: number) {
  const seconds = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = String(seconds % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/** Plain-language versions of the backend's error codes: what happened and
 * what to do. Codes not listed keep the backend's own sentence. */
const ERROR_COPY: Record<string, string> = {
  APP_SHUTTING_DOWN: "Murmela is quitting.",
  TRANSCRIPTION_EMPTY: "No speech was heard. Try speaking closer to the microphone.",
  NO_SPEECH: "No speech was heard. Try speaking closer to the microphone.",
  MODEL_MISSING: "No speech engine is set up for this yet. Choose one in Settings → Engines.",
  MODEL_CONTEXT_UNSUPPORTED: "This engine can't be used here. Choose another one in Settings → Engines.",
  MODEL_UNSUPPORTED_PLATFORM: "This engine doesn't run on this computer.",
  MODEL_ENGINE_UNSUPPORTED: "This engine isn't supported by this version of Murmela.",
  MODEL_BUSY: "The engine is busy with another job. Try again in a moment.",
  MODEL_RUNTIME_MISSING: "Part of this engine is missing. Download it again in Settings → Engines.",
  MODEL_INCOMPLETE: "This engine's download is incomplete. Download it again in Settings → Engines.",
  MODEL_LOAD_FAILED: "The engine couldn't start. Try again, or download it again in Settings → Engines.",
  MODEL_UNSUPPORTED_LANGUAGE: "This engine doesn't support the chosen language. Pick Automatic or another language in Settings → Dictation.",
  MODEL_AUDIO_TOO_LONG: "This recording is too long for the chosen engine. Pick another file engine in Settings → Engines.",
  MODEL_DOWNLOAD_CANCELED: "Download canceled.",
  MODEL_TERMS_REQUIRED: "Accept the Gemma Terms of Use first, then start the download again.",
  MODEL_WORKER_FAILED: "The speech engine stopped unexpectedly. Try again; if it keeps happening, restart Murmela.",
  QWEN_INFERENCE_FAILED: "The speech engine stopped unexpectedly. Try again; if it keeps happening, restart Murmela.",
  LIVE_PAIR_WORKER_FAILED: "Live dictation stopped unexpectedly. Try again; if it keeps happening, restart Murmela.",
  LIVE_PAIR_PROTOCOL: "Live dictation stopped unexpectedly. Try again; if it keeps happening, restart Murmela.",
  LIVE_PAIR_CAPTURE_ORDER: "Live dictation stopped unexpectedly. Try again; if it keeps happening, restart Murmela.",
  CAPTURE_ORDER: "Recording stopped unexpectedly. Try again; if it keeps happening, restart Murmela.",
  LIVE_PAIR_TIMEOUT: "Live dictation took too long and was stopped. Try again.",
  LIVE_PAIR_FINALIZE_TIMEOUT: "Live dictation took too long to finish and was stopped. Try again.",
  LIVE_PAIR_CAPTURE_STALLED: "The microphone stopped sending sound. Check it with the microphone test in Settings → Dictation.",
  CAPTURE_FAILED: "The microphone stopped. Stop recording to keep what was captured.",
  LIVE_PAIR_BUSY: "Live dictation is still busy. Try again in a moment.",
  DICTATION_CANCELED: "Canceled.",
  JOB_CANCELED: "Canceled.",
  TRANSCRIPTION_CANCELED: "Canceled.",
  JOB_ALREADY_RUNNING: "This file is already being transcribed.",
  JOB_ALREADY_EXISTS: "This file is already being transcribed.",
  JOB_NOT_FOUND: "This transcription no longer exists.",
  REVIEW_NOT_FOUND: "This transcript no longer exists.",
  REVIEW_CONFLICT: "This transcript changed in the meantime. The latest version is shown.",
  DECODER_REPETITION: "The engine got stuck repeating itself. Try again, or use another engine.",
};

/** Developer-facing messages without a code, matched by their wording. */
const MESSAGE_COPY: Array<[RegExp, string]> = [
  [/recording worker (?:timed out|is not available)|capture setup timed out|capture (?:buffer|state) unavailable/i,
    "The microphone didn't respond. Try again, or reset dictation on the Dictate screen."],
  [/no default microphone is available/i,
    "No microphone found. Connect one, or choose it in Settings → Dictation."],
  [/unsupported microphone sample format/i,
    "This microphone's audio format isn't supported. Try another microphone."],
  [/diarization worker exited/i, "Speaker identification stopped unexpectedly. Try again."],
  [/translation protocol mismatch/i,
    "The translation engine needs repair: Settings → Dictation → Check and repair."],
  [/watchdog expired/i,
    "Transcription took too long and was stopped. Try again, or pick a faster file engine."],
  [/review storage unavailable/i, "The transcript couldn't be opened. Restart Murmela and try again."],
  [/record audio first so the app has a normalized wav/i, "Record something first."],
  [/timed out while inserting/i,
    "Pasting took too long. Use “Paste last dictation” to try again."],
  [/(?:overlay|dictation state|status|shortcut state|recovery state) unavailable|lock was poisoned/i,
    "Something inside Murmela got stuck. Reset dictation on the Dictate screen, or restart Murmela."],
];

/** Turns a backend error into a sentence for people: a known code or wording
 * becomes plain language; otherwise the machine code prefix is dropped. */
export function readableError(message: string): string {
  const text = message.trim();
  // Preview errors arrive as "model_error: MODEL_MISSING: …"; look past the
  // outer lowercase code first.
  const outer = /^[a-z][a-z0-9_]*:\s+([\s\S]+)$/.exec(text);
  if (outer) return readableError(outer[1]);
  const coded = /^([A-Z][A-Z0-9_]*[A-Z0-9]):\s*([\s\S]*)$/.exec(text);
  if (coded && ERROR_COPY[coded[1]]) return ERROR_COPY[coded[1]];
  for (const [pattern, copy] of MESSAGE_COPY) {
    if (pattern.test(text)) return copy;
  }
  // Strips a leading machine code such as "io_error: " or "DISK_SPACE_LOW: ".
  return text.replace(/^(?:[a-z][a-z0-9_]*|[A-Z][A-Z0-9_]*[A-Z0-9]):\s+/, "");
}

/** The message to show for anything a command or promise rejected with.
 * Tauri commands reject with plain strings, not Error objects. */
export function describeError(error: unknown, fallback = "Something went wrong. Please try again.") {
  const raw =
    error instanceof Error
      ? error.message
      : typeof error === "string"
        ? error
        : error && typeof error === "object" && "message" in error && typeof error.message === "string"
          ? error.message
          : "";
  return raw.trim() ? readableError(raw) : fallback;
}
