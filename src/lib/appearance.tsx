import { useEffect, useState, type ReactNode } from "react";
import { getSettings } from "./api";
import type { AppSettings } from "../types/domain";

type Preferences = Pick<AppSettings, "appearance" | "motionPreference"> & {
  seriousMode?: boolean;
};
const CACHE_KEY = "murmela-appearance";
/** Key used before the Murmela rename; read once so the first launch keeps its theme. */
const LEGACY_CACHE_KEY = "blabber-appearance";
let preferences: Preferences = {
  appearance: "system",
  motionPreference: "system",
  seriousMode: false,
};
const subscribers = new Set<() => void>();

export function applyAppearance(next: Preferences) {
  preferences = {
    appearance: next.appearance ?? "system",
    motionPreference: next.motionPreference ?? "system",
    seriousMode: next.seriousMode ?? false,
  };
  const dark =
    preferences.appearance === "dark" ||
    (preferences.appearance === "system" &&
      window.matchMedia?.("(prefers-color-scheme: dark)").matches);
  const reduced =
    preferences.motionPreference === "reduced" ||
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  document.documentElement.dataset.motion = reduced ? "reduced" : "full";
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(preferences));
  } catch {
    /* Storage is optional. */
  }
  subscribers.forEach((notify) => notify());
}

// Resolve appearance before React paints, including during native startup.
try {
  preferences = {
    ...preferences,
    ...JSON.parse(
      localStorage.getItem(CACHE_KEY) ??
        localStorage.getItem(LEGACY_CACHE_KEY) ??
        "{}",
    ),
  };
} catch {
  /* Use system defaults. */
}
applyAppearance(preferences);

export function useReducedMotion() {
  const [reduced, setReduced] = useState(
    document.documentElement.dataset.motion === "reduced",
  );
  useEffect(() => {
    const update = () =>
      setReduced(document.documentElement.dataset.motion === "reduced");
    subscribers.add(update);
    update();
    return () => {
      subscribers.delete(update);
    };
  }, []);
  return reduced;
}

/** False in Serious mode: no Murmelmeter, playful lines or easter eggs.
 * Shared by every window through the same cache as the theme. */
export function usePlayful() {
  const [playful, setPlayful] = useState(!preferences.seriousMode);
  useEffect(() => {
    const update = () => setPlayful(!preferences.seriousMode);
    subscribers.add(update);
    update();
    return () => {
      subscribers.delete(update);
    };
  }, []);
  return playful;
}

export function AppearanceProvider({ children }: { children: ReactNode }) {
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const updateSystem = () => applyAppearance(preferences);
    const queries = [
      "(prefers-color-scheme: dark)",
      "(prefers-reduced-motion: reduce)",
    ].map((query) => window.matchMedia?.(query));
    queries.forEach((query) => query?.addEventListener("change", updateSystem));
    void getSettings()
      .then((settings) => {
        if (!disposed) applyAppearance(settings);
      })
      .catch(() => undefined);
    if ("__TAURI_INTERNALS__" in window) {
      void import("@tauri-apps/api/event")
        .then(async ({ listen }) => {
          const cleanup = await listen<AppSettings>(
            "settings-changed",
            ({ payload }) => applyAppearance(payload),
          );
          if (disposed) cleanup();
          else unlisten = cleanup;
        })
        .catch(() => undefined);
    }
    const onStorage = (event: StorageEvent) => {
      if (event.key === CACHE_KEY && event.newValue) {
        try {
          applyAppearance(JSON.parse(event.newValue));
        } catch {
          /* Ignore invalid cached preferences. */
        }
      }
    };
    window.addEventListener("storage", onStorage);
    return () => {
      disposed = true;
      unlisten?.();
      queries.forEach((query) =>
        query?.removeEventListener("change", updateSystem),
      );
      window.removeEventListener("storage", onStorage);
    };
  }, []);
  return children;
}
