import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Settings value for "copy twice" (⌘C ⌘C); matches quick_translate::DOUBLE_COPY. */
export const DOUBLE_COPY = "DoubleCopy";
/** Matches quick_translate::DEFAULT_IGNORED_APPS. */
export const DEFAULT_IGNORED_APPS = [
  "com.apple.keychainaccess",
  "com.apple.Passwords",
  "com.1password.1password",
  "com.agilebits.onepassword7",
];

/** A selected text the panel translates (quick_translate::QuickTranslateRequest). */
export interface QuickTranslateRequest {
  id: string;
  text: string;
  sourceLanguage: string | null;
  targetLanguage: string;
  myLanguage: string;
  /** Where text in myLanguage goes. */
  otherLanguage: string;
  appName: string | null;
  canReplace: boolean;
}

/** A text to open on the Translate screen ("Edit in Murmela"). */
export interface QuickTranslateOpen {
  text: string;
  sourceLanguage: string | null;
  targetLanguage: string;
}

export type ReplaceOutcome = "replacing" | "needs_accessibility";

export interface AppInfo {
  bundleId: string;
  name: string | null;
}

const desktop = () => "__TAURI_INTERNALS__" in window;
/** `?quick-demo` previews the panel in a browser with a sample text. */
const demo = () => !desktop() && new URLSearchParams(window.location.search).has("quick-demo");

const DEMO_REQUEST: QuickTranslateRequest = {
  id: "demo",
  text: "Hi Joni, could we move our session to Thursday afternoon? I have a conflict on Wednesday.\n\nThanks, Sarah",
  sourceLanguage: "en",
  targetLanguage: "de",
  myLanguage: "de",
  otherLanguage: "en",
  appName: "Mail",
  canReplace: true,
};

export async function currentQuickTranslation(): Promise<QuickTranslateRequest | null> {
  if (demo()) return DEMO_REQUEST;
  if (!desktop()) return null;
  return invoke("quick_translate_current");
}
export async function listenQuickTranslation(callback: (request: QuickTranslateRequest) => void) {
  if (!desktop()) return () => {};
  return listen<QuickTranslateRequest>("quick-translate-request", (event) => callback(event.payload));
}
export async function listenQuickTranslateOpen(callback: (request: QuickTranslateOpen) => void) {
  if (!desktop()) return () => {};
  return listen<QuickTranslateOpen>("quick-translate-open", (event) => callback(event.payload));
}
export async function closeQuickTranslation() {
  if (!desktop()) return;
  await invoke("quick_translate_close");
}
export async function resizeQuickTranslation(height: number) {
  if (!desktop()) return;
  await invoke("quick_translate_resize", { height });
}
export async function copyQuickTranslation(text: string) {
  if (!desktop()) {
    await navigator.clipboard?.writeText(text);
    return;
  }
  await invoke("quick_translate_copy", { text });
}
export async function replaceWithQuickTranslation(text: string): Promise<ReplaceOutcome> {
  if (!desktop()) return "replacing";
  return invoke("quick_translate_replace", { text });
}
export async function editQuickTranslation(text: string, sourceLanguage: string | null, targetLanguage: string) {
  if (!desktop()) return;
  await invoke("quick_translate_edit", { text, sourceLanguage, targetLanguage });
}
export async function rememberQuickTarget(targetLanguage: string) {
  if (!desktop()) return;
  await invoke("quick_translate_set_target", { targetLanguage });
}
/** Pauses for `minutes`, or turns quick translation off when null. */
export async function pauseQuickTranslation(minutes: number | null) {
  if (!desktop()) return;
  await invoke("quick_translate_pause", { minutes });
}
export async function requestAccessibility() {
  if (!desktop()) return;
  await invoke("request_accessibility_access");
}
/** The app "Insert into…" pastes into: the last one used before Murmela. */
export async function insertTargetName(): Promise<string | null> {
  if (!desktop()) return demo() || new URLSearchParams(window.location.search).has("translate-demo") ? "Mail" : null;
  return invoke("quick_translate_insert_target");
}
export async function insertIntoLastApp(text: string): Promise<ReplaceOutcome> {
  if (!desktop()) return "replacing";
  return invoke("quick_translate_insert", { text });
}
/** Text copied in another app during the last `maxAgeSeconds`, or null. */
export async function recentCopy(maxAgeSeconds: number): Promise<string | null> {
  if (!desktop()) return null;
  return invoke("quick_translate_recent_copy", { maxAgeSeconds });
}
/** Tells the main window the Library has a new translation. */
export async function announceLibraryChange() {
  if (!desktop()) return;
  const { emit } = await import("@tauri-apps/api/event");
  await emit("text-translations-changed");
}
export async function systemTranslationLanguage(): Promise<string> {
  if (!desktop()) return "en";
  return invoke("quick_translate_system_language");
}
export async function describeApps(bundleIds: string[]): Promise<AppInfo[]> {
  if (!desktop()) return bundleIds.map((bundleId) => ({ bundleId, name: null }));
  return invoke("quick_translate_describe_apps", { bundleIds });
}
export async function pickApp(): Promise<AppInfo | null> {
  if (!desktop()) return null;
  return invoke("quick_translate_pick_app");
}
