import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ReviewJobStatus } from "../types/domain";
const mocks = vi.hoisted(() => ({
  get: vi.fn(),
  listener: null as null | ((job: ReviewJobStatus) => void),
}));
vi.mock("../lib/reviewApi", async (original) => ({
  ...(await original<typeof import("../lib/reviewApi")>()),
  getReviewJobs: mocks.get,
  listenReviewJobs: async (fn: (job: ReviewJobStatus) => void) => {
    mocks.listener = fn;
    return () => {};
  },
}));
import { useReviewJobs } from "./useReviewJobs";
const job = (
  stage: ReviewJobStatus["stage"],
  time: number,
): ReviewJobStatus => ({
  jobId: "retry",
  reference: { kind: "saved", id: "transcript" },
  stage,
  statusText: stage,
  error: null,
  resultRevision: null,
  startedAtMs: 1,
  updatedAtMs: time,
});
beforeEach(() => {
  mocks.get.mockReset().mockResolvedValue([]);
});
afterEach(() => vi.useRealTimers());
it("restores backend retries on mount and ignores older events after completion", async () => {
  mocks.get.mockResolvedValue([job("diarizing", 2)]);
  const { result } = renderHook(() => useReviewJobs());
  await waitFor(() => expect(result.current.jobs[0]?.stage).toBe("diarizing"));
  act(() => mocks.listener?.(job("completed", 4)));
  act(() => mocks.listener?.(job("diarizing", 3)));
  expect(result.current.jobs[0].stage).toBe("completed");
});
it("does not overlap fallback polls while a snapshot is still pending", async () => {
  vi.useFakeTimers();
  mocks.get
    .mockResolvedValueOnce([job("diarizing", 2)])
    .mockImplementation(() => new Promise(() => {}));
  renderHook(() => useReviewJobs());
  await act(async () => {});
  const initial = mocks.get.mock.calls.length;
  await act(async () => {
    vi.advanceTimersByTime(12000);
  });
  expect(mocks.get.mock.calls.length).toBe(initial);
});
