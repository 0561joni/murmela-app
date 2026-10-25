import { describeError } from "../lib/formatting";
import { TranslationResult } from "../components/TranslationResult";
import { retryDictationTranslation } from "../lib/translationApi";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button } from "../components/Feedback";
import { ProTag, openLicenseSettings } from "../components/ProHint";
import { useLicense } from "../hooks/useLicense";
import {
  ReviewPlayer,
  type ReviewPlayerHandle,
} from "../components/ReviewPlayer";
import {
  VirtualPassages,
  type PassageListHandle,
} from "../components/VirtualPassages";
import {
  needsSpeakerReview,
  speakerMap,
  timestamp,
} from "../lib/speakerLabels";
import {
  cancelReviewJob,
  copyReview,
  editReview,
  exportReview,
  getReview,
  renameReviewTitle,
  isReviewJobActive,
  listenReviewUpdates,
  reviewKey,
  ReviewApiError,
  startReviewJob,
} from "../lib/reviewApi";
import { getFriendlyModelName } from "../lib/modelPresentation";
import type {
  FileQueueItem,
  ReviewDocument,
  ReviewEdit,
  ReviewJobStatus,
  ReviewRef,
  TranscriptExportFormat,
  TranscriptSummary,
} from "../types/domain";
/** Speakers of a recorded call come from its two channels; re-running diarization on the mix would lose that. */
const CALL_CHANNELS_MODEL_ID = "meeting-channels-v1";

function canIdentifySpeakersAgain(detail: ReviewDocument["detail"]) {
  return (
    detail.sourceType === "file_upload" ||
    (detail.sourceType === "meeting" && detail.diarizationModelId !== CALL_CHANNELS_MODEL_ID)
  );
}

interface Props {
  reference: ReviewRef;
  originLabel: string;
  onBack: () => void;
  onUpdated: (summary: TranscriptSummary) => void;
  jobs: ReviewJobStatus[];
  onJobStarted: (job: ReviewJobStatus) => void;
  initialJob?: FileQueueItem;
  onStopInitial?: () => Promise<void>;
  onResolveModel?: () => void;
  onDelete?: (id: string) => Promise<void>;
}
type Editor =
  | { type: "rename" | "merge" | "link"; speakerId: string }
  | { type: "assign" }
  | { type: "add" }
  | { type: "retry" }
  | { type: "title" }
  | { type: "delete" };
