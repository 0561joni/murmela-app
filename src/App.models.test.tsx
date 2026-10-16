import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DownloadableModel, InstalledModel, ModelDownloadStatus } from "./types/domain";
import { settingsFixture } from "./test/fixtures";

const mocks = vi.hoisted(() => ({
  getSettings: vi.fn(),
  listInstalledModels: vi.fn(),
  listDownloadableModels: vi.fn(),
  updateSettings: vi.fn(),
  listeners: new Set<(status: ModelDownloadStatus) => void>(),
}));
vi.mock("./lib/api", async (original) => ({
  ...(await original<typeof import("./lib/api")>()),
  getSettings: mocks.getSettings,
  listInstalledModels: mocks.listInstalledModels,
  listDownloadableModels: mocks.listDownloadableModels,
  updateSettings: mocks.updateSettings,
  getModelDownloadStatuses: async () => [],
  listenModelDownloadStatus: async (listener: (status: ModelDownloadStatus) => void) => {
    mocks.listeners.add(listener);
    return () => { mocks.listeners.delete(listener); };
  },
  getStartupStatus: async () => ({ phase: "workspace", step: 6, totalSteps: 6 }),
  listenStartupStatus: async () => () => undefined,
}));

import { App } from "./App";

const streamer: DownloadableModel = {
  id: "test-streaming-engine",
  engine: "test-streaming",
  modelName: "Streamer",
  description: "Live shortcut dictation",
  sizeBytes: 2_477_512_064,
  profile: "accurate",
  availability: "available",
  availabilityReason: null,
  installed: false,
  requirements: "Apple Silicon",
  artifactCount: 1,
  capability: "asr",
  capabilities: {
    supportedContexts: ["shortcut_dictation"],
    streamingTranscription: true,
    nativeDiarization: false,
    timestampedSegments: false,
    contextSupport: true,
    languageControl: "automatic_and_fixed",
    maximumAudioDurationMs: 300_000,
  },
};
const installedStreamer: InstalledModel = {
  ...streamer,
  variant: "streaming",
  localPath: "/models/test-streaming-engine",
  isDefault: false,
};
const completion: ModelDownloadStatus = {
  modelId: streamer.id,
  modelName: streamer.modelName,
  state: "completed",
  downloadedBytes: streamer.sizeBytes,
  totalBytes: streamer.sizeBytes,
  progressPercent: 100,
  errorMessage: null,
  currentArtifact: null,
  artifactIndex: 1,
  artifactCount: 1,
};

describe("Model installation in the workspace", () => {
  beforeEach(() => {
    mocks.listeners.clear();
    mocks.getSettings.mockReset().mockResolvedValue(settingsFixture);
    mocks.listInstalledModels.mockReset().mockResolvedValue([]);
    mocks.listDownloadableModels.mockReset().mockResolvedValue([streamer]);
    mocks.updateSettings.mockReset().mockImplementation(async (patch) => ({ ...settingsFixture, ...patch }));
  });

  it("makes a downloaded streaming engine selectable immediately without navigation or restart", async () => {
    render(<App />);
    await waitFor(() => expect(mocks.listInstalledModels).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    fireEvent.click(screen.getByRole("button", { name: /Download engines/ }));
    await screen.findByRole("button", { name: /Download Streamer/ });
    await waitFor(() => expect(mocks.listeners.size).toBe(2));

    mocks.listInstalledModels.mockClear().mockResolvedValue([installedStreamer]);
    mocks.listDownloadableModels.mockResolvedValue([{ ...streamer, installed: true }]);
    await act(async () => {
      for (const listener of mocks.listeners) listener(completion);
    });
    expect(mocks.listInstalledModels).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: /Download Streamer/ })).toBeNull();
    expect(screen.getByText("Installed")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Dictation engine" }));
    fireEvent.click(within(screen.getByRole("listbox")).getByRole("option", { name: /Streamer/ }));
    await waitFor(() => expect(mocks.updateSettings).toHaveBeenCalledWith({
      shortcutDictationSelectedModelId: streamer.id,
      shortcutDictationModelProfile: "accurate",
    }));
    expect(screen.getByRole("button", { name: "Record button engine" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "File engine" })).toHaveProperty("disabled", true);
  });

  it("updates the installed picker even if the settings refresh fails", async () => {
    render(<App />);
    await waitFor(() => expect(mocks.listInstalledModels).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    await waitFor(() => expect(mocks.listeners.size).toBe(2));
    mocks.getSettings.mockRejectedValue(new Error("Settings temporarily unavailable"));
    mocks.listInstalledModels.mockResolvedValue([installedStreamer]);
    await act(async () => {
      for (const listener of mocks.listeners) listener(completion);
    });
    expect(screen.getByRole("alert").textContent).toContain("Settings temporarily unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Dictation engine" }));
    expect(within(screen.getByRole("listbox")).getByRole("option", { name: /Streamer/ })).toBeTruthy();
  });

  it("reads again when completion arrives during an older model refresh", async () => {
    render(<App />);
    await waitFor(() => expect(mocks.listInstalledModels).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    await waitFor(() => expect(mocks.listeners.size).toBe(2));

    let finishOlderRead!: (models: InstalledModel[]) => void;
    mocks.listInstalledModels.mockClear()
      .mockImplementationOnce(() => new Promise<InstalledModel[]>((resolve) => { finishOlderRead = resolve; }))
      .mockResolvedValue([installedStreamer]);
    await act(async () => {
      for (const listener of mocks.listeners) listener({ ...completion, modelId: "another-model" });
    });
    expect(mocks.listInstalledModels).toHaveBeenCalledTimes(1);

    await act(async () => {
      for (const listener of mocks.listeners) listener(completion);
      finishOlderRead([]);
    });
    await waitFor(() => expect(mocks.listInstalledModels).toHaveBeenCalledTimes(2));
    fireEvent.click(screen.getByRole("button", { name: "Dictation engine" }));
    expect(within(screen.getByRole("listbox")).getByRole("option", { name: /Streamer/ })).toBeTruthy();
  });
});
