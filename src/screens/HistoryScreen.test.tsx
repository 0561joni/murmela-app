import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TranscriptSummary } from "../types/domain";

const apiMocks = vi.hoisted(() => ({
  listTranscripts: vi.fn(),
}));

vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  listTranscripts: apiMocks.listTranscripts,
}));

import { HistoryScreen } from "./HistoryScreen";

const transcript: TranscriptSummary = {
  id: "transcript-1",
  createdAt: "2026-08-16T00:00:00Z",
  sourceType: "file_upload",
  title: "Meeting",
  plainText: "Meeting transcript",
  status: "completed",
  detectedLanguages: ["en"],
  durationMs: 1_000,
  modelName: "Local model",
  qualityStatus: "clean",
  recoveredRegionCount: 0,
  diarizationStatus: "completed",
  speakerCount: 2,
};

describe("HistoryScreen", () => {
  beforeEach(() => {
    apiMocks.listTranscripts.mockReset().mockResolvedValue([transcript]);
  });

  it("searches through the native API and filters by workflow", async () => {
    const dictation = {
      ...transcript,
      id: "dictation-1",
      title: "Voice note",
      sourceType: "quick_dictate" as const,
    };
    render(
      <HistoryScreen
        transcripts={[transcript, dictation]}
        onReview={vi.fn()}
        onDeleteAll={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Dictation" }));
    expect(
      screen.queryByRole("button", { name: "Open transcript Meeting" }),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: "Open transcript Voice note" }),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    fireEvent.change(
      screen.getByRole("searchbox", { name: "Search transcripts" }),
      { target: { value: "Meeting" } },
    );
    await waitFor(() =>
      expect(apiMocks.listTranscripts).toHaveBeenCalledWith("Meeting"),
    );
    expect(
      screen.queryByRole("button", { name: "Open transcript Voice note" }),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: "Open transcript Meeting" }),
    ).toBeTruthy();
  });

  it("opens a transcript on the Review screen", () => {
    const onReview = vi.fn();
    render(
      <HistoryScreen
        transcripts={[transcript]}
        onReview={onReview}
        onDeleteAll={vi.fn()}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Open transcript Meeting" }),
    );
    expect(onReview).toHaveBeenCalledWith("transcript-1");
  });
});
