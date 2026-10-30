import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TextTranslationEvent, TextTranslationRequest } from "./lib/textTranslation";
import type { QuickTranslateRequest } from "./lib/quickTranslateApi";

const mocks = vi.hoisted(() => ({
  listener: null as null | ((event: TextTranslationEvent) => void),
  current: null as unknown as QuickTranslateRequest,
  requests: [] as TextTranslationRequest[],
  finish: [] as (() => void)[],
  copy: vi.fn(async () => undefined),
  replace: vi.fn(async () => "replacing" as const),
  edit: vi.fn(async () => undefined),
  close: vi.fn(async () => undefined),
  remember: vi.fn(async () => undefined),
  pause: vi.fn(async () => undefined),
  cancel: vi.fn(async () => undefined),
  pro: true,
  saveHistory: false,
  save: vi.fn(async (document: unknown) => document),
  announce: vi.fn(async () => undefined),
  alternatives: vi.fn(async (_request: unknown) => [{ text: "Treffen", probability: 0.4 }]),
  rewrite: vi.fn(async () => ({ text: "DE: Could we move our Treffen?", issues: [] })),
}));

vi.mock("./hooks/useLicense", async () => {
  const actual = await vi.importActual<typeof import("./hooks/useLicense")>("./hooks/useLicense");
  return { ...actual, useLicense: () => ({ status: { pro: mocks.pro } }) };
});

vi.mock("./lib/api", async () => {
  const actual = await vi.importActual<typeof import("./lib/api")>("./lib/api");
  return { ...actual, getSettings: vi.fn(async () => ({ quickTranslateSaveHistory: mocks.saveHistory })) };
});

vi.mock("./lib/textTranslation", async () => {
  const actual = await vi.importActual<typeof import("./lib/textTranslation")>("./lib/textTranslation");
  return {
    ...actual,
    translateText: vi.fn(
      (request: TextTranslationRequest) =>
        new Promise((resolve) => {
          mocks.requests.push(request);
          mocks.finish.push(() => resolve({ requestId: request.requestId, status: "completed" }));
        }),
    ),
    cancelTextTranslation: mocks.cancel,
    saveTextTranslation: mocks.save,
    getTranslationAlternatives: mocks.alternatives,
    rewriteTranslation: mocks.rewrite,
    getTranslatorStatus: vi.fn(async () => ({ installedModelIds: ["translategemma-12b-q6-k", "translategemma-4b-q6-k"] })),
    listenTextTranslation: vi.fn(async (callback: (event: TextTranslationEvent) => void) => {
      mocks.listener = callback;
      return () => {
        mocks.listener = null;
      };
    }),
  };
});

vi.mock("./lib/quickTranslateApi", async () => {
  const actual = await vi.importActual<typeof import("./lib/quickTranslateApi")>("./lib/quickTranslateApi");
  return {
    ...actual,
    currentQuickTranslation: vi.fn(async () => mocks.current),
    listenQuickTranslation: vi.fn(async () => () => {}),
    closeQuickTranslation: mocks.close,
    resizeQuickTranslation: vi.fn(async () => undefined),
    copyQuickTranslation: mocks.copy,
    replaceWithQuickTranslation: mocks.replace,
    editQuickTranslation: mocks.edit,
    rememberQuickTarget: mocks.remember,
    pauseQuickTranslation: mocks.pause,
    announceLibraryChange: mocks.announce,
  };
});

import { QuickTranslateApp } from "./QuickTranslateApp";

/** Translates every segment of the latest request and finishes it. */
async function translateAll(prefix = "DE: ") {
  const request = mocks.requests[mocks.requests.length - 1];
  await act(async () => {
    for (const segment of request.segments) {
      mocks.listener?.({ kind: "segment", requestId: request.requestId, index: segment.index, text: prefix + segment.text, issues: [] });
    }
    mocks.finish[mocks.finish.length - 1]();
  });
}

