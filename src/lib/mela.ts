import { useEffect, useRef, useState } from "react";
import type { OutputMode } from "../types/domain";

/**
 * Mela, the marmot companion: a small frosted-glass figurine that reacts to
 * what Murmela is doing. She is drawn in SVG and CSS shipped with the app;
 * nothing is fetched.
 *
 * Whether she is on is a per-Mac preference in localStorage, shared by the
 * main window and the overlay through the storage event. `?mela=on|off`
 * overrides it in the browser preview.
 */
export type MelaMood =
  | "idle"
  | "listen"
  | "think"
  | "work"
  | "happy"
  | "lang"
  | "oops"
  | "private"
  | "sleep"
  | "offline"
  | "egg";

const STORAGE_KEY = "murmela-companion";
const subscribers = new Set<() => void>();

export function readMelaEnabled(): boolean {
  try {
    const fromQuery = new URLSearchParams(window.location.search).get("mela");
    if (fromQuery) return fromQuery !== "off";
  } catch {
    /* No query in this context. */
  }
  try {
    // "crystal" was the stored value while several looks were being compared.
    const stored = localStorage.getItem(STORAGE_KEY);
    return stored === "on" || stored === "crystal";
  } catch {
    return false;
  }
}

export function setMelaEnabled(enabled: boolean) {
  try {
    localStorage.setItem(STORAGE_KEY, enabled ? "on" : "off");
  } catch {
    /* Storage is optional. */
  }
  subscribers.forEach((notify) => notify());
}

export function useMelaEnabled() {
  const [enabled, setEnabled] = useState(readMelaEnabled);
  useEffect(() => {
    const update = () => setEnabled(readMelaEnabled());
    const onStorage = (event: StorageEvent) => {
      if (event.key === STORAGE_KEY) update();
    };
    subscribers.add(update);
    window.addEventListener("storage", onStorage);
    return () => {
      subscribers.delete(update);
      window.removeEventListener("storage", onStorage);
    };
  }, []);
  return enabled;
}

/** Simple three-band flags, enough to recognise the target language at 20 px. */
export interface MelaFlag {
  bands: string[];
  vertical?: boolean;
  sun?: boolean;
  label: string;
}
export const MELA_FLAGS: Record<Exclude<OutputMode, "original">, MelaFlag> = {
  en: { bands: ["#012169", "#ffffff", "#c8102e"], label: "EN" },
  de: { bands: ["#1d1d1f", "#dd0000", "#ffce00"], label: "DE" },
  fr: { bands: ["#0055a4", "#ffffff", "#ef4135"], vertical: true, label: "FR" },
  es: { bands: ["#aa151b", "#f1bf00", "#aa151b"], label: "ES" },
  "es-AR": { bands: ["#74acdf", "#ffffff", "#74acdf"], sun: true, label: "AR" },
  it: { bands: ["#009246", "#ffffff", "#ce2b37"], vertical: true, label: "IT" },
  "pt-BR": { bands: ["#009c3b", "#ffdf00", "#009c3b"], label: "BR" },
  nl: { bands: ["#ae1c28", "#ffffff", "#21468b"], label: "NL" },
  pl: { bands: ["#ffffff", "#dc143c"], label: "PL" },
};

export function melaFlag(mode: OutputMode | null | undefined): MelaFlag | null {
  if (!mode || mode === "original") return null;
  return MELA_FLAGS[mode] ?? null;
}

/** What the pill is doing, mapped to Mela. Hidden means she ducks away. */
export function overlayMood(phase: string): MelaMood | "hidden" {
  switch (phase) {
    case "listening":
      return "listen";
    case "processing":
      return "think";
    case "inserted":
    case "clipboard_only":
      return "happy";
    case "failed":
      return "oops";
    case "mode":
      return "lang";
    case "hidden":
      return "hidden";
    default:
      return "idle";
  }
}

export interface WindowMoodInput {
  dictation: "listening" | "processing" | null;
  /** The Library, a transcript or a translation is on screen. */
  showingContent: boolean;
  translating: boolean;
  filesWorking: boolean;
  /** A short-lived reaction (a finished job, a new target language). */
  moment: "happy" | "lang" | "oops" | null;
  offline: boolean;
  egg: boolean;
  idle: boolean;
}

