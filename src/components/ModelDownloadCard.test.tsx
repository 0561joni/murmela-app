import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ModelDownloadStatus } from "../types/domain";
import { ModelDownloadCard } from "./ModelDownloadCard";

const status = (patch: Partial<ModelDownloadStatus> = {}): ModelDownloadStatus => ({
  modelId: "translategemma-4b-q6-k",
  modelName: "TranslateGemma 4B",
  state: "downloading",
  downloadedBytes: 800_000_000,
  totalBytes: 3_200_000_000,
  progressPercent: 25,
  errorMessage: null,
  currentArtifact: null,
  artifactIndex: null,
  artifactCount: 1,
  ...patch,
});

describe("ModelDownloadCard", () => {
  it("shows progress and size for a running download and cancels it", async () => {
    const onCancel = vi.fn().mockResolvedValue(undefined);
    render(<ModelDownloadCard downloads={[status()]} onCancel={onCancel} />);
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("25");
    expect(screen.getByText("Downloading TranslateGemma 4B")).toBeTruthy();
    expect(screen.getByText("800 MB of 3.2 GB")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(onCancel).toHaveBeenCalledWith("translategemma-4b-q6-k"));
  });

  it("stays hidden when nothing is downloading", () => {
    const { container } = render(
      <ModelDownloadCard downloads={[status({ state: "completed", progressPercent: 100 })]} onCancel={vi.fn()} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("ignores old failures but keeps one it watched until dismissed", () => {
    const failed = status({ state: "failed", errorMessage: "Network unavailable" });
    const view = render(<ModelDownloadCard downloads={[failed]} onCancel={vi.fn()} />);
    expect(view.container.innerHTML).toBe("");

    view.rerender(<ModelDownloadCard downloads={[status()]} onCancel={vi.fn()} />);
    view.rerender(<ModelDownloadCard downloads={[failed]} onCancel={vi.fn()} />);
    expect(screen.getByRole("alert").textContent).toContain("Network unavailable");

    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(view.container.innerHTML).toBe("");
  });
});
