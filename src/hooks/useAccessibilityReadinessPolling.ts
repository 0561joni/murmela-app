import { useCallback, useEffect, useRef, useState } from "react";
import type { DictationReadiness } from "../types/domain";

interface AccessibilityReadinessPollingOptions {
  intervalMs?: number;
  timeoutMs?: number;
}

const DEFAULT_INTERVAL_MS = 1000;
const DEFAULT_TIMEOUT_MS = 60_000;

/**
 * Briefly poll the live macOS Accessibility state after System Settings opens.
 * A recursive timeout keeps requests from overlapping when a check is slow.
 */
export function useAccessibilityReadinessPolling(
  refreshReadiness: () => Promise<DictationReadiness | null>,
  options: AccessibilityReadinessPollingOptions = {},
) {
  const intervalMs = options.intervalMs ?? DEFAULT_INTERVAL_MS;
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const [isPolling, setIsPolling] = useState(false);
  const timerRef = useRef<number | null>(null);
  const runIdRef = useRef(0);

  const clearTimer = useCallback(() => {
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const stopPolling = useCallback(() => {
    runIdRef.current += 1;
    clearTimer();
    setIsPolling(false);
  }, [clearTimer]);

  const startPolling = useCallback(() => {
    clearTimer();
    const runId = ++runIdRef.current;
    const expiresAt = Date.now() + timeoutMs;
    setIsPolling(true);

    const check = async () => {
      const nextReadiness = await refreshReadiness();
      if (runIdRef.current !== runId) {
        return;
      }

      if (
        (nextReadiness &&
          (!nextReadiness.accessibilityRequired || nextReadiness.accessibilityGranted)) ||
        Date.now() >= expiresAt
      ) {
        timerRef.current = null;
        setIsPolling(false);
        return;
      }

      timerRef.current = window.setTimeout(
        check,
        Math.min(intervalMs, Math.max(0, expiresAt - Date.now())),
      );
    };

    timerRef.current = window.setTimeout(check, Math.min(intervalMs, timeoutMs));
  }, [clearTimer, intervalMs, refreshReadiness, timeoutMs]);

  useEffect(
    () => () => {
      runIdRef.current += 1;
      clearTimer();
    },
    [clearTimer],
  );

  return { isPolling, startPolling, stopPolling };
}