export function ReviewWorkspace(props: Props) {
  const { status: license } = useLicense();
  const [document, setDocument] = useState<ReviewDocument | null>(null);
  const documentRef = useRef(document);
  documentRef.current = document;
  const [error, setError] = useState("");
  const [errorCode, setErrorCode] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [selected, setSelected] = useState(new Set<string>());
  const [needsOnly, setNeedsOnly] = useState(false);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [follow, setFollow] = useState(true);
  const followRef = useRef(follow);
  followRef.current = follow;
  const [editor, setEditor] = useState<Editor | null>(null);
  const editorAnchor = useRef<HTMLElement | null>(null);
  const [name, setName] = useState("");
  const [targetId, setTargetId] = useState("");
  const [assignmentMode, setAssignmentMode] = useState("one");
  const [overlap, setOverlap] = useState<string[]>([]);
  const [knownCount, setKnownCount] = useState<string>("auto");
  const [reset, setReset] = useState(false);
  const [exportFormat, setExportFormat] =
    useState<TranscriptExportFormat>("txt");
  const [exportVariant, setExportVariant] = useState<"plain" | "translation">("translation");
  const player = useRef<ReviewPlayerHandle>(null);
  const list = useRef<PassageListHandle>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const alive = useRef(true);
  const requestSequence = useRef(0);
  const key = reviewKey(props.reference);
  const updated = useRef(props.onUpdated);
  updated.current = props.onUpdated;
  const accept = useCallback((next: ReviewDocument) => {
    if (
      !alive.current ||
      (documentRef.current && (documentRef.current.revision > next.revision ||
        (documentRef.current.revision === next.revision &&
          JSON.stringify(documentRef.current.detail.translation) === JSON.stringify(next.detail.translation))))
    )
      return;
    const changed = documentRef.current?.revision !== next.revision;
    documentRef.current = next;
    setDocument(next);
    if (changed && next.reference.kind === "saved")
      updated.current({
        id: next.detail.id,
        createdAt: next.detail.createdAt,
        sourceType: next.detail.sourceType,
        title: next.detail.title,
        plainText: next.detail.plainText,
        status: next.detail.status,
        detectedLanguages: next.detail.detectedLanguages,
        durationMs: next.detail.durationMs,
        modelName: next.detail.modelName,
        qualityStatus: next.detail.qualityStatus,
        recoveredRegionCount: next.detail.recoveredRegionCount,
        diarizationStatus: next.detail.diarizationStatus,
        speakerCount: next.detail.speakerCount,
      });
  }, []);
  const refresh = useCallback(async () => {
    const seq = ++requestSequence.current;
    try {
      const next = await getReview(props.reference);
      if (seq === requestSequence.current) accept(next);
    } catch (e) {
      if (alive.current && seq === requestSequence.current)
        setError(describeError(e));
    }
  }, [key, accept]);
  useEffect(() => {
    alive.current = true;
    let cleanup: (() => void) | undefined;
    void refresh();
    void listenReviewUpdates((ref) => {
      if (reviewKey(ref) === key) void refresh();
    })
      .then((fn) => {
        if (!alive.current) fn();
        else cleanup = fn;
      })
      .catch(() => {});
    heading.current?.focus();
    return () => {
      alive.current = false;
      cleanup?.();
    };
  }, [key, refresh]);
  useEffect(() => {
    if (props.initialJob?.resultRevision) void refresh();
  }, [props.initialJob?.resultRevision, refresh]);
  const job = useMemo(
    () =>
      props.jobs
        .filter((j) => reviewKey(j.reference) === key)
        .sort((a, b) => b.startedAtMs - a.startedAtMs)[0],
    [props.jobs, key],
  );
  useEffect(() => {
    if (job && !isReviewJobActive(job)) void refresh();
  }, [job?.jobId, job?.stage, refresh]);
  const jobRef = useRef(job);
  jobRef.current = job;
  const working = Boolean(job && isReviewJobActive(job));
  const initialWorking = Boolean(
    props.initialJob &&
      ["diarizing", "saving"].includes(props.initialJob.stage),
  );
  const names = useMemo(
    () => speakerMap(document?.detail.speakers ?? []),
    [document?.detail.speakers],
  );
  const manual = useMemo(
    () => new Set(document?.manualSegmentIds ?? []),
    [document?.manualSegmentIds],
  );
  const segments = document?.detail.segments ?? [];
  const reviewSegments = useMemo(
    () => segments.filter((s) => needsSpeakerReview(s, manual)),
    [segments, manual],
  );
  const visibleSegments = needsOnly ? reviewSegments : segments;
  const timeline = useMemo(
    () => [...segments].sort((a, b) => a.startMs - b.startMs),
    [segments],
  );
  const timelineRef = useRef(timeline);
  timelineRef.current = timeline;
  const lastActive = useRef<string | null>(null);
  const onTime = useCallback((ms: number) => {
    const items = timelineRef.current;
    let a = 0,
      b = items.length;
    while (a < b) {
      const m = (a + b) >>> 1;
      if (items[m].startMs <= ms) a = m + 1;
      else b = m;
    }
    const s = items[a - 1];
    const id = s && ms < s.endMs ? s.id : null;
    if (id !== lastActive.current) {
      lastActive.current = id;
      setActiveId(id);
      if (id && followRef.current) list.current?.reveal(id);
    }
  }, []);
  const onSeek = useCallback((ms: number) => player.current?.seek(ms), []);
  const onManualScroll = useCallback(() => setFollow(false), []);
  const audioResolved = useCallback(() => setNotice(""), []);
  const onSelect = useCallback(
    (id: string) =>
      setSelected((current) => {
        const next = new Set(current);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      }),
    [],
  );
  const openEditor = useCallback((value: Editor) => {
    editorAnchor.current = window.document.activeElement as HTMLElement;
    setEditor(value);
    setError("");
    setName("");
    setTargetId("");
    setOverlap([]);
    setAssignmentMode("one");
    if (value.type === "title")
      setName(documentRef.current?.detail.title ?? "");
    if (value.type === "rename")
      setName(
        documentRef.current?.detail.speakers.find(
          (s) => s.speakerId === value.speakerId,
        )?.displayName ?? "",
      );
    if (value.type === "retry") {
      setKnownCount(
        (jobRef.current?.speakerCount !== undefined
          ? jobRef.current.speakerCount
          : documentRef.current?.detail.diarizationSpeakerCountHint
        )?.toString() ?? "auto",
      );
      setReset(false);
    }
  }, []);
  const assignOne = useCallback(
    (id: string) => {
      setSelected(new Set([id]));
      openEditor({ type: "assign" });
      const segment = documentRef.current?.detail.segments.find(
        (s) => s.id === id,
      );
      setTargetId(
        segment?.speakerId ??
          documentRef.current?.detail.speakers[0]?.speakerId ??
          "",
      );
    },
    [openEditor],
  );
  const closeEditor = useCallback(() => {
    setEditor(null);
    requestAnimationFrame(() => editorAnchor.current?.focus());
  }, []);
  const run = async (action: () => Promise<void>) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError("");
    setErrorCode("");
    try {
      await action();
    } catch (e) {
      setError(describeError(e));
      setErrorCode(e instanceof ReviewApiError ? e.code : "");
      if (e instanceof ReviewApiError && e.code === "REVIEW_CONFLICT")
        await refresh();
    } finally {
      busyRef.current = false;
      if (alive.current) setBusy(false);
    }
  };
  const apply = async (edit: ReviewEdit) =>
    run(async () => {
      const current = documentRef.current;
      if (!current) return;
      const next = await editReview(props.reference, current.revision, edit);
      accept(next);
      setSelected(new Set());
      setNotice(
        edit.type === "undo"
          ? "Last correction undone."
          : "Speaker correction saved.",
      );
      if (editor) closeEditor();
    });
  const submit = () => {
    if (!editor) return;
    if (editor.type === "title")
      void run(async () => {
        const summary = await renameReviewTitle(props.reference, name);
        updated.current(summary);
        if (documentRef.current) {
          const next = {
            ...documentRef.current,
            detail: { ...documentRef.current.detail, title: summary.title },
          };
          documentRef.current = next;
          setDocument(next);
        }
        await refresh();
        closeEditor();
      });
    if (editor.type === "delete")
      void run(async () => {
        await props.onDelete?.(props.reference.id);
        props.onBack();
      });
    if (editor.type === "rename")
      void apply({ type: "rename", speakerId: editor.speakerId, name });
    if (editor.type === "add") void apply({ type: "add_speaker", name });
    if (editor.type === "merge")
      void apply({ type: "merge", speakerIds: [editor.speakerId], targetId });
    if (editor.type === "link")
      void apply({
        type: "merge",
        speakerIds: [targetId],
        targetId: editor.speakerId,
      });
    if (editor.type === "assign")
      void apply({
        type: "assign",
        segmentIds: [...selected],
        speakerIds:
          assignmentMode === "unknown" || assignmentMode === "new"
            ? []
            : assignmentMode === "overlap"
              ? overlap
              : [targetId],
        newSpeakerName: assignmentMode === "new" ? name : null,
      });
    if (editor.type === "retry")
      void run(async () => {
        const count = knownCount === "auto" ? null : Number(knownCount);
        if (
          count !== null &&
          (!Number.isInteger(count) || count < 1 || count > 20)
        )
          throw new Error("Enter a whole speaker count from 1 to 20.");
        props.onJobStarted(await startReviewJob(props.reference, count, reset));
        closeEditor();
      });
  };
  const jumpReview = (direction: number) => {
    if (!reviewSegments.length) return;
    const current = reviewSegments.findIndex((s) => s.id === activeId);
    const index =
      current < 0
        ? direction > 0
          ? 0
          : reviewSegments.length - 1
        : (current + direction + reviewSegments.length) % reviewSegments.length;
    const s = reviewSegments[index];
    setActiveId(s.id);
    setFollow(false);
    list.current?.reveal(s.id);
  };
  const editingSpeaker =
    editor && "speakerId" in editor ? names.get(editor.speakerId) : null;
  const title =
    editor?.type === "title"
      ? "Rename transcript"
      : editor?.type === "delete"
        ? "Delete transcript?"
        : editor?.type === "rename"
          ? "Rename speaker"
          : editor?.type === "add"
            ? "Add speaker"
            : editor?.type === "merge"
              ? `Merge ${editingSpeaker?.displayName ?? "speaker"}`
              : editor?.type === "link"
                ? `Link ${editingSpeaker?.displayName ?? "speaker"}`
                : editor?.type === "retry"
                  ? "Identify speakers again"
                  : "Assign selected passages";
  const disableSubmit =
    busy ||
    ((editor?.type === "title" ||
      editor?.type === "rename" ||
      editor?.type === "add" ||
      (editor?.type === "assign" && assignmentMode === "new")) &&
      !name.trim()) ||
    ((editor?.type === "merge" ||
      editor?.type === "link" ||
      (editor?.type === "assign" && assignmentMode === "one")) &&
      !targetId) ||
    (editor?.type === "assign" &&
      assignmentMode === "overlap" &&
      overlap.length < 2);
  return (
    <section className="screen review-screen">
      <div inert={editor ? true : undefined}>
        <Button icon="chevronLeft" variant="ghost" onClick={props.onBack}>
          Back to {props.originLabel}
        </Button>
        <header className="review-heading">
          <div>
            <p className="eyebrow">TRANSCRIPT REVIEW</p>
            <h1 ref={heading} tabIndex={-1}>
              {document?.detail.title ?? "Loading transcript…"}
            </h1>
            <p className="muted">
              {props.reference.kind === "saved"
                ? "Saved to Library"
                : "Session only · available until dismissed or Murmela closes"}
            </p>
            {props.reference.kind === "saved" ? (
              <div className="review-inline-actions">
                <Button
                  disabled={!document || busy}
                  variant="ghost"
                  onClick={() => openEditor({ type: "title" })}
                >
                  Rename transcript
                </Button>
                {props.onDelete ? (
                  <Button
                    disabled={busy || working || initialWorking}
                    variant="ghost"
                    onClick={() => openEditor({ type: "delete" })}
                  >
                    Delete transcript
                  </Button>
                ) : null}
              </div>
            ) : null}
          </div>
          <div className="review-output-actions">
            <Button
              disabled={!document || busy}
              onClick={() =>
                void run(async () => {
                  await copyReview(props.reference, "speaker_aware");
                  setNotice("Copied with speakers.");
                })
              }
            >
              Copy with speakers
            </Button>
            <Button
              disabled={!document || busy}
              onClick={() =>
                void run(async () => {
                  await copyReview(props.reference, "plain");
                  setNotice("Plain text copied.");
                })
              }
            >
              {document?.detail.translation ? "Copy original text" : "Copy text"}
            </Button>
            {document?.detail.translation ? <label className="review-export-label"><span className="sr-only">Export text version</span><select aria-label="Export text version" value={document.detail.translation.status === "completed" ? exportVariant : "plain"} onChange={(event) => setExportVariant(event.target.value as "plain" | "translation")} disabled={!["txt", "md"].includes(exportFormat)}><option value="translation" disabled={document.detail.translation.status !== "completed"}>Translation</option><option value="plain">Original</option></select></label> : null}
            <label className="review-export-label">
              <span className="sr-only">Export format</span>
              <select
                aria-label="Export format"
                value={exportFormat}
                onChange={(e) =>
                  setExportFormat(e.target.value as TranscriptExportFormat)
                }
              >
                {["txt", "md", "srt", "vtt", "json"].map((f) => (
                  <option value={f} key={f}>
                    {f.toUpperCase()}
                  </option>
                ))}
              </select>
            </label>
            <Button
              disabled={!document || busy}
              onClick={() =>
                void run(async () => {
                  const result = await exportReview(
                    props.reference,
                    exportFormat,
                    document?.detail.translation?.status === "completed" && ["txt", "md"].includes(exportFormat) ? exportVariant : undefined,
                  );
                  if (result.path) setNotice("Transcript exported.");
                })
              }
            >
              Export
            </Button>
          </div>
        </header>
        {error && !editor ? (
          <p className="error-text" role="alert">
            {error}
          </p>
        ) : null}
        {notice ? (
          <p className="review-notice" role="status">
            {notice}
          </p>
        ) : null}
        {initialWorking || working ? (
          <div className="review-job surface" role="status">
            <div>
              <strong>
                {initialWorking
                  ? "Text ready · Identifying speakers"
                  : job?.stage === "queued"
                    ? "Speaker retry queued"
                    : "Identifying speakers"}
              </strong>
              <p className="muted">
                {initialWorking
                  ? props.initialJob?.statusText
                  : job?.statusText}
              </p>
              <Elapsed
                since={
                  initialWorking
                    ? (props.initialJob?.startedAt ?? Date.now())
                    : job!.startedAtMs
                }
              />
            </div>
            <Button
              disabled={busy || job?.stage === "canceling"}
              onClick={() =>
                void run(async () => {
                  if (working) await cancelReviewJob(job!.jobId);
                  else await props.onStopInitial?.();
                })
              }
            >
              Stop identifying speakers
            </Button>
          </div>
        ) : null}
        {job && !working && job.stage !== "completed" ? (
          <div className="review-job" role="status">
            <p>{job.error?.message ?? job.statusText}</p>
            {job.error?.code === "MODEL_UNAVAILABLE" ? (
              <Button onClick={props.onResolveModel}>
                Open model settings
              </Button>
            ) : null}
          </div>
        ) : null}
        {document ? (
          <>
            {document.detail.sourceType !== "quick_dictate" ? (
              <ReviewPlayer
                ref={player}
                reference={props.reference}
                durationMs={document.detail.durationMs}
                onTime={onTime}
                onResolved={audioResolved}
              />
            ) : null}
            <section className="review-speakers surface" aria-label="Speakers">
              <div className="section-header">
                <h2>
                  Speakers{" "}
                  <span className="count-label">
                    {document.detail.speakers.length}
                  </span>
                </h2>
                <div className="review-inline-actions">
                  <Button
                    disabled={busy}
                    onClick={() => openEditor({ type: "add" })}
                  >
                    Add speaker
                  </Button>
                  <Button
                    disabled={busy || !document.canUndo}
                    onClick={() => void apply({ type: "undo" })}
                  >
                    Undo correction
                  </Button>
                  <Button
                    disabled={
                      busy ||
                      working ||
                      initialWorking ||
                      !canIdentifySpeakersAgain(document.detail)
                    }
                    onClick={() =>
                      license && !license.pro
                        ? openLicenseSettings()
                        : openEditor({ type: "retry" })
                    }
                    title={license && !license.pro ? "Speaker labels are part of Murmela Pro" : undefined}
                  >
                    {document.detail.diarizationStatus === "not_requested"
                      ? "Identify speakers"
                      : "Identify again"}
                    {license && !license.pro ? <> <ProTag /></> : null}
                  </Button>
                </div>
              </div>
              {document.detail.speakers.length ? (
                <div className="review-speaker-roster">
                  {document.detail.speakers.map((s) => (
                    <div className="review-speaker-card" key={s.speakerId}>
                      <strong
                        className={`speaker-label speaker-color-${s.speakerOrder % 6}`}
                      >
                        {s.displayName}
                      </strong>
                      {document.unmatchedSpeakerIds.includes(s.speakerId) ? (
                        <span className="muted">Needs linking</span>
                      ) : null}
                      <div>
                        <button
                          aria-label={`Rename ${s.displayName}`}
                          onClick={() =>
                            openEditor({
                              type: "rename",
                              speakerId: s.speakerId,
                            })
                          }
                        >
                          Rename
                          <span className="sr-only"> {s.displayName}</span>
                        </button>
                        <button
                          aria-label={`${document.unmatchedSpeakerIds.includes(s.speakerId) ? "Link" : "Merge"} ${s.displayName}`}
                          disabled={document.detail.speakers.length < 2}
                          onClick={() =>
                            openEditor({
                              type: document.unmatchedSpeakerIds.includes(
                                s.speakerId,
                              )
                                ? "link"
                                : "merge",
                              speakerId: s.speakerId,
                            })
                          }
                        >
                          {document.unmatchedSpeakerIds.includes(s.speakerId)
                            ? "Link"
                            : "Merge"}
                          <span className="sr-only"> {s.displayName}</span>
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
              ) : (
                <p className="muted">
                  {initialWorking
                    ? "Speaker labels will appear here when identification finishes."
                    : "No speakers identified. You can identify speakers or assign them yourself."}
                </p>
              )}
              {document.detail.diarizationWarning ? (
                <p className="warning-text">
                  {document.detail.diarizationWarning}
                </p>
              ) : null}
              <details className="review-details">
                <summary>Identification details</summary>
                <p>
                  {document.detail.diarizationSource === "native_model"
                    ? `Built into ${getFriendlyModelName(document.detail.modelName ?? "")}`
                    : document.detail.diarizationModelId
                      ? `Identified by Murmela · ${document.detail.diarizationSpeakerCountHint === null ? "number of speakers detected automatically" : `${document.detail.diarizationSpeakerCountHint} speakers, as you set`}`
                      : "Speaker identification has not run."}
                </p>
                {document.detail.diarizationClusteringThreshold != null ? (
                  <p className="muted">
                    Matching strictness:{" "}
                    {document.detail.diarizationClusteringThreshold.toFixed(2)}
                  </p>
                ) : null}
              </details>
            </section>
            <div className="review-tools">
              <label>
                <input
                  type="checkbox"
                  checked={needsOnly}
                  onChange={(e) => setNeedsOnly(e.target.checked)}
                />{" "}
                Needs review ({reviewSegments.length})
              </label>
              <Button
                disabled={!reviewSegments.length}
                variant="ghost"
                onClick={() => jumpReview(-1)}
              >
                Previous
              </Button>
              <Button
                disabled={!reviewSegments.length}
                variant="ghost"
                onClick={() => jumpReview(1)}
              >
                Next
              </Button>
              <Button
                variant="ghost"
                disabled={follow}
                onClick={() => {
                  setFollow(true);
                  if (activeId) list.current?.reveal(activeId);
                }}
              >
                {follow ? "Following playback" : "Follow playback"}
              </Button>
              <span className="muted">
                {segments.length}{" "}
                {segments.length === 1 ? "passage" : "passages"}
              </span>
            </div>
            <p className="muted review-legend">
              “Likely” is a tentative match. “Uncertain” has no clear match. “+”
              means overlapping speakers. Select passages to correct them.
            </p>
            {selected.size ? (
              <div className="review-selection" role="status">
                <strong>{selected.size} selected</strong>
                <Button
                  disabled={busy}
                  onClick={() => {
                    openEditor({ type: "assign" });
                    setTargetId(document.detail.speakers[0]?.speakerId ?? "");
                  }}
                >
                  Assign speakers
                </Button>
                <Button variant="ghost" onClick={() => setSelected(new Set())}>
                  Clear selection
                </Button>
              </div>
            ) : null}
            {document.detail.translation ? <TranslationResult output={document.detail.translation}
              onRetry={props.reference.kind === "saved" ? async () => {
                const output = await retryDictationTranslation(props.reference.id);
                await refresh();
                if (output.status !== "completed") throw new Error(output.errorMessage ?? "Translation failed.");
              } : undefined} /> : segments.length ? (
              <VirtualPassages
                ref={list}
                segments={visibleSegments}
                speakers={names}
                manual={manual}
                selected={selected}
                activeId={activeId}
                onSelect={onSelect}
                onAssign={assignOne}
                onSeek={onSeek}
                onManualScroll={onManualScroll}
              />
            ) : (
              <div className="surface review-empty">
                {document.detail.plainText || "No speech was transcribed."}
              </div>
            )}
            {document.detail.transcriptionWarnings.map((w, i) => (
              <p className="warning-text" key={i}>
                {timestamp(w.startMs)} · {w.reason}
              </p>
            ))}
          </>
        ) : null}
      </div>
      {editor ? (
        <ReviewDialog title={title} onClose={closeEditor} busy={busy}>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              submit();
            }}
          >
            {editor.type === "title" ? (
              <label className="field-stack">
                Transcript title
                <input
                  autoFocus
                  maxLength={200}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </label>
            ) : null}
            {editor.type === "delete" ? (
              <p>
                Delete this saved transcript and its speaker corrections from
                Library? This cannot be undone. Your original recording stays in
                place.
              </p>
            ) : null}
            {editor.type === "rename" || editor.type === "add" ? (
              <label className="field-stack">
                Speaker name
                <input
                  autoFocus
                  maxLength={80}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </label>
            ) : null}
            {editor.type === "merge" || editor.type === "link" ? (
              <>
                <p>
                  {editor.type === "merge"
                    ? "All passages assigned to this speaker will use the selected speaker. You can undo this correction."
                    : "Choose the detected speaker that belongs to this saved name. The name and corrected passages will be preserved."}
                </p>
                <label className="field-stack">
                  {editor.type === "merge" ? "Merge into" : "Detected speaker"}
                  <select
                    autoFocus
                    value={targetId}
                    onChange={(e) => setTargetId(e.target.value)}
                  >
                    <option value="">Choose a speaker</option>
                    {document?.detail.speakers
                      .filter((s) => s.speakerId !== editor.speakerId)
                      .map((s) => (
                        <option key={s.speakerId} value={s.speakerId}>
                          {s.displayName}
                        </option>
                      ))}
                  </select>
                </label>
              </>
            ) : null}
            {editor.type === "assign" ? (
              <>
                <p>
                  Apply to {selected.size} selected passage
                  {selected.size === 1 ? "" : "s"}. Text and timestamps stay the
                  same.
                </p>
                <label className="field-stack">
                  Assignment
                  <select
                    autoFocus
                    value={assignmentMode}
                    onChange={(e) => setAssignmentMode(e.target.value)}
                  >
                    <option value="one">One speaker</option>
                    <option value="overlap">Overlapping speakers</option>
                    <option value="new">New speaker</option>
                    <option value="unknown">Unknown speaker</option>
                  </select>
                </label>
                {assignmentMode === "one" ? (
                  <label className="field-stack">
                    Speaker
                    <select
                      value={targetId}
                      onChange={(e) => setTargetId(e.target.value)}
                    >
                      <option value="">Choose a speaker</option>
                      {document?.detail.speakers.map((s) => (
                        <option key={s.speakerId} value={s.speakerId}>
                          {s.displayName}
                        </option>
                      ))}
                    </select>
                  </label>
                ) : assignmentMode === "new" ? (
                  <label className="field-stack">
                    New speaker name
                    <input
                      value={name}
                      maxLength={80}
                      onChange={(e) => setName(e.target.value)}
                    />
                  </label>
                ) : assignmentMode === "overlap" ? (
                  <fieldset>
                    <legend>Choose at least two speakers</legend>
                    {document?.detail.speakers.map((s) => (
                      <label className="review-checkbox" key={s.speakerId}>
                        <input
                          type="checkbox"
                          checked={overlap.includes(s.speakerId)}
                          onChange={(e) =>
                            setOverlap((current) =>
                              e.target.checked
                                ? [...current, s.speakerId]
                                : current.filter((id) => id !== s.speakerId),
                            )
                          }
                        />
                        {s.displayName}
                      </label>
                    ))}
                  </fieldset>
                ) : (
                  <p className="muted">
                    These passages will be explicitly marked as manually
                    unassigned.
                  </p>
                )}
              </>
            ) : null}
            {editor.type === "retry" ? (
              <>
                <p>
                  Run the local speaker model again using the original audio.
                  Your text and passage boundaries stay the same.
                </p>
                {document?.detail.diarizationSource === "native_model" ? (
                  <p className="warning-text">
                    This uses the standalone speaker model to replace built-in
                    speaker labels.
                  </p>
                ) : null}
                <label className="field-stack">
                  How many people speak
                  <select
                    autoFocus
                    value={knownCount === "auto" ? "auto" : "known"}
                    onChange={(e) =>
                      setKnownCount(
                        e.target.value === "auto"
                          ? "auto"
                          : String(document?.detail.speakerCount ?? 2),
                      )
                    }
                  >
                    <option value="auto">Count people automatically</option>
                    <option value="known">I know how many people</option>
                  </select>
                </label>
                {knownCount !== "auto" ? (
                  <label className="field-stack">
                    Exact number of speakers
                    <input
                      type="number"
                      min={1}
                      max={20}
                      step={1}
                      value={knownCount}
                      onChange={(e) => setKnownCount(e.target.value)}
                    />
                  </label>
                ) : null}
                <label className="review-checkbox">
                  <input
                    type="checkbox"
                    checked={reset}
                    onChange={(e) => setReset(e.target.checked)}
                  />
                  Reset manual corrections and speaker names
                </label>
                <p className={reset ? "warning-text" : "muted"}>
                  {reset
                    ? "Corrections and names will be reset only if identification succeeds."
                    : "Manual corrections and confidently matched names will be preserved. Unmatched names stay available for linking."}
                </p>
              </>
            ) : null}
            {error ? (
              <p className="error-text" role="alert">
                {error}
              </p>
            ) : null}
            {errorCode === "MODEL_UNAVAILABLE" ? (
              <Button type="button" onClick={props.onResolveModel}>
                Open model settings
              </Button>
            ) : null}
            <div className="review-dialog-actions">
              <Button type="button" disabled={busy} onClick={closeEditor}>
                Cancel
              </Button>
              <Button type="submit" variant="primary" disabled={disableSubmit}>
                {busy
                  ? "Working…"
                  : editor.type === "retry"
                    ? "Start identification"
                    : editor.type === "delete"
                      ? "Delete transcript"
                      : editor.type === "title"
                        ? "Save title"
                        : "Save correction"}
              </Button>
            </div>
          </form>
        </ReviewDialog>
      ) : null}
    </section>
  );
}
function Elapsed({ since }: { since: number }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  return <span className="muted">Elapsed {timestamp(now - since)}</span>;
}
function ReviewDialog({
  title,
  children,
  onClose,
  busy,
}: {
  title: string;
  children: React.ReactNode;
  onClose: () => void;
  busy: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const hidden: HTMLElement[] = [];
    let current: HTMLElement | null = ref.current;
    while (current && current !== window.document.body) {
      const parent = current.parentElement;
      if (parent)
        for (const sibling of parent.children) {
          if (
            sibling !== current &&
            sibling instanceof HTMLElement &&
            !sibling.inert
          ) {
            sibling.inert = true;
            hidden.push(sibling);
          }
        }
      current = parent;
    }
    ref.current?.querySelector<HTMLElement>("input,select,button")?.focus();
    return () =>
      hidden.forEach((element) => {
        element.inert = false;
      });
  }, []);
  return (
    <div className="review-dialog-backdrop">
      <div
        ref={ref}
        className="review-dialog surface"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(e) => {
          if (e.key === "Escape" && !busy) {
            e.preventDefault();
            onClose();
          }
          if (e.key === "Tab") {
            const items = Array.from(
              e.currentTarget.querySelectorAll<HTMLElement>(
                'button:not(:disabled),input:not(:disabled),select:not(:disabled),[tabindex="0"]',
              ),
            );
            const first = items[0],
              last = items[items.length - 1];
            if (e.shiftKey && window.document.activeElement === first) {
              e.preventDefault();
              last?.focus();
            } else if (!e.shiftKey && window.document.activeElement === last) {
              e.preventDefault();
              first?.focus();
            }
          }
        }}
      >
        <h2>{title}</h2>
        {children}
      </div>
    </div>
  );
}
