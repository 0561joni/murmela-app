import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  AppSettings,
  DownloadableModel,
  InstalledModel,
  ModelDownloadStatus,
  SettingsPatch,
} from "../types/domain";

const apiMocks = vi.hoisted(() => ({
  getModelDownloadStatuses: vi.fn(),
  getLivePairStatus: vi.fn(),
  listenLivePairStatus: vi.fn(),
  listDownloadableModels: vi.fn(),
  listInputDevices: vi.fn(),
  listenModelDownloadStatus: vi.fn(),
  startModelDownload: vi.fn(),
  getModelTermsStatus: vi.fn(),
  acceptModelTerms: vi.fn(),
  deleteModel: vi.fn(),
  rescanModelsFolder: vi.fn(),
  suspendShortcutCapture: vi.fn(),
  resumeShortcutCapture: vi.fn(),
  getLicenseStatus: vi.fn(),
}));

vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  getModelDownloadStatuses: apiMocks.getModelDownloadStatuses,
  getLivePairStatus: apiMocks.getLivePairStatus,
  listenLivePairStatus: apiMocks.listenLivePairStatus,
  listDownloadableModels: apiMocks.listDownloadableModels,
  listInputDevices: apiMocks.listInputDevices,
  listenModelDownloadStatus: apiMocks.listenModelDownloadStatus,
  startModelDownload: apiMocks.startModelDownload,
  getModelTermsStatus: apiMocks.getModelTermsStatus,
  acceptModelTerms: apiMocks.acceptModelTerms,
  deleteModel: apiMocks.deleteModel,
  rescanModelsFolder: apiMocks.rescanModelsFolder,
  suspendShortcutCapture: apiMocks.suspendShortcutCapture,
  resumeShortcutCapture: apiMocks.resumeShortcutCapture,
  getLicenseStatus: apiMocks.getLicenseStatus,
}));

import { SettingsScreen } from "./SettingsScreen";
import { resetLicenseCache } from "../hooks/useLicense";

const proLicense = { pro: true, keyHint: "••••4D51", licensedTo: "Ada", activatedAt: "2026-10-14T12:00:00Z" };
const freeLicense = { pro: false, keyHint: null, licensedTo: null, activatedAt: null };

beforeEach(() => {
  // Most tests exercise Pro features; the free version has its own tests below.
  resetLicenseCache();
  apiMocks.getLicenseStatus.mockReset().mockResolvedValue(proLicense);
});

const initialSettings: AppSettings = {
  defaultMode: "file_transcribe",
  shortcut: "CmdOrCtrl+Shift+Space",
  translationEnabled: false,
  translationCycleShortcut: "Ctrl+Alt+L",
  translationModelId: "translategemma-12b-q6-k",
  shortcutMode: "push_to_talk",
  languageMode: "auto",
  fixedLanguage: null,
  preferredInputDevice: null,
  insertBehavior: "paste",
  launchAtLoginEnabled: false,
  gpuEnabled: true,
  shortcutDictationModelProfile: "balanced",
  shortcutDictationSelectedModelId: null,
  quickDictateModelProfile: "balanced",
  quickDictateSelectedModelId: null,
  fileTranscribeModelProfile: "balanced",
  fileTranscribeSelectedModelId: null,
  appearance: "system",
  motionPreference: "system",
  saveHistory: true,
  soundsEnabled: true,
  volumeDuckingEnabled: true,
  fileDiarizationEnabled: false,
  livePairChunkMs: 560,
  livePairKeepLoaded: true,
  pasteLastShortcut: "Ctrl+Alt+V",
  seriousMode: false,
  autoUpdateEnabled: true,
};

const asrModel: DownloadableModel = {
  id: "ggml-small-bin",
  engine: "whisper.cpp",
  modelName: "ggml-small.bin",
  description: "ASR model",
  sizeBytes: 487_601_967,
  profile: "balanced",
  availability: "available",
  availabilityReason: null,
  installed: true,
  requirements: null,
  artifactCount: 1,
  capability: "asr",
};

const diarizationModel: DownloadableModel = {
  id: "sherpa-diarization-pyannote3-eres2net-voxceleb-v2",
  engine: "sherpa-onnx",
  modelName: "Offline speaker diarization",
  description: "Local speaker separation",
  sizeBytes: 32_478_041,
  profile: "balanced",
  availability: "available",
  availabilityReason: null,
  installed: false,
  requirements: "CPU-only · approximately 46 MB download",
  artifactCount: 2,
  capability: "diarization",
};

