import { useCallback, useEffect, useSyncExternalStore } from "react";
import { activateLicense, deactivateLicense, getLicenseStatus } from "../lib/api";
import { describeError } from "../lib/formatting";
import type { LicenseStatus } from "../types/domain";

/** An activation started by a `murmela://activate?key=…` link rather than the form. */
export type LinkActivation =
  | { state: "activating"; key: string }
  | { state: "failed"; key: string; message: string }
  | null;

// One shared copy, so every screen unlocks the moment a key is activated.
let current: LicenseStatus | null = null;
let link: LinkActivation = null;
let snapshot: { status: LicenseStatus | null; link: LinkActivation } = { status: current, link };
let loading: Promise<void> | null = null;
const listeners = new Set<() => void>();

function notify() {
  snapshot = { status: current, link };
  listeners.forEach((listener) => listener());
}

function publish(next: LicenseStatus) {
  current = next;
  notify();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function load() {
  loading ??= getLicenseStatus()
    .then(publish)
    .catch(() => {
      loading = null;
    });
  return loading;
}

/** Test helper: forget the cached status between tests. */
export function resetLicenseCache() {
  current = null;
  link = null;
  loading = null;
  notify();
}

/** Activates the key from an activation link; the License card shows progress and errors. */
export async function activateFromLink(key: string) {
  if (link?.state === "activating") return;
  link = { state: "activating", key };
  notify();
  try {
    current = await activateLicense(key);
    link = null;
  } catch (reason) {
    link = { state: "failed", key, message: describeError(reason, "Murmela could not activate this license.") };
  }
  notify();
}

/** The Pro license on this Mac. `pro` is false until the status has loaded. */
export function useLicense() {
  const { status, link: linkActivation } = useSyncExternalStore(subscribe, () => snapshot);

  useEffect(() => {
    void load();
  }, []);

  const activate = useCallback(async (licenseKey: string) => {
    publish(await activateLicense(licenseKey));
    link = null;
    notify();
  }, []);
  const deactivate = useCallback(async () => publish(await deactivateLicense()), []);

  return { status, pro: status?.pro ?? false, linkActivation, activate, deactivate };
}

/** True when an error from the backend means "this is a Pro feature". */
export function isProRequiredError(error: unknown) {
  const text =
    typeof error === "string"
      ? error
      : error && typeof error === "object"
        ? String((error as { code?: unknown; message?: unknown }).code ?? (error as { message?: unknown }).message ?? "")
        : "";
  return text.startsWith("PRO_REQUIRED");
}
