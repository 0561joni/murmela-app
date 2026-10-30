import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TextTranslationEvent, TextTranslationRequest, TranslatorStatus } from "../lib/textTranslation";
import type { AppSettings } from "../types/domain";
import type { AppDictationUpdate } from "../lib/api";

const mocks = vi.hoisted(() => ({
  listener: null as null | ((event: TextTranslationEvent) => void),
  status: null as unknown as TranslatorStatus,
  translateText: vi.fn(),
  saveTextTranslation: vi.fn(),
  cancelTextTranslation: vi.fn(),
  copyTextToClipboard: vi.fn(),
  dictationUpdate: null as null | ((update: AppDictationUpdate) => void),
  startAppDictation: vi.fn(),
  finishAppDictation: vi.fn(),
  recentCopy: vi.fn(async (): Promise<string | null> => null),
  insertTarget: vi.fn(async (): Promise<string | null> => null),
  insert: vi.fn(async (): Promise<"replacing" | "needs_accessibility"> => "replacing"),
}));

vi.mock("../lib/quickTranslateApi", async () => {
  const actual = await vi.importActual<typeof import("../lib/quickTranslateApi")>("../lib/quickTranslateApi");
  return { ...actual, recentCopy: mocks.recentCopy, insertTargetName: mocks.insertTarget, insertIntoLastApp: mocks.insert };
});

vi.mock("../lib/textTranslation", async () => {
  const actual = await vi.importActual<typeof import("../lib/textTranslation")>("../lib/textTranslation");
  return {
    ...actual,
    getTranslatorStatus: vi.fn(async () => mocks.status),
    detectTextLanguage: vi.fn(async (text: string) => (/\b(und|danke|wir)\b/i.test(text) ? { code: "de", reliable: true } : { code: "en", reliable: true })),
    translateText: mocks.translateText,
    cancelTextTranslation: mocks.cancelTextTranslation,
    saveTextTranslation: mocks.saveTextTranslation,
    listenTextTranslation: vi.fn(async (callback: (event: TextTranslationEvent) => void) => {
      mocks.listener = callback;
      return () => {
        mocks.listener = null;
      };
    }),
  };
});

vi.mock("../lib/api", async () => {
  const actual = await vi.importActual<typeof import("../lib/api")>("../lib/api");
  return {
    ...actual,
    copyTextToClipboard: mocks.copyTextToClipboard,
    startAppDictation: mocks.startAppDictation,
    listDownloadableModels: vi.fn(async () => [
      { id: "translategemma-12b-q6-k", engine: "llama.cpp-translation", modelName: "TranslateGemma 12B", description: "", sizeBytes: 9_660_827_392, profile: "accurate", availability: "available", availabilityReason: null, installed: false, requirements: "16 GB", artifactCount: 1, capability: "translation" },
      { id: "translategemma-4b-q6-k", engine: "llama.cpp-translation", modelName: "TranslateGemma 4B", description: "", sizeBytes: 3_190_755_840, profile: "fast", availability: "available", availabilityReason: null, installed: false, requirements: "8 GB", artifactCount: 1, capability: "translation" },
    ]),
    getModelDownloadStatuses: vi.fn(async () => []),
    listenModelDownloadStatus: vi.fn(async () => () => {}),
  };
});

const { TranslateScreen } = await import("./TranslateScreen");
const { clearTranslationCache } = await import("../lib/textTranslation");

const readyStatus: TranslatorStatus = {
  supported: true,
  modelId: "translategemma-12b-q6-k",
  modelName: "TranslateGemma 12B",
  installedModelIds: ["translategemma-12b-q6-k"],
  loaded: true,
  memoryAvailableBytes: 16e9,
  memoryNeededBytes: 12.8e9,
  languages: [],
};

/** Answers each request like the backend: one event per segment, then the outcome. */
function translateLikeBackend(request: TextTranslationRequest) {
  for (const segment of request.segments) {
    mocks.listener?.({ kind: "segment", requestId: request.requestId, index: segment.index, text: `EN:${segment.text}`, issues: [] });
  }
  return Promise.resolve({ requestId: request.requestId, modelId: readyStatus.modelId!, status: "completed", translated: request.segments.length, failed: 0, promptVersion: 5 });
}

function renderScreen() {
  return render(
    <TranslateScreen
      settings={{ saveHistory: true } as AppSettings}
      transcripts={[]}
      openRequest={null}
      dragging={false}
      onOpenSettings={() => {}}
      onLibraryChanged={() => {}}
    />,
  );
}

function type(text: string) {
  fireEvent.change(screen.getByLabelText("Text to translate"), { target: { value: text } });
}

