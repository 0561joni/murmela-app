import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  DictationReadiness,
  DownloadableModel,
  ModelDownloadStatus,
  QuickDictationStatusResponse,
} from "../types/domain";
import { FirstRunSetup, type FirstRunSetupProps } from "./FirstRunSetup";

const api = vi.hoisted(() => ({
  catalog: [] as DownloadableModel[],
  freeSpace: 50_000_000_000 as number | null,
  listener: null as ((status: ModelDownloadStatus) => void) | null,
  startModelDownload: vi.fn(),
}));

vi.mock("../lib/api", () => ({
  listDownloadableModels: vi.fn(async () => api.catalog),
  getModelsFreeSpace: vi.fn(async () => api.freeSpace),
  getModelDownloadStatuses: vi.fn(async () => []),
  listenModelDownloadStatus: vi.fn(async (handler: (status: ModelDownloadStatus) => void) => {
    api.listener = handler;
    return () => {
      api.listener = null;
    };
  }),
  startModelDownload: api.startModelDownload,
  cancelModelDownload: vi.fn(async () => undefined),
}));

function asr(id: string, modelName: string, sizeBytes: number, extra: Partial<DownloadableModel> = {}): DownloadableModel {
  return {
    id,
    engine: "engine",
    modelName,
    description: `${modelName} description`,
    sizeBytes,
    profile: "accurate",
    availability: "available",
    availabilityReason: null,
    installed: false,
    requirements: null,
    artifactCount: 1,
    capability: "asr",
    ...extra,
  };
}

const livePair = asr("live-pair", "Live pair · Nemotron + Parakeet", 1_400_000_000, {
  capabilities: {
    supportedContexts: ["shortcut_dictation"],
    nativeDiarization: false,
    timestampedSegments: false,
    contextSupport: true,
    languageControl: "automatic_and_fixed",
    maximumAudioDurationMs: 300_000,
    streamingTranscription: true,
  },
});
const qwen = asr("qwen3-asr-1.7b-bf16", "Qwen3-ASR-1.7B", 4_700_000_000);
const whisper = asr("ggml-large-v3-turbo-q5_0-bin", "ggml-large-v3-turbo-q5_0.bin", 574_000_000);
const moss = asr("moss-transcribe-diarize-0.9b-f16", "MOSS Transcribe + Diarize 0.9B F16", 1_800_000_000);

const readiness: DictationReadiness = {
  hasModel: false,
  shortcutModelReady: false,
  microphone: "not_determined",
  firstRunCompleted: false,
  shortcutRegistered: true,
  autoPasteEnabled: true,
  accessibilityRequired: true,
  accessibilityGranted: false,
};

function quick(overrides: Partial<QuickDictationStatusResponse> = {}): QuickDictationStatusResponse {
  return {
    state: "idle",
    registeredShortcut: "CmdOrCtrl+Shift+Space",
    shortcutMode: "push_to_talk",
    isRegistered: true,
    lastTranscriptText: null,
    lastTranscriptId: null,
    lastRecordingPath: null,
    lastErrorMessage: null,
    lastModelName: null,
    lastInsertOutcome: null,
    lastDurationMs: null,
    ...overrides,
  };
}

function props(overrides: Partial<FirstRunSetupProps> = {}): FirstRunSetupProps {
  return {
    settings: null,
    readiness,
    quickStatus: quick(),
    isPollingAccessibility: false,
    onUseModel: vi.fn(async () => undefined),
    onRequestMicrophone: vi.fn(async () => undefined),
    onOpenMicrophoneSettings: vi.fn(async () => undefined),
    onResolveAccessibility: vi.fn(async () => undefined),
    onOpenShortcutSettings: vi.fn(async () => undefined),
    onFinish: vi.fn(async () => undefined),
    ...overrides,
  };
}

