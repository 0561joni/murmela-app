import { useRef } from "react";
import type React from "react";
import {
  ActionButton,
  Button,
  PageHeader,
  Progress,
} from "../components/Feedback";
import { AppIcon } from "../components/IconButton";
import { copyTextToClipboard } from "../lib/api";
import { copyReview } from "../lib/reviewApi";
import { readableError, formatBytes, formatDuration, formatListDuration } from "../lib/formatting";
import type { FileQueueItem } from "../types/domain";

export const isFileWorking = (stage: FileQueueItem["stage"]) =>
  ["queued", "preparing", "transcribing", "diarizing", "saving"].includes(
    stage,
  );
const labels: Record<FileQueueItem["stage"], string> = {
  queued: "Queued",
  preparing: "Preparing",
  transcribing: "Transcribing",
  diarizing: "Identifying speakers",
  saving: "Saving",
  completed: "Transcript ready",
  failed: "Failed",
  canceled: "Canceled",
};

interface Props {
  modelReady?: boolean;
  speakerMode?: string;
  onReview: (item: FileQueueItem) => void;
  onDismiss?: (id: string) => Promise<void>;
  onResolveModel?: () => void;
  onOpenSpeakerSettings?: () => void;
  items: FileQueueItem[];
  dragging: boolean;
  speakerCountHint: number | null;
  showSpeakerOptions: boolean;
  onSpeakerCountHintChange: (value: number | null) => void;
  onDragChange: (active: boolean) => void;
  onPick: () => Promise<void>;
  onDrop: (files: FileList) => Promise<void>;
  onCancel: (id: string) => Promise<void>;
  onRetry: (id: string) => Promise<void>;
}

