import { useCallback, useEffect, useState } from "react";
import {
  getReviewJobs,
  isReviewJobActive,
  listenReviewJobs,
} from "../lib/reviewApi";
import type { ReviewJobStatus } from "../types/domain";
export function useReviewJobs(enabled = true) {
  const [jobs, setJobs] = useState<ReviewJobStatus[]>([]);
  const accept = useCallback(
    (job: ReviewJobStatus) =>
      setJobs((current) => {
        const old = current.find((j) => j.jobId === job.jobId);
        if (old && old.updatedAtMs >= job.updatedAtMs) return current;
        return [...current.filter((j) => j.jobId !== job.jobId), job];
      }),
    [],
  );
  const active = jobs.some(isReviewJobActive);
  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let cleanup: (() => void) | undefined;
    let pending = false;
    const refresh = async () => {
      if (pending || disposed) return;
      pending = true;
      try {
        const next = await getReviewJobs();
        if (!disposed) next.forEach(accept);
      } catch {
        /* Reconnect on focus or the next fallback poll. */
      } finally {
        pending = false;
      }
    };
    void listenReviewJobs((job) => {
      if (!disposed) accept(job);
    })
      .then((fn) => {
        if (disposed) fn();
        else cleanup = fn;
      })
      .catch(() => {});
    void refresh();
    window.addEventListener("focus", refresh);
    const timer = active
      ? window.setInterval(() => void refresh(), 3000)
      : undefined;
    return () => {
      disposed = true;
      cleanup?.();
      window.removeEventListener("focus", refresh);
      if (timer) clearInterval(timer);
    };
  }, [enabled, active, accept]);
  return { jobs, accept };
}
