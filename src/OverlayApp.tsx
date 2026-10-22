import { outputLanguageLabel } from "./lib/translationApi";
import type { OutputMode } from "./types/domain";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { PASTE_SHORTCUT } from "./lib/formatting";
import { AppIcon } from "./components/IconButton";
import { OverlayWaveform } from "./components/OverlayWaveform";
import { splitLiveTranscript } from "./lib/liveTranscript";
import { usePlayful } from "./lib/appearance";
import { LISTENING_LINES, PROCESSING_LINES, pickLine } from "./lib/murmelmeter";
import { Mela } from "./components/Mela";
import { melaFlag, overlayMood, useMelaEnabled } from "./lib/mela";

/** How long the playful listening line stays before the pill shrinks to the waveform. */
const PLAYFUL_LINE_MS = 2200;

/** Plain progress labels that may be swapped for a playful line. */
const NEUTRAL_PROGRESS = new Set(["Finishing", "Transcribing"]);

type OverlayPhase =
  | "mode"
  | "hidden"
  | "listening"
  | "processing"
  | "inserted"
  | "clipboard_only"
  | "failed"
  | "notice";
interface OverlayPayload {
  phase: OverlayPhase;
  audioLevel: number;
  outputMode?: OutputMode;
  statusText?: string | null;
  revision?: number;
  sessionId?: string | null;
  liveText?: string;
  tentativeText?: string;
  streamingState?: "preparing" | "loading" | "waiting" | "listening" | "catching_up" | "finishing" | "finalizing" | "translating" | "failed" | null;
  lagMs?: number;
  durationLimitReached?: boolean;
}