describe("QuickTranslateApp", () => {
  beforeEach(() => {
    mocks.requests = [];
    mocks.finish = [];
    mocks.current = {
      id: "r1",
      text: "Could we move our session?\n\nThanks, Sarah",
      sourceLanguage: "en",
      targetLanguage: "de",
      myLanguage: "de",
      otherLanguage: "en",
      appName: "Mail",
      canReplace: true,
    };
    vi.clearAllMocks();
    mocks.pro = true;
    mocks.saveHistory = false;
    document.caretRangeFromPoint = undefined as never;
  });

  it("translates the selection and replaces it with ↩", async () => {
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    expect(mocks.requests[0]).toMatchObject({ sourceLanguage: "en", targetLanguage: "de" });
    expect(mocks.requests[0].segments.map((segment) => segment.text)).toEqual(["Could we move our session?", "Thanks, Sarah"]);
    expect(screen.getByText("English")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Replace/ })).toHaveProperty("disabled", true);

    await translateAll();
    expect(screen.getByText(/DE: Could we move our session\?/)).toBeTruthy();
    fireEvent.keyDown(window, { key: "Enter" });
    await waitFor(() => expect(mocks.replace).toHaveBeenCalledWith("DE: Could we move our session?\n\nDE: Thanks, Sarah"));
    expect(screen.getByText(/from Mail/)).toBeTruthy();
  });

  it("shows only finished paragraphs while translating", async () => {
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    const request = mocks.requests[0];
    await act(async () => {
      mocks.listener?.({ kind: "segment", requestId: request.requestId, index: 0, text: "Erster Absatz", issues: [] });
    });
    expect(screen.getByText("Erster Absatz")).toBeTruthy();
    expect(screen.queryByText(/Thanks, Sarah/)).toBeNull();
  });

  it("copies with ⌘C and closes; Esc closes", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    await translateAll();
    fireEvent.keyDown(window, { key: "c", metaKey: true });
    await waitFor(() => expect(mocks.copy).toHaveBeenCalledWith("DE: Could we move our session?\n\nDE: Thanks, Sarah"));
    expect(screen.getByText("Copied")).toBeTruthy();
    await act(async () => {
      vi.advanceTimersByTime(700);
    });
    expect(mocks.close).toHaveBeenCalled();
    vi.useRealTimers();

    mocks.close.mockClear();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(mocks.close).toHaveBeenCalled();
  });

  it("explains Accessibility when Replace needs it", async () => {
    mocks.replace.mockResolvedValueOnce("needs_accessibility" as never);
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    await translateAll();
    fireEvent.click(screen.getByRole("button", { name: /Replace/ }));
    expect(await screen.findByText("One step before Replace")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Allow in System Settings" })).toBeTruthy();
  });

  it("picking another language translates again and remembers it", async () => {
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: /Change language/ }));
    const options = screen.getAllByRole("option");
    expect(options[0].textContent).toContain("Your language");
    fireEvent.click(screen.getByRole("button", { name: "French" }));
    await waitFor(() => expect(mocks.requests).toHaveLength(2));
    expect(mocks.requests[1].targetLanguage).toBe("fr");
    expect(mocks.remember).toHaveBeenCalledWith("fr");
    expect(mocks.cancel).toHaveBeenCalledWith(mocks.requests[0].requestId);
  });

  it("offers Copy, not Replace, when there is no app to put it into", async () => {
    mocks.current = { ...mocks.current, canReplace: false, appName: null };
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    await translateAll();
    expect(screen.queryByRole("button", { name: /Replace/ })).toBeNull();
    fireEvent.keyDown(window, { key: "Enter" });
    await waitFor(() => expect(mocks.copy).toHaveBeenCalled());
  });

  it("never shows the source text as a result when translation fails", async () => {
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    const translate = (await import("./lib/textTranslation")).translateText as unknown as ReturnType<typeof vi.fn>;
    expect(translate).toHaveBeenCalledTimes(1);
    await act(async () => {
      // A failure the panel cannot recover from.
      mocks.finish.pop();
    });
    translate.mockImplementationOnce(async () => {
      throw new Error("TRANSLATION_HELPER_FAILED: boom");
    });
    fireEvent.click(screen.getByRole("button", { name: /Change language/ }));
    fireEvent.click(screen.getByRole("button", { name: "French" }));
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.queryByText(/Could we move our session/)).toBeNull();
    expect(screen.getByRole("button", { name: /Replace/ })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: /Copy/ })).toHaveProperty("disabled", true);
  });

  it("falls back to the compact engine when the large one doesn't fit in memory", async () => {
    const translate = (await import("./lib/textTranslation")).translateText as unknown as ReturnType<typeof vi.fn>;
    translate.mockImplementationOnce(async () => {
      throw new Error("TRANSLATION_MEMORY_LOW: TranslateGemma 12B needs about 13 GB of free memory; 12.8 GB are free.");
    });
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    expect(mocks.requests[0].modelId).toBe("translategemma-4b-q6-k");
    await translateAll();
    expect(screen.getByText(/Used the compact engine/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /Replace/ })).toHaveProperty("disabled", false);
  });

  it("edits in Murmela and pauses", async () => {
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: /Edit in Murmela/ }));
    expect(mocks.edit).toHaveBeenCalledWith(mocks.current.text, "en", "de");
    fireEvent.click(screen.getByRole("button", { name: /Pause or turn off/ }));
    fireEvent.click(screen.getByRole("button", { name: "Pause for 1 hour" }));
    expect(mocks.pause).toHaveBeenCalledWith(60);
  });

  it("keeps the translation in the Library only when Settings asks for it", async () => {
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    await translateAll();
    await act(async () => undefined);
    expect(mocks.save).not.toHaveBeenCalled();

    mocks.saveHistory = true;
    fireEvent.click(screen.getByRole("button", { name: /Change language/ }));
    fireEvent.click(screen.getByRole("button", { name: "French" }));
    await waitFor(() => expect(mocks.requests).toHaveLength(2));
    await translateAll("FR: ");
    await waitFor(() => expect(mocks.save).toHaveBeenCalledTimes(1));
    expect(mocks.save.mock.calls[0][0]).toMatchObject({
      sourceText: mocks.current.text,
      outputText: "FR: Could we move our session?\n\nFR: Thanks, Sarah",
      targetLanguage: "fr",
      sourceKind: "text",
    });
    await waitFor(() => expect(mocks.announce).toHaveBeenCalled());
  });

  /** Points caretRangeFromPoint at `offset` characters into the clicked paragraph. */
  function clickWord(paragraph: HTMLElement, offset: number) {
    document.caretRangeFromPoint = (() => {
      const range = document.createRange();
      range.setStart(paragraph.firstChild!, offset);
      return range;
    }) as never;
    fireEvent.click(paragraph);
  }

  it("a clicked word offers alternatives and rewrites the paragraph (Pro)", async () => {
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    await translateAll();
    const paragraph = screen.getByText("DE: Could we move our session?");
    clickWord(paragraph, "DE: Could we move our ses".length);
    await waitFor(() => expect(mocks.alternatives).toHaveBeenCalled());
    expect(mocks.alternatives.mock.calls[0][0]).toMatchObject({ word: "session", prefix: "DE: Could we move our ", targetLanguage: "de" });
    fireEvent.click(await screen.findByRole("menuitem", { name: /Treffen/ }));
    expect(await screen.findByText("DE: Could we move our Treffen?")).toBeTruthy();
    expect(screen.getByText(/Replaced “session” with “Treffen”/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(screen.getByText("DE: Could we move our session?")).toBeTruthy();
  });

  it("explains that word alternatives are part of Pro", async () => {
    mocks.pro = false;
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    await translateAll();
    clickWord(screen.getByText("DE: Could we move our session?"), 8);
    expect(screen.getByText(/part of Murmela Pro/)).toBeTruthy();
    expect(mocks.alternatives).not.toHaveBeenCalled();
  });

  it("switches to your other language when the text was already yours", async () => {
    mocks.current = { ...mocks.current, text: "Hallo Sarah, Donnerstag Nachmittag passt mir gut.", sourceLanguage: null, targetLanguage: "de" };
    render(<QuickTranslateApp />);
    await waitFor(() => expect(mocks.requests).toHaveLength(1));
    await translateAll("");
    await waitFor(() => expect(mocks.requests).toHaveLength(2));
    expect(mocks.requests[1]).toMatchObject({ sourceLanguage: "de", targetLanguage: "en" });
    await translateAll("EN: ");
    expect(screen.getByText(/This is already German, so Murmela translated it into English/)).toBeTruthy();
    expect(mocks.requests).toHaveLength(2);
  });
});
