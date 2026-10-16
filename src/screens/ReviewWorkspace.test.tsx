import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { reviewFixture } from "../test/reviewFixture";
import { fileFixture } from "../test/fixtures";
import type { ReviewDocument, ReviewRef } from "../types/domain";
const mocks = vi.hoisted(() => ({
  get: vi.fn(),
  edit: vi.fn(),
  start: vi.fn(),
  cancel: vi.fn(),
  copy: vi.fn(),
  export: vi.fn(),
  seek: vi.fn(),
  listener: null as null | ((ref: ReviewRef) => void),
  license: vi.fn(),
}));
vi.mock("../lib/api", async (original) => ({
  ...(await original<typeof import("../lib/api")>()),
  getLicenseStatus: mocks.license,
}));
vi.mock("../lib/reviewApi", async (original) => ({
  ...(await original<typeof import("../lib/reviewApi")>()),
  getReview: mocks.get,
  editReview: mocks.edit,
  startReviewJob: mocks.start,
  cancelReviewJob: mocks.cancel,
  copyReview: mocks.copy,
  exportReview: mocks.export,
  listenReviewUpdates: async (fn: (ref: ReviewRef) => void) => {
    mocks.listener = fn;
    return () => {
      mocks.listener = null;
    };
  },
}));
vi.mock("../components/ReviewPlayer", async () => {
  const React = await import("react");
  return {
    ReviewPlayer: React.forwardRef((_props, ref) => {
      React.useImperativeHandle(ref, () => ({ seek: mocks.seek }));
      return <div>Audio player</div>;
    }),
  };
});
import { ReviewWorkspace } from "./ReviewWorkspace";
import { resetLicenseCache } from "../hooks/useLicense";
let document: ReviewDocument;
const props = () => ({
  reference: document.reference,
  originLabel: "Files",
  onBack: vi.fn(),
  onUpdated: vi.fn(),
  jobs: [],
  onJobStarted: vi.fn(),
});
beforeEach(() => {
  resetLicenseCache();
  mocks.license.mockReset().mockResolvedValue({ pro: true, tier: "lifetime", updatesUntil: null, releaseDate: "2026-10-16", keyHint: "••••4D51", licensedTo: "Ada", activatedAt: "2026-10-14T12:00:00Z" });
  document = reviewFixture();
  mocks.get
    .mockReset()
    .mockImplementation(async () => structuredClone(document));
  mocks.edit.mockReset();
  mocks.copy.mockReset().mockResolvedValue(undefined);
  mocks.export.mockReset().mockResolvedValue({ path: null });
  mocks.start.mockReset();
  mocks.cancel.mockReset().mockResolvedValue(undefined);
  mocks.seek.mockReset();
});
describe("Shared transcript review", () => {
  it("exports the retained original after failure and accepts a translation update at the same review revision", async () => {
    document.detail.translation = {
      sessionId: "translation", sourceText: document.detail.plainText, outputText: null,
      targetLanguage: "fr", status: "failed", modelId: "translategemma-12b-q6-k",
      modelRevision: "pinned", promptVersion: 4, transcriptId: document.detail.id, errorMessage: "Runtime stopped",
    };
    render(<ReviewWorkspace {...props()} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    expect(screen.getByRole("combobox", { name: "Export text version" })).toHaveProperty("value", "plain");
    fireEvent.click(screen.getByRole("button", { name: /^Export$/ }));
    await waitFor(() => expect(mocks.export).toHaveBeenCalledWith(document.reference, "txt", undefined));
    document.detail.translation = { ...document.detail.translation, status: "completed", outputText: "Bonjour, voici la traduction.", errorMessage: null };
    act(() => mocks.listener?.(document.reference));
    await screen.findByText("Bonjour, voici la traduction.");
    expect(screen.getByRole("combobox", { name: "Export text version" })).toHaveProperty("value", "translation");
  });
  it("reuses the latest failed retry's speaker-count selection", async () => {
    const failed = {
      jobId: "last-retry",
      reference: document.reference,
      stage: "failed" as const,
      statusText: "The previous result was kept.",
      error: null,
      resultRevision: null,
      speakerCount: 4,
      startedAtMs: 2,
      updatedAtMs: 3,
    };
    render(<ReviewWorkspace {...props()} jobs={[failed]} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    fireEvent.click(screen.getByRole("button", { name: "Identify again" }));
    expect(screen.getByLabelText("Exact number of speakers")).toHaveProperty(
      "value",
      "4",
    );
  });
  it("opens from either origin and leaves text usable while speakers run", async () => {
    const p = props();
    const stop = vi.fn().mockResolvedValue(undefined);
    render(
      <ReviewWorkspace
        {...p}
        initialJob={{ ...fileFixture, stage: "diarizing", resultRevision: 1 }}
        onStopInitial={stop}
      />,
    );
    await screen.findByRole("heading", { name: "Weekly planning" });
    expect(
      screen
        .getByRole("button", { name: "Copy text" })
        .hasAttribute("disabled"),
    ).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Copy text" }));
    await waitFor(() =>
      expect(mocks.copy).toHaveBeenCalledWith(document.reference, "plain"),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Stop identifying speakers" }),
    );
    await waitFor(() => expect(stop).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "Back to Files" }));
    expect(p.onBack).toHaveBeenCalledOnce();
  });
  it("assigns existing passages without changing text and exposes undo", async () => {
    const p = props();
    render(<ReviewWorkspace {...p} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Select passage at 00:00" }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Select passage at 00:06" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Assign speakers" }));
    const dialog = screen.getByRole("dialog", {
      name: "Assign selected passages",
    });
    fireEvent.change(
      within(dialog).getByLabelText("Speaker", { exact: true }),
      { target: { value: "b" } },
    );
    const next = structuredClone(document);
    next.revision = 2;
    next.canUndo = true;
    next.manualSegmentIds = ["passage-0", "passage-1"];
    mocks.edit.mockResolvedValue(next);
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Save correction" }),
    );
    await waitFor(() =>
      expect(mocks.edit).toHaveBeenCalledWith(document.reference, 1, {
        type: "assign",
        segmentIds: ["passage-0", "passage-1"],
        speakerIds: ["b"],
        newSpeakerName: null,
      }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(
      screen
        .getByRole("button", { name: "Undo correction" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(screen.getAllByText("Manually assigned")).toHaveLength(2);
  });
  it("supports explicit overlap and new speaker assignments", async () => {
    render(<ReviewWorkspace {...props()} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    fireEvent.click(
      screen.getByRole("button", {
        name: "Change speaker for passage at 00:06",
      }),
    );
    let dialog = screen.getByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Assignment"), {
      target: { value: "overlap" },
    });
    expect(
      within(dialog)
        .getByRole("button", { name: "Save correction" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "Maya" }));
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "Leo" }));
    mocks.edit.mockResolvedValue({ ...document, revision: 2 });
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Save correction" }),
    );
    await waitFor(() =>
      expect(mocks.edit).toHaveBeenCalledWith(
        document.reference,
        1,
        expect.objectContaining({ speakerIds: ["a", "b"] }),
      ),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    fireEvent.click(
      screen.getByRole("button", {
        name: "Change speaker for passage at 00:00",
      }),
    );
    dialog = screen.getByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Assignment"), {
      target: { value: "new" },
    });
    fireEvent.change(within(dialog).getByLabelText("New speaker name"), {
      target: { value: "Nora" },
    });
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Save correction" }),
    );
    await waitFor(() =>
      expect(mocks.edit).toHaveBeenLastCalledWith(
        document.reference,
        2,
        expect.objectContaining({ speakerIds: [], newSpeakerName: "Nora" }),
      ),
    );
  });
  it("reuses the exact count and preserves corrections by default on retry", async () => {
    const p = props();
    mocks.start.mockResolvedValue({
      jobId: "retry",
      reference: document.reference,
      stage: "queued",
    });
    render(<ReviewWorkspace {...p} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    fireEvent.click(screen.getByRole("button", { name: "Identify again" }));
    const dialog = screen.getByRole("dialog");
    expect(
      within(dialog).getByLabelText("Exact number of speakers"),
    ).toHaveProperty("value", "2");
    expect(within(dialog).getByRole("checkbox")).toHaveProperty(
      "checked",
      false,
    );
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Start identification" }),
    );
    await waitFor(() =>
      expect(mocks.start).toHaveBeenCalledWith(document.reference, 2, false),
    );
    expect(p.onJobStarted).toHaveBeenCalled();
  });
  it("restores queued retries on remount and can cancel them", async () => {
    const job = {
      jobId: "retry",
      reference: document.reference,
      stage: "queued" as const,
      statusText: "Waiting for local file processing…",
      error: null,
      resultRevision: null,
      startedAtMs: Date.now(),
      updatedAtMs: Date.now(),
    };
    const first = render(<ReviewWorkspace {...props()} jobs={[job]} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    first.unmount();
    render(<ReviewWorkspace {...props()} jobs={[job]} originLabel="Library" />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    expect(screen.getByText("Speaker retry queued")).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Stop identifying speakers" }),
    );
    await waitFor(() => expect(mocks.cancel).toHaveBeenCalledWith("retry"));
  });
  it("filters uncertain passages and connects timestamp clicks to audio", async () => {
    render(<ReviewWorkspace {...props()} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    fireEvent.click(screen.getByRole("checkbox", { name: "Needs review (1)" }));
    expect(
      screen.queryByRole("button", { name: "Play from 00:00" }),
    ).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Play from 00:06" }));
    expect(mocks.seek).toHaveBeenCalledWith(6000);
  });
  it("keeps newer edits when an older refresh response arrives late", async () => {
    render(<ReviewWorkspace {...props()} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    let resolve!: (d: ReviewDocument) => void;
    mocks.get.mockReturnValueOnce(
      new Promise<ReviewDocument>((r) => {
        resolve = r;
      }),
    );
    act(() => mocks.listener?.(document.reference));
    fireEvent.click(screen.getByRole("button", { name: "Rename Maya" }));
    const dialog = screen.getByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Speaker name"), {
      target: { value: "Maya Chen" },
    });
    const next = structuredClone(document);
    next.revision = 2;
    next.detail.speakers[0].displayName = "Maya Chen";
    mocks.edit.mockResolvedValue(next);
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Save correction" }),
    );
    await screen.findByRole("button", { name: "Rename Maya Chen" });
    await act(async () => resolve(document));
    expect(
      screen.getByRole("button", { name: "Rename Maya Chen" }),
    ).toBeTruthy();
  });
  it("preserves session-only results and closes editors with restored focus", async () => {
    document.reference = { kind: "session", id: "session" };
    render(<ReviewWorkspace {...props()} />);
    await screen.findByRole("heading", { name: "Weekly planning" });
    expect(screen.getByText(/Session only/)).toBeTruthy();
    const button = screen.getByRole("button", { name: "Rename Maya" });
    button.focus();
    fireEvent.click(button);
    const dialog = screen.getByRole("dialog");
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    await waitFor(() => expect(window.document.activeElement).toBe(button));
  });
});

describe("Speaker identification on the free version", () => {
  it("opens Settings → License instead of starting a job", async () => {
    mocks.license.mockResolvedValue({ pro: false, tier: null, updatesUntil: null, releaseDate: "2026-10-16", keyHint: null, licensedTo: null, activatedAt: null });
    const opened = vi.fn();
    window.addEventListener("murmela:open-license", opened);
    render(<ReviewWorkspace {...props()} />);
    const button = await screen.findByRole("button", { name: /Identify again\s*Pro/ });
    fireEvent.click(button);
    expect(opened).toHaveBeenCalledTimes(1);
    expect(mocks.start).not.toHaveBeenCalled();
    window.removeEventListener("murmela:open-license", opened);
  });
});