export function OverlayApp() {
  const [latest, setStatus] = useState<OverlayPayload>({
    phase: "hidden",
    audioLevel: 0,
  });
  // While the pill animates out it keeps showing what it last said.
  const lastShown = useRef<OverlayPayload>(latest);
  if (latest.phase !== "hidden") lastShown.current = latest;
  const hidden = latest.phase === "hidden";
  const status = hidden ? { ...lastShown.current, audioLevel: 0 } : latest;
  const playful = usePlayful();
  // A fresh line for each dictation that never changes while it runs.
  const [lineSeed, setLineSeed] = useState(0);
  const viewport = useRef<HTMLDivElement>(null);
  const [overflow, setOverflow] = useState(false);
  const committed = status.liveText ?? "";
  const tentative = status.tentativeText ?? "";
  const displayedText = committed + tentative;
  const styledText = useMemo(() => splitLiveTranscript(committed, tentative), [committed, tentative]);
  useEffect(() => {
    const element = viewport.current;
    if (!element) return;
    setOverflow(element.scrollWidth > element.clientWidth);
    const reducedMotion = document.documentElement.dataset.motion === "reduced"
      || window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    const behavior = reducedMotion ? "auto" : "smooth";
    if (element.scrollTo) element.scrollTo({ left: element.scrollWidth, behavior });
    else element.scrollLeft = element.scrollWidth;
  }, [displayedText, status.sessionId]);
  // Browser preview only: overlay.html?preview=listening|processing|streaming|inserted|failed|mela (add &mela=on to show Mela)
  useEffect(() => {
    if ("__TAURI_INTERNALS__" in window) return;
    const preview = new URLSearchParams(window.location.search).get("preview");
    if (!preview) return;
    const started = Date.now();
    const tick = () => {
      const t = (Date.now() - started) / 1000;
      const speech = Math.max(0, Math.sin(t * 2.3) * 0.6 + Math.sin(t * 5.1) * 0.25 + 0.25);
      const base = { revision: Math.round(t * 10), sessionId: "preview", audioLevel: speech };
      setStatus(
        preview === "mela"
          ? melaPreviewStep(t % MELA_PREVIEW_SECONDS, base)
          : preview === "streaming"
          ? { ...base, phase: "listening", streamingState: "listening", statusText: "Listening", liveText: "Murmela turns speech into text", tentativeText: " on this Mac" }
          : preview === "processing"
            ? { ...base, phase: "processing" }
            : preview === "inserted" || preview === "failed" || preview === "clipboard_only" || preview === "notice"
              ? { ...base, phase: preview }
              : { ...base, phase: "listening" },
      );
    };
    tick();
    const timer = window.setInterval(tick, 100);
    return () => window.clearInterval(timer);
  }, []);
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    let receivedEvent = false;
    let unlisten: (() => void) | undefined;
    void listen<OverlayPayload>("quick-dictation-overlay", ({ payload }) => {
      receivedEvent = true;
      if (!disposed) setStatus((current) => (payload.revision ?? 0) >= (current.revision ?? 0) ? payload : current);
    })
      .then(async (cleanup) => {
        if (disposed) {
          cleanup();
          return;
        }
        unlisten = cleanup;
        const snapshot = await invoke<OverlayPayload>(
          "get_dictation_overlay_status",
        );
        if (!disposed && !receivedEvent) setStatus(snapshot);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const { phase } = status;
  const listening = phase === "listening";
  useEffect(() => {
    if (listening) setLineSeed((seed) => seed + 1);
  }, [listening, status.sessionId]);
  const seed = `${status.sessionId ?? ""}:${lineSeed}`;
  const playfulListening = playful && !status.streamingState ? pickLine(LISTENING_LINES, seed) : null;
  const playfulProcessing =
    playful && !status.streamingState && (!status.statusText || NEUTRAL_PROGRESS.has(status.statusText))
      ? pickLine(PROCESSING_LINES, seed)
      : null;
  const label =
    status.streamingState && (phase === "listening" || phase === "processing") ? status.statusText || "Listening" : phase === "mode" ? "Output language" : phase === "listening"
      ? status.statusText || playfulListening || "Listening"
      : phase === "processing"
        ? playfulProcessing || status.statusText || "Transcribing"
        : phase === "inserted"
          ? status.statusText || "Pasted" + (status.durationLimitReached ? " · 5-minute limit" : "")
          : phase === "clipboard_only"
            ? status.statusText || "Copied · " + PASTE_SHORTCUT
            : phase === "failed"
              ? status.statusText || "Didn't work · open Murmela"
              : phase === "notice"
                ? status.statusText || ""
                : "";
  const result =
    phase === "inserted" ||
    phase === "clipboard_only" ||
    phase === "failed" ||
    phase === "notice";
  const level = Math.pow(Math.max(0, Math.min(1, status.audioLevel)), 0.65);
  const streaming = Boolean(status.streamingState) && !result;
  const outputMode = status.outputMode ?? "original";

  // The playful line greets each dictation, then the pill settles to the waveform.
  const [greeting, setGreeting] = useState(true);
  useEffect(() => {
    setGreeting(true);
    if (!listening) return;
    const timer = window.setTimeout(() => setGreeting(false), PLAYFUL_LINE_MS);
    return () => window.clearTimeout(timer);
  }, [listening, status.sessionId]);
  const caption =
    phase === "listening" && status.statusText
      ? status.statusText
      : phase === "listening" && playfulListening && greeting
        ? playfulListening
        : null;

  // The pill glides to the width of its content.
  const pill = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState<number | null>(null);
  useLayoutEffect(() => {
    const element = content.current;
    if (!element) return;
    const measure = () => setWidth(Math.ceil(element.scrollWidth));
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  // On appearing, the pill pops in at its final width instead of sliding from
  // the previous dictation's width.
  const [entering, setEntering] = useState(false);
  useEffect(() => {
    if (hidden) return;
    setEntering(true);
    const timer = window.setTimeout(() => setEntering(false), 260);
    return () => window.clearTimeout(timer);
  }, [hidden]);
  // The waveform drives the glow without re-rendering React on every frame.
  const setGlow = useCallback((value: number) => {
    pill.current?.style.setProperty("--glow", value.toFixed(3));
  }, []);

  const tone =
    phase === "failed" ? "error" : phase === "clipboard_only" ? "copied" : phase === "notice" ? "notice" : phase === "inserted" ? "success" : null;
  const mela = useMelaEnabled() && playful;
  const melaMood = overlayMood(latest.phase);

  const stateClass = streaming ? "is-streaming" : result ? "is-result" : phase === "processing" ? "is-processing" : phase === "mode" ? "is-mode" : "is-listening";

  return (
    <div
      className={"overlay-root" + (hidden ? " is-hidden" : "")}
      aria-hidden={hidden}
    >
      <div
        ref={pill}
        className={`overlay-capsule ${stateClass}${tone ? ` tone-${tone}` : ""}${entering ? " is-entering" : ""}${mela ? " has-mela" : ""}`}
        role="group"
        aria-label="Dictation overlay"
        style={width ? { width: width + "px" } : undefined}
      >
        <span className="overlay-glow" aria-hidden="true" />
        <span className="sr-only" role="status" aria-live="polite" aria-atomic="true">{`${label} · ${outputLanguageLabel(outputMode)}`}{streaming && committed ? ` · ${committed}` : ""}</span>
        <div className="overlay-surface">
        <div ref={content} className="overlay-content">
          {outputMode !== "original" && !result ? (
            <span className="overlay-language">{outputLanguageLabel(outputMode)}</span>
          ) : null}
          {streaming ? <>
            <OverlayWaveform level={level} mode={phase === "listening" ? "listening" : "processing"} bars={5} onLevel={setGlow} />
            <div ref={viewport} className={"overlay-live-text" + (overflow ? " has-overflow" : "")} aria-label="Live transcript" aria-live="off">
              {displayedText ? <><span className="overlay-live-committed">{styledText.committed}</span><span className="overlay-live-tentative" aria-label={styledText.tentative ? "Tentative text, may change" : undefined}>{styledText.tentative}</span></> : <span className="overlay-live-placeholder">{label}</span>}
            </div>
            {displayedText ? <span className="overlay-live-state" title={label}>{label}</span> : null}
          </> : phase === "mode" ? (
            <span className="overlay-mode-hint">
              <AppIcon name="languages" />
              {outputMode === "original" ? <><strong>Original</strong> · Output language</> : "Output language"}
            </span>
          ) : phase === "processing" ? (
            <>
              <OverlayWaveform level={0} mode="processing" onLevel={setGlow} />
              <span className="overlay-phase-label">{label}</span>
            </>
          ) : result ? (
            <span
              className={
                "overlay-result" +
                (phase === "failed"
                  ? " is-error"
                  : phase === "clipboard_only"
                    ? " is-copied"
                    : phase === "notice"
                      ? " is-notice"
                      : "")
              }
            >
              <span className="overlay-result-icon" aria-hidden="true">
                <AppIcon name={phase === "failed" || phase === "notice" ? "info" : phase === "clipboard_only" ? "copy" : "check"} />
              </span>
              {label}
            </span>
          ) : (
            <>
              <OverlayWaveform level={level} mode={listening ? "listening" : "rest"} onLevel={setGlow} />
              {caption ? (
                <span className={"overlay-phase-label" + (status.statusText ? " overlay-limit-notice" : "")}>
                  {caption}
                </span>
              ) : null}
            </>
          )}
        </div>
        </div>
        {mela ? (
          <Mela
            place="pill"
            mood={melaMood === "hidden" ? "idle" : melaMood}
            away={melaMood === "hidden"}
            level={listening ? level : 0}
            flag={melaFlag(outputMode)}
          />
        ) : null}
      </div>
    </div>
  );
}

/** One loop of `overlay.html?preview=mela`: dictate, switch language twice, finish, then fail once. */
const MELA_PREVIEW_SECONDS = 21;
function melaPreviewStep(t: number, base: Pick<OverlayPayload, "revision" | "sessionId" | "audioLevel">): OverlayPayload {
  const at = (phase: OverlayPhase, outputMode: OutputMode = "original", extra: Partial<OverlayPayload> = {}) => ({ ...base, phase, outputMode, ...extra });
  if (t < 0.6) return at("hidden");
  if (t < 4.6) return at("listening");
  if (t < 6.6) return at("mode", "de");
  if (t < 8.6) return at("mode", "es-AR");
  if (t < 11.6) return at("listening", "es-AR");
  if (t < 13.8) return at("processing", "es-AR");
  if (t < 15.6) return at("inserted", "es-AR", { statusText: "Pasted · Español (Argentina)" });
  if (t < 16.8) return at("hidden", "es-AR");
  if (t < 19.6) return at("failed", "original", { statusText: "No microphone found · check Settings" });
  return at("hidden");
}
