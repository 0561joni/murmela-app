import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { AppIcon } from "./components/IconButton";
import { AlternativesMenu, type AlternativesState } from "./components/AlternativesMenu";
import { useLicense, isProRequiredError } from "./hooks/useLicense";
import { getSettings } from "./lib/api";
import { describeError, readableError } from "./lib/formatting";
import {
  closeQuickTranslation,
  copyQuickTranslation,
  currentQuickTranslation,
  announceLibraryChange,
  editQuickTranslation,
  listenQuickTranslation,
  pauseQuickTranslation,
  rememberQuickTarget,
  replaceWithQuickTranslation,
  requestAccessibility,
  resizeQuickTranslation,
  type QuickTranslateRequest,
} from "./lib/quickTranslateApi";
import {
  COMPACT_TRANSLATION_MODEL_ID,
  TRANSLATION_LANGUAGES,
  cancelTextTranslation,
  getTranslationAlternatives,
  getTranslatorStatus,
  joinPieces,
  languageName,
  listenTextTranslation,
  sameLanguage,
  rewriteTranslation,
  saveTextTranslation,
  segmentText,
  translateText,
  translationTitle,
  wordAt,
  type SegmentedText,
} from "./lib/textTranslation";

type Mode = "translate" | "access" | "copied";
type Stage = "waiting" | "checking" | "loading" | "translating" | null;

interface Run {
  requestId: string;
  segmented: SegmentedText;
  outputs: (string | null)[];
  failed: number;
  done: boolean;
}

/** A clicked word in a translated paragraph, while its alternatives menu is open. */
interface WordEdit {
  requestId: string;
  index: number;
  text: string;
  start: number;
  end: number;
  word: string;
  state: AlternativesState;
  /** The card's height before the menu opened; it grows to make room. */
  baseHeight: number;
}

/** Short names for the panel's language menu; the Translate screen shows the long ones. */
const SHORT_NAMES: Record<string, string> = {
  es: "Spanish (Spain)",
  "es-AR": "Spanish (Argentina)",
  "pt-BR": "Portuguese",
};
const shortName = (code: string) => SHORT_NAMES[code] ?? languageName(code);

/** How much the card grows while a word's alternatives menu is open. */
const ALTERNATIVES_ROOM = 250;

/** Most words in common: the "translation" came back as the text itself. */
export function looksUnchanged(source: string, output: string) {
  const words = (text: string) => new Set(text.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean));
  const a = words(source);
  const b = words(output);
  if (a.size === 0 || b.size === 0) return false;
  let shared = 0;
  for (const word of a) if (b.has(word)) shared += 1;
  return shared / Math.max(a.size, b.size) >= 0.6;
}

function newId() {
  return typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : String(Date.now() + Math.random());
}

/**
 * The quick-translation panel: copy text twice (⌘C ⌘C) in any app and it
 * opens by the pointer with the translation. ↩ replaces the selection, ⌘C
 * copies, Esc closes. It never activates Murmela, so the app being worked in
 * keeps its selection.
 */
