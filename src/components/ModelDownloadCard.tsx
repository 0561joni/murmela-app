import { useEffect, useRef, useState } from "react";
import { formatBytes, readableError } from "../lib/formatting";
import type { ModelDownloadStatus } from "../types/domain";
import { ActionButton, Progress } from "./Feedback";
import { AppIcon, IconButton } from "./IconButton";

interface Props {
  /** Download statuses to watch; only these models are shown. */
  downloads: ModelDownloadStatus[];
  onCancel: (modelId: string) => Promise<unknown>;
}

/**
 * Floating card that follows a model download wherever it was started, so the
 * person can keep working on the current screen. Shows progress, size, speed
 * and time left, and keeps a failure until dismissed. Completion is announced by
 * the app-wide "Model installed" toast.
 */
export function ModelDownloadCard({ downloads, onCancel }: Props) {
  const rate = useDownloadRate(downloads);
  const [dismissedFailure, setDismissedFailure] = useState<string | null>(null);
  const previous = useRef<Record<string, ModelDownloadStatus["state"]>>({});
  // Failures are shown only for downloads this card saw running, not stale ones.
  const [watched, setWatched] = useState<ReadonlySet<string>>(() => new Set());

  useEffect(() => {
    for (const entry of downloads) {
      const before = previous.current[entry.modelId];
      if (before !== entry.state && entry.state === "failed") setDismissedFailure(null);
      if (entry.state === "downloading" && !watched.has(entry.modelId)) {
        setWatched((current) => new Set(current).add(entry.modelId));
      }
      previous.current[entry.modelId] = entry.state;
    }
  }, [downloads, watched]);

  const active = downloads.find((entry) => entry.state === "downloading");
  const failed = downloads.find(
    (entry) => entry.state === "failed" && entry.modelId !== dismissedFailure && watched.has(entry.modelId),
  );

  if (active) {
    const percent = active.progressPercent;
    const sizeText = active.totalBytes
      ? `${formatBytes(active.downloadedBytes)} of ${formatBytes(active.totalBytes)}`
      : `${formatBytes(active.downloadedBytes)} downloaded`;
    const speed = rate[active.modelId];
    const remaining =
      speed && active.totalBytes ? (active.totalBytes - active.downloadedBytes) / speed : null;
    return (
      <aside className="download-card" aria-label={`Downloading ${active.modelName}`}>
        <div className="download-card-head">
          <span className="download-card-icon" aria-hidden="true">
            <AppIcon name="download" />
          </span>
          <div className="download-card-title">
            <strong>Downloading {active.modelName}</strong>
            <span role="status">
              {percent == null ? "Starting…" : `${Math.round(percent)}%`}
              {remaining != null && remaining > 1 ? ` · ${formatRemaining(remaining)}` : ""}
            </span>
          </div>
          <ActionButton variant="ghost" success="" action={() => onCancel(active.modelId)}>
            Cancel
          </ActionButton>
        </div>
        <Progress value={percent} label={`${active.modelName} download`} />
        <div className="download-card-meta">
          <span>{sizeText}</span>
          {speed ? <span>{formatBytes(speed)}/s</span> : null}
          {active.artifactCount > 1 && active.artifactIndex ? (
            <span>File {active.artifactIndex} of {active.artifactCount}</span>
          ) : null}
        </div>
      </aside>
    );
  }

  if (failed) {
    return (
      <aside className="download-card is-error" aria-label={`${failed.modelName} download failed`}>
        <div className="download-card-head">
          <span className="download-card-icon" aria-hidden="true">
            <AppIcon name="download" />
          </span>
          <div className="download-card-title">
            <strong>{failed.modelName} download stopped</strong>
            <span role="alert">
              {failed.errorMessage ? readableError(failed.errorMessage) : "Please try again."}
            </span>
          </div>
          <IconButton icon="xmark" size="compact" label="Dismiss" onClick={() => setDismissedFailure(failed.modelId)} />
        </div>
      </aside>
    );
  }

  return null;
}

/** Smoothed bytes per second for each downloading model. */
function useDownloadRate(downloads: ModelDownloadStatus[]) {
  const samples = useRef<Record<string, { at: number; bytes: number; rate: number | null }>>({});
  const [rate, setRate] = useState<Record<string, number>>({});
  useEffect(() => {
    const now = performance.now();
    let changed = false;
    const next: Record<string, number> = {};
    for (const entry of downloads) {
      if (entry.state !== "downloading") {
        delete samples.current[entry.modelId];
        continue;
      }
      const last = samples.current[entry.modelId];
      if (!last || entry.downloadedBytes < last.bytes) {
        samples.current[entry.modelId] = { at: now, bytes: entry.downloadedBytes, rate: null };
        continue;
      }
      const seconds = (now - last.at) / 1000;
      if (seconds < 0.5) {
        if (last.rate) next[entry.modelId] = last.rate;
        continue;
      }
      const instant = (entry.downloadedBytes - last.bytes) / seconds;
      const smoothed = last.rate == null ? instant : last.rate * 0.7 + instant * 0.3;
      samples.current[entry.modelId] = { at: now, bytes: entry.downloadedBytes, rate: smoothed };
      if (smoothed > 0) next[entry.modelId] = smoothed;
      changed = true;
    }
    if (changed || Object.keys(next).length !== Object.keys(rate).length) setRate(next);
    // `rate` is only compared, never a trigger.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [downloads]);
  return rate;
}

function formatRemaining(seconds: number) {
  if (seconds < 60) return "less than a minute left";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `about ${minutes} min left`;
  const hours = Math.floor(minutes / 60);
  return `about ${hours} h ${minutes % 60} min left`;
}