function Harness({
  onSave,
  onReload,
  installedModels = [],
  initial = {},
}: {
  onSave: (patch: SettingsPatch) => void;
  onReload: () => Promise<void>;
  installedModels?: InstalledModel[];
  initial?: Partial<AppSettings>;
}) {
  const [settings, setSettings] = useState({ ...initialSettings, ...initial });
  return (
    <SettingsScreen
      settings={settings}
      installedModels={installedModels}
      onSave={async (patch) => {
        onSave(patch);
        setSettings((current) => ({ ...current, ...patch }));
      }}
      onReloadModelState={onReload}
    />
  );
}

function status(
  state: ModelDownloadStatus["state"],
  progressPercent: number | null,
): ModelDownloadStatus {
  return {
    modelId: diarizationModel.id,
    modelName: diarizationModel.modelName,
    state,
    downloadedBytes:
      progressPercent === null
        ? 0
        : Math.round((diarizationModel.sizeBytes * progressPercent) / 100),
    totalBytes: diarizationModel.sizeBytes,
    progressPercent,
    errorMessage: state === "failed" ? "Network unavailable" : null,
    currentArtifact: state === "downloading" ? "segmentation.onnx" : null,
    artifactIndex: state === "downloading" ? 1 : null,
    artifactCount: 2,
  };
}

