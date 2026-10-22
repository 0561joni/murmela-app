import { describe, expect, it } from "vitest";
import {
  formatBytes,
  formatCalendarDay,
  formatClock,
  formatDateTime,
  formatDuration,
  formatShortDate,
  describeError,
  readableError,
} from "./formatting";

describe("shared output formatting", () => {
  it("never shows fractional seconds in durations", () => {
    expect(formatDuration(60.009)).toBe("1m");
    expect(formatDuration(492.629)).toBe("8m 13s");
    expect(formatDuration(20)).toBe("20s");
    expect(formatDuration(3_723)).toBe("1h 2m 3s");
  });

  it("uses one clock style below and above an hour", () => {
    expect(formatClock(65_999)).toBe("01:05");
    expect(formatClock(4_503_000)).toBe("1:15:03");
  });

  it("uses decimal units like Finder", () => {
    expect(formatBytes(4_221_849)).toBe("4.2 MB");
    expect(formatBytes(487_601_967)).toBe("488 MB");
    expect(formatBytes(4_703_041_355)).toBe("4.7 GB");
    expect(formatBytes(512)).toBe("512 B");
  });

  it("formats dates in the system's language", () => {
    const iso = new Date(2026, 8, 25, 23, 4).toISOString();
    expect(formatShortDate(iso, "de-DE")).toMatch(/^25\. Sept?\.$/);
    expect(formatShortDate(iso, "en-US")).toBe("Sep 25");
    expect(formatDateTime(iso, "de-DE")).toBe("25.09.2026, 23:04");
    expect(formatDateTime(iso, "en-US")).toBe("Sep 25, 2026, 11:04 PM");
  });

  it("formats calendar days without shifting them across time zones", () => {
    expect(formatCalendarDay("2027-10-16", "en-US")).toBe("October 16, 2027");
    expect(formatCalendarDay("2027-10-16", "de-DE")).toBe("16. Oktober 2027");
    expect(formatCalendarDay("not a day", "en-US")).toBe("not a day");
  });
});

describe("readableError", () => {
  it("strips lowercase and uppercase machine codes but keeps plain sentences", () => {
    expect(readableError("io_error: Disk unavailable")).toBe("Disk unavailable");
    expect(readableError("DISK_SPACE_LOW: Free up space and try again.")).toBe(
      "Free up space and try again.",
    );
    expect(readableError("Note: this stays")).toBe("Note: this stays");
  });
});

describe("describeError", () => {
  it("explains known codes and developer messages in plain language", () => {
    expect(readableError("MODEL_MISSING: no whisper.cpp model is installed for profile balanced")).toBe(
      "No speech engine is set up for this yet. Choose one in Settings → Engines.",
    );
    expect(readableError("LIVE_PAIR_PROTOCOL: Unordered helper response.")).toMatch(/^Live dictation stopped unexpectedly/);
    expect(readableError("MODEL_WORKER_FAILED: [METAL] Command buffer execution failed: Insufficient Memory (00000008:kIOGPUCommandBufferCallbackErrorOutOfMemory).")).toMatch(/^Your Mac ran out of memory/);
    expect(readableError("MODEL_OUT_OF_MEMORY: VibeVoice-ASR ran out of memory (signal: 9 (SIGKILL))")).toMatch(/^Your Mac ran out of memory/);
    expect(readableError("recording worker timed out")).toMatch(/^The microphone didn't respond/);
    expect(readableError("LIVE_PAIR_SETUP: Live dictation requires Apple Silicon and macOS 14 or newer.")).toBe(
      "Live dictation requires Apple Silicon and macOS 14 or newer.",
    );
  });

  it("reads strings, errors and objects, with a fallback", () => {
    expect(describeError("TRANSCRIPTION_EMPTY: whisper produced no segments")).toMatch(/^No speech was heard/);
    expect(describeError(new Error("Disk is read-only"))).toBe("Disk is read-only");
    expect(describeError({ message: "JOB_CANCELED: x" })).toBe("Canceled.");
    expect(describeError(undefined, "Could not save.")).toBe("Could not save.");
    expect(describeError("  ")).toBe("Something went wrong. Please try again.");
  });
});

describe("one-key shortcuts", () => {
  it("shows the 🌐 and Right Option keys by name", async () => {
    const { formatShortcutForDisplay, isOneKeyShortcut } = await import("./formatting");
    expect(formatShortcutForDisplay("Fn")).toBe("🌐 fn");
    expect(formatShortcutForDisplay("RightAlt")).toBe("Right ⌥");
    expect(formatShortcutForDisplay("CmdOrCtrl+Shift+Space")).toBe("⌘+⇧+Space");
    expect(formatShortcutForDisplay("Ctrl+Alt+L")).toBe("⌃+⌥+L");
    expect(isOneKeyShortcut("Fn") && isOneKeyShortcut("RightAlt")).toBe(true);
    expect(isOneKeyShortcut("CmdOrCtrl+Shift+Space")).toBe(false);
  });
});
