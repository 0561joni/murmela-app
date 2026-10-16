import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Matches text_translation::LANGUAGES in the backend. */
export const TRANSLATION_LANGUAGES = [
  { code: "en", name: "English" },
  { code: "de", name: "German" },
  { code: "fr", name: "French" },
  { code: "es", name: "Spanish (Spain)" },
  { code: "es-AR", name: "Spanish (Argentina)" },
  { code: "it", name: "Italian" },
  { code: "pt-BR", name: "Portuguese (Brazil)" },
  { code: "nl", name: "Dutch" },
  { code: "pl", name: "Polish" },
] as const;

export type TranslationTone = "auto" | "informal" | "formal";
export const COMPACT_TRANSLATION_MODEL_ID = "translategemma-4b-q6-k";
export const LARGE_TRANSLATION_MODEL_ID = "translategemma-12b-q6-k";
/** The longest text that translates by itself while typing; longer work waits for ⌘↵. */
export const LIVE_CHARACTER_LIMIT = 4000;
export const MAX_TRANSLATION_CHARACTERS = 200_000;

export function languageName(code: string | null | undefined) {
  return TRANSLATION_LANGUAGES.find((language) => language.code === code)?.name ?? "Unknown language";
}

/** "es-AR" and "es" are the same language for "already in this language". */
export function sameLanguage(a: string | null | undefined, b: string | null | undefined) {
  if (!a || !b) return false;
  return a.split("-")[0] === b.split("-")[0];
}

const ADDRESS: Record<string, { informal: string; formal: string }> = {
  de: { informal: "du", formal: "Sie" },
  fr: { informal: "tu", formal: "vous" },
  es: { informal: "tú", formal: "usted" },
  "es-AR": { informal: "vos", formal: "usted" },
  it: { informal: "tu", formal: "Lei" },
  "pt-BR": { informal: "você", formal: "o senhor" },
  nl: { informal: "je", formal: "u" },
  pl: { informal: "ty", formal: "Pan/Pani" },
};

/** Tone choices with the target language's own words, e.g. "Informal (tu)". */
export function toneLabel(tone: TranslationTone, target: string) {
  if (tone === "auto") return "Match the source";
  const words = ADDRESS[target];
  const base = tone === "formal" ? "Formal" : "Informal";
  return words ? `${base} (${words[tone]})` : base;
}

export function hasFormsOfAddress(target: string) {
  return target in ADDRESS;
}

/* ---------- Segments ---------- */

export interface TextPiece {
  text: string;
  /** Code blocks, front matter, URLs and lines without letters are kept as they are. */
  translatable: boolean;
  /** Whitespace between this piece and the next (or the end of the text). */
  after: string;
}

export interface SegmentedText {
  before: string;
  pieces: TextPiece[];
}