describe("Settings speaker identification", () => {
  let downloadListener:
    ((status: ModelDownloadStatus) => void | Promise<void>) | undefined;

  beforeEach(() => {
    downloadListener = undefined;
    apiMocks.getModelDownloadStatuses.mockReset().mockResolvedValue([]);
    apiMocks.getLivePairStatus.mockReset().mockResolvedValue({
      state: "off", message: null, loadMs: null, rssBytes: null,
    });
    apiMocks.listenLivePairStatus.mockReset().mockResolvedValue(() => undefined);
    apiMocks.listDownloadableModels
      .mockReset()
      .mockResolvedValue([asrModel, diarizationModel]);
    apiMocks.listInputDevices.mockReset().mockResolvedValue([]);
    apiMocks.listenModelDownloadStatus
      .mockReset()
      .mockImplementation(async (listener) => {
        downloadListener = listener;
        return () => undefined;
      });
    apiMocks.startModelDownload
      .mockReset()
      .mockResolvedValue(status("downloading", 0));
    apiMocks.suspendShortcutCapture.mockReset().mockResolvedValue(undefined);
    apiMocks.resumeShortcutCapture.mockReset().mockResolvedValue(undefined);
    apiMocks.deleteModel.mockReset().mockResolvedValue(undefined);
    apiMocks.rescanModelsFolder.mockReset().mockResolvedValue([]);
    apiMocks.getModelTermsStatus.mockReset().mockResolvedValue({ required: false, accepted: true });
    apiMocks.acceptModelTerms.mockReset().mockResolvedValue(undefined);
  });

  it("offers the 🌐 key, Right ⌥ and a key combination as the dictation shortcut", async () => {
    const onSave = vi.fn();
    render(<Harness onSave={onSave} onReload={vi.fn().mockResolvedValue(undefined)} />);
    fireEvent.click(screen.getByRole("button", { name: "Dictation" }));
    const keys = await screen.findByRole("radiogroup", { name: "Dictation key" });
    expect(within(keys).getByRole("radio", { name: /Key combination/ }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByText("Shortcut behavior")).toBeTruthy();

    fireEvent.click(within(keys).getByRole("radio", { name: /fn key/ }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith({ shortcut: "Fn" }));
    expect(await screen.findByText(/Double-tap it to talk hands-free/)).toBeTruthy();
    expect(screen.queryByText("Shortcut behavior")).toBeNull();
    // The browser stand-in reports macOS's default: a tap opens the emoji picker.
    expect(await screen.findByRole("button", { name: "Open Keyboard settings" })).toBeTruthy();

    fireEvent.click(within(keys).getByRole("radio", { name: /Right ⌥ Option/ }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith({ shortcut: "RightAlt" }));
    expect(screen.queryByRole("button", { name: "Open Keyboard settings" })).toBeNull();
  });

  it("asks for the Gemma terms before downloading the translation engine", async () => {
    const translationModel: DownloadableModel = {
      ...asrModel,
      id: "translategemma-12b-q6-k",
      engine: "llama.cpp-translation",
      modelName: "TranslateGemma 12B Q6_K",
      installed: false,
      capability: "translation",
    };
    apiMocks.listDownloadableModels.mockResolvedValue([asrModel, translationModel]);
    apiMocks.getModelTermsStatus.mockResolvedValue({ required: true, accepted: false });
    render(<Harness onSave={vi.fn()} onReload={vi.fn().mockResolvedValue(undefined)} />);
    fireEvent.click(screen.getByRole("button", { name: "Dictation" }));
    fireEvent.click(await screen.findByRole("button", { name: "Download TranslateGemma 12B Q6_K" }));

    const dialog = await screen.findByRole("dialog", { name: "Gemma Terms of Use" });
    expect(apiMocks.startModelDownload).not.toHaveBeenCalled();
    expect(within(dialog).getByText(/Gemma is provided under and subject to the Gemma Terms of Use/)).toBeTruthy();
    const agree = within(dialog).getByRole("button", { name: "Agree and download" });
    expect((agree as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(within(dialog).getByRole("checkbox"));
    fireEvent.click(agree);
    await waitFor(() => expect(apiMocks.acceptModelTerms).toHaveBeenCalledWith("translategemma-12b-q6-k"));
    await waitFor(() => expect(apiMocks.startModelDownload).toHaveBeenCalledWith("translategemma-12b-q6-k"));
    expect(screen.queryByRole("dialog", { name: "Gemma Terms of Use" })).toBeNull();
  });

  it("deletes an installed model after confirmation and refreshes both model lists", async () => {
    const custom: DownloadableModel = { ...asrModel, origin: "custom" };
    apiMocks.listDownloadableModels
      .mockResolvedValueOnce([custom, diarizationModel])
      .mockResolvedValue([diarizationModel]);
    const onReload = vi.fn().mockResolvedValue(undefined);
    render(<Harness onSave={vi.fn()} onReload={onReload} />);
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    fireEvent.click(screen.getByRole("button", { name: /Download engines/ }));
    expect(
      await screen.findByText(/Added to the models folder by hand/),
    ).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Delete Whisper Small" }));
    expect(screen.getByText("Delete Whisper Small and free 488 MB?")).toBeTruthy();
    expect(apiMocks.deleteModel).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));

    await waitFor(() => expect(apiMocks.deleteModel).toHaveBeenCalledWith("ggml-small-bin"));
    await waitFor(() => expect(screen.queryByText("Whisper Small")).toBeNull());
    expect(onReload).toHaveBeenCalled();
  });

  it("keeps the model when the deletion is not confirmed and reports failures", async () => {
    apiMocks.listDownloadableModels.mockResolvedValue([asrModel]);
    apiMocks.deleteModel.mockRejectedValue(
      new Error("MODEL_BUSY: A dictation or transcription is running."),
    );
    render(<Harness onSave={vi.fn()} onReload={vi.fn().mockResolvedValue(undefined)} />);
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    fireEvent.click(screen.getByRole("button", { name: /Download engines/ }));

    fireEvent.click(await screen.findByRole("button", { name: "Delete Whisper Small" }));
    fireEvent.click(screen.getByRole("button", { name: "Keep" }));
    expect(screen.queryByText(/and free/)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Delete Whisper Small" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect((await screen.findByRole("alert")).textContent).toBe("The engine is busy with another job. Try again in a moment.");
    expect(screen.getByText("Whisper Small")).toBeTruthy();
  });

  it("lists models added by hand with a use-at-your-own-risk note", async () => {
    const custom: DownloadableModel = {
      ...asrModel,
      id: "ggml-large-v3-bin",
      modelName: "ggml-large-v3.bin",
      profile: "accurate",
      origin: "custom",
    };
    apiMocks.listDownloadableModels.mockResolvedValue([custom]);
    render(<Harness onSave={vi.fn()} onReload={vi.fn().mockResolvedValue(undefined)} />);
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    fireEvent.click(screen.getByRole("button", { name: /Download engines/ }));

    expect(await screen.findByText(/use at your own risk/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Delete Whisper Large V3" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Download Whisper/ })).toBeNull();
  });

  it("rescans the models folder and reloads the installed models", async () => {
    const onReload = vi.fn().mockResolvedValue(undefined);
    render(<Harness onSave={vi.fn()} onReload={onReload} />);
    fireEvent.click(screen.getByRole("button", { name: "Advanced" }));
    fireEvent.click(screen.getByRole("button", { name: "Rescan models folder" }));
    await waitFor(() => expect(apiMocks.rescanModelsFolder).toHaveBeenCalled());
    await waitFor(() => expect(onReload).toHaveBeenCalled());
  });

  it("captures the language shortcut separately and restores registration before saving", async () => {
    const save = vi.fn();
    render(<Harness onSave={save} onReload={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Dictation" }));
    fireEvent.click(screen.getByRole("button", { name: "Set language shortcut" }));
    await screen.findByText("Press a shortcut… Esc to cancel");
    expect(apiMocks.suspendShortcutCapture).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(window, { key: "ArrowLeft", code: "ArrowLeft", metaKey: true, shiftKey: true });
    await waitFor(() => expect(save).toHaveBeenCalledWith({ translationCycleShortcut: "CmdOrCtrl+Shift+Left" }));
    expect(apiMocks.resumeShortcutCapture.mock.invocationCallOrder[0]).toBeLessThan(save.mock.invocationCallOrder[0]);
    expect(save).not.toHaveBeenCalledWith(expect.objectContaining({ shortcut: expect.anything() }));
    fireEvent.click(await screen.findByRole("button", { name: "Reset language shortcut to default" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith({ translationCycleShortcut: "Ctrl+Alt+L" }));
  });

  it("sets and resets the paste-last shortcut", async () => {
    const save = vi.fn();
    render(<Harness onSave={save} onReload={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Dictation" }));
    const card = screen.getByRole("article", { name: "Paste last dictation" });
    expect(within(card).getByText("⌃+⌥+V")).toBeTruthy();
    fireEvent.click(within(card).getByRole("button", { name: "Set paste-last shortcut" }));
    await within(card).findByText("Press a shortcut… Esc to cancel");
    expect(screen.queryByText("Listening for shortcut… Press Esc to cancel.")).toBeNull();
    fireEvent.keyDown(window, { key: "b", code: "KeyB", metaKey: true, altKey: true });
    await waitFor(() => expect(save).toHaveBeenCalledWith({ pasteLastShortcut: "CmdOrCtrl+Alt+B" }));
    fireEvent.click(await within(card).findByRole("button", { name: "Reset paste-last shortcut to default" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith({ pasteLastShortcut: "Ctrl+Alt+V" }));
  });

  it("chooses the spoken language", async () => {
    const save = vi.fn();
    render(<Harness onSave={save} onReload={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Dictation" }));
    const select = screen.getByRole("combobox", { name: "Language you speak" }) as HTMLSelectElement;
    expect(select.value).toBe("auto");
    fireEvent.change(select, { target: { value: "de" } });
    await waitFor(() => expect(save).toHaveBeenCalledWith({ languageMode: "fixed", fixedLanguage: "de" }));
    fireEvent.change(select, { target: { value: "auto" } });
    await waitFor(() => expect(save).toHaveBeenCalledWith({ languageMode: "auto", fixedLanguage: null }));
  });

  it("turns on Serious mode from Appearance", async () => {
    const save = vi.fn();
    render(<Harness onSave={save} onReload={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Appearance & feedback" }));
    fireEvent.click(screen.getByRole("button", { name: "Serious mode" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith({ seriousMode: true }));
    expect(screen.getByRole("button", { name: "Reset Murmelmeter" })).toBeTruthy();
  });

  it("keeps Control and Command apart when capturing a shortcut on macOS", async () => {
    const save = vi.fn();
    render(<Harness onSave={save} onReload={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Dictation" }));
    expect(screen.getByText("⌃+⌥+L")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Set language shortcut" }));
    await screen.findByText("Press a shortcut… Esc to cancel");
    fireEvent.keyDown(window, { key: "k", code: "KeyK", ctrlKey: true, altKey: true });
    await waitFor(() => expect(save).toHaveBeenCalledWith({ translationCycleShortcut: "Ctrl+Alt+K" }));
  });

  it("does not confirm a failed appearance save and retains the saved preference", async () => {
    const onSave = vi.fn().mockRejectedValue(new Error("Disk is read-only"));
    render(
      <SettingsScreen
        settings={initialSettings}
        installedModels={[]}
        onSave={onSave}
        onReloadModelState={vi.fn()}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Appearance & feedback" }),
    );
    const appearance = screen.getByRole("combobox", {
      name: "Appearance",
    }) as HTMLSelectElement;
    fireEvent.change(appearance, { target: { value: "dark" } });
    await screen.findByText("Disk is read-only");
    expect(onSave).toHaveBeenCalledWith({ appearance: "dark" });
    expect(appearance.value).toBe("system");
    expect(appearance.disabled).toBe(false);
    expect(screen.queryByText("Saved")).toBeNull();
  });

  it("keeps speaker labels locked on the free version and points to the license", async () => {
    apiMocks.getLicenseStatus.mockResolvedValue(freeLicense);
    const onSave = vi.fn();
    render(<Harness onSave={onSave} onReload={vi.fn().mockResolvedValue(undefined)} />);

    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    expect(await screen.findByText("Speaker labels are part of Murmela Pro.")).toBeTruthy();
    const row = screen.getByText("Speaker identification").closest(".setting-row") as HTMLElement;
    const switchButton = within(row).getByRole("button") as HTMLButtonElement;
    expect(switchButton.disabled).toBe(true);
    fireEvent.click(switchButton);
    expect(onSave).not.toHaveBeenCalled();

    const opened = vi.fn();
    window.addEventListener("murmela:open-license", opened);
    fireEvent.click(screen.getByRole("button", { name: "Add license key" }));
    expect(opened).toHaveBeenCalledTimes(1);
    window.removeEventListener("murmela:open-license", opened);
  });

  it("uses one switch, shows progress, and refreshes immediately after installation", async () => {
    const onSave = vi.fn();
    const onReload = vi.fn().mockResolvedValue(undefined);
    render(<Harness onSave={onSave} onReload={onReload} />);

    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    const row = await screen.findByText("Speaker identification");
    expect(screen.queryByText("In-app Quick Dictate")).toBeNull();
    expect(screen.queryByText("Speaker count")).toBeNull();
    expect(screen.queryByText("Offline speaker diarization")).toBeNull();

    fireEvent.click(
      within(row.closest(".setting-row") as HTMLElement).getByRole("button"),
    );
    await waitFor(() => {
      expect(onSave).toHaveBeenCalledWith({ fileDiarizationEnabled: true });
      expect(screen.getByText("Starting")).toBeTruthy();
    });

    await act(async () => {
      await downloadListener?.(status("downloading", 42));
    });
    expect(screen.getByText("Installing the speaker model — 42%")).toBeTruthy();

    apiMocks.listDownloadableModels.mockResolvedValue([
      asrModel,
      { ...diarizationModel, installed: true },
    ]);
    await act(async () => {
      await downloadListener?.(status("completed", 100));
    });
    await waitFor(() => {
      expect(apiMocks.listDownloadableModels).toHaveBeenCalledTimes(2);
      expect(onReload).toHaveBeenCalledTimes(1);
      expect(
        screen.getByText("The local speaker model is installed."),
      ).toBeTruthy();
      expect(within(row.closest(".setting-row") as HTMLElement).getByText("On")).toBeTruthy();
    });
  });

  it("uses named symbols for microphone, folder, and shortcut actions", async () => {
    render(
      <Harness
        onSave={vi.fn()}
        onReload={vi.fn().mockResolvedValue(undefined)}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Dictation" }));
    await screen.findByText("Speaker identification");
    expect(
      screen.getByRole("button", { name: "Start microphone test" }),
    ).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Set custom shortcut" }),
    ).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Reset shortcut to default" }),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Advanced" }));
    expect(screen.getByRole("button", { name: "Open in Finder" })).toBeTruthy();
  });

  it("shows the resident live pair with its memory cost and saves its options", async () => {
    let pushStatus: ((status: unknown) => void) | undefined;
    apiMocks.listenLivePairStatus.mockImplementation(async (listener) => {
      pushStatus = listener;
      return () => undefined;
    });
    const installed: InstalledModel[] = [
      {
        id: "live-pair",
        engine: "fluidaudio-live-pair",
        modelName: "Live pair · Nemotron + Parakeet",
        variant: "CoreML · streaming + final pass",
        localPath: "/models/live-pair",
        sizeBytes: 1_854_503_633,
        isDefault: false,
        profile: "fast",
      },
    ];
    const onSave = vi.fn();
    render(
      <Harness
        onSave={onSave}
        onReload={vi.fn().mockResolvedValue(undefined)}
        installedModels={installed}
        initial={{ shortcutDictationSelectedModelId: "live-pair" }}
      />,
    );
    await screen.findByText("Speaker identification");
    fireEvent.click(screen.getByRole("button", { name: "Advanced" }));
    const card = screen.getByRole("article", { name: "Live dictation tuning" });
    expect(within(card).getByText(/Not loaded.*1\.4 GB of memory/)).toBeTruthy();
    await waitFor(() => expect(pushStatus).toBeDefined());
    act(() => pushStatus!({ state: "ready", message: null, loadMs: 212, rssBytes: 70_000_000 }));
    expect(within(card).getByText(/Ready · loaded in 0\.2 s/)).toBeTruthy();

    fireEvent.change(within(card).getByLabelText("Live preview"), { target: { value: "1120" } });
    await waitFor(() => expect(onSave).toHaveBeenCalledWith({ livePairChunkMs: 1120 }));
    fireEvent.click(within(card).getByRole("button", { name: "Keep Live dictation loaded" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith({ livePairKeepLoaded: false }));
    act(() => pushStatus!({ state: "unloaded", message: "Unloaded under memory pressure. Reloads on the next dictation.", loadMs: null, rssBytes: null }));
    expect(within(card).getByText(/Unloaded to free memory/)).toBeTruthy();

    // The launch-at-login tip sits with the engine choice, not the tuning.
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    const engine = screen.getByRole("article", { name: "Dictation engine" });
    fireEvent.click(within(engine).getByRole("button", { name: "Launch at login" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith({ launchAtLoginEnabled: true }));
    await waitFor(() => expect(within(engine).queryByRole("button", { name: "Launch at login" })).toBeNull());
  });

  it("hides live dictation options when no streaming model is installed", async () => {
    render(<Harness onSave={vi.fn()} onReload={vi.fn().mockResolvedValue(undefined)} />);
    await screen.findByText("Speaker identification");
    fireEvent.click(screen.getByRole("button", { name: "Advanced" }));
    expect(screen.queryByRole("article", { name: "Live dictation tuning" })).toBeNull();
  });

  it("uses a dictation engine that the record button follows, and a file engine", async () => {
    const installed: InstalledModel[] = [
      {
        id: "ggml-medium-bin",
        engine: "whisper.cpp",
        modelName: "ggml-medium.bin",
        variant: "accurate",
        localPath: "/models/ggml-medium.bin",
        sizeBytes: 1_533_763_059,
        isDefault: true,
        profile: "accurate",
      },
      {
        id: "qwen3-asr-1.7b-bf16",
        engine: "qwen3_asr_c",
        modelName: "Qwen3-ASR-1.7B",
        variant: "1.7B BF16",
        localPath: "/models/qwen3-asr-1.7b-bf16",
        sizeBytes: 4_703_041_355,
        isDefault: false,
        profile: "accurate",
      },
      {
        id: "live-pair",
        engine: "fluidaudio-live-pair",
        modelName: "Live pair · Nemotron + Parakeet",
        variant: "CoreML · streaming + final pass",
        localPath: "/models/live-pair",
        sizeBytes: 1_854_503_633,
        isDefault: false,
        profile: "fast",
        capabilities: {
          supportedContexts: ["shortcut_dictation"],
          nativeDiarization: false,
          timestampedSegments: false,
          contextSupport: true,
          languageControl: "automatic_and_fixed",
          maximumAudioDurationMs: 300_000,
          streamingTranscription: true,
        },
      },
    ];
    const onSave = vi.fn();
    render(
      <Harness
        onSave={onSave}
        onReload={vi.fn().mockResolvedValue(undefined)}
        installedModels={installed}
      />,
    );
    await screen.findByText("Speaker identification");

    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    // Nothing chosen yet: the record button shows its own picker.
    expect(screen.getByRole("button", { name: "Record button engine" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Dictation engine" }));
    expect(
      within(screen.getByRole("listbox")).getByRole("option", {
        name: /Live dictation.*Recommended/,
      }),
    ).toBeTruthy();
    fireEvent.click(within(screen.getByRole("listbox")).getAllByRole("option")[0]);
    // The record button follows the dictation engine when it can run it.
    await waitFor(() =>
      expect(onSave).toHaveBeenCalledWith({
        shortcutDictationSelectedModelId: installed[0].id,
        shortcutDictationModelProfile: "accurate",
        quickDictateSelectedModelId: installed[0].id,
        quickDictateModelProfile: "accurate",
      }),
    );
    expect(screen.getByText("Uses the dictation engine")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Record button engine" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /Record button/ }));
    fireEvent.click(screen.getByRole("button", { name: "Record button engine" }));
    fireEvent.click(
      within(screen.getByRole("listbox")).getByRole("option", { name: /Qwen.*Recommended/ }),
    );
    await waitFor(() =>
      expect(onSave).toHaveBeenCalledWith({
        quickDictateSelectedModelId: installed[1].id,
        quickDictateModelProfile: "accurate",
      }),
    );

    // A shortcut-only engine leaves the record button's own engine alone.
    onSave.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Dictation engine" }));
    fireEvent.click(
      within(screen.getByRole("listbox")).getByRole("option", { name: /Live dictation/ }),
    );
    await waitFor(() =>
      expect(onSave).toHaveBeenCalledWith({
        shortcutDictationSelectedModelId: "live-pair",
        shortcutDictationModelProfile: "fast",
      }),
    );
    expect(screen.getByText("Uses Qwen")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "File engine" }));
    fireEvent.click(
      within(screen.getByRole("listbox")).getByRole("option", {
        name: /Qwen.*Recommended/,
      }),
    );
    await waitFor(() =>
      expect(onSave).toHaveBeenCalledWith({
        fileTranscribeSelectedModelId: installed[1].id,
        fileTranscribeModelProfile: "accurate",
      }),
    );
  });

  it("keeps download cards simple and exposes technical details through information", async () => {
    apiMocks.listDownloadableModels.mockResolvedValue([
      { ...asrModel, installed: false },
    ]);
    render(
      <Harness
        onSave={vi.fn()}
        onReload={vi.fn().mockResolvedValue(undefined)}
      />,
    );
    await screen.findByText("Speaker identification");
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    fireEvent.click(screen.getByRole("button", { name: /Download engines/ }));

    expect(screen.getByText("Whisper Small")).toBeTruthy();
    expect(screen.queryByText("ggml-small.bin")).toBeNull();
    expect(screen.getByLabelText("Speed 4 of 5, Accuracy 1.5 of 5")).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Download Whisper Small" }),
    ).toBeTruthy();
    expect(screen.getByText("488 MB · Works for: Shortcut · Record button · Files")).toBeTruthy();

    fireEvent.click(
      screen.getByRole("button", { name: "About Whisper Small" }),
    );
    const dialog = screen.getByRole("dialog", { name: "Whisper Small" });
    expect(within(dialog).getByText("ggml-small.bin")).toBeTruthy();
    expect(within(dialog).getByText("488 MB")).toBeTruthy();
  });

  it("removes the download button immediately even when the installed-model refresh fails", async () => {
    apiMocks.listDownloadableModels.mockResolvedValue([{ ...asrModel, installed: false }]);
    let rejectRefresh!: (error: Error) => void;
    const onReload = vi.fn(() => new Promise<void>((_resolve, reject) => { rejectRefresh = reject; }));
    render(<Harness onSave={vi.fn()} onReload={onReload} />);
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    fireEvent.click(screen.getByRole("button", { name: /Download engines/ }));
    await screen.findByRole("button", { name: "Download Whisper Small" });

    act(() => { downloadListener?.({ ...status("completed", 100), modelId: asrModel.id }); });
    expect(screen.queryByRole("button", { name: "Download Whisper Small" })).toBeNull();
    expect(screen.getByText("Installed")).toBeTruthy();
    expect(screen.getByText("1 of 1 installed")).toBeTruthy();

    await act(async () => { rejectRefresh(new Error("failed to read installed models")); });
    expect(screen.getByRole("alert").textContent).toContain("list could not be refreshed");
    expect(screen.getByRole("alert").textContent).toContain("failed to read installed models");
    expect(screen.queryByText("Downloaded")).toBeNull();
    expect(screen.queryByRole("button", { name: "Download Whisper Small" })).toBeNull();
  });

  it("does not replace a live completion with an older catalog or status snapshot", async () => {
    let resolveCatalog!: (models: DownloadableModel[]) => void;
    let resolveStatuses!: (statuses: ModelDownloadStatus[]) => void;
    apiMocks.listDownloadableModels
      .mockImplementationOnce(() => new Promise<DownloadableModel[]>((resolve) => { resolveCatalog = resolve; }))
      .mockResolvedValue([{ ...asrModel, installed: true }]);
    apiMocks.getModelDownloadStatuses.mockImplementation(() =>
      new Promise<ModelDownloadStatus[]>((resolve) => { resolveStatuses = resolve; }),
    );
    const onReload = vi.fn().mockResolvedValue(undefined);
    render(<Harness onSave={vi.fn()} onReload={onReload} />);
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    fireEvent.click(screen.getByRole("button", { name: /Download engines/ }));
    await waitFor(() => expect(apiMocks.getModelDownloadStatuses).toHaveBeenCalledTimes(1));
    await act(async () => { downloadListener?.({ ...status("completed", 100), modelId: asrModel.id }); });
    expect(screen.getByText("Installed")).toBeTruthy();

    await act(async () => {
      resolveCatalog([{ ...asrModel, installed: false }]);
      resolveStatuses([{ ...status("downloading", 42), modelId: asrModel.id }]);
    });
    expect(screen.getByText("1 of 1 installed")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Cancel download|Download Whisper Small/ })).toBeNull();
    expect(screen.queryByText("42%")).toBeNull();
    expect(onReload).toHaveBeenCalledTimes(1);
  });

  it("cleans up a subscription that finishes registering after the Models view unmounts", async () => {
    let resolveSubscription!: (cleanup: () => void) => void;
    apiMocks.listenModelDownloadStatus.mockImplementation(() =>
      new Promise<() => void>((resolve) => { resolveSubscription = resolve; }),
    );
    const cleanup = vi.fn();
    const { unmount } = render(<Harness onSave={vi.fn()} onReload={vi.fn()} />);
    unmount();
    await act(async () => { resolveSubscription(cleanup); });
    expect(cleanup).toHaveBeenCalledTimes(1);
    expect(apiMocks.getModelDownloadStatuses).not.toHaveBeenCalled();
  });

  it("offers a retry without turning off the desired setting", async () => {
    render(
      <Harness
        onSave={vi.fn()}
        onReload={vi.fn().mockResolvedValue(undefined)}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    const row = await screen.findByText("Speaker identification");
    fireEvent.click(
      within(row.closest(".setting-row") as HTMLElement).getByRole("button"),
    );

    await act(async () => {
      await downloadListener?.(status("failed", null));
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Retry speaker model download" }),
    );
    expect(apiMocks.startModelDownload).toHaveBeenCalledWith(
      diarizationModel.id,
    );
    expect(screen.getByText("Retry needed")).toBeTruthy();
  });

  it("turns the desired setting off while installation is in progress", async () => {
    const onSave = vi.fn();
    render(
      <Harness
        onSave={onSave}
        onReload={vi.fn().mockResolvedValue(undefined)}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Engines" }));
    const row = await screen.findByText("Speaker identification");
    const settingRow = row.closest(".setting-row") as HTMLElement;
    const switchButton = within(settingRow).getByRole("button");
    await waitFor(() => expect(switchButton).toHaveProperty("disabled", false));
    fireEvent.click(switchButton);

    await act(async () => {
      await downloadListener?.(status("downloading", 42));
    });
    fireEvent.click(switchButton);

    await waitFor(() => {
      expect(onSave).toHaveBeenLastCalledWith({
        fileDiarizationEnabled: false,
      });
      expect(within(settingRow).getByText("Off")).toBeTruthy();
      expect(
        screen.queryByText("Installing the speaker model — 42%"),
      ).toBeNull();
    });
  });
});
