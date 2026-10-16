import { applyAppearance } from "./lib/appearance";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { StartupStatus } from "./types/domain";

const apiMocks = vi.hoisted(() => ({
  getStartupStatus: vi.fn<() => Promise<StartupStatus>>(),
  completeStartupHandoff: vi.fn<() => Promise<void>>(),
  quitApp: vi.fn<() => Promise<void>>(),
  restartApp: vi.fn<() => Promise<void>>(),
  listener: null as ((status: StartupStatus) => void) | null,
}));

vi.mock("./lib/api", () => ({
  getStartupStatus: apiMocks.getStartupStatus,
  completeStartupHandoff: apiMocks.completeStartupHandoff,
  quitApp: apiMocks.quitApp,
  restartApp: apiMocks.restartApp,
  listenStartupStatus: vi.fn(
    async (listener: (status: StartupStatus) => void) => {
      apiMocks.listener = listener;
      return () => {
        apiMocks.listener = null;
      };
    },
  ),
}));

import { SplashApp } from "./SplashApp";

const filesStatus: StartupStatus = { phase: "files", step: 1, totalSteps: 6 };

describe("SplashApp", () => {
  beforeEach(() => {
    apiMocks.listener = null;
    apiMocks.getStartupStatus.mockReset().mockResolvedValue(filesStatus);
    apiMocks.completeStartupHandoff.mockReset().mockResolvedValue();
    apiMocks.quitApp.mockReset().mockResolvedValue();
    apiMocks.restartApp.mockReset().mockResolvedValue();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("catches up from the snapshot and renders real stage progress", async () => {
    apiMocks.getStartupStatus.mockResolvedValue({
      phase: "library",
      step: 4,
      totalSteps: 6,
    });
    render(<SplashApp />);

    expect(await screen.findByText("Opening your library")).toBeTruthy();
    // Playful by default; the headline still says what is happening.
    expect(screen.getByText("Dusting off your words")).toBeTruthy();
    act(() => applyAppearance({ appearance: "system", motionPreference: "system", seriousMode: true }));
    expect(
      screen.getByText("Loading transcripts, settings, and vocabulary"),
    ).toBeTruthy();
    act(() => applyAppearance({ appearance: "system", motionPreference: "system", seriousMode: false }));
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe(
      "4",
    );
    expect(screen.getByText("4 of 6")).toBeTruthy();
  });

  it("waits for the minimum display time and completion reaction before handoff", async () => {
    vi.useFakeTimers();
    render(<SplashApp />);
    await act(async () => Promise.resolve());

    act(() => {
      apiMocks.listener?.({ phase: "ready", step: 6, totalSteps: 6 });
    });
    await act(async () => {
      vi.advanceTimersByTime(899);
    });
    expect(apiMocks.completeStartupHandoff).not.toHaveBeenCalled();

    await act(async () => {
      vi.advanceTimersByTime(181);
    });
    expect(apiMocks.completeStartupHandoff).toHaveBeenCalledTimes(1);
  });

  it("offers recovery for slow and failed startup", async () => {
    vi.useFakeTimers();
    render(<SplashApp />);
    await act(async () => Promise.resolve());

    await act(async () => {
      vi.advanceTimersByTime(15_000);
    });
    expect(screen.getByText(/Still preparing your local engine/)).toBeTruthy();

    await act(async () => {
      vi.advanceTimersByTime(15_000);
    });
    fireEvent.click(screen.getByRole("button", { name: "Restart Murmela" }));
    expect(apiMocks.restartApp).toHaveBeenCalledTimes(1);

    act(() => {
      apiMocks.listener?.({
        phase: "failed",
        step: 3,
        totalSteps: 6,
        errorMessage: "Audio initialization failed.",
      });
    });
    expect(screen.getByText("Murmela couldn’t start")).toBeTruthy();
    fireEvent.click(screen.getByText("Technical details"));
    expect(screen.getByText("Audio initialization failed.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Quit" }));
    expect(apiMocks.quitApp).toHaveBeenCalledTimes(1);
  });
});