const FENCE = /^\s*(`{3,}|~{3,})/;

/**
 * Splits text into paragraphs (blank-line separated). Each paragraph is
 * translated on its own, so editing one re-translates only that one. Markdown
 * code fences and front matter are kept whole and never translated.
 */
export function segmentText(input: string): SegmentedText {
  const text = input.replace(/\r\n?/g, "\n");
  const lines: { text: string; start: number; end: number }[] = [];
  let offset = 0;
  for (const line of text.split("\n")) {
    lines.push({ text: line, start: offset, end: offset + line.length });
    offset += line.length + 1;
  }
  const blocks: { start: number; end: number; translatable: boolean }[] = [];
  let i = 0;
  if (lines[0]?.text.trim() === "---") {
    const close = lines.findIndex((line, index) => index > 0 && index < 200 && line.text.trim() === "---");
    if (close > 0) {
      blocks.push({ start: lines[0].start, end: lines[close].end, translatable: false });
      i = close + 1;
    }
  }
  while (i < lines.length) {
    if (!lines[i].text.trim()) {
      i += 1;
      continue;
    }
    const fence = FENCE.exec(lines[i].text);
    if (fence) {
      const marker = fence[1][0].repeat(3);
      let close = i + 1;
      while (close < lines.length && !lines[close].text.trim().startsWith(marker)) close += 1;
      const last = Math.min(close, lines.length - 1);
      blocks.push({ start: lines[i].start, end: lines[last].end, translatable: false });
      i = last + 1;
      continue;
    }
    const first = i;
    while (i + 1 < lines.length && lines[i + 1].text.trim() && !FENCE.test(lines[i + 1].text)) i += 1;
    const content = text.slice(lines[first].start, lines[i].end);
    blocks.push({ start: lines[first].start, end: lines[i].end, translatable: isTranslatable(content) });
    i += 1;
  }
  return {
    before: text.slice(0, blocks[0]?.start ?? text.length),
    pieces: blocks.map((block, index) => ({
      text: text.slice(block.start, block.end),
      translatable: block.translatable,
      after: text.slice(block.end, blocks[index + 1]?.start ?? text.length),
    })),
  };
}

function isTranslatable(text: string) {
  if (!/\p{L}/u.test(text)) return false;
  return !/^\s*(?:https?:\/\/|www\.)\S+\s*$/.test(text);
}

/** Puts the text back together; untranslatable pieces and missing ones keep the source. */
export function joinPieces(segmented: SegmentedText, outputs: (string | null | undefined)[]) {
  return (
    segmented.before +
    segmented.pieces.map((piece, index) => (piece.translatable ? outputs[index] ?? piece.text : piece.text) + piece.after).join("")
  );
}

/* ---------- Cache ---------- */

export interface CachedSegment {
  text: string;
  issues: string[];
}

/** Translations by source paragraph, kept while Murmela runs. */
const cache = new Map<string, CachedSegment>();
const CACHE_LIMIT = 3000;

export function segmentKey(text: string, source: string, target: string, tone: TranslationTone) {
  return `${source}\u0000${target}\u0000${tone}\u0000${text}`;
}
export function cachedSegment(key: string) {
  return cache.get(key);
}
export function cacheSegment(key: string, value: CachedSegment) {
  cache.delete(key);
  cache.set(key, value);
  if (cache.size > CACHE_LIMIT) cache.delete(cache.keys().next().value!);
}
export function clearTranslationCache() {
  cache.clear();
}

/** "notes.md" → "notes.fr.md"; typed text → "Translation.fr.txt". */
export function translatedFileName(fileName: string | null, target: string) {
  if (!fileName) return `Translation.${target}.txt`;
  const dot = fileName.lastIndexOf(".");
  const stem = dot > 0 ? fileName.slice(0, dot) : fileName;
  const extension = dot > 0 ? fileName.slice(dot + 1).toLowerCase() : "txt";
  return `${stem}.${target}.${["md", "markdown"].includes(extension) ? "md" : "txt"}`;
}

/** A short Library title from the file name or the first line. */
export function translationTitle(fileName: string | null, text: string) {
  if (fileName) return fileName;
  const line = text.trim().split("\n")[0]?.replace(/^#+\s*/, "").trim() ?? "";
  if (!line) return "Translation";
  return line.length > 60 ? line.slice(0, 59).trimEnd() + "…" : line;
}

/* ---------- Backend ---------- */

export interface TranslatorStatus {
  supported: boolean;
  modelId: string | null;
  modelName: string | null;
  installedModelIds: string[];
  loaded: boolean;
  memoryAvailableBytes: number | null;
  memoryNeededBytes: number | null;
  languages: { code: string; name: string }[];
}

export interface DetectedLanguage {
  code: string;
  reliable: boolean;
}

export interface TextTranslationRequest {
  requestId: string;
  segments: { index: number; text: string }[];
  sourceLanguage: string;
  targetLanguage: string;
  tone: TranslationTone;
  modelId?: string | null;
  allowLowMemory?: boolean;
}

/** One paragraph and the place in its translation to work on (see `wordAt`). */
export interface ParagraphEditRequest {
  requestId: string;
  sourceText: string;
  translation: string;
  /** Alternatives: the translation before the word. Rewrite: the part to keep, ending with the chosen word. */
  prefix: string;
  word?: string | null;
  sourceLanguage: string;
  targetLanguage: string;
  tone: TranslationTone;
  modelId?: string | null;
}

export interface WordAlternative {
  text: string;
  probability: number;
}

export interface RewrittenParagraph {
  text: string;
  issues: string[];
}

const WORD_CHARACTER = /[\p{L}\p{N}\p{M}'’-]/u;

/** The word at character `offset` of `text` (or just before it), without edge punctuation. */
export function wordAt(text: string, offset: number): { start: number; end: number; word: string } | null {
  let at = Math.max(0, Math.min(offset, text.length));
  if (!WORD_CHARACTER.test(text[at] ?? "") && WORD_CHARACTER.test(text[at - 1] ?? "")) at -= 1;
  if (!WORD_CHARACTER.test(text[at] ?? "")) return null;
  let start = at;
  let end = at + 1;
  while (start > 0 && WORD_CHARACTER.test(text[start - 1])) start -= 1;
  while (end < text.length && WORD_CHARACTER.test(text[end])) end += 1;
  while (start < end && /['’-]/.test(text[start])) start += 1;
  while (end > start && /['’-]/.test(text[end - 1])) end -= 1;
  if (start === end) return null;
  return { start, end, word: text.slice(start, end) };
}

export interface TextTranslationOutcome {
  requestId: string;
  modelId: string;
  status: "completed" | "canceled" | "partial";
  translated: number;
  failed: number;
  promptVersion: number;
}

export type TextTranslationEvent =
  | { kind: "stage"; requestId: string; stage: "waiting" | "checking" | "loading" | "translating"; index: number | null; chunkIndex: number | null; chunkCount: number | null }
  | { kind: "segment"; requestId: string; index: number; text: string; issues: string[] }
  | { kind: "segmentFailed"; requestId: string; index: number; code: string };

export interface OpenedTextFile {
  name: string;
  text: string;
}

export interface SavedSegment {
  source: string;
  output: string | null;
}

export interface SavedTextTranslation {
  id: string;
  createdAt?: string;
  updatedAt?: string;
  title: string;
  sourceKind: "text" | "file" | "transcript";
  fileName: string | null;
  sourceText: string;
  outputText: string;
  segments: SavedSegment[];
  sourceLanguage: string | null;
  targetLanguage: string;
  tone: TranslationTone;
  modelId: string | null;
}

export interface TextTranslationSummary {
  id: string;
  createdAt: string;
  updatedAt: string;
  title: string;
  sourceKind: string;
  sourceLanguage: string | null;
  targetLanguage: string;
  snippet: string;
}

const desktop = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/* Browser preview: `?translate-demo` fakes a translator so the screen can be checked without the app. */
const demo = () => !desktop() && typeof window !== "undefined" && new URLSearchParams(window.location.search).has("translate-demo");
const demoListeners = new Set<(event: TextTranslationEvent) => void>();
const demoCanceled = new Set<string>();
function demoTranslate(request: TextTranslationRequest): Promise<TextTranslationOutcome> {
  return new Promise((resolve) => {
    let position = 0;
    const emit = (event: TextTranslationEvent) => demoListeners.forEach((listener) => listener(event));
    const step = () => {
      if (demoCanceled.has(request.requestId)) {
        resolve({ requestId: request.requestId, modelId: LARGE_TRANSLATION_MODEL_ID, status: "canceled", translated: position, failed: 0, promptVersion: 5 });
        return;
      }
      const segment = request.segments[position];
      if (!segment) {
        resolve({ requestId: request.requestId, modelId: LARGE_TRANSLATION_MODEL_ID, status: "completed", translated: position, failed: 0, promptVersion: 5 });
        return;
      }
      emit({ kind: "stage", requestId: request.requestId, stage: "translating", index: segment.index, chunkIndex: null, chunkCount: null });
      window.setTimeout(() => {
        if (!demoCanceled.has(request.requestId)) {
          emit({ kind: "segment", requestId: request.requestId, index: segment.index, text: `[${request.targetLanguage}] ${segment.text}`, issues: [] });
        }
        position += 1;
        step();
      }, 700);
    };
    window.setTimeout(step, 200);
  });
}
const BROWSER_STATUS: TranslatorStatus = {
  supported: false,
  modelId: null,
  modelName: null,
  installedModelIds: [],
  loaded: false,
  memoryAvailableBytes: null,
  memoryNeededBytes: null,
  languages: [...TRANSLATION_LANGUAGES],
};

export async function getTranslatorStatus(): Promise<TranslatorStatus> {
  if (demo()) return { ...BROWSER_STATUS, supported: true, modelId: LARGE_TRANSLATION_MODEL_ID, modelName: "TranslateGemma 12B", installedModelIds: [LARGE_TRANSLATION_MODEL_ID], memoryAvailableBytes: 16e9, memoryNeededBytes: 12.8e9 };
  if (!desktop()) return BROWSER_STATUS;
  return invoke("get_translator_status");
}
export async function detectTextLanguage(text: string): Promise<DetectedLanguage | null> {
  if (demo()) return /\b(und|der|die|das|nicht|danke|wir|ist)\b/i.test(text) ? { code: "de", reliable: true } : { code: "en", reliable: true };
  if (!desktop()) return null;
  return invoke("detect_text_language", { text });
}
export async function translateText(request: TextTranslationRequest): Promise<TextTranslationOutcome> {
  if (demo()) return demoTranslate(request);
  return invoke("translate_text", { request });
}
export async function getTranslationAlternatives(request: ParagraphEditRequest): Promise<WordAlternative[]> {
  if (demo()) {
    await new Promise((resolve) => window.setTimeout(resolve, 450));
    const word = request.word ?? "";
    return ["option", "choice", "variant", "alternative", "pick"]
      .map((text) => (word[0] && word[0] === word[0].toUpperCase() ? text[0].toUpperCase() + text.slice(1) : text))
      .map((text, index) => ({ text, probability: 0.5 / (index + 1) }));
  }
  return invoke("translation_alternatives", { request });
}
export async function rewriteTranslation(request: ParagraphEditRequest): Promise<RewrittenParagraph> {
  if (demo()) {
    await new Promise((resolve) => window.setTimeout(resolve, 600));
    const kept = request.prefix;
    const before = kept.replace(/[\p{L}\p{N}\p{M}'’-]+$/u, "");
    const rest = wordAt(request.translation, before.length);
    return { text: kept + request.translation.slice(rest ? rest.end : before.length), issues: [] };
  }
  return invoke("rewrite_translation", { request });
}
export async function cancelTextTranslation(requestId?: string): Promise<void> {
  if (demo() && requestId) demoCanceled.add(requestId);
  if (!desktop()) return;
  await invoke("cancel_text_translation", { requestId: requestId ?? null });
}
export async function pickTextFile(): Promise<OpenedTextFile | null> {
  return invoke("pick_text_file");
}
export async function readTextFile(path: string): Promise<OpenedTextFile> {
  return invoke("read_text_file", { path });
}
export async function saveTranslationFile(fileName: string, contents: string): Promise<string | null> {
  return invoke("save_translation_file", { fileName, contents });
}
export async function saveTextTranslation(document: SavedTextTranslation): Promise<SavedTextTranslation | null> {
  if (!desktop()) return null;
  return invoke("save_text_translation", { document });
}
export async function listTextTranslations(query = ""): Promise<TextTranslationSummary[]> {
  if (!desktop()) return [];
  return invoke("list_text_translations", { query });
}
export async function getTextTranslation(id: string): Promise<SavedTextTranslation | null> {
  return invoke("get_text_translation", { id });
}
export async function deleteTextTranslation(id: string): Promise<void> {
  await invoke("delete_text_translation", { id });
}
export async function listenTextTranslation(callback: (event: TextTranslationEvent) => void) {
  if (demo()) {
    demoListeners.add(callback);
    return () => {
      demoListeners.delete(callback);
    };
  }
  if (!desktop()) return () => {};
  return listen<TextTranslationEvent>("text-translation", (event) => callback(event.payload));
}
