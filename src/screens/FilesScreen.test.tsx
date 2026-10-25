import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { FilesScreen } from "./FilesScreen";
import { fileFixture, resultFixture } from "../test/fixtures";
const props = () => ({
  items: [fileFixture],
  dragging: false,
  speakerCountHint: null,
  showSpeakerOptions: true,
  onSpeakerCountHintChange: vi.fn(),
  onDragChange: vi.fn(),
  onPick: vi.fn().mockResolvedValue(undefined),
  onDrop: vi.fn().mockResolvedValue(undefined),
  onCancel: vi.fn().mockResolvedValue(undefined),
  onRetry: vi.fn().mockResolvedValue(undefined),
  onReview: vi.fn(),
});
describe("File workspace", () => {
  it("explains speaker labels in plain words with an example", () => {
    render(<FilesScreen {...props()} />);
    const select = screen.getByLabelText("Who's speaking") as HTMLSelectElement;
    expect([...select.options].map((option) => option.text)).toEqual([
      "Count people automatically",
      "I know how many people",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "About speaker labels" }));
    expect(screen.getByRole("dialog", { name: "Speaker labels" }).textContent).toContain("Speaker 1 · 00:12");
  });
  it("opens published text by reference even before content hydration finishes", () => {
    const onReview = vi.fn();
    const item = {
      ...fileFixture,
      stage: "diarizing" as const,
      result: null,
      reviewRef: { kind: "saved" as const, id: "published" },
      resultRevision: 1,
    };
    render(<FilesScreen {...props()} items={[item]} onReview={onReview} />);
    fireEvent.click(screen.getByRole("button", { name: "Review transcript" }));
    expect(onReview).toHaveBeenCalledWith(item);
    expect(
      screen.getByRole("button", { name: "Stop identifying speakers" }),
    ).toBeTruthy();
    expect(screen.getByText("Saved to Library")).toBeTruthy();
  });
  it("keeps unknown progress indeterminate and supports cancellation", async () => {
    const current = props();
    render(<FilesScreen {...current} />);
    expect(screen.getByRole("progressbar").hasAttribute("aria-valuenow")).toBe(
      false,
    );
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() =>
      expect(current.onCancel).toHaveBeenCalledWith("file-1"),
    );
    expect(
      await screen.findByRole("button", { name: "Cancellation requested" }),
    ).toBeTruthy();
  });
  it("retains failed-file explanations and requests a retry for the same job", async () => {
    const current = props();
    render(
      <FilesScreen
        {...current}
        items={[
          {
            ...fileFixture,
            stage: "failed",
            errorMessage: "Source file moved",
          },
        ]}
      />,
    );
    expect(screen.getByRole("alert").textContent).toBe("Source file moved");
    fireEvent.click(
      screen.getByRole("button", { name: "Retry transcription" }),
    );
    await waitFor(() => expect(current.onRetry).toHaveBeenCalledWith("file-1"));
  });
  it("presents cancellation without an error and preserves unsaved results", () => {
    render(
      <FilesScreen
        {...props()}
        items={[
          {
            ...fileFixture,
            stage: "canceled",
            errorMessage: "Canceled by the user",
          },
          {
            ...fileFixture,
            id: "file-2",
            stage: "completed",
            result: {
              sourceFile: fileFixture.sourceFile,
              resolvedModel: null,
              result: resultFixture,
              savedTranscript: null,
            },
          },
        ]}
      />,
    );
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("Not saved to Library")).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Review transcript" }),
    ).toBeTruthy();
  });
  it("uses native progress when available and offers the finished transcript", () => {
    render(
      <FilesScreen
        {...props()}
        items={[
          {
            ...fileFixture,
            progressPercent: 42,
            result: {
              sourceFile: fileFixture.sourceFile,
              resolvedModel: null,
              result: resultFixture,
              savedTranscript: null,
            },
          },
        ]}
      />,
    );
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe(
      "42",
    );
    expect(screen.getByRole("button", { name: "Review transcript" })).toBeTruthy();
  });
});
