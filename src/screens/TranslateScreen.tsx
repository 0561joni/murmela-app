import { useCallback, useEffect, useMemo, useReducer, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { ActionButton, Button, PageHeader } from "../components/Feedback";
import { AppIcon, IconButton } from "../components/IconButton";
import { AlternativesMenu, type AlternativesState } from "../components/AlternativesMenu";
import { GemmaTermsDialog } from "../components/GemmaTermsDialog";
import { ModelDownloadCard } from "../components/ModelDownloadCard";
import {
  acceptModelTerms,
  cancelModelDownload,
  copyTextToClipboard,
  getModelDownloadStatuses,
  getModelTermsStatus,
  listDownloadableModels,
  listenModelDownloadStatus,
  startAppDictation,
  startModelDownload,
  type AppDictation,
} from "../lib/api";
import { describeError, formatBytes, readableError } from "../lib/formatting";
import { isProRequiredError, useLicense } from "../hooks/useLicense";
import { ProHint, ProTag } from "../components/ProHint";
import {
  COMPACT_TRANSLATION_MODEL_ID,
  LIVE_CHARACTER_LIMIT,
  MAX_TRANSLATION_CHARACTERS,
  TRANSLATION_LANGUAGES,
  cacheSegment,
  cachedSegment,
  cancelTextTranslation,
  detectTextLanguage,
  getTextTranslation,
  getTranslationAlternatives,
  getTranslatorStatus,
  hasFormsOfAddress,
  joinPieces,
  languageName,
  listenTextTranslation,
  pickTextFile,
  readTextFile,
  rewriteTranslation,
  sameLanguage,
  saveTextTranslation,
  saveTranslationFile,
  segmentKey,
  segmentText,
  toneLabel,
  translateText,
  translatedFileName,
  translationTitle,
  wordAt,
  type DetectedLanguage,
  type ParagraphEditRequest,
  type SavedTextTranslation,
  type TranslationTone,
  type TranslatorStatus,
} from "../lib/textTranslation";
import type { AppSettings, DownloadableModel, ModelDownloadStatus, TranscriptSummary } from "../types/domain";

export type TranslateOpenRequest =
  | { kind: "saved"; id: string; nonce: number }
  | { kind: "file"; path: string; nonce: number };

interface TranslateScreenProps {
  settings: AppSettings | null;
  transcripts: TranscriptSummary[];
  openRequest: TranslateOpenRequest | null;
  dragging: boolean;
  onOpenSettings: () => void;
  onLibraryChanged: () => void;
}

interface DocumentInfo {
  id: string;
  sourceKind: SavedTextTranslation["sourceKind"];
  fileName: string | null;
  title: string | null;
}

interface Job {
  requestId: string;
  total: number;
  done: number;
  stage: "starting" | "waiting" | "checking" | "loading" | "translating";
  chunkIndex: number | null;
  chunkCount: number | null;
  startedAt: number;
}

/** A clicked word in a translated paragraph, while its alternatives menu is open. */
interface WordEdit {
  requestId: string;
  index: number;
  key: string;
  source: string;
  text: string;
  start: number;
  end: number;
  word: string;
  state: AlternativesState;
}

/** The last word replaced through alternatives, so it can be undone. */
interface LastEdit {
  key: string;
  previous: { text: string; issues: string[] };
  from: string;
  to: string;
}

const newDocument = (sourceKind: DocumentInfo["sourceKind"] = "text", fileName: string | null = null, title: string | null = null): DocumentInfo => ({
  id: crypto.randomUUID(),
  sourceKind,
  fileName,
  title,
});

function stored(key: string, fallback: string) {
  try {
    return window.localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}
function store(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // Remembering the choice is a convenience only.
  }
}

const ISSUE_COPY: Record<string, string> = {
  numbers_changed: "a number differs from the original",
  links_changed: "a link differs from the original",
  preface: "the model may have added a note",
  paragraphs_changed: "the paragraph layout changed",
  mixed_address: "it mixes tu and vous",
  tuteo: "it uses tú instead of vos",
};

export function TranslateScreen(props: TranslateScreenProps) {
  const [status, setStatus] = useState<TranslatorStatus | null>(null);
  const [models, setModels] = useState<DownloadableModel[]>([]);
  const [downloads, setDownloads] = useState<Record<string, ModelDownloadStatus>>({});
  const [termsModelId, setTermsModelId] = useState<string | null>(null);

  const [documentInfo, setDocumentInfo] = useState<DocumentInfo>(() => newDocument());
  const [sourceText, setSourceText] = useState("");
  const [sourceChoice, setSourceChoice] = useState("auto");
  const [detected, setDetected] = useState<DetectedLanguage | null>(null);
  const [target, setTarget] = useState(() => {
    const value = stored("murmela-translate-target", "en");
    return TRANSLATION_LANGUAGES.some((language) => language.code === value) ? value : "en";
  });
  const [tone, setTone] = useState<TranslationTone>(() => {
    const value = stored("murmela-translate-tone", "auto");
    return value === "formal" || value === "informal" ? value : "auto";
  });
  const [live, setLive] = useState(() => stored("murmela-translate-live", "on") !== "off");
  const [version, bump] = useReducer((value: number) => value + 1, 0);
  const [job, setJob] = useState<Job | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { status: license } = useLicense();
  const [proNotice, setProNotice] = useState<"document" | "alternatives" | null>(null);
  const [memoryWarning, setMemoryWarning] = useState<string | null>(null);
  const [allowLowMemory, setAllowLowMemory] = useState(false);
  const [modelOverride, setModelOverride] = useState<string | null>(null);
  const [needsManual, setNeedsManual] = useState(false);
  const [hovered, setHovered] = useState<number | null>(null);
  const [libraryMenuOpen, setLibraryMenuOpen] = useState(false);
  const [usedModelId, setUsedModelId] = useState<string | null>(null);
  const [savedToLibrary, setSavedToLibrary] = useState(false);
  const [wordEdit, setWordEdit] = useState<WordEdit | null>(null);
  const [lastEdit, setLastEdit] = useState<LastEdit | null>(null);
  const [wordAnchor, setWordAnchor] = useState<HTMLElement | null>(null);
  const wordEditRef = useRef<WordEdit | null>(null);
  wordEditRef.current = wordEdit;
  const [, setTick] = useState(0);
  const [dictation, setDictation] = useState<"off" | "starting" | "listening" | "finishing">("off");
  const dictationRef = useRef<AppDictation | null>(null);
  // The text that was in the box when dictation started; spoken words follow it.
  const dictationBase = useRef({ text: "", prefix: "" });
  const dictating = dictation !== "off";

  const failed = useRef(new Map<string, string>());
  const jobRef = useRef<{ requestId: string; keys: Map<number, string>; pending: Set<string> } | null>(null);
  const followUp = useRef(false);
  const lastShown = useRef<(string | null)[]>([]);
  const sourceRef = useRef<HTMLTextAreaElement>(null);
  const backdropRef = useRef<HTMLDivElement>(null);

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await getTranslatorStatus());
    } catch (reason) {
      setError(describeError(reason, "Translation is not available right now."));
    }
  }, []);

  useEffect(() => {
    void refreshStatus();
    void listDownloadableModels().then((all) => setModels(all.filter((model) => model.capability === "translation"))).catch(() => undefined);
    void getModelDownloadStatuses().then((all) => setDownloads(Object.fromEntries(all.map((entry) => [entry.modelId, entry])))).catch(() => undefined);
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listenModelDownloadStatus((entry) => {
      setDownloads((current) => ({ ...current, [entry.modelId]: entry }));
      if (entry.state === "completed") {
        void refreshStatus();
        void listDownloadableModels().then((all) => setModels(all.filter((model) => model.capability === "translation")));
      }
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refreshStatus]);

  // This screen stays mounted while hidden, so pick up a model chosen in Settings meanwhile.
  const selectedModelId = props.settings?.translationModelId;
  useEffect(() => {
    setModelOverride(null);
    setMemoryWarning(null);
    setAllowLowMemory(false);
    void refreshStatus();
  }, [selectedModelId, refreshStatus]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listenTextTranslation((event) => {
      const current = jobRef.current;
      if (!current || event.requestId !== current.requestId) return;
      if (event.kind === "stage") {
        setJob((value) => value && { ...value, stage: event.stage, chunkIndex: event.chunkIndex, chunkCount: event.chunkCount });
        return;
      }
      const key = current.keys.get(event.index);
      if (key) {
        if (event.kind === "segment") {
          cacheSegment(key, { text: event.text, issues: event.issues });
          failed.current.delete(key);
        } else {
          failed.current.set(key, event.code);
        }
        current.pending.delete(key);
      }
      setJob((value) => value && { ...value, done: value.done + 1, stage: "translating", chunkIndex: null, chunkCount: null });
      bump();
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    });
    return () => {
      disposed = true;
      unlisten?.();
      const current = jobRef.current;
      if (current) void cancelTextTranslation(current.requestId);
      jobRef.current = null;
    };
  }, []);

  // Waiting is usually a hand-over between two of our own requests; only say so when it lasts.
  useEffect(() => {
    if (job?.stage !== "waiting") return;
    const timer = window.setTimeout(() => setTick((value) => value + 1), 900);
    return () => window.clearTimeout(timer);
  }, [job?.stage]);

  useEffect(() => {
    const text = sourceText.trim();
    if (text.length < 12) {
      setDetected(null);
      return;
    }
    const timer = window.setTimeout(() => {
      void detectTextLanguage(text.slice(0, 2000)).then(setDetected).catch(() => setDetected(null));
    }, 250);
    return () => window.clearTimeout(timer);
  }, [sourceText]);

  useEffect(() => store("murmela-translate-target", target), [target]);
  useEffect(() => store("murmela-translate-tone", tone), [tone]);
  useEffect(() => store("murmela-translate-live", live ? "on" : "off"), [live]);

  const effectiveSource = sourceChoice === "auto" ? detected?.code ?? null : sourceChoice;
  const requestSource = sourceChoice === "auto" ? detected?.reliable ? detected.code : "auto" : sourceChoice;
  const alreadyInTarget = sameLanguage(effectiveSource, target) && sourceText.trim().length > 0;
  const tooLong = sourceText.length > MAX_TRANSLATION_CHARACTERS;
  const segmented = useMemo(() => segmentText(sourceText), [sourceText]);
  const keys = useMemo(
    () => segmented.pieces.map((piece) => (piece.translatable ? segmentKey(piece.text, requestSource, target, tone) : null)),
    [segmented, requestSource, target, tone],
  );
  // `version` changes whenever the cache gets a new segment.
  const outputs = useMemo(() => keys.map((key) => (key ? cachedSegment(key)?.text ?? null : null)), [keys, version]); // eslint-disable-line react-hooks/exhaustive-deps
  const translatableCount = keys.filter(Boolean).length;
  const complete = translatableCount > 0 && keys.every((key) => !key || cachedSegment(key) || failed.current.has(key));
  const anyOutput = outputs.some((value) => value !== null);
  const outputText = useMemo(() => joinPieces(segmented, outputs), [segmented, outputs]);
  const modelId = modelOverride ?? status?.modelId ?? null;
  const modelName = models.find((model) => model.id === modelId)?.modelName ?? status?.modelName ?? "TranslateGemma";
  const ready = Boolean(status?.supported && status.modelId);

  const run = useCallback(
    async (force: boolean) => {
      if (!ready || alreadyInTarget || tooLong || !sourceText.trim()) return;
      if (memoryWarning && !allowLowMemory && !modelOverride) return;
      const missing = segmented.pieces
        .map((piece, index) => ({ piece, index, key: keys[index] }))
        .filter((entry): entry is { piece: typeof entry.piece; index: number; key: string } =>
          entry.key !== null && !cachedSegment(entry.key) && (force || !failed.current.has(entry.key)));
      if (missing.length === 0) {
        setNeedsManual(false);
        return;
      }
      const characters = missing.reduce((sum, entry) => sum + entry.piece.text.length, 0);
      if (!force && characters > LIVE_CHARACTER_LIMIT) {
        setNeedsManual(true);
        return;
      }
      setNeedsManual(false);
      const current = jobRef.current;
      if (current && !force) {
        // Keep a running request that still does useful work; pick up the rest afterwards.
        const needed = new Set(missing.map((entry) => entry.key));
        if (![...current.pending].some((key) => !needed.has(key))) {
          followUp.current = true;
          return;
        }
      }
      if (current) void cancelTextTranslation(current.requestId);
      if (force) missing.forEach((entry) => failed.current.delete(entry.key));
      const requestId = crypto.randomUUID();
      jobRef.current = { requestId, keys: new Map(missing.map((entry) => [entry.index, entry.key])), pending: new Set(missing.map((entry) => entry.key)) };
      followUp.current = false;
      setJob({ requestId, total: missing.length, done: 0, stage: "starting", chunkIndex: null, chunkCount: null, startedAt: Date.now() });
      setError(null);
      try {
        const outcome = await translateText({
          requestId,
          segments: missing.map((entry) => ({ index: entry.index, text: entry.piece.text })),
          sourceLanguage: requestSource,
          targetLanguage: target,
          tone,
          modelId: modelOverride,
          allowLowMemory,
        });
        if (jobRef.current?.requestId !== requestId) return;
        jobRef.current = null;
        setJob(null);
        if (outcome.status !== "canceled") {
          setUsedModelId(outcome.modelId);
          void refreshStatus();
        }
        if (followUp.current) {
          followUp.current = false;
          window.setTimeout(() => runRef.current(false), 0);
        }
      } catch (reason) {
        if (jobRef.current?.requestId !== requestId) return;
        jobRef.current = null;
        setJob(null);
        const message = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : String(reason);
        if (message.startsWith("TRANSLATION_MEMORY_LOW")) setMemoryWarning(readableError(message));
        else if (!message.startsWith("TRANSLATION_CANCELED")) setError(describeError(reason, "Translation failed."));
      }
    },
    [ready, alreadyInTarget, tooLong, sourceText, memoryWarning, allowLowMemory, modelOverride, segmented, keys, requestSource, target, tone, refreshStatus],
  );
  const runRef = useRef(run);
  runRef.current = run;

  useEffect(() => {
    // Dictated text is translated once it is final, like typed text after a pause.
    if (!live || dictating) return;
    const timer = window.setTimeout(() => void runRef.current(false), 650);
    return () => window.clearTimeout(timer);
  }, [live, dictating, sourceText, requestSource, target, tone, ready, allowLowMemory, modelOverride]);

  useEffect(() => () => dictationRef.current?.dispose(), []);

  async function toggleDictation() {
    if (dictation === "listening") {
      setDictation("finishing");
      try {
        await dictationRef.current?.finish();
      } catch (reason) {
        dictationRef.current?.dispose();
        dictationRef.current = null;
        setDictation("off");
        setError(describeError(reason, "Could not stop dictating."));
      }
      return;
    }
    if (dictation !== "off") return;
    const text = sourceText;
    dictationBase.current = { text, prefix: text === "" || /\s$/.test(text) ? text : `${text} ` };
    setDictation("starting");
    setError(null);
    try {
      dictationRef.current = await startAppDictation((update) => {
        const base = dictationBase.current;
        if (update.kind === "live") {
          setSourceText(base.prefix + update.text);
        } else if (update.kind === "finishing") {
          setDictation("finishing");
          return;
        } else {
          dictationRef.current = null;
          setDictation("off");
          if (update.kind === "failed") {
            // Whatever streamed in stays, so the words are not lost.
            setError(describeError(update.message, "Dictation failed."));
          } else {
            setSourceText(update.text?.trim() ? base.prefix + update.text.trim() : base.text);
          }
          window.requestAnimationFrame(() => sourceRef.current?.focus());
        }
        setSavedToLibrary(false);
      });
      setDictation((current) => (current === "starting" ? "listening" : current));
    } catch (reason) {
      setDictation("off");
      setError(describeError(reason, "Could not start dictating."));
    }
  }

  // Keep the Library entry current once the whole text is translated.
  useEffect(() => {
    if (job || !complete || !anyOutput || !props.settings?.saveHistory) return;
    const document: SavedTextTranslation = {
      id: documentInfo.id,
      title: documentInfo.title ?? translationTitle(documentInfo.fileName, sourceText),
      sourceKind: documentInfo.sourceKind,
      fileName: documentInfo.fileName,
      sourceText,
      outputText,
      segments: segmented.pieces.map((piece, index) => ({ source: piece.text, output: piece.translatable ? outputs[index] : null })),
      sourceLanguage: effectiveSource,
      targetLanguage: target,
      tone,
      modelId: usedModelId,
    };
    const timer = window.setTimeout(() => {
      void saveTextTranslation(document)
        .then((saved) => {
          if (!saved) return;
          setSavedToLibrary(true);
          props.onLibraryChanged();
        })
        .catch(() => undefined);
    }, 1200);
    return () => window.clearTimeout(timer);
  }, [job, complete, anyOutput, outputText]); // eslint-disable-line react-hooks/exhaustive-deps

  function stop() {
    const current = jobRef.current;
    followUp.current = false;
    if (current) void cancelTextTranslation(current.requestId);
  }

  const closeAlternatives = useCallback(() => {
    const current = wordEditRef.current;
    if (current && (current.state.status === "loading" || current.state.status === "rewriting")) {
      void cancelTextTranslation(current.requestId);
    }
    setWordEdit(null);
  }, []);

  function editRequest(edit: WordEdit, prefix: string): ParagraphEditRequest {
    return {
      requestId: edit.requestId,
      sourceText: edit.source,
      translation: edit.text,
      prefix,
      word: edit.word,
      sourceLanguage: requestSource,
      targetLanguage: target,
      tone,
      modelId: modelOverride,
    };
  }

  /** A click on a translated paragraph opens alternatives for the word under the pointer. */
  function openAlternatives(event: ReactMouseEvent<HTMLParagraphElement>, index: number, key: string, source: string, text: string) {
    const selection = window.getSelection();
    if (selection && !selection.isCollapsed) return; // Selecting text to copy.
    const caret = document.caretRangeFromPoint?.(event.clientX, event.clientY);
    if (!caret || !event.currentTarget.contains(caret.startContainer)) return;
    const before = document.createRange();
    before.selectNodeContents(event.currentTarget);
    before.setEnd(caret.startContainer, caret.startOffset);
    const found = wordAt(text, before.toString().length);
    if (!found) {
      closeAlternatives();
      return;
    }
    if (wordEdit && wordEdit.key === key && wordEdit.start === found.start) return;
    closeAlternatives();
    if (license && !license.pro) {
      setProNotice("alternatives");
      return;
    }
    const edit: WordEdit = { requestId: crypto.randomUUID(), index, key, source, text, ...found, state: { status: "loading" } };
    setWordEdit(edit);
    getTranslationAlternatives(editRequest(edit, text.slice(0, found.start)))
      .then((items) => {
        setWordEdit((current) => (current?.requestId === edit.requestId ? { ...current, state: { status: "ready", items } } : current));
      })
      .catch((reason) => {
        const message = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : String(reason);
        if (isProRequiredError(reason)) setProNotice("alternatives");
        setWordEdit((current) => {
          if (current?.requestId !== edit.requestId) return current;
          if (isProRequiredError(reason)) return null;
          if (message.startsWith("TRANSLATION_CANCELED")) return null;
          return { ...current, state: { status: "error", message: describeError(reason, "Could not find alternatives.") } };
        });
      });
  }

  async function pickAlternative(choice: string) {
    const edit = wordEditRef.current;
    if (!edit) return;
    const requestId = crypto.randomUUID();
    const rewriting: WordEdit = { ...edit, requestId, state: { status: "rewriting", choice } };
    setWordEdit(rewriting);
    try {
      const result = await rewriteTranslation(editRequest(rewriting, edit.text.slice(0, edit.start) + choice));
      if (wordEditRef.current?.requestId !== requestId) return;
      const previous = cachedSegment(edit.key);
      // The paragraph was translated again meanwhile: keep the newer text.
      if (!previous || previous.text !== edit.text) {
        setWordEdit(null);
        return;
      }
      cacheSegment(edit.key, { text: result.text, issues: result.issues });
      setLastEdit({ key: edit.key, previous, from: edit.word, to: choice });
      setWordEdit(null);
      bump();
    } catch (reason) {
      if (wordEditRef.current?.requestId !== requestId) return;
      const message = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : String(reason);
      if (message.startsWith("TRANSLATION_CANCELED")) setWordEdit(null);
      else setWordEdit({ ...rewriting, state: { status: "error", message: describeError(reason, "Could not rewrite this paragraph.") } });
    }
  }

  function undoLastEdit() {
    if (!lastEdit) return;
    cacheSegment(lastEdit.key, lastEdit.previous);
    setLastEdit(null);
    bump();
  }

  function loadText(text: string, info: DocumentInfo, language?: { source: string | null; target: string; tone: TranslationTone }) {
    stop();
    closeAlternatives();
    setLastEdit(null);
    failed.current.clear();
    lastShown.current = [];
    setDocumentInfo(info);
    setSourceText(text);
    setSavedToLibrary(false);
    setError(null);
    setNeedsManual(false);
    if (language) {
      setSourceChoice(language.source ?? "auto");
      setTarget(language.target);
      setTone(language.tone);
    }
    window.requestAnimationFrame(() => sourceRef.current?.focus());
  }

  function clear() {
    loadText("", newDocument());
  }

  async function openFile() {
    try {
      const file = await pickTextFile();
      if (file) loadText(file.text, newDocument("file", file.name));
    } catch (reason) {
      if (isProRequiredError(reason)) setProNotice("document");
      else setError(describeError(reason, "Could not open the file."));
    }
  }

  // Opening a saved translation (from the Library) or a dropped file.
  useEffect(() => {
    const request = props.openRequest;
    if (!request) return;
    let disposed = false;
    void (async () => {
      try {
        if (request.kind === "file") {
          const file = await readTextFile(request.path);
          if (!disposed) loadText(file.text, newDocument("file", file.name));
          return;
        }
        const saved = await getTextTranslation(request.id);
        if (disposed) return;
        if (!saved) {
          setError("This translation is no longer in the Library.");
          return;
        }
        const source = saved.sourceLanguage ?? "auto";
        for (const segment of saved.segments) {
          if (segment.output) cacheSegment(segmentKey(segment.source, source, saved.targetLanguage, saved.tone), { text: segment.output, issues: [] });
        }
        setUsedModelId(saved.modelId);
        loadText(saved.sourceText, { id: saved.id, sourceKind: saved.sourceKind, fileName: saved.fileName, title: saved.title }, {
          source: saved.sourceLanguage,
          target: saved.targetLanguage,
          tone: saved.tone,
        });
        setSavedToLibrary(true);
        bump();
      } catch (reason) {
        if (disposed) return;
        if (isProRequiredError(reason)) setProNotice("document");
        else setError(describeError(reason, "Could not open this text."));
      }
    })();
    return () => {
      disposed = true;
    };
  }, [props.openRequest?.nonce]); // eslint-disable-line react-hooks/exhaustive-deps

  function swapLanguages() {
    if (!effectiveSource) return;
    const nextTarget = effectiveSource;
    const nextSource = target;
    if (complete && anyOutput) loadText(outputText, newDocument());
    setSourceChoice(nextSource);
    setTarget(nextTarget);
  }

  async function downloadModel(id: string) {
    const terms = await getModelTermsStatus(id);
    if (terms.required && !terms.accepted) {
      setTermsModelId(id);
      return;
    }
    await startModelDownload(id);
  }

  const shownOutputs = segmented.pieces.map((piece, index) => {
    const key = keys[index];
    if (!key) return { kind: "verbatim" as const, text: piece.text };
    const value = cachedSegment(key);
    if (value) return { kind: "done" as const, text: value.text, issues: value.issues };
    if (failed.current.has(key)) return { kind: "failed" as const, text: piece.text };
    const working = Boolean(jobRef.current?.pending.has(key));
    const stale = lastShown.current[index] ?? null;
    return { kind: working ? ("working" as const) : ("missing" as const), text: stale };
  });
  useEffect(() => {
    lastShown.current = shownOutputs.map((entry, index) =>
      entry.kind === "done" ? entry.text : lastShown.current[index] ?? null,
    );
  });

  const paragraphCount = segmented.pieces.length;
  const characterCount = sourceText.length;
  const downloadsActive = Object.values(downloads).some((entry) => entry.state === "downloading");
  const compactInstalled = status?.installedModelIds.includes(COMPACT_TRANSLATION_MODEL_ID) ?? false;
  const compactModel = models.find((model) => model.id === COMPACT_TRANSLATION_MODEL_ID);
  const canSwap = Boolean(effectiveSource);
  const waitingVisible = job?.stage === "waiting" && Date.now() - job.startedAt > 800;

  let jobLabel = "";
  if (job) {
    if (job.stage === "checking") jobLabel = "Checking the translation model (first use only)…";
    else if (job.stage === "loading") jobLabel = `Loading ${modelName}…`;
    else if (job.stage === "waiting") jobLabel = waitingVisible ? "Waiting for a dictation or transcription to finish…" : "Translating…";
    else {
      const current = Math.min(job.done + 1, job.total);
      jobLabel = job.total > 1 ? `Translating · paragraph ${current} of ${job.total}` : "Translating…";
      if (job.chunkIndex && job.chunkCount) jobLabel += ` · part ${job.chunkIndex} of ${job.chunkCount}`;
    }
  }

  if (status && !status.supported) {
    return (
      <section className="screen translate-screen">
        <PageHeader title="Translate" />
        <div className="empty-state glass-panel">
          <AppIcon name="languages" />
          <h3>Not available on this computer yet</h3>
          <p className="muted">Translation runs on this computer and currently needs a Mac with Apple Silicon.</p>
        </div>
      </section>
    );
  }

  const translationModels = models.length ? models : [];
  const needsSetup = status !== null && status.supported && !status.modelId;
  const twelveNeed = (models.find((model) => model.id !== COMPACT_TRANSLATION_MODEL_ID)?.sizeBytes ?? 9_660_827_392) + 3_100_000_000;
  const recommendCompact = status?.memoryAvailableBytes != null && status.memoryAvailableBytes < twelveNeed;

  return (
    <section className={"screen translate-screen" + (props.dragging ? " is-dragging" : "")}>
      <PageHeader title="Translate" description="Type, paste or open a text. It is translated on this Mac and never leaves it.">
        {ready ? (
          <Button variant="ghost" title="Change the translation engine in Settings → Languages" onClick={props.onOpenSettings}>
            <span role="status">{modelName} · {status?.loaded || (usedModelId !== null && usedModelId === modelId) ? "ready" : "loads on first use"}</span>
          </Button>
        ) : null}
      </PageHeader>

      {termsModelId ? (
        <GemmaTermsDialog
          onCancel={() => setTermsModelId(null)}
          onAccept={async () => {
            const id = termsModelId;
            await acceptModelTerms(id);
            setTermsModelId(null);
            await startModelDownload(id);
          }}
        />
      ) : null}

      {needsSetup ? (
        <article className="translate-setup glass-panel" aria-label="Set up translation">
          <h2>Download a translation model to start</h2>
          <p className="muted">Translation runs entirely on this Mac. Pick the model that fits your memory; you can add the other one later in Settings → Languages.</p>
          <ul className="translate-setup-models">
            {translationModels.map((model) => {
              const download = downloads[model.id];
              const compact = model.id === COMPACT_TRANSLATION_MODEL_ID;
              const recommended = compact === recommendCompact;
              return (
                <li key={model.id} className={recommended ? "is-recommended" : undefined}>
                  <div>
                    <strong>{compact ? "Compact" : "Best quality"} · {model.modelName}</strong>
                    {recommended ? <span className="status-pill status-pill-success">Recommended for this Mac</span> : null}
                    <p className="muted">{formatBytes(model.sizeBytes)} download · {model.requirements}</p>
                  </div>
                  {download?.state === "downloading" ? (
                    <div className="translate-setup-progress">
                      <span className="status-pill status-pill-processing">Downloading · {Math.round(download.progressPercent ?? 0)}%</span>
                    </div>
                  ) : (
                    <ActionButton
                      variant={recommended ? "primary" : "secondary"}
                      icon="download"
                      disabled={downloadsActive || model.availability !== "available"}
                      action={() => downloadModel(model.id)}
                      success=""
                    >
                      Download
                    </ActionButton>
                  )}
                  {download?.errorMessage ? <p role="alert" className="warning-text">{readableError(download.errorMessage)}</p> : null}
                </li>
              );
            })}
          </ul>
          <p className="muted">Uses Google&apos;s TranslateGemma, subject to the <a href="https://ai.google.dev/gemma/terms" target="_blank" rel="noreferrer">Gemma terms</a>. You agree to them before the download starts.</p>
        </article>
      ) : null}

      <div className="translate-langbar" aria-label="Languages">
        <label className="translate-picker">
          <span>From</span>
          <select aria-label="Source language" value={sourceChoice} onChange={(event) => setSourceChoice(event.target.value)}>
            <option value="auto">{detected ? `${languageName(detected.code)} · detected` : "Detect language"}</option>
            {TRANSLATION_LANGUAGES.map((language) => (
              <option key={language.code} value={language.code}>{language.name}</option>
            ))}
          </select>
        </label>
        <IconButton icon="swap" label="Swap languages" disabled={!canSwap} onClick={swapLanguages} />
        <label className="translate-picker">
          <span>To</span>
          <select aria-label="Target language" value={target} onChange={(event) => setTarget(event.target.value)}>
            {TRANSLATION_LANGUAGES.map((language) => (
              <option key={language.code} value={language.code}>{language.name}</option>
            ))}
          </select>
          {hasFormsOfAddress(target) ? (
            <select aria-label="Tone" className="translate-tone" value={tone} onChange={(event) => setTone(event.target.value as TranslationTone)}>
              {(["auto", "informal", "formal"] as const).map((value) => (
                <option key={value} value={value}>{toneLabel(value, target)}</option>
              ))}
            </select>
          ) : null}
        </label>
        <label className="translate-live">
          <input type="checkbox" checked={live} onChange={(event) => setLive(event.target.checked)} />
          <span>Translate as I type</span>
        </label>
      </div>

      {memoryWarning && !allowLowMemory && !modelOverride ? (
        <div className="translate-banner is-warning" role="alert">
          <p>
            <strong>Not enough free memory.</strong> {memoryWarning} Close some apps, or translate anyway; your Mac may slow down while it works.
          </p>
          <div className="translate-banner-actions">
            {compactInstalled && modelId !== COMPACT_TRANSLATION_MODEL_ID ? (
              <Button variant="primary" onClick={() => { setModelOverride(COMPACT_TRANSLATION_MODEL_ID); setMemoryWarning(null); window.setTimeout(() => void runRef.current(true), 0); }}>
                Use the compact model
              </Button>
            ) : !compactInstalled && compactModel?.availability === "available" ? (
              <ActionButton icon="download" disabled={downloadsActive} action={() => downloadModel(COMPACT_TRANSLATION_MODEL_ID)}>
                Download the compact model ({formatBytes(compactModel.sizeBytes)})
              </ActionButton>
            ) : null}
            <Button onClick={() => { setAllowLowMemory(true); setMemoryWarning(null); window.setTimeout(() => void runRef.current(true), 0); }}>
              Translate anyway
            </Button>
          </div>
        </div>
      ) : null}
      {error ? <p className="error-text" role="alert">{error}</p> : null}
      {proNotice && !license?.pro ? (
        <ProHint>
          {proNotice === "document"
            ? "Opening and translating whole documents is part of Murmela Pro. You can still paste text here."
            : "Clicking a word for alternatives is part of Murmela Pro."}
        </ProHint>
      ) : null}
      {wordEdit ? (
        <AlternativesMenu
          word={wordEdit.word}
          anchor={wordAnchor}
          state={wordEdit.state}
          onPick={(choice) => void pickAlternative(choice)}
          onClose={closeAlternatives}
        />
      ) : null}
      <ModelDownloadCard
        downloads={Object.values(downloads).filter((entry) => models.some((model) => model.id === entry.modelId))}
        onCancel={cancelModelDownload}
      />

      <div className="translate-panes">
        <div className="translate-pane">
          <div className="translate-pane-body translate-source">
            <div className="translate-source-backdrop" ref={backdropRef} aria-hidden="true">
              {segmented.before}
              {segmented.pieces.map((piece, index) => (
                <span key={index}>
                  {hovered === index ? <mark>{piece.text}</mark> : piece.text}
                  {piece.after}
                </span>
              ))}
              {"\n"}
            </div>
            <textarea
              ref={sourceRef}
              onScroll={(event) => {
                if (backdropRef.current) backdropRef.current.scrollTop = event.currentTarget.scrollTop;
              }}
              aria-label="Text to translate"
              placeholder={dictating ? "Listening…" : "Type or paste text…"}
              value={sourceText}
              readOnly={dictating}
              spellCheck
              onChange={(event) => {
                setSourceText(event.target.value);
                setSavedToLibrary(false);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault();
                  void run(true);
                } else if (event.key === "Escape" && job) {
                  event.preventDefault();
                  stop();
                }
              }}
            />
            {!sourceText && !dictating ? (
              <div className={"translate-drop-hint" + (props.dragging ? " is-dragging" : "")}>
                <AppIcon name="upload" />
                <p>
                  <strong>{props.dragging ? "Drop to open" : "Or drop a text file here"}</strong>
                  <span className="muted">.txt and .md files up to 200,000 characters{license?.pro ? "" : " · "}{license?.pro ? null : <ProTag />}</span>
                </p>
              </div>
            ) : null}
          </div>
          <div className="translate-pane-foot">
            <IconButton
              icon={dictating ? "microphoneActive" : "microphone"}
              label={dictation === "listening" ? "Stop dictating" : dictation === "off" ? "Dictate" : dictation === "starting" ? "Starting…" : "Finishing…"}
              size="compact"
              state={dictation === "listening" ? "selected" : dictation === "off" ? "default" : "busy"}
              className={"translate-mic" + (dictation === "listening" ? " is-listening" : "")}
              aria-pressed={dictation === "listening"}
              disabled={dictation === "starting" || dictation === "finishing"}
              onClick={() => void toggleDictation()}
            />
            {dictating ? (
              <span className="translate-mic-status" role="status">
                {dictation === "starting" ? "Starting the microphone…" : dictation === "listening" ? "Listening · click the microphone to stop" : "Finishing…"}
              </span>
            ) : (
              <span className="muted">
                {documentInfo.fileName ? `${documentInfo.fileName} · ` : ""}
                {characterCount.toLocaleString()} characters{paragraphCount > 1 ? ` · ${paragraphCount} paragraphs` : ""}
              </span>
            )}
            <span className="translate-pane-actions">
              <Button variant="ghost" icon="folder" disabled={dictating} onClick={() => void openFile()}>Open file…</Button>
              <span className="translate-library-menu">
                <Button variant="ghost" icon="library" aria-expanded={libraryMenuOpen} disabled={props.transcripts.length === 0 || dictating} onClick={() => setLibraryMenuOpen((open) => !open)}>
                  From Library
                </Button>
                {libraryMenuOpen ? (
                  <ul className="translate-library-list" role="menu" aria-label="Recent transcripts">
                    {props.transcripts.slice(0, 8).map((transcript) => (
                      <li key={transcript.id}>
                        <button
                          type="button"
                          role="menuitem"
                          onClick={() => {
                            setLibraryMenuOpen(false);
                            loadText(transcript.plainText, newDocument("transcript", null, transcript.title));
                          }}
                        >
                          <strong>{transcript.title}</strong>
                          <span className="muted">{transcript.plainText.slice(0, 80)}</span>
                        </button>
                      </li>
                    ))}
                  </ul>
                ) : null}
              </span>
              {sourceText ? <Button variant="ghost" icon="xmark" disabled={dictating} onClick={clear}>Clear</Button> : null}
            </span>
          </div>
        </div>

        <div className="translate-pane translate-output" aria-live="polite">
          <div className="translate-pane-body">
            {!sourceText.trim() ? (
              <p className="muted translate-placeholder">The translation appears here, paragraph by paragraph.</p>
            ) : alreadyInTarget ? (
              <p className="muted translate-placeholder">This text is already in {languageName(target)}. Choose another language under “To”.</p>
            ) : tooLong ? (
              <p className="muted translate-placeholder">This text has {characterCount.toLocaleString()} characters. The limit is 200,000. Split it and translate the parts.</p>
            ) : (
              segmented.pieces.map((piece, index) => {
                const shown = shownOutputs[index];
                const className = "translate-para" + (hovered === index ? " is-linked" : "");
                const hover = { onMouseEnter: () => setHovered(index), onMouseLeave: () => setHovered(null) };
                if (shown.kind === "verbatim") {
                  return <pre key={index} className={className + " is-verbatim"} {...hover}>{shown.text}</pre>;
                }
                if (shown.kind === "failed") {
                  return (
                    <div key={index} className={className + " is-failed"} {...hover}>
                      <p>{shown.text}</p>
                      <p className="translate-para-note">
                        Couldn&apos;t translate this paragraph.{" "}
                        <button type="button" className="link-button" onClick={() => void run(true)}>Try again</button>
                      </p>
                    </div>
                  );
                }
                if (shown.kind === "done") {
                  const key = keys[index] as string;
                  const edit = wordEdit && wordEdit.key === key && wordEdit.text === shown.text ? wordEdit : null;
                  return (
                    <div key={index} className={className + (edit?.state.status === "rewriting" ? " is-rewriting" : "")} {...hover}>
                      <p className="translate-para-words" onClick={(event) => openAlternatives(event, index, key, piece.text, shown.text)}>
                        {edit ? (
                          <>
                            {shown.text.slice(0, edit.start)}
                            <mark ref={setWordAnchor} className="translate-word">{edit.word}</mark>
                            <span className="translate-para-rest">{shown.text.slice(edit.end)}</span>
                          </>
                        ) : (
                          shown.text
                        )}
                      </p>
                      {shown.issues.length ? (
                        <p className="translate-para-note">Check this paragraph: {shown.issues.map((issue) => ISSUE_COPY[issue] ?? issue).join(", ")}.</p>
                      ) : null}
                      {lastEdit?.key === key ? (
                        <p className="translate-para-note is-edit" role="status">
                          Replaced “{lastEdit.from}” with “{lastEdit.to}”.{" "}
                          <button type="button" className="link-button" onClick={undoLastEdit}>Undo</button>
                        </p>
                      ) : null}
                    </div>
                  );
                }
                return shown.text ? (
                  <div key={index} className={className + " is-stale"} {...hover}><p>{shown.text}</p></div>
                ) : (
                  <div key={index} className={className + (shown.kind === "working" ? " is-pending" : " is-missing")} aria-hidden="true" {...hover}>
                    <p>{piece.text}</p>
                  </div>
                );
              })
            )}
          </div>
          <div className="translate-pane-foot">
            {job ? (
              <>
                <span role="status">{jobLabel}</span>
                {job.total > 1 ? (
                  <span className="translate-progress" aria-hidden="true"><i style={{ width: `${Math.round((job.done / job.total) * 100)}%` }} /></span>
                ) : null}
                <span className="translate-pane-actions">
                  <Button variant="danger" icon="stop" onClick={stop}>Stop</Button>
                </span>
              </>
            ) : needsManual || (!live && sourceText.trim() && !complete && !alreadyInTarget) ? (
              <>
                <span className="muted">{needsManual ? "Long text: translating it takes a while." : "Ready to translate."}</span>
                <span className="translate-pane-actions">
                  <Button variant="primary" icon="languages" disabled={!ready || tooLong} onClick={() => void run(true)}>
                    Translate <kbd>⌘↵</kbd>
                  </Button>
                </span>
              </>
            ) : complete && anyOutput ? (
              <>
                <span className="muted">
                  Click a word for alternatives · {modelName}{savedToLibrary ? " · saved in Library" : ""}
                </span>
                <span className="translate-pane-actions">
                  <ActionButton variant="ghost" icon="download" success="Saved" action={async () => {
                    await saveTranslationFile(translatedFileName(documentInfo.fileName, target), outputText);
                  }}>
                    Save as…
                  </ActionButton>
                  <ActionButton variant="primary" icon="copy" success="Copied" action={() => copyTextToClipboard(outputText)}>
                    Copy
                  </ActionButton>
                </span>
              </>
            ) : (
              <span className="muted">{ready ? (status?.loaded ? "Ready" : "The model loads when you start; the first translation takes a little longer.") : needsSetup ? "Download a model to start." : ""}</span>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}