/** Priority order: never look at content, then live work, then moments, then rest. */
export function windowMood(input: WindowMoodInput): MelaMood {
  if (input.showingContent) return "private";
  if (input.dictation === "listening") return "listen";
  if (input.dictation === "processing" || input.translating) return "think";
  if (input.moment) return input.moment;
  if (input.filesWorking) return "work";
  if (input.egg) return "egg";
  if (input.offline) return "offline";
  if (input.idle) return "sleep";
  return "idle";
}

/** Groundhog Day, 2 February. `?mela-date=02-02` fakes the date in the preview. */
export function isGroundhogDay(now = new Date()) {
  try {
    const fake = new URLSearchParams(window.location.search).get("mela-date");
    if (fake) return fake === "02-02";
  } catch {
    /* Use the real date. */
  }
  return now.getMonth() === 1 && now.getDate() === 2;
}

/** Minutes without activity before Mela hibernates. `?mela-idle=<ms>` shortens it for demos. */
export function melaIdleMs() {
  try {
    const value = Number(new URLSearchParams(window.location.search).get("mela-idle"));
    if (Number.isFinite(value) && value > 0) return value;
  } catch {
    /* Use the default. */
  }
  return 3 * 60_000;
}

const MOMENT_MS = 2600;

/**
 * Mela's mood in the main window, from what the app is doing. Adds the short
 * moments (a job just finished, a new target language), idle hibernation,
 * offline and the Groundhog Day egg on top of windowMood's priorities.
 */
export function useMelaWindowMood(input: {
  enabled: boolean;
  dictation: WindowMoodInput["dictation"];
  showingContent: boolean;
  translating: boolean;
  filesWorking: boolean;
  outputMode: OutputMode | null | undefined;
  /** Dictation ended with an error message on screen. */
  failed: boolean;
}) {
  const { enabled, dictation, showingContent, translating, filesWorking, outputMode, failed } = input;
  const [moment, setMoment] = useState<WindowMoodInput["moment"]>(null);
  const [idle, setIdle] = useState(false);
  const [offline, setOffline] = useState(() => typeof navigator !== "undefined" && navigator.onLine === false);
  const [egg, setEgg] = useState(() => isGroundhogDay());
  const previous = useRef({ filesWorking, dictation, outputMode, failed });

  const flash = (next: NonNullable<WindowMoodInput["moment"]>) => {
    setMoment(next);
    window.setTimeout(() => setMoment((current) => (current === next ? null : current)), MOMENT_MS);
  };
  useEffect(() => {
    const before = previous.current;
    if (before.filesWorking && !filesWorking) flash("happy");
    if (before.dictation && !dictation) flash(failed ? "oops" : "happy");
    else if (failed && !before.failed) flash("oops");
    if (outputMode && outputMode !== before.outputMode && outputMode !== "original") flash("lang");
    previous.current = { filesWorking, dictation, outputMode, failed };
  }, [filesWorking, dictation, outputMode, failed]);

  // Hibernate after a quiet stretch; any activity in the window wakes her.
  useEffect(() => {
    if (!enabled) return;
    let timer = 0;
    const wake = () => {
      setIdle(false);
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setIdle(true), melaIdleMs());
    };
    wake();
    const events = ["pointerdown", "keydown", "wheel"] as const;
    events.forEach((name) => window.addEventListener(name, wake, { passive: true }));
    return () => {
      window.clearTimeout(timer);
      events.forEach((name) => window.removeEventListener(name, wake));
    };
  }, [enabled, dictation, filesWorking, translating, showingContent]);

  useEffect(() => {
    const update = () => setOffline(navigator.onLine === false);
    window.addEventListener("online", update);
    window.addEventListener("offline", update);
    return () => {
      window.removeEventListener("online", update);
      window.removeEventListener("offline", update);
    };
  }, []);

  useEffect(() => {
    if (!egg) return;
    const timer = window.setTimeout(() => setEgg(false), 6000);
    return () => window.clearTimeout(timer);
  }, [egg]);

  return {
    mood: windowMood({ dictation, showingContent, translating, filesWorking, moment, offline, egg, idle }),
    flag: melaFlag(outputMode),
  };
}