export function FilesScreen(props: Props) {
  const dragDepth = useRef(0);
  const activeCount = props.items.filter((item) =>
    isFileWorking(item.stage),
  ).length;
  const doneCount = props.items.filter((item) => item.stage === "completed").length;
  const dropHandlers = {
    onDragEnter: (event: React.DragEvent) => {
      event.preventDefault();
      dragDepth.current += 1;
      props.onDragChange(true);
    },
    onDragOver: (event: React.DragEvent) => event.preventDefault(),
    onDragLeave: (event: React.DragEvent) => {
      event.preventDefault();
      dragDepth.current = Math.max(0, dragDepth.current - 1);
      if (!dragDepth.current) props.onDragChange(false);
    },
    onDrop: (event: React.DragEvent) => {
      event.preventDefault();
      dragDepth.current = 0;
      props.onDragChange(false);
      if (event.dataTransfer.files.length)
        void props.onDrop(event.dataTransfer.files);
    },
  };
  return (
    <section
      className={"screen files-screen" + (props.dragging ? " is-dragging" : "")}
      {...dropHandlers}
    >
      <PageHeader title="Transcribe files">
        {props.speakerMode ? (
          props.onOpenSpeakerSettings ? (
            <Button variant="ghost" onClick={props.onOpenSpeakerSettings} title="Change in Settings → Engines">
              <span role="status">{props.speakerMode}</span>
            </Button>
          ) : (
            <span className="files-speaker-mode" role="status">{props.speakerMode}</span>
          )
        ) : null}
        <ActionButton
          variant="primary"
          icon="plus"
          action={props.onPick}
          success=""
          disabled={props.modelReady === false}
        >
          Choose files
        </ActionButton>
      </PageHeader>
      {props.modelReady === false ? (
        <aside className="setup-panel">
          <strong>Finish setup</strong>
          <div className="setup-row">
            <span>Download a speech model before transcribing files.</span>
            <Button onClick={props.onResolveModel}>Download a model</Button>
          </div>
        </aside>
      ) : null}
      {props.showSpeakerOptions ? (
        <div className="files-speaker-options">
          <label htmlFor="speaker-count-mode">Speakers</label>
          <select
            id="speaker-count-mode"
            aria-label="Known speaker count"
            value={props.speakerCountHint === null ? "auto" : "estimate"}
            onChange={(event) =>
              props.onSpeakerCountHintChange(
                event.target.value === "auto" ? null : 2,
              )
            }
          >
            <option value="auto">Detect automatically</option>
            <option value="estimate">Exact number</option>
          </select>
          {props.speakerCountHint !== null ? (
            <input
              type="number"
              aria-label="Exact number of speakers"
              min={1}
              max={20}
              value={props.speakerCountHint}
              onChange={(event) =>
                props.onSpeakerCountHintChange(
                  Math.min(20, Math.max(1, Number(event.target.value) || 1)),
                )
              }
            />
          ) : null}
          <span className="muted">Applies to files you add next. Use an exact number only when you’re sure.</span>
        </div>
      ) : null}
      {props.items.length === 0 || props.dragging ? (
        <div className={"file-dropzone" + (props.dragging ? " is-dragging" : "")}>
          <AppIcon name="upload" />
          <p>
            <strong>{props.dragging ? "Drop to transcribe" : "Drop audio or video files here"}</strong>
            <span className="muted">
              {props.dragging ? "Files are added to the queue." : "Audio (WAV, MP3, M4A, OPUS) or video (MP4, MOV, M4V, MKV, WEBM, AVI). Only the sound is transcribed."}
            </span>
          </p>
        </div>
      ) : null}
      {props.items.length > 0 ? (
        <section className="file-queue" aria-labelledby="queue-heading">
          <div className="section-header">
            <h2 id="queue-heading" className="section-title">Queue</h2>
            <span className="section-header-spacer" />
            <span className="muted queue-summary">
              {[activeCount ? activeCount + " in progress" : "", doneCount ? doneCount + " done" : ""].filter(Boolean).join(" · ")}
            </span>
          </div>
          <ul className="file-queue-list">
            {props.items.map((item) => {
              const working = isFileWorking(item.stage);
              const hasText = Boolean(item.result || item.reviewRef);
              const saved = item.reviewRef?.kind === "saved" || Boolean(item.result?.savedTranscript);
              const unsaved = item.reviewRef?.kind === "session" || (item.result && !item.result.savedTranscript);
              const tone =
                item.stage === "completed" ? "success" : item.stage === "failed" ? "error" : working ? "busy" : "neutral";
              const statusLabel =
                hasText && item.stage === "diarizing"
                  ? "Text ready · identifying speakers"
                  : item.stage === "completed"
                    ? saved ? "Saved to Library" : "Not saved to Library"
                    : labels[item.stage];
              return (
                <li className={"queue-item stage-" + item.stage} key={item.id}>
                  <div className="queue-item-row">
                    <div className="queue-item-copy">
                      <h3 title={item.sourceFile.originalName}>{item.sourceFile.originalName}</h3>
                      <span className="queue-item-meta">
                        {formatBytes(item.sourceFile.sizeBytes)}
                        {item.sourceFile.durationMs
                          ? " · " + formatListDuration(item.sourceFile.durationMs)
                          : ""}
                        {working && hasText && saved ? <> · <span>Saved to Library</span></> : null}
                      </span>
                      {item.stage === "canceled" ? (
                        <p className="muted queue-item-note">
                          Canceled. Retry when you’re ready.
                        </p>
                      ) : null}
                    </div>
                    <div className="queue-item-status">
                      <span className={"status-text is-" + tone} role="status">
                        {statusLabel}
                      </span>
                      {working && item.stage !== "queued" ? (
                        <>
                          <span className="queue-progress-meta">
                            {item.progressPercent == null
                              ? item.statusText || "Working…"
                              : Math.round(item.progressPercent) + "%"}
                            {item.etaSeconds != null
                              ? " · about " + formatDuration(item.etaSeconds) + " left"
                              : ""}
                          </span>
                          <Progress
                            value={item.progressPercent}
                            label={"Transcribing " + item.sourceFile.originalName}
                          />
                        </>
                      ) : null}
                    </div>
                    {item.stage === "failed" && item.errorMessage ? (
                      <div className="queue-item-problem" role="alert">
                        <AppIcon name="info" />
                        <p>{readableError(item.errorMessage)}</p>
                      </div>
                    ) : null}
                    <div className="queue-actions">
                      {working ? (
                        <ActionButton
                          variant="ghost"
                          action={() => props.onCancel(item.id)}
                          success="Cancellation requested"
                        >
                          {hasText && item.stage === "diarizing"
                            ? "Stop identifying speakers"
                            : "Cancel"}
                        </ActionButton>
                      ) : null}
                      {item.stage === "failed" || item.stage === "canceled" ? (
                        <ActionButton
                          icon="retry"
                          action={() => props.onRetry(item.id)}
                          success="Queued again"
                        >
                          Retry transcription
                        </ActionButton>
                      ) : null}
                      {item.result || item.reviewRef ? (
                        <>
                          <ActionButton
                            variant="ghost"
                            icon="copy"
                            action={() =>
                              item.reviewRef
                                ? copyReview(item.reviewRef, "plain")
                                : copyTextToClipboard(item.result!.result.plainText)
                            }
                            success="Copied"
                          >
                            Copy text
                          </ActionButton>
                          <Button onClick={() => props.onReview(item)}>
                            Review transcript
                          </Button>
                        </>
                      ) : null}
                      {props.onDismiss && !working ? (
                        <ActionButton
                          variant="ghost"
                          icon="xmark"
                          aria-label={unsaved ? "Dismiss session result" : "Dismiss from queue"}
                          title={unsaved ? "Removes this unsaved transcript" : "Remove from this list. The transcript stays in Library."}
                          action={() => props.onDismiss!(item.id)}
                          success=""
                        >
                          Dismiss
                        </ActionButton>
                      ) : null}
                    </div>
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}
    </section>
  );
}
