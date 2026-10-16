import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DictationReadiness } from "../types/domain";
import { useAccessibilityReadinessPolling } from "./useAccessibilityReadinessPolling";

const accessMissing: DictationReadiness = {
  hasModel: true,
  shortcutModelReady: true,
  microphone: "granted",
  firstRunCompleted: true,
  shortcutRegistered: true,
  autoPasteEnabled: true,
  accessibilityRequired: true,
  accessibilityGranted: false,
};

const accessGranted: DictationReadiness = {
  ...accessMissing,
  accessibilityGranted: true,
};

describe("useAccessibilityReadinessPolling", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("keeps checking until a delayed permission grant is detected", async () => {
    const refreshReadiness = vi
      .fn<() => Promise<DictationReadiness | null>>()
      .mockResolvedValueOnce(accessMissing)
      .mockResolvedValueOnce(accessMissing)
      .mockResolvedValueOnce(accessGranted);
    const { result } = renderHook(() =>
      useAccessibilityReadinessPolling(refreshReadiness),
    );

    act(() => result.current.startPolling());
    expect(result.current.isPolling).toBe(true);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });

    expect(refreshReadiness).toHaveBeenCalledTimes(3);
    expect(result.current.isPolling).toBe(false);
  });

  it("stops after the configured timeout when access is still missing", async () => {
    const refreshReadiness = vi.fn(async () => accessMissing);
    const { result } = renderHook(() =>
      useAccessibilityReadinessPolling(refreshReadiness, {
        intervalMs: 1000,
        timeoutMs: 2500,
      }),
    );

    act(() => result.current.startPolling());
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });

    expect(refreshReadiness).toHaveBeenCalledTimes(3);
    expect(result.current.isPolling).toBe(false);
  });

  it("cancels pending checks when the component unmounts", async () => {
    const refreshReadiness = vi.fn(async () => accessMissing);
    const { result, unmount } = renderHook(() =>
      useAccessibilityReadinessPolling(refreshReadiness),
    );

    act(() => result.current.startPolling());
    unmount();
    await vi.advanceTimersByTimeAsync(5000);

    expect(refreshReadiness).not.toHaveBeenCalled();
  });

  it("replaces an existing polling loop instead of creating a duplicate", async () => {
    const refreshReadiness = vi.fn(async () => accessMissing);
    const { result } = renderHook(() =>
      useAccessibilityReadinessPolling(refreshReadiness),
    );

    act(() => {
      result.current.startPolling();
      result.current.startPolling();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });

    expect(refreshReadiness).toHaveBeenCalledTimes(1);
  });
});
