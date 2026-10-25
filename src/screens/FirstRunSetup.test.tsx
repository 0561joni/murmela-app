import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  DictationOutputState,
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
  termsAccepted: false,
  acceptModelTerms: vi.fn(async () => undefined),
  outputListener: null as ((state: DictationOutputState) => void) | null,
  outputState: null as DictationOutputState | null,
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
  getModelTermsStatus: vi.fn(async () => ({ required: true, accepted: api.termsAccepted })),
  acceptModelTerms: api.acceptModelTerms,
  getGlobeKeyAction: vi.fn(async () => "do_nothing"),
  openKeyboardSettings: vi.fn(async () => undefined),
}));

vi.mock("../lib/textTranslation", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/textTranslation")>()),
  getTranslatorStatus: vi.fn(async () => ({ memoryAvailableBytes: 8e9 })),
}));

vi.mock("../lib/translationApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/translationApi")>()),
  getDictationOutputState: vi.fn(async () => api.outputState),
  listenDictationOutput: vi.fn(async (handler: (state: DictationOutputState) => void) => {
    api.outputListener = handler;
    return () => undefined;
  }),
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
const gemma4b = asr("translategemma-4b-q6-k", "TranslateGemma 4B", 3_190_755_840, { capability: "translation" });
const gemma12b = asr("translategemma-12b-q6-k", "TranslateGemma 12B", 9_660_827_392, { capability: "translation" });
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
    onSaveSettings: vi.fn(async () => undefined),
    onFinish: vi.fn(async () => undefined),
    ...overrides,
  };
}

/** Leaves the first step, where an installed engine needs no download. */
async function passNeeds() {
  fireEvent.click(await screen.findByRole("button", { name: "Continue" }));
  await screen.findByRole("heading", { name: "Translate as you speak" });
}

