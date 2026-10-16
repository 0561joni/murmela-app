import { invoke } from "@tauri-apps/api/core";
import {
  getTranscript,
  renameTranscript,
  getFileTranscriptionStatuses,
  copyTextToClipboard,
} from "./api";
import { speakerLabel, speakerMap } from "./speakerLabels";
import type {
  ReviewAudio,
  ReviewDocument,
  ReviewEdit,
  ReviewJobStatus,
  ReviewRef,
  TranscriptCopyVariant,
  TranscriptExportFormat,
  TranscriptExportResult,
} from "../types/domain";
const native = () => "__TAURI_INTERNALS__" in window;
export const reviewKey = (ref: ReviewRef) => `${ref.kind}:${ref.id}`;
export class ReviewApiError extends Error {
  constructor(
    public code: string,
    message: string,
  ) {
    super(message);
  }
}
async function command<T>(
  name: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (e) {
    if (typeof e === "object" && e && "message" in e)
      throw new ReviewApiError(
        "code" in e ? String(e.code) : "REVIEW_ERROR",
        String(e.message),
      );
    throw new Error(String(e));
  }
}
const previews = new Map<string, ReviewDocument>();
const previewUndo = new Map<string, ReviewDocument[]>();
export async function getReview(reference: ReviewRef): Promise<ReviewDocument> {
  if (native()) return command("get_review", { reference });
  const cached = previews.get(reviewKey(reference));
  if (cached) return structuredClone(cached);
  let detail;
  if (reference.kind === "saved") detail = await getTranscript(reference.id);
  else {
    const item = (await getFileTranscriptionStatuses()).find(
      (s) => s.jobId === reference.id,
    )?.result;
    if (!item) throw new Error("This session is no longer available.");
    detail = {
      ...item.result,
      id: reference.id,
      createdAt: new Date().toISOString(),
      title: item.sourceFile.originalName,
      sourceType: "file_upload" as const,
      status: "completed" as const,
      durationMs: item.sourceFile.durationMs,
      speakerCount: item.result.speakers.length || null,
      transcriptionWarnings: item.result.warnings,
    };
  }
  const document = {
    reference,
    detail,
    revision: 1,
    manualSegmentIds: [],
    unmatchedSpeakerIds: [],
    canUndo: false,
  };
  previews.set(reviewKey(reference), document);
  return structuredClone(document);
}
export async function renameReviewTitle(reference: ReviewRef, title: string) {
  const summary = await renameTranscript(reference.id, title);
  const cached = previews.get(reviewKey(reference));
  if (!native() && cached) {
    cached.detail.title = summary.title;
    cached.revision++;
  }
  return summary;
}
export async function editReview(
  reference: ReviewRef,
  expectedRevision: number,
  edit: ReviewEdit,
): Promise<ReviewDocument> {
  if (native())
    return command("edit_review", { reference, expectedRevision, edit });
  const document = await getReview(reference);
  const key = reviewKey(reference);
  if (document.revision !== expectedRevision)
    throw new ReviewApiError(
      "REVIEW_CONFLICT",
      "This transcript changed. Try again.",
    );
  const undo = previewUndo.get(key) ?? [];
  if (edit.type === "undo") {
    const previous = undo.pop();
    if (!previous) throw new Error("Nothing to undo.");
    previous.revision = document.revision + 1;
    previous.canUndo = undo.length > 0;
    previews.set(key, previous);
    previewUndo.set(key, undo);
    return structuredClone(previous);
  }
  undo.push(structuredClone(document));
  if (undo.length > 20) undo.shift();
  previewUndo.set(key, undo);
  const add = (name: string) => {
    if (!name.trim() || name.trim().length > 80)
      throw new Error("Enter a speaker name with 1–80 characters.");
    const id = crypto.randomUUID();
    document.detail.speakers.push({
      speakerId: id,
      displayName: name.trim(),
      speakerOrder: document.detail.speakers.length,
    });
    return id;
  };
  if (edit.type === "rename") {
    const s = document.detail.speakers.find(
      (s) => s.speakerId === edit.speakerId,
    );
    if (s) s.displayName = edit.name.trim();
  }
  if (edit.type === "add_speaker") add(edit.name);
  if (edit.type === "assign") {
    const ids = [...edit.speakerIds];
    if (edit.newSpeakerName) ids.push(add(edit.newSpeakerName));
    document.detail.segments = document.detail.segments.map((s) =>
      edit.segmentIds.includes(s.id)
        ? {
            ...s,
            speakerId: ids.length === 1 ? ids[0] : null,
            speakerIds: ids.length ? ids : null,
            speakerConfidence: null,
            speakerAttribution:
              ids.length > 1 ? "overlap" : ids.length ? "assigned" : "none",
          }
        : s,
    );
    document.manualSegmentIds = [
      ...new Set([...document.manualSegmentIds, ...edit.segmentIds]),
    ];
  }
  if (edit.type === "merge") {
    const sources = new Set(
      edit.speakerIds.filter((id) => id !== edit.targetId),
    );
    for (const s of document.detail.segments) {
      const ids = [
        ...new Set([
          ...(s.speakerIds ?? []),
          ...(s.speakerId ? [s.speakerId] : []),
        ]),
      ];
      if (ids.some((id) => sources.has(id))) {
        const merged = [
          ...new Set(ids.map((id) => (sources.has(id) ? edit.targetId : id))),
        ];
        s.speakerIds = merged;
        s.speakerId = merged.length === 1 ? merged[0] : null;
        s.speakerAttribution = merged.length === 1 ? "assigned" : "overlap";
        document.manualSegmentIds.push(s.id);
      }
    }
    document.detail.speakers = document.detail.speakers.filter(
      (s) => !sources.has(s.speakerId),
    );
  }
  const active = new Set(
    document.detail.segments.flatMap((s) => [
      ...(s.speakerIds ?? []),
      ...(s.speakerId ? [s.speakerId] : []),
    ]),
  );
  document.detail.speakerCount = active.size || null;
  document.detail.manualSegmentIds = [...document.manualSegmentIds];
  document.revision++;
  document.canUndo = true;
  previews.set(key, document);
  return structuredClone(document);
}
export async function getReviewJobs(): Promise<ReviewJobStatus[]> {
  return native() ? command("get_review_job_statuses") : [];
}
export async function startReviewJob(
  reference: ReviewRef,
  speakerCount: number | null,
  reset: boolean,
): Promise<ReviewJobStatus> {
  if (!native())
    throw new Error("Speaker identification runs in the desktop app.");
  return command("start_review_job", { reference, speakerCount, reset });
}
export async function cancelReviewJob(jobId: string): Promise<void> {
  if (native()) await command("cancel_review_job", { jobId });
}
export async function resolveReviewAudio(
  reference: ReviewRef,
  replacementPath: string | null = null,
  fallback = false,
): Promise<ReviewAudio> {
  if (!native())
    throw new Error("Audio playback is available in the desktop app.");
  return command("resolve_review_audio", {
    reference,
    replacementPath,
    fallback,
  });
}
export async function releaseReviewAudio(token: string): Promise<void> {
  if (native()) await command("release_review_audio", { token });
}
export async function copyReview(
  reference: ReviewRef,
  variant: TranscriptCopyVariant,
): Promise<void> {
  if (native()) return command("copy_review", { reference, variant });
  const { detail, manualSegmentIds } = await getReview(reference);
  if (variant === "translation") {
    if (detail.translation?.status !== "completed" || detail.translation.outputText == null)
      throw new Error("No completed translation is available.");
    return copyTextToClipboard(detail.translation.outputText);
  }
  const manual = new Set(manualSegmentIds);
  const names = speakerMap(detail.speakers);
  await copyTextToClipboard(
    variant === "plain"
      ? detail.plainText
      : detail.segments
          .map((s) => {
            const label =
              manual.has(s.id) && s.speakerAttribution === "none"
                ? "Unknown speaker"
                : speakerLabel(s, names);
            return label ? `${label}: ${s.text}` : s.text;
          })
          .join("\n\n"),
  );
}
export async function exportReview(
  reference: ReviewRef,
  format: TranscriptExportFormat,
  variant?: TranscriptCopyVariant,
): Promise<TranscriptExportResult> {
  if (!native()) throw new Error("Export is available in the desktop app.");
  return command("export_review", { reference, format, variant });
}
export async function listenReviewUpdates(
  handler: (reference: ReviewRef) => void,
): Promise<() => void> {
  if (!native()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<ReviewRef>("review-updated", (e) => handler(e.payload));
}
export async function listenReviewJobs(
  handler: (job: ReviewJobStatus) => void,
): Promise<() => void> {
  if (!native()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<ReviewJobStatus>("review-job-status", (e) =>
    handler(e.payload),
  );
}
export const isReviewJobActive = (job: ReviewJobStatus) =>
  !["completed", "failed", "canceled"].includes(job.stage);