export function QuickTranslateApp() {
  const [request, setRequest] = useState<QuickTranslateRequest | null>(null);
  const [target, setTarget] = useState("en");
  const [run, setRun] = useState<Run | null>(null);
  const [stage, setStage] = useState<Stage>(null);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("translate");
  const [menu, setMenu] = useState<"language" | "pause" | null>(null);
  const [wordEdit, setWordEdit] = useState<WordEdit | null>(null);
  const wordEditRef = useRef<WordEdit | null>(null);
  wordEditRef.current = wordEdit;
  const [wordAnchor, setWordAnchor] = useState<HTMLElement | null>(null);
  const [lastWordEdit, setLastWordEdit] = useState<{ index: number; previous: string; from: string; to: string } | null>(null);
  const [proNote, setProNote] = useState(false);
  /** The text was in your language after all, so the panel switched target once. */
  const [switchedNote, setSwitchedNote] = useState<string | null>(null);
  const switched = useRef(false);
  const { status: license } = useLicense();
  /** The run already saved in the Library, so a re-render doesn't save it twice. */
  const savedRun = useRef<string | null>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const runRef = useRef<Run | null>(null);
  runRef.current = run;

  const complete = Boolean(run?.done);
  const translatedAny = Boolean(run?.outputs.some((value) => value != null));
  // Only translated paragraphs show: joinPieces fills missing ones with the
  // source text, which must never look like a result.
  const output =
    run && translatedAny
      ? complete
        ? joinPieces(run.segmented, run.outputs).trim()
        : run.segmented.pieces
            .map((piece, index) => (piece.translatable ? run.outputs[index] : piece.text))
            .filter((text): text is string => Boolean(text))
            .join("\n\n")
            .trim()
      : "";
  const ready = complete && !error && output.length > 0;

  /** The engine and memory choice for this panel, kept when the target changes. */
  const engineRef = useRef<{ modelId: string | null; allowLowMemory: boolean }>({ modelId: null, allowLowMemory: false });
  const [engineNote, setEngineNote] = useState<string | null>(null);
  const [memoryWarning, setMemoryWarning] = useState<string | null>(null);

  const translate = useCallback(async (text: string, source: string | null, targetLanguage: string) => {
    const previous = runRef.current;
    if (previous && !previous.done) void cancelTextTranslation(previous.requestId);
    const segmented = segmentText(text);
    const requestId = newId();
    const next: Run = { requestId, segmented, outputs: segmented.pieces.map(() => null), failed: 0, done: false };
    runRef.current = next;
    setRun(next);
    setStage(null);
    setError(null);
    setMemoryWarning(null);
    const segments = segmented.pieces
      .map((piece, index) => ({ index, text: piece.text, translatable: piece.translatable }))
      .filter((piece) => piece.translatable)
      .map(({ index, text: pieceText }) => ({ index, text: pieceText }));
    const current = () => runRef.current?.requestId === requestId;
    try {
      await translateText({
        requestId,
        segments,
        sourceLanguage: source ?? "auto",
        targetLanguage,
        tone: "auto",
        modelId: engineRef.current.modelId,
        allowLowMemory: engineRef.current.allowLowMemory,
      });
    } catch (reason) {
      if (!current()) return;
      const message = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : String(reason);
      if (message.startsWith("TRANSLATION_CANCELED")) return;
      if (message.startsWith("TRANSLATION_MEMORY_LOW")) {
        // The large engine doesn't fit right now: the compact one usually does.
        const status = await getTranslatorStatus().catch(() => null);
        if (
          current() &&
          engineRef.current.modelId !== COMPACT_TRANSLATION_MODEL_ID &&
          status?.installedModelIds.includes(COMPACT_TRANSLATION_MODEL_ID)
        ) {
          engineRef.current.modelId = COMPACT_TRANSLATION_MODEL_ID;
          setEngineNote("Used the compact engine: not enough free memory for the large one right now.");
          void translate(text, source, targetLanguage);
          return;
        }
        if (current()) setMemoryWarning(readableError(message));
        return;
      }
      setError(describeError(reason, "Murmela couldn't translate this."));
    } finally {
      if (current()) {
        setRun((value) => (value && value.requestId === requestId ? { ...value, done: true } : value));
        setStage(null);
      }
    }
  }, []);

  const show = useCallback(
    (next: QuickTranslateRequest) => {
      setRequest(next);
      setTarget(next.targetLanguage);
      engineRef.current = { modelId: null, allowLowMemory: false };
      setEngineNote(null);
      setWordEdit(null);
      setLastWordEdit(null);
      setProNote(false);
      setSwitchedNote(null);
      switched.current = false;
      setMode("translate");
      setMenu(null);
      void translate(next.text, next.sourceLanguage, next.targetLanguage);
    },
    [translate],
  );

  // The request that opened the panel, and every later one.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void currentQuickTranslation().then((current) => {
      if (!disposed && current) show(current);
    });
    void listenQuickTranslation(show).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [show]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listenTextTranslation((event) => {
      const current = runRef.current;
      if (!current || event.requestId !== current.requestId) return;
      if (event.kind === "stage") {
        setStage(event.stage);
        return;
      }
      setRun((value) => {
        if (!value || value.requestId !== event.requestId) return value;
        const outputs = [...value.outputs];
        if (event.kind === "segment") outputs[event.index] = event.text;
        return { ...value, outputs, failed: value.failed + (event.kind === "segmentFailed" ? 1 : 0) };
      });
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const close = useCallback(() => {
    const current = runRef.current;
    if (current && !current.done) void cancelTextTranslation(current.requestId);
    setMenu(null);
    void closeQuickTranslation();
  }, []);

  const copy = useCallback(async () => {
    if (!ready) return;
    try {
      await copyQuickTranslation(output);
      setMode("copied");
      window.setTimeout(close, 650);
    } catch (reason) {
      setError(describeError(reason, "Couldn't copy the translation."));
    }
  }, [close, output, ready]);

  const replace = useCallback(async () => {
    if (!ready || !request?.canReplace) return;
    try {
      const outcome = await replaceWithQuickTranslation(output);
      if (outcome === "needs_accessibility") setMode("access");
    } catch (reason) {
      setError(describeError(reason, "Couldn't put the translation into the app."));
    }
  }, [output, ready, request]);

  const edit = useCallback(() => {
    if (!request) return;
    void editQuickTranslation(request.text, request.sourceLanguage, target);
  }, [request, target]);

  const closeAlternatives = useCallback(() => {
    const current = wordEditRef.current;
    if (current && (current.state.status === "loading" || current.state.status === "rewriting")) {
      void cancelTextTranslation(current.requestId);
    }
    setWordEdit(null);
  }, []);

  function editRequest(index: number, text: string, prefix: string, word: string, requestId: string) {
    return {
      requestId,
      sourceText: run?.segmented.pieces[index]?.text ?? "",
      translation: text,
      prefix,
      word,
      sourceLanguage: request?.sourceLanguage ?? "auto",
      targetLanguage: target,
      tone: "auto" as const,
      modelId: engineRef.current.modelId,
    };
  }

  /** A click on a translated paragraph opens alternatives for the word under the pointer (Pro). */
  function openAlternatives(event: React.MouseEvent<HTMLParagraphElement>, index: number, text: string) {
    if (!complete) return;
    const selection = window.getSelection();
    if (selection && !selection.isCollapsed) return; // Selecting text to copy.
    const caret = document.caretRangeFromPoint?.(event.clientX, event.clientY);
    if (!caret || !event.currentTarget.contains(caret.startContainer)) return;
    const before = document.createRange();
    before.selectNodeContents(event.currentTarget);
    before.setEnd(caret.startContainer, caret.startOffset);
    const found = wordAt(text, before.toString().length);
    closeAlternatives();
    if (!found) return;
    if (license && !license.pro) {
      setProNote(true);
      return;
    }
    const baseHeight = Math.ceil(cardRef.current?.getBoundingClientRect().height ?? 0);
    const next: WordEdit = { requestId: newId(), index, text, ...found, state: { status: "loading" }, baseHeight };
    setWordEdit(next);
    getTranslationAlternatives(editRequest(index, text, text.slice(0, found.start), found.word, next.requestId))
      .then((items) => {
        setWordEdit((value) => (value?.requestId === next.requestId ? { ...value, state: { status: "ready", items } } : value));
      })
      .catch((reason) => {
        const message = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : String(reason);
        if (isProRequiredError(reason)) setProNote(true);
        setWordEdit((value) => {
          if (value?.requestId !== next.requestId) return value;
          if (isProRequiredError(reason) || message.startsWith("TRANSLATION_CANCELED")) return null;
          return { ...value, state: { status: "error", message: describeError(reason, "Could not find alternatives.") } };
        });
      });
  }

  async function pickAlternative(choice: string) {
    const current = wordEditRef.current;
    if (!current) return;
    const requestId = newId();
    setWordEdit({ ...current, requestId, state: { status: "rewriting", choice } });
    try {
      const result = await rewriteTranslation(
        editRequest(current.index, current.text, current.text.slice(0, current.start) + choice, current.word, requestId),
      );
      if (wordEditRef.current?.requestId !== requestId) return;
      setRun((value) => {
        if (!value || value.outputs[current.index] !== current.text) return value;
        const outputs = [...value.outputs];
        outputs[current.index] = result.text;
        return { ...value, outputs };
      });
      setLastWordEdit({ index: current.index, previous: current.text, from: current.word, to: choice });
      setWordEdit(null);
    } catch (reason) {
      if (wordEditRef.current?.requestId !== requestId) return;
      const message = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : String(reason);
      if (message.startsWith("TRANSLATION_CANCELED")) setWordEdit(null);
      else setWordEdit({ ...current, requestId, state: { status: "error", message: describeError(reason, "Could not rewrite this paragraph.") } });
    }
  }

  function undoWordEdit() {
    if (!lastWordEdit) return;
    setRun((value) => {
      if (!value) return value;
      const outputs = [...value.outputs];
      outputs[lastWordEdit.index] = lastWordEdit.previous;
      return { ...value, outputs };
    });
    setLastWordEdit(null);
  }

  // Keep finished panel translations in the Library when Settings asks for it.
  useEffect(() => {
    if (!ready || !run || !request || savedRun.current === run.requestId) return;
    const runId = run.requestId;
    savedRun.current = runId;
    void getSettings()
      .then((settings) => {
        if (!settings.quickTranslateSaveHistory) return null;
        return saveTextTranslation({
          id: newId(),
          title: translationTitle(null, request.text),
          sourceKind: "text",
          fileName: null,
          sourceText: request.text,
          outputText: output,
          segments: run.segmented.pieces.map((piece, index) => ({ source: piece.text, output: piece.translatable ? run.outputs[index] : null })),
          sourceLanguage: request.sourceLanguage,
          targetLanguage: target,
          tone: "auto",
          modelId: engineRef.current.modelId,
        });
      })
      .then((saved) => {
        if (saved) void announceLibraryChange();
      })
      .catch(() => undefined);
  }, [ready, run?.requestId]); // eslint-disable-line react-hooks/exhaustive-deps

  // "Translated" into your own language and nothing changed: the text was
  // yours all along (short texts fool the detector), so go the other way.
  useEffect(() => {
    if (!ready || !request || switched.current) return;
    // A confident detection of another language is trusted.
    if (request.sourceLanguage && !sameLanguage(request.sourceLanguage, request.myLanguage)) return;
    if (!sameLanguage(target, request.myLanguage) || sameLanguage(request.otherLanguage, request.myLanguage)) return;
    if (!looksUnchanged(request.text, output)) return;
    switched.current = true;
    setSwitchedNote(`This is already ${languageName(request.myLanguage)}, so Murmela translated it into ${shortName(request.otherLanguage)}.`);
    setTarget(request.otherLanguage);
    void translate(request.text, request.myLanguage, request.otherLanguage);
  }, [ready, run?.requestId]); // eslint-disable-line react-hooks/exhaustive-deps

  function chooseTarget(code: string) {
    switched.current = true;
    setSwitchedNote(null);
    closeAlternatives();
    setLastWordEdit(null);
    setMenu(null);
    if (!request || code === target) return;
    setTarget(code);
    void rememberQuickTarget(code);
    void translate(request.text, request.sourceLanguage, code);
  }

  // Keys: ↩ replaces (or copies when there is no app to put it into), ⌘C
  // copies unless text in the panel is selected, ⌘E edits, Esc closes.
  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        if (menu) setMenu(null);
        else if (wordEditRef.current) closeAlternatives();
        else close();
        return;
      }
      if (menu || wordEditRef.current) return;
      if (event.key === "Enter" && !event.shiftKey && !event.metaKey && !event.altKey) {
        // A focused button handles its own Enter.
        if (event.target instanceof Element && event.target.closest("button")) return;
        event.preventDefault();
        if (mode === "access") return;
        void (request?.canReplace ? replace() : copy());
        return;
      }
      if (event.metaKey && event.key.toLowerCase() === "c" && !window.getSelection()?.toString()) {
        event.preventDefault();
        void copy();
        return;
      }
      if (event.metaKey && event.key.toLowerCase() === "e") {
        event.preventDefault();
        edit();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [close, closeAlternatives, copy, edit, menu, mode, replace, request]);

  // Clicking into another app closes the panel, as a menu would.
  useEffect(() => {
    function onBlur() {
      if (mode !== "access") close();
    }
    window.addEventListener("blur", onBlur);
    return () => window.removeEventListener("blur", onBlur);
  }, [close, mode]);

  // The window follows the card's height.
  useLayoutEffect(() => {
    const card = cardRef.current;
    if (!card || typeof ResizeObserver === "undefined") return;
    let last = 0;
    const observer = new ResizeObserver(() => {
      const height = Math.ceil(card.getBoundingClientRect().height);
      if (Math.abs(height - last) < 2) return;
      last = height;
      void resizeQuickTranslation(height);
    });
    observer.observe(card);
    return () => observer.disconnect();
  }, []);

  if (!request) return <div ref={cardRef} className="quick-card is-empty" />;

  const source = request.sourceLanguage;
  const pending = run ? run.segmented.pieces.filter((piece, index) => piece.translatable && run.outputs[index] == null).length : 0;
  const translating = Boolean(run && !run.done);
  const status = stage === "waiting" ? "Waiting for your dictation to finish…" : stage === "loading" || stage === "checking" ? "Starting the translator…" : null;
  const sameAsSource = source !== null && sameLanguage(source, target);

  return (
    <div
      ref={cardRef}
      className={"quick-card" + (menu ? ` has-${menu}-menu` : "")}
      style={wordEdit ? { minHeight: wordEdit.baseHeight + ALTERNATIVES_ROOM } : undefined}
      role="dialog"
      aria-label="Translation"
    >
      <header className="quick-header">
        <span className="quick-mark" aria-hidden="true" />
        <span className="quick-direction">
          {source ? <span className="quick-source">{languageName(source)}</span> : null}
          {source ? <span className="quick-arrow" aria-hidden="true">→</span> : null}
          <button
            type="button"
            className="quick-target"
            aria-haspopup="listbox"
            aria-expanded={menu === "language"}
            aria-label={`Translate into ${shortName(target)}. Change language`}
            onClick={() => setMenu(menu === "language" ? null : "language")}
          >
            {shortName(target)}
            <AppIcon name="chevronDown" />
          </button>
        </span>
        <span className="quick-header-actions">
          <button
            type="button"
            className="quick-icon"
            aria-label="Pause or turn off ⌘C twice"
            title="Pause or turn off"
            aria-expanded={menu === "pause"}
            onClick={() => setMenu(menu === "pause" ? null : "pause")}
          >
            <AppIcon name="pause" />
          </button>
          <button type="button" className="quick-icon" aria-label="Close" title="Close (Esc)" onClick={close}>
            <AppIcon name="xmark" />
          </button>
        </span>
        {menu === "language" ? (
          <ul className="quick-menu quick-language-menu" role="listbox" aria-label="Translate into">
            {[request.myLanguage, ...TRANSLATION_LANGUAGES.map((language) => language.code).filter((code) => code !== request.myLanguage)].map((code) => (
              <li key={code} role="option" aria-selected={code === target}>
                <button type="button" onClick={() => chooseTarget(code)}>
                  <span className="quick-menu-check" aria-hidden="true">{code === target ? <AppIcon name="check" /> : null}</span>
                  {shortName(code)}
                  {code === request.myLanguage ? <span className="quick-menu-note">Your language</span> : null}
                </button>
              </li>
            ))}
          </ul>
        ) : null}
        {menu === "pause" ? (
          <ul className="quick-menu quick-pause-menu" aria-label="Pause">
            <li><button type="button" onClick={() => void pauseQuickTranslation(60)}>Pause for 1 hour</button></li>
            <li><button type="button" onClick={() => void pauseQuickTranslation(null)}>Turn off ⌘C twice</button></li>
            <li className="quick-menu-hint">Turn it on again in Settings → Languages.</li>
          </ul>
        ) : null}
      </header>

      {mode === "access" ? (
        <div className="quick-body quick-access">
          <p><strong>One step before Replace</strong></p>
          <p>
            To put the translation in place of your selection, Murmela presses ⌘V for you. macOS calls this permission
            Accessibility. Murmela doesn't read your screen or what you type.
          </p>
          <div className="quick-actions">
            <button type="button" className="quick-button" onClick={() => void copy()} disabled={!ready}>
              <AppIcon name="copy" />Copy instead
            </button>
            <span className="quick-spacer" />
            <button
              type="button"
              className="quick-button is-primary"
              onClick={() => {
                void requestAccessibility();
                close();
              }}
            >
              Allow in System Settings
            </button>
          </div>
        </div>
      ) : (
        <>
          <div className={"quick-body" + (translating && !output ? " is-pending" : "")} aria-live="polite" aria-busy={translating}>
            {output && run ? (
              <div className="quick-output">
                {run.segmented.pieces.map((piece, index) => {
                  if (!piece.translatable) return <p key={index}>{piece.text}</p>;
                  const text = run.outputs[index];
                  if (text == null) return null;
                  const editing = wordEdit && wordEdit.index === index && wordEdit.text === text ? wordEdit : null;
                  return (
                    <p
                      key={index}
                      className={"quick-para" + (complete ? " is-clickable" : "") + (editing?.state.status === "rewriting" ? " is-rewriting" : "")}
                      onClick={(event) => openAlternatives(event, index, text)}
                    >
                      {editing ? (
                        <>
                          {text.slice(0, editing.start)}
                          <mark ref={setWordAnchor} className="quick-word">{editing.word}</mark>
                          {text.slice(editing.end)}
                        </>
                      ) : (
                        text
                      )}
                    </p>
                  );
                })}
              </div>
            ) : null}
            {lastWordEdit ? (
              <p className="quick-status" role="status">
                Replaced “{lastWordEdit.from}” with “{lastWordEdit.to}”.{" "}
                <button type="button" className="link-button" onClick={undoWordEdit}>Undo</button>
              </p>
            ) : null}
            {switchedNote && output ? <p className="quick-status">{switchedNote}</p> : null}
            {proNote ? <p className="quick-status">Clicking a word for alternatives is part of Murmela Pro.</p> : null}
            {translating && pending > 0 ? (
              <span className="quick-shimmer" aria-label={status ?? "Translating…"}>
                <span />
                <span />
              </span>
            ) : null}
            {status ? <p className="quick-status">{status}</p> : null}
            {sameAsSource && complete ? <p className="quick-status">This text is already in {languageName(target)}. Pick another language above.</p> : null}
            {run && run.done && run.failed > 0 ? <p className="quick-warning">Some parts couldn't be translated and are shown as they were.</p> : null}
            {error ? <p className="quick-warning" role="alert">{error}</p> : null}
            {memoryWarning ? (
              <div className="quick-warning" role="alert">
                <p>Not enough free memory. {memoryWarning} Close some apps, or translate anyway; your Mac may slow down while it works.</p>
                <button
                  type="button"
                  className="quick-button"
                  onClick={() => {
                    if (!request) return;
                    engineRef.current.allowLowMemory = true;
                    void translate(request.text, request.sourceLanguage, target);
                  }}
                >
                  Translate anyway
                </button>
              </div>
            ) : null}
            {engineNote && output ? <p className="quick-status">{engineNote}</p> : null}
          </div>
          <footer className="quick-actions">
            <button type="button" className="quick-button" disabled={!ready} onClick={() => void copy()} title="Copy (⌘C)">
              <AppIcon name={mode === "copied" ? "check" : "copy"} />
              {mode === "copied" ? "Copied" : "Copy"}
            </button>
            <button type="button" className="quick-button" onClick={edit} title="Edit in Murmela (⌘E)">
              <AppIcon name="pencil" />Edit in Murmela
            </button>
            <span className="quick-spacer" />
            {request.canReplace ? (
              <button
                type="button"
                className="quick-button is-primary"
                disabled={!ready}
                onClick={() => void replace()}
                title={request.appName ? `Replace the selection in ${request.appName} (↩)` : "Replace the selection (↩)"}
              >
                Replace <kbd aria-hidden="true">↩</kbd>
              </button>
            ) : null}
          </footer>
          <p className="quick-footnote">
            <span aria-hidden="true">🔒</span> Translated on this Mac{request.appName ? <> · from {request.appName}</> : null}
          </p>
        </>
      )}
      {wordEdit ? (
        <AlternativesMenu
          word={wordEdit.word}
          anchor={wordAnchor}
          state={wordEdit.state}
          onPick={(choice) => void pickAlternative(choice)}
          onClose={closeAlternatives}
        />
      ) : null}
    </div>
  );
}
