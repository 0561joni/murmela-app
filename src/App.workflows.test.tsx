import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { resultFixture, settingsFixture } from "./test/fixtures";
import type { RecordingStatusResponse } from "./types/domain";

const mocks = vi.hoisted(() => ({
  start: vi.fn(),
  stop: vi.fn(),
  preview: vi.fn(),
  reset: vi.fn(),
  getRecording: vi.fn(),
  getSettings: vi.fn(),
  report: vi.fn(),
}));
vi.mock("./lib/api", async (original) => ({
  ...(await original<typeof import("./lib/api")>()),
  getSettings: mocks.getSettings,
  getRecordingStatus: mocks.getRecording,
  startRecordingSession: mocks.start,
  stopRecordingSession: mocks.stop,
  previewTranscription: mocks.preview,
  resetQuickDictation: mocks.reset,
  reportManualFeedback: mocks.report,
  getStartupStatus: () =>
    Promise.resolve({ phase: "workspace", step: 6, totalSteps: 6 }),
  listenStartupStatus: async () => () => undefined,
}));

const idle: RecordingStatusResponse = {
  state: "idle",
  currentSessionId: null,
  activeInputDevice: null,
  lastRecordingPath: null,
  lastErrorMessage: null,
  durationMs: null,
  sampleRateHz: null,
  channels: null,
};
describe("Workspace lifecycle", () => {
  beforeEach(() => {
    mocks.getSettings.mockReset().mockResolvedValue(settingsFixture);
    mocks.getRecording.mockReset().mockResolvedValue(idle);
    mocks.start.mockReset().mockImplementation(async () => {
      const recording = {
        ...idle,
        state: "listening",
        currentSessionId: "manual-1",
      };
      mocks.getRecording.mockResolvedValue(recording);
      return recording;
    });
    mocks.stop.mockReset().mockImplementation(async () => {
      mocks.getRecording.mockResolvedValue({ ...idle, state: "success" });
      return {
        sessionId: "manual-1",
        filePath: "/temp/recording.wav",
        durationMs: 1000,
        sampleRateHz: 16000,
        channels: 1,
      };
    });
    mocks.preview.mockReset();
    mocks.reset.mockReset().mockImplementation(async () => {
      mocks.getRecording.mockResolvedValue(idle);
      return {
        state: "idle",
        registeredShortcut: null,
        lastErrorMessage: null,
      };
    });
    mocks.report.mockReset().mockResolvedValue(undefined);
  });
  it("honors the default file workspace", async () => {
    mocks.getSettings.mockResolvedValue({
      ...settingsFixture,
      defaultMode: "file_transcribe",
    });
    render(<App />);
    expect(
      await screen.findByRole("button", { name: "Choose files" }),
    ).toBeTruthy();
    expect(
      screen
        .getByRole("button", { name: "Transcribe files" })
        .getAttribute("aria-current"),
    ).toBe("page");
  });
  it("preserves active recording across navigation", async () => {
    render(<App />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Start recording" }),
      ).toHaveProperty("disabled", false),
    );
    fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
    expect(
      await screen.findByRole("button", { name: "Stop and transcribe" }),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Transcribe files" }));
    expect(screen.getByLabelText("Dictation in progress")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Dictate" }));
    expect(
      screen.getByRole("button", { name: "Stop and transcribe" }),
    ).toBeTruthy();
    expect(mocks.start).toHaveBeenCalledTimes(1);
  });
  it("allows reset during processing and ignores the old result and its sound", async () => {
    let finish!: (value: unknown) => void;
    mocks.preview.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    render(<App />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Start recording" }),
      ).toHaveProperty("disabled", false),
    );
    fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
    fireEvent.click(
      await screen.findByRole("button", { name: "Stop and transcribe" }),
    );
    await waitFor(() => expect(mocks.preview).toHaveBeenCalledTimes(1));
    fireEvent.click(
      screen.getByRole("button", { name: "Reset stuck dictation" }),
    );
    await waitFor(() => expect(mocks.reset).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Start recording" }),
      ).toHaveProperty("disabled", false),
    );
    await act(async () =>
      finish({
        sourceKind: "quick_dictate",
        resolvedModel: null,
        error: null,
        result: resultFixture,
      }),
    );
    expect(screen.queryByText("A useful thought.")).toBeNull();
    expect(mocks.report).not.toHaveBeenCalled();
  });
});
