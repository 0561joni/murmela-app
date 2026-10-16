import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DictationOutput, DictationOutputState, OutputMode, QuickDictationStatusResponse } from "../types/domain";

/** Original, then every output language in the order the language shortcut visits them. */
export const OUTPUT_MODES: { value: OutputMode; label: string }[] = [
  { value: "original", label: "Original" },
  { value: "en", label: "English" },
  { value: "de", label: "Deutsch" },
  { value: "fr", label: "Français" },
  { value: "es", label: "Español" },
  { value: "es-AR", label: "Español (AR)" },
  { value: "it", label: "Italiano" },
  { value: "pt-BR", label: "Português (BR)" },
  { value: "nl", label: "Nederlands" },
  { value: "pl", label: "Polski" },
];
export function outputLanguageLabel(mode: OutputMode) {
  return OUTPUT_MODES.find((item) => item.value === mode)?.label ?? "Original";
}
const desktop = () => "__TAURI_INTERNALS__" in window;
export async function getDictationOutputState(): Promise<DictationOutputState> {
  if (!desktop()) return { outputMode: "original", busy: false, stage: "idle", statusText: "", ready: false, errorMessage: "Local translation is available in the desktop app.", lastOutput: null };
  return invoke("get_dictation_output_state");
}
export async function setDictationOutputMode(mode: OutputMode): Promise<DictationOutputState> {
  if (!desktop()) throw new Error("Local translation is available in the desktop app.");
  return invoke("set_dictation_output_mode", { mode });
}
export async function retryDictationTranslation(transcriptId?: string): Promise<DictationOutput> {
  return invoke("retry_dictation_translation", { transcriptId });
}
export async function retryStreamingDictation(): Promise<QuickDictationStatusResponse> {
  return invoke("retry_streaming_dictation");
}
export async function listenDictationOutput(callback: (state: DictationOutputState) => void) {
  if (!desktop()) return () => {};
  return listen<DictationOutputState>("dictation-output-status", (event) => callback(event.payload));
}
export async function listenTranslationErrors(callback: (message: string) => void) {
  if (!desktop()) return () => {};
  const cleanups = await Promise.all(["dictation-mode-error", "translation-setup-error"].map(
    (name) => listen<string>(name, (event) => callback(event.payload)),
  ));
  return () => cleanups.forEach((cleanup) => cleanup());
}