describe("FirstRunSetup", () => {
  beforeEach(() => {
    api.catalog = [whisper, moss, qwen, livePair];
    api.freeSpace = 50_000_000_000;
    api.listener = null;
    api.termsAccepted = false;
    api.acceptModelTerms.mockClear();
    api.outputListener = null;
    api.outputState = null;
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

  it("explains what is needed and downloads the engine only after the user agrees", async () => {
    const current = props();
    render(<FirstRunSetup {...current} />);
    expect(await screen.findByRole("heading", { name: "What Murmela needs" })).toBeTruthy();
    expect(screen.getByText("Microphone access")).toBeTruthy();
    const engines = await screen.findByRole("radiogroup", { name: "Dictation engine" });
    await waitFor(() => {
      const radios = within(engines).getAllByRole("radio") as HTMLInputElement[];
      expect(radios.find((radio) => radio.checked)?.value).toBe(livePair.id);
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(api.startModelDownload).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /^Download .+ and continue$/ }));
    await waitFor(() => expect(api.startModelDownload).toHaveBeenCalledWith(livePair.id));
    expect(await screen.findByRole("heading", { name: "Translate as you speak" })).toBeTruthy();
    expect(api.startModelDownload).toHaveBeenCalledTimes(1);
    expect(await screen.findByText(/Downloading Live dictation/)).toBeTruthy();
    expect(current.onUseModel).not.toHaveBeenCalled();
    api.listener?.({
      modelId: livePair.id,
      modelName: livePair.modelName,
      state: "completed",
      downloadedBytes: 1,
      totalBytes: 1,
      progressPercent: 100,
      errorMessage: null,
      currentArtifact: null,
      artifactIndex: null,
      artifactCount: 1,
    });
    await waitFor(() => expect(current.onUseModel).toHaveBeenCalledWith(livePair.id));
  });

  it("preselects a smaller engine when the recommended one does not fit", async () => {
    api.freeSpace = 2_000_000_000;
    render(<FirstRunSetup {...props()} />);
    const engines = await screen.findByRole("radiogroup", { name: "Dictation engine" });
    await waitFor(() => {
      const radios = within(engines).getAllByRole("radio") as HTMLInputElement[];
      expect(radios.find((radio) => radio.checked)?.value).toBe(whisper.id);
    });
    fireEvent.click(screen.getByRole("button", { name: /^Download .+ and continue$/ }));
    await waitFor(() => expect(api.startModelDownload).toHaveBeenCalledWith(whisper.id));
  });

  it("downloads nothing when the user picks Not now", async () => {
    const current = props();
    render(<FirstRunSetup {...current} />);
    await screen.findByRole("radiogroup", { name: "Dictation engine" });
    fireEvent.click(await screen.findByRole("button", { name: "Not now" }));
    expect(await screen.findByRole("heading", { name: "Translate as you speak" })).toBeTruthy();
    expect(api.startModelDownload).not.toHaveBeenCalled();
  });

  it("uses an installed engine without downloading", async () => {
    api.catalog = [{ ...qwen, installed: true }, whisper, livePair];
    const current = props();
    render(<FirstRunSetup {...current} />);
    await waitFor(() => expect(current.onUseModel).toHaveBeenCalledWith(qwen.id));
    expect(api.startModelDownload).not.toHaveBeenCalled();
  });

  it("keeps the current engine when dictation already works", async () => {
    const current = props({ readiness: { ...readiness, shortcutModelReady: true } });
    render(<FirstRunSetup {...current} />);
    expect(await screen.findByRole("heading", { name: "What Murmela needs" })).toBeTruthy();
    expect(screen.getByText("Already installed.")).toBeTruthy();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(api.startModelDownload).not.toHaveBeenCalled();
    expect(current.onUseModel).not.toHaveBeenCalled();
  });

  it("asks for the microphone and pasting permissions", async () => {
    api.catalog = [{ ...qwen, installed: true }];
    const current = props();
    render(<FirstRunSetup {...current} />);
    await passNeeds();
    fireEvent.click(await screen.findByRole("button", { name: /No, just dictate/ }));
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
    await passNeeds();
    fireEvent.click(await screen.findByRole("button", { name: /No, just dictate/ }));
    fireEvent.click(await screen.findByRole("button", { name: "Continue" }));
    expect(await screen.findByText(/^Hold/)).toBeTruthy();
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

  it("asks which languages to translate into and downloads the engine after the Gemma terms", async () => {
    api.catalog = [{ ...qwen, installed: true }, gemma4b, gemma12b];
    const current = props();
    render(<FirstRunSetup {...current} />);
    await passNeeds();
    fireEvent.click(await screen.findByRole("button", { name: /Yes, translate too/ }));
    expect(await screen.findByRole("heading", { name: "Translate as you speak" })).toBeTruthy();
    expect(screen.queryByRole("radiogroup", { name: "Translation engine" })).toBeNull();
    fireEvent.click(screen.getByRole("checkbox", { name: "Français" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Deutsch" }));
    // 8 GB free: the compact engine is recommended and preselected.
    const engines = await screen.findByRole("radiogroup", { name: "Translation engine" });
    expect(engines.textContent).toContain("Recommended for this Mac");
    await waitFor(() => {
      const radios = within(engines).getAllByRole("radio") as HTMLInputElement[];
      expect(radios.map((radio) => radio.checked)).toEqual([true, false]);
    });
    fireEvent.click(screen.getByRole("button", { name: "Download and continue" }));
    await waitFor(() => expect(current.onSaveSettings).toHaveBeenCalledWith({ translationCycleLanguages: ["de", "fr"] }));
    // Not yet accepted: the terms come first, then the download.
    fireEvent.click(await screen.findByRole("checkbox", { name: /I agree/ }));
    fireEvent.click(screen.getByRole("button", { name: /Agree and download/ }));
    await waitFor(() => expect(api.acceptModelTerms).toHaveBeenCalledWith(gemma4b.id));
    await waitFor(() => expect(api.startModelDownload).toHaveBeenCalledWith(gemma4b.id));
    expect(await screen.findByRole("heading", { name: "Let Murmela listen and type" })).toBeTruthy();
  });

  it("saves the languages for later where no translation engine is available", async () => {
    api.catalog = [{ ...qwen, installed: true }];
    const current = props();
    render(<FirstRunSetup {...current} />);
    await passNeeds();
    fireEvent.click(await screen.findByRole("button", { name: /Yes, translate too/ }));
    fireEvent.click(await screen.findByRole("checkbox", { name: "Polski" }));
    expect(screen.getByText(/needs an Apple Silicon Mac/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    await waitFor(() => expect(current.onSaveSettings).toHaveBeenCalledWith({ translationCycleLanguages: ["pl"] }));
    expect(await screen.findByRole("heading", { name: "Let Murmela listen and type" })).toBeTruthy();
    expect(api.startModelDownload).not.toHaveBeenCalled();
  });

  it("turns translation on at once when its engine is already installed", async () => {
    api.catalog = [{ ...qwen, installed: true }, { ...gemma12b, installed: true }];
    const current = props();
    render(<FirstRunSetup {...current} />);
    await passNeeds();
    fireEvent.click(await screen.findByRole("button", { name: /Yes, translate too/ }));
    fireEvent.click(await screen.findByRole("checkbox", { name: "English" }));
    fireEvent.click(await screen.findByRole("button", { name: "Continue" }));
    await waitFor(() =>
      expect(current.onSaveSettings).toHaveBeenCalledWith({ translationEnabled: true, translationModelId: gemma12b.id }),
    );
    expect(api.startModelDownload).not.toHaveBeenCalled();
  });

  it("ticks off the practice steps as the user dictates and switches language", async () => {
    api.catalog = [{ ...qwen, installed: true }];
    api.outputState = { outputMode: "original", busy: false, stage: "idle", statusText: "", ready: true, errorMessage: null, lastOutput: null };
    const current = props({
      settings: { translationEnabled: true, shortcut: "Fn" } as FirstRunSetupProps["settings"],
      readiness: { ...readiness, shortcutModelReady: true, microphone: "granted" },
      quickStatus: quick({ registeredShortcut: "Fn" }),
    });
    const { rerender } = render(<FirstRunSetup {...current} />);
    await passNeeds();
    fireEvent.click(await screen.findByRole("button", { name: /No, just dictate/ }));
    fireEvent.click(await screen.findByRole("button", { name: "Continue" }));
    const practice = await screen.findByRole("list", { name: "Practice" });
    const items = () => Array.from(practice.querySelectorAll("li")).map((item) => item.className);
    expect(items()).toEqual(["is-active", "", ""]);
    rerender(<FirstRunSetup {...current} quickStatus={quick({ registeredShortcut: "Fn", state: "listening" })} />);
    await waitFor(() => expect(items()).toEqual(["is-done", "is-active", ""]));
    rerender(
      <FirstRunSetup
        {...current}
        quickStatus={quick({ registeredShortcut: "Fn", state: "inserted", lastTranscriptId: "t1", lastTranscriptText: "Hallo" })}
      />,
    );
    await waitFor(() => expect(items()).toEqual(["is-done", "is-done", "is-active"]));
    // The paste did not reach the box, so the words are put there directly.
    expect((screen.getByRole("textbox", { name: "Try dictation here" }) as HTMLTextAreaElement).value).toBe("Hallo");
    // A second later the status returns to idle; the step stays done.
    rerender(
      <FirstRunSetup
        {...current}
        quickStatus={quick({ registeredShortcut: "Fn", state: "idle", lastTranscriptId: "t1", lastTranscriptText: "Hallo" })}
      />,
    );
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(items()).toEqual(["is-done", "is-done", "is-active"]);
    expect(practice.textContent).toContain("Now: Original");
    await waitFor(() => expect(api.outputListener).not.toBeNull());
    api.outputListener?.({ ...api.outputState!, outputMode: "en" });
    await waitFor(() => expect(items()).toEqual(["is-done", "is-done", "is-done"]));
    expect(practice.textContent).toContain("Now: English");
  });

  it("asks first whether to translate and skips the language setup on No", async () => {
    api.catalog = [{ ...qwen, installed: true }, gemma4b, gemma12b];
    render(<FirstRunSetup {...props()} />);
    await passNeeds();
    expect(screen.getByText("Do you want Murmela to translate what you dictate?")).toBeTruthy();
    expect(screen.queryByRole("checkbox", { name: "Deutsch" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Yes, translate too/ }));
    expect(await screen.findByRole("checkbox", { name: "Deutsch" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Skip this step" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    fireEvent.click(await screen.findByRole("button", { name: /No, just dictate/ }));
    expect(await screen.findByRole("heading", { name: "Let Murmela listen and type" })).toBeTruthy();
    expect(api.startModelDownload).not.toHaveBeenCalled();
  });

  it("can be skipped at any step", async () => {
    const current = props();
    render(<FirstRunSetup {...current} />);
    fireEvent.click(await screen.findByRole("button", { name: "Skip setup" }));
    await waitFor(() => expect(current.onFinish).toHaveBeenCalled());
  });
});