describe("TranslateScreen", () => {
  beforeEach(() => {
    clearTranslationCache();
    window.localStorage.clear();
    mocks.status = readyStatus;
    mocks.translateText.mockReset().mockImplementation(translateLikeBackend);
    mocks.saveTextTranslation.mockReset().mockResolvedValue(null);
    mocks.cancelTextTranslation.mockReset().mockResolvedValue(undefined);
    mocks.copyTextToClipboard.mockReset().mockResolvedValue(undefined);
    mocks.dictationUpdate = null;
    mocks.finishAppDictation.mockReset().mockResolvedValue(undefined);
    mocks.startAppDictation.mockReset().mockImplementation(async (onUpdate: (update: AppDictationUpdate) => void) => {
      mocks.dictationUpdate = onUpdate;
      return { finish: mocks.finishAppDictation, dispose: () => {} };
    });
  });

  it("dictates into the text box and translates the final text", async () => {
    renderScreen();
    await screen.findByText(/TranslateGemma 12B · ready/);
    type("Hallo und danke.");
    await screen.findByText("EN:Hallo und danke.");

    fireEvent.click(screen.getByRole("button", { name: "Dictate" }));
    const stop = await screen.findByRole("button", { name: "Stop dictating" });
    expect(stop.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByText("Listening · click the microphone to stop")).toBeTruthy();
    const box = screen.getByLabelText("Text to translate") as HTMLTextAreaElement;
    expect(box.readOnly).toBe(true);

    act(() => mocks.dictationUpdate?.({ kind: "live", text: "Wir sehen" }));
    expect(box.value).toBe("Hallo und danke. Wir sehen");
    act(() => mocks.dictationUpdate?.({ kind: "live", text: "Wir sehen uns und danke" }));
    expect(box.value).toBe("Hallo und danke. Wir sehen uns und danke");
    // Nothing is translated while the words are still changing.
    await new Promise((resolve) => setTimeout(resolve, 800));
    expect(mocks.translateText).toHaveBeenCalledTimes(1);

    fireEvent.click(stop);
    await waitFor(() => expect(mocks.finishAppDictation).toHaveBeenCalled());
    act(() => mocks.dictationUpdate?.({ kind: "finishing" }));
    act(() => mocks.dictationUpdate?.({ kind: "done", text: "Wir sehen uns, und danke." }));
    expect(box.value).toBe("Hallo und danke. Wir sehen uns, und danke.");
    expect(box.readOnly).toBe(false);
    await screen.findByText("EN:Hallo und danke. Wir sehen uns, und danke.");
    expect(screen.getByRole("button", { name: "Dictate" })).toBeTruthy();
  });

  it("explains why dictation could not start", async () => {
    mocks.startAppDictation.mockRejectedValueOnce("No default microphone is available");
    renderScreen();
    await screen.findByText(/TranslateGemma 12B · ready/);
    fireEvent.click(screen.getByRole("button", { name: "Dictate" }));
    await screen.findByText("No microphone found. Connect one, or choose it in Settings → Dictation.");
    expect(screen.getByRole("button", { name: "Dictate" })).toBeTruthy();
  });

  it("translates while typing, paragraph by paragraph, and re-translates only what changed", async () => {
    renderScreen();
    await screen.findByText(/TranslateGemma 12B · ready/);
    type("Hallo und danke.\n\nWir sehen uns.");
    await screen.findByText("EN:Wir sehen uns.");
    expect(screen.getByText("EN:Hallo und danke.")).toBeTruthy();
    expect(mocks.translateText).toHaveBeenCalledTimes(1);
    const first = mocks.translateText.mock.calls[0][0] as TextTranslationRequest;
    expect(first).toMatchObject({ sourceLanguage: "de", targetLanguage: "en", tone: "auto" });
    expect(first.segments).toEqual([{ index: 0, text: "Hallo und danke." }, { index: 1, text: "Wir sehen uns." }]);

    type("Hallo und danke.\n\nWir sehen uns morgen.");
    await screen.findByText("EN:Wir sehen uns morgen.");
    const second = mocks.translateText.mock.calls[1][0] as TextTranslationRequest;
    expect(second.segments).toEqual([{ index: 1, text: "Wir sehen uns morgen." }]);

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(mocks.copyTextToClipboard).toHaveBeenCalledWith("EN:Hallo und danke.\n\nEN:Wir sehen uns morgen."));
    await waitFor(() => expect(mocks.saveTextTranslation).toHaveBeenCalled(), { timeout: 3000 });
    expect(mocks.saveTextTranslation.mock.lastCall?.[0]).toMatchObject({
      sourceKind: "text",
      targetLanguage: "en",
      sourceLanguage: "de",
      outputText: "EN:Hallo und danke.\n\nEN:Wir sehen uns morgen.",
    });
  });

  it("follows a model chosen in Settings while the screen stays mounted", async () => {
    const compact = { ...readyStatus, modelId: "translategemma-4b-q6-k", modelName: "TranslateGemma 4B", installedModelIds: ["translategemma-12b-q6-k", "translategemma-4b-q6-k"] };
    mocks.status = compact;
    mocks.translateText.mockImplementation((request: TextTranslationRequest) =>
      translateLikeBackend(request).then((outcome) => ({ ...outcome, modelId: compact.modelId })));
    const props = { transcripts: [], openRequest: null, dragging: false, onOpenSettings: () => {}, onLibraryChanged: () => {} };
    const { rerender } = render(<TranslateScreen {...props} settings={{ saveHistory: true, translationModelId: compact.modelId } as AppSettings} />);
    await screen.findByText(/TranslateGemma 4B · ready/);
    type("Hallo und danke.");
    await screen.findByText("EN:Hallo und danke.");

    mocks.status = { ...compact, modelId: readyStatus.modelId, modelName: readyStatus.modelName, loaded: false };
    rerender(<TranslateScreen {...props} settings={{ saveHistory: true, translationModelId: readyStatus.modelId } as AppSettings} />);
    await screen.findByText(/TranslateGemma 12B · loads on first use/);
  });

  it("offers a way forward when memory is low", async () => {
    mocks.translateText.mockRejectedValueOnce("TRANSLATION_MEMORY_LOW: TranslateGemma 12B needs about 13 GB of free memory; 8.7 GB are free.");
    renderScreen();
    await screen.findByText(/TranslateGemma 12B · ready/);
    type("Hallo und danke.");
    await screen.findByText(/Not enough free memory/);
    expect(screen.getByText(/8.7 GB are free/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Translate anyway" }));
    await screen.findByText("EN:Hallo und danke.");
    expect(mocks.translateText.mock.lastCall?.[0]).toMatchObject({ allowLowMemory: true });
  });

  it("does not translate text that is already in the target language", async () => {
    renderScreen();
    await screen.findByText(/TranslateGemma 12B · ready/);
    type("We will see each other tomorrow.");
    await screen.findByText(/already in English/);
    await new Promise((resolve) => setTimeout(resolve, 800));
    expect(mocks.translateText).not.toHaveBeenCalled();
  });

  it("waits for ⌘↵ when live translation is off", async () => {
    window.localStorage.setItem("murmela-translate-live", "off");
    renderScreen();
    await screen.findByText(/TranslateGemma 12B · ready/);
    type("Hallo und danke.");
    await new Promise((resolve) => setTimeout(resolve, 800));
    expect(mocks.translateText).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByLabelText("Text to translate"), { key: "Enter", metaKey: true });
    await screen.findByText("EN:Hallo und danke.");
  });

  it("asks for a model download first and recommends the one that fits", async () => {
    mocks.status = { ...readyStatus, modelId: null, modelName: null, installedModelIds: [], loaded: false, memoryAvailableBytes: 8.7e9 };
    renderScreen();
    await screen.findByText("Download a translation model to start");
    const recommended = await screen.findByText("Recommended for this Mac");
    expect(recommended.closest("li")?.textContent).toContain("Compact");
  });

  it("explains when the computer cannot translate", async () => {
    mocks.status = { ...readyStatus, supported: false };
    renderScreen();
    await screen.findByText("Not available on this computer yet");
  });

  describe("coming back from another app", () => {
    function renderActive() {
      return render(
        <TranslateScreen
          settings={{ saveHistory: true } as AppSettings}
          transcripts={[]}
          openRequest={null}
          dragging={false}
          active
          onOpenSettings={() => {}}
          onLibraryChanged={() => {}}
        />,
      );
    }

    beforeEach(() => {
      mocks.recentCopy.mockReset().mockResolvedValue(null);
      mocks.insertTarget.mockReset().mockResolvedValue(null);
      mocks.insert.mockReset().mockResolvedValue("replacing");
    });

    it("fills an empty screen with what was just copied, and Clear empties it", async () => {
      mocks.recentCopy.mockResolvedValue("Hallo und danke.");
      renderActive();
      const box = screen.getByLabelText("Text to translate") as HTMLTextAreaElement;
      await waitFor(() => expect(box.value).toBe("Hallo und danke."));
      expect(mocks.recentCopy).toHaveBeenCalledWith(60);
      expect(screen.getByText(/From clipboard/)).toBeTruthy();
      await screen.findByText("EN:Hallo und danke.");
      fireEvent.click(screen.getByRole("button", { name: "Clear" }));
      expect(box.value).toBe("");
      expect(screen.queryByText(/From clipboard/)).toBeNull();
    });

    it("never replaces text that is already there", async () => {
      mocks.recentCopy.mockResolvedValue("Something copied");
      renderActive();
      type("Mein eigener Text.");
      await act(async () => {
        window.dispatchEvent(new Event("focus"));
      });
      expect((screen.getByLabelText("Text to translate") as HTMLTextAreaElement).value).toBe("Mein eigener Text.");
    });

    it("inserts the translation into the app used before Murmela", async () => {
      mocks.insertTarget.mockResolvedValue("Mail");
      renderActive();
      await screen.findByText(/TranslateGemma 12B · ready/);
      type("Hallo und danke.");
      await screen.findByText("EN:Hallo und danke.");
      fireEvent.click(await screen.findByRole("button", { name: "Insert into Mail" }));
      await waitFor(() => expect(mocks.insert).toHaveBeenCalledWith("EN:Hallo und danke."));

      mocks.insert.mockResolvedValueOnce("needs_accessibility");
      fireEvent.keyDown(screen.getByLabelText("Text to translate"), { key: "Enter", metaKey: true, shiftKey: true });
      expect(await screen.findByText("Why Murmela asks for Accessibility")).toBeTruthy();
    });
  });
});