describe("FirstRunSetup", () => {
  beforeEach(() => {
    api.catalog = [whisper, moss, qwen, livePair];
    api.freeSpace = 50_000_000_000;
    api.listener = null;
    api.startModelDownload.mockReset().mockImplementation(async (modelId: string) => ({
      modelId,
      modelName: modelId,
      state: "downloading",
      downloadedBytes: 0,
      totalBytes: 1,
      progressPercent: 0,
      errorMessage: null,
      currentArtifact: null,
      artifactIndex: null,
      artifactCount: 1,
    }));
  });

  it("offers only the curated dictation engines, recommended first", async () => {
    render(<FirstRunSetup {...props()} />);
    const radios = await screen.findAllByRole("radio");
    expect(radios.map((radio) => radio.closest("label")?.querySelector("strong")?.textContent)).toEqual([
      "Live dictation · Experimental",
      "Qwen",
      "Whisper Turbo Compact",
    ]);
    await waitFor(() => expect((radios[0] as HTMLInputElement).checked).toBe(true));
    expect(screen.getByText("Works for: Shortcut")).toBeTruthy();
  });

  it("disables engines that do not fit on the disk and preselects one that does", async () => {
    api.freeSpace = 3_000_000_000;
    render(<FirstRunSetup {...props()} />);
    const radios = (await screen.findAllByRole("radio")) as HTMLInputElement[];
    expect(radios[1].disabled).toBe(true);
    await waitFor(() => expect(radios[0].checked).toBe(true));
    expect(screen.getByText(/Not enough free disk space/)).toBeTruthy();
  });

  it("downloads the chosen engine and makes it the shortcut model once installed", async () => {
    const current = props();
    render(<FirstRunSetup {...current} />);
    const radios = await screen.findAllByRole("radio");
    fireEvent.click(radios[2]);
    fireEvent.click(screen.getByRole("button", { name: "Download and continue" }));
    await waitFor(() => expect(api.startModelDownload).toHaveBeenCalledWith(whisper.id));
    expect(await screen.findByRole("heading", { name: "Let Murmela listen and type" })).toBeTruthy();
    expect(current.onUseModel).not.toHaveBeenCalled();
    api.listener?.({
      modelId: whisper.id,
      modelName: whisper.modelName,
      state: "completed",
      downloadedBytes: 1,
      totalBytes: 1,
      progressPercent: 100,
      errorMessage: null,
      currentArtifact: null,
      artifactIndex: null,
      artifactCount: 1,
    });
    await waitFor(() => expect(current.onUseModel).toHaveBeenCalledWith(whisper.id));
  });

  it("uses an installed engine without downloading", async () => {
    api.catalog = [{ ...qwen, installed: true }, whisper];
    const current = props();
    render(<FirstRunSetup {...current} />);
    fireEvent.click(await screen.findByRole("button", { name: "Use this engine" }));
    await waitFor(() => expect(current.onUseModel).toHaveBeenCalledWith(qwen.id));
    expect(api.startModelDownload).not.toHaveBeenCalled();
  });

  it("asks for the microphone and pasting permissions", async () => {
    api.catalog = [{ ...qwen, installed: true }];
    const current = props();
    render(<FirstRunSetup {...current} />);
    fireEvent.click(await screen.findByRole("button", { name: "Use this engine" }));
    fireEvent.click(await screen.findByRole("button", { name: "Allow microphone" }));
    expect(current.onRequestMicrophone).toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Allow pasting" }));
    expect(current.onResolveAccessibility).toHaveBeenCalled();
  });

  it("celebrates the first dictation made after the try step opened", async () => {
    api.catalog = [{ ...qwen, installed: true }];
    const current = props({
      readiness: { ...readiness, shortcutModelReady: true, microphone: "granted" },
      quickStatus: quick({ lastTranscriptId: "old", lastTranscriptText: "Old words", state: "inserted" }),
    });
    const { rerender } = render(<FirstRunSetup {...current} />);
    fireEvent.click(await screen.findByRole("button", { name: "Use this engine" }));
    fireEvent.click(await screen.findByRole("button", { name: "Continue" }));
    expect(await screen.findByText(/hold/)).toBeTruthy();
    expect(screen.getByText("⌘+⇧+Space")).toBeTruthy();
    expect(screen.queryByText(/Old words/)).toBeNull();
    rerender(
      <FirstRunSetup
        {...current}
        quickStatus={quick({ lastTranscriptId: "new", lastTranscriptText: "Hello Murmela", state: "inserted" })}
      />,
    );
    expect(await screen.findByText("It works! Murmela heard: “Hello Murmela”")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Start talking" }));
    await waitFor(() => expect(current.onFinish).toHaveBeenCalled());
  });

  it("can be skipped at any step", async () => {
    const current = props();
    render(<FirstRunSetup {...current} />);
    fireEvent.click(await screen.findByRole("button", { name: "Skip setup" }));
    await waitFor(() => expect(current.onFinish).toHaveBeenCalled());
  });
});
