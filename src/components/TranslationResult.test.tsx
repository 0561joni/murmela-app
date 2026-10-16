import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TranslationResult } from "./TranslationResult";
import type { DictationOutput } from "../types/domain";

const copy = vi.hoisted(() => vi.fn());
vi.mock("../lib/api", () => ({ copyTextToClipboard: copy }));
const output: DictationOutput = {
  sessionId: "session", sourceText: "Kannst du morgen kommen?", outputText: "¿Podés venir mañana?",
  targetLanguage: "es-AR", status: "completed", modelId: "translategemma-12b-q6-k",
  modelRevision: "pinned", promptVersion: 1, transcriptId: null, errorMessage: null,
};

describe("Separate translation and original", () => {
  beforeEach(() => copy.mockReset().mockResolvedValue(undefined));
  it("defaults to the translation and copies each version independently", async () => {
    render(<TranslationResult output={output} />);
    expect(screen.getByText(output.outputText!)).toBeTruthy();
    expect(screen.queryByText(output.sourceText)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Copy translation" }));
    await waitFor(() => expect(copy).toHaveBeenLastCalledWith(output.outputText));
    fireEvent.click(screen.getByRole("button", { name: /^Original$/ }));
    expect(screen.getByText(output.sourceText)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Copy original" }));
    await waitFor(() => expect(copy).toHaveBeenLastCalledWith(output.sourceText));
  });
  it("retains the source on failure and retries without a clipboard side effect", async () => {
    const retry = vi.fn().mockResolvedValue(undefined);
    render(<TranslationResult output={{ ...output, outputText: null, status: "failed", errorMessage: "Runtime stopped" }} onRetry={retry} />);
    expect(screen.getByRole("alert").textContent).toContain("Runtime stopped");
    expect(screen.getByText(output.sourceText)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Copy translation" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Retry translation" }));
    await waitFor(() => expect(retry).toHaveBeenCalledTimes(1));
    expect(copy).not.toHaveBeenCalled();
  });
});
