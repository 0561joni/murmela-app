import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CallStatus, FileQueueItem } from "../types/domain";

const apiMocks = vi.hoisted(() => ({
  getCallCaptureSupport: vi.fn(),
  getCallStatus: vi.fn(),
  startCallRecording: vi.fn(),
  stopCallRecording: vi.fn(),
  discardCallRecording: vi.fn(),
  listPendingCallRecordings: vi.fn(),
  transcribePendingCallRecording: vi.fn(),
  deletePendingCallRecording: vi.fn(),
  revealBrowserHelper: vi.fn(),
}));

vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  ...apiMocks,
}));

import { MeetingsScreen } from "./MeetingsScreen";

const idle: CallStatus = {
  recording: false,
  callId: null,
  mode: null,
  title: null,
  elapsedMs: 0,
  limitMs: 21_600_000,
  microphone: null,
  system: null,
  browser: null,
  bridgePort: null,
};

const recording: CallStatus = {
  ...idle,
  recording: true,
  callId: "c1",
  mode: "call",
  title: "Client call",
  elapsedMs: 65_000,
  microphone: { label: "MacBook Pro Microphone", level: 0.4, error: null },
  system: { label: "Sound from other apps", level: 0.6, error: null },
  browser: {
    platform: "meet",
    meetingTitle: "Weekly",
    participants: ["Ada Lovelace", "Ben Weber"],
    speaking: ["Ada Lovelace"],
    msSinceLastReport: 300,
  },
  bridgePort: 47615,
};

function item(overrides: Partial<FileQueueItem>): FileQueueItem {
  return {
    id: "job-1",
    sourceFile: {
      filePath: "/Users/ada/Library/Application Support/com.murmela.app/meetings/abc/call.wav",
      originalName: "Client call.wav",
      mimeType: "audio/wav",
      sizeBytes: 1000,
      durationMs: 65_000,
      sha256: null,
    },
    stage: "completed",
    progressPercent: 100,
    processedMs: null,
    totalMs: null,
    etaSeconds: null,
    statusText: "Transcription completed.",
    result: null,
    errorMessage: null,
    startedAt: 1,
    reviewRef: { kind: "saved", id: "t1" },
    ...overrides,
  };
}

function renderScreen(items: FileQueueItem[] = [], pro = false) {
  const onReview = vi.fn();
  const onRetry = vi.fn().mockResolvedValue(undefined);
  render(<MeetingsScreen items={items} pro={pro} onReview={onReview} onRetry={onRetry} />);
  return { onReview, onRetry };
}

beforeEach(() => {
  for (const mock of Object.values(apiMocks)) mock.mockReset();
  apiMocks.getCallCaptureSupport.mockResolvedValue({ supported: true, reason: null });
  apiMocks.getCallStatus.mockResolvedValue(idle);
  apiMocks.listPendingCallRecordings.mockResolvedValue([]);
  apiMocks.startCallRecording.mockResolvedValue(recording);
  apiMocks.stopCallRecording.mockResolvedValue({ jobId: "job-2", sourceFile: item({}).sourceFile });
  apiMocks.revealBrowserHelper.mockResolvedValue(undefined);
});

describe("MeetingsScreen", () => {
  it("starts a call recording with its name and reminds about consent", async () => {
    renderScreen();
    expect(screen.getByText(/Let everyone know you are recording/)).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "  Client call  " } });
    fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
    await waitFor(() => expect(apiMocks.startCallRecording).toHaveBeenCalledWith("call", "Client call"));
    expect(await screen.findByRole("button", { name: "Stop and transcribe" })).toBeTruthy();
  });

  it("records in the room when the call's sound can't be captured", async () => {
    apiMocks.getCallCaptureSupport.mockResolvedValue({
      supported: false,
      reason: "Recording a call's sound needs macOS 14.4 or later.",
    });
    renderScreen();
    expect(await screen.findByText(/needs macOS 14.4/)).toBeTruthy();
    const onCall = screen.getByRole("button", { name: "On a call" }) as HTMLButtonElement;
    expect(onCall.disabled).toBe(true);
    expect(screen.getByRole("button", { name: "In the room" }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
    await waitFor(() => expect(apiMocks.startCallRecording).toHaveBeenCalledWith("room", null));
  });

  it("shows both levels and the names from the browser while recording, then stops and transcribes", async () => {
    apiMocks.getCallStatus.mockResolvedValue(recording);
    renderScreen();
    expect(await screen.findByText("Client call")).toBeTruthy();
    expect(screen.getByText(/01:05 recorded/)).toBeTruthy();
    expect(screen.getByRole("meter", { name: "You level" }).getAttribute("aria-valuenow")).toBe("40");
    expect(screen.getByRole("meter", { name: "Other side level" }).getAttribute("aria-valuenow")).toBe("60");
    expect(screen.getByText(/Names from Google Meet: Ada Lovelace, Ben Weber · speaking: Ada Lovelace/)).toBeTruthy();

    apiMocks.getCallStatus.mockResolvedValue(idle);
    fireEvent.click(screen.getByRole("button", { name: "Stop and transcribe" }));
    await waitFor(() => expect(apiMocks.stopCallRecording).toHaveBeenCalledWith(null));
    expect(await screen.findByRole("button", { name: "Start recording" })).toBeTruthy();
  });

  it("says when the browser helper is not connected", async () => {
    apiMocks.getCallStatus.mockResolvedValue({ ...recording, browser: null });
    renderScreen();
    expect(await screen.findByText(/Browser helper not connected/)).toBeTruthy();
  });

  it("asks before discarding a recording", async () => {
    apiMocks.getCallStatus.mockResolvedValue(recording);
    apiMocks.discardCallRecording.mockResolvedValue(undefined);
    renderScreen();
    fireEvent.click(await screen.findByRole("button", { name: "Discard" }));
    expect(apiMocks.discardCallRecording).not.toHaveBeenCalled();
    apiMocks.getCallStatus.mockResolvedValue(idle);
    fireEvent.click(screen.getByRole("button", { name: "Delete recording" }));
    await waitFor(() => expect(apiMocks.discardCallRecording).toHaveBeenCalled());
  });

  it("offers recordings that were not transcribed and lists finished ones for Review", async () => {
    apiMocks.listPendingCallRecordings.mockResolvedValue([
      { id: "p1", title: "Interrupted recording", startedAt: "2026-10-15T09:00:00Z", durationMs: 600_000, mode: "call", interrupted: true },
    ]);
    apiMocks.transcribePendingCallRecording.mockResolvedValue({ jobId: "j", sourceFile: item({}).sourceFile });
    const { onReview } = renderScreen([
      item({}),
      item({ id: "other", sourceFile: { ...item({}).sourceFile, filePath: "/Users/ada/Desktop/interview.wav" } }),
    ]);
    expect(await screen.findByText("Interrupted recording")).toBeTruthy();
    expect(screen.getByText(/recovered after Murmela stopped/)).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Transcribe" }));
    });
    expect(apiMocks.transcribePendingCallRecording).toHaveBeenCalledWith("p1");

    // Only the call recording is listed, not the dropped file.
    const open = screen.getAllByRole("button", { name: "Open in Review" });
    expect(open).toHaveLength(1);
    fireEvent.click(open[0]);
    expect(onReview).toHaveBeenCalledWith(expect.objectContaining({ id: "job-1" }));
  });

  it("explains speaker separation by license", () => {
    renderScreen([], false);
    expect(screen.getByText(/With Murmela Pro, Murmela also tells the people on it apart/)).toBeTruthy();
  });
});
