import { useCallback, useEffect, useRef, useState } from "react";
import { ActionButton, Button, PageHeader, Progress } from "../components/Feedback";
import {
  deletePendingCallRecording,
  discardCallRecording,
  getCallCaptureSupport,
  getCallStatus,
  isCallRecordingPath,
  listPendingCallRecordings,
  revealBrowserHelper,
  startCallRecording,
  stopCallRecording,
  transcribePendingCallRecording,
} from "../lib/api";
import { describeError, formatClock, formatDateTime, formatListDuration, readableError } from "../lib/formatting";
import type {
  CallCaptureSupport,
  CallMode,
  CallStatus,
  FileQueueItem,
  PendingCallRecording,
} from "../types/domain";

const PLATFORM_NAMES: Record<string, string> = {
  meet: "Google Meet",
  teams: "Microsoft Teams",
  zoom: "Zoom",
};

/** The call's sound counts as missing after this long without a signal. */
const QUIET_SYSTEM_MS = 20_000;

const WORKING = ["queued", "preparing", "transcribing", "diarizing", "saving"];

interface Props {
  /** Every file job; the ones made from recordings are shown here too. */
  items: FileQueueItem[];
  pro: boolean;
  onReview: (item: FileQueueItem) => void;
  onRetry: (id: string) => Promise<void>;
}

export function MeetingsScreen({ items, pro, onReview, onRetry }: Props) {
  const [support, setSupport] = useState<CallCaptureSupport | null>(null);
  const [status, setStatus] = useState<CallStatus | null>(null);
  const [mode, setMode] = useState<CallMode>("call");
  const [title, setTitle] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"start" | "stop" | null>(null);
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const [pending, setPending] = useState<PendingCallRecording[]>([]);
  const [helperError, setHelperError] = useState<string | null>(null);
  const lastSystemSound = useRef(0);
  const stopping = useRef(false);

  const refreshPending = useCallback(() => {
    listPendingCallRecordings()
      .then(setPending)
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    let current = true;
    getCallCaptureSupport()
      .then((next) => {
        if (!current) return;
        setSupport(next);
        if (!next.supported) setMode("room");
      })
      .catch(() => current && setSupport({ supported: false, reason: null }));
    getCallStatus()
      .then((next) => current && setStatus(next))
      .catch(() => undefined);
    refreshPending();
    return () => {
      current = false;
    };
  }, [refreshPending]);

  const recording = status?.recording ?? false;

  const stop = useCallback(async () => {
    if (stopping.current) return;
    stopping.current = true;
    setBusy("stop");
    setError(null);
    try {
      await stopCallRecording(null);
      setStatus(await getCallStatus());
      setTitle("");
      refreshPending();
    } catch (reason) {
      setError(describeError(reason, "The recording could not be stopped."));
      setStatus(await getCallStatus().catch(() => null));
      refreshPending();
    } finally {
      stopping.current = false;
      setBusy(null);
    }
  }, [refreshPending]);

  useEffect(() => {
    if (!recording) return;
    lastSystemSound.current = Date.now();
    const timer = window.setInterval(() => {
      getCallStatus()
        .then((next) => {
          setStatus(next);
          if ((next.system?.level ?? 0) > 0.02) lastSystemSound.current = Date.now();
          if (next.recording && next.elapsedMs >= next.limitMs) void stop();
        })
        .catch(() => undefined);
    }, 200);
    return () => window.clearInterval(timer);
  }, [recording, stop]);

  // Calls being transcribed or finished in this session (they also appear under Files).
  useEffect(() => {
    refreshPending();
  }, [items.length, refreshPending]);

  async function start() {
    setBusy("start");
    setError(null);
    try {
      setStatus(await startCallRecording(mode, title.trim() || null));
    } catch (reason) {
      setError(describeError(reason, "The recording could not start."));
    } finally {
      setBusy(null);
    }
  }

  async function discard() {
    setConfirmDiscard(false);
    try {
      await discardCallRecording();
      setStatus(await getCallStatus());
    } catch (reason) {
      setError(describeError(reason, "The recording could not be deleted."));
    }
  }

  const callJobs = items.filter((item) => isCallRecordingPath(item.sourceFile.filePath));
  const systemQuiet =
    recording && status?.system && Date.now() - lastSystemSound.current > QUIET_SYSTEM_MS;

  return (
    <section className="screen meetings-screen">
      <PageHeader
        title="Meetings"
        description="Record a call or a meeting in the room. Transcribed on this computer, with who said what."
      />

      {recording && status ? (
        <article className="glass-subtle settings-card meetings-live" aria-label="Recording">
          <div className="meetings-live-head">
            <span className="meetings-rec-dot" aria-hidden="true" />
            <div>
              <h2>{status.title}</h2>
              <p className="muted" role="timer">
                {formatClock(status.elapsedMs)} recorded
                {status.elapsedMs > status.limitMs - 10 * 60_000
                  ? ` · stops at ${formatClock(status.limitMs)}`
                  : ""}
              </p>
            </div>
          </div>
          <LevelRow label="You" detail={status.microphone?.label} level={status.microphone?.level ?? 0} error={status.microphone?.error} />
          {status.system ? (
            <LevelRow
              label="Other side"
              detail="Sound of the call"
              level={status.system.level}
              error={status.system.error}
            />
          ) : null}
          {systemQuiet ? (
            <p className="muted meetings-hint" role="status">
              No sound from the call yet. Make sure the call plays on this computer. On a Mac, Murmela needs
              permission under System Settings → Privacy &amp; Security → Screen &amp; System Audio Recording.
            </p>
          ) : null}
          {status.mode === "call" ? <BrowserLine status={status} /> : null}
          {error ? (
            <p className="error-text" role="alert">
              {readableError(error)}
            </p>
          ) : null}
          <div className="meetings-actions">
            <Button variant="primary" icon="stop" busy={busy === "stop"} onClick={() => void stop()}>
              Stop and transcribe
            </Button>
            {confirmDiscard ? (
              <>
                <span className="muted">Delete this recording?</span>
                <Button variant="danger" onClick={() => void discard()}>
                  Delete recording
                </Button>
                <Button variant="ghost" onClick={() => setConfirmDiscard(false)}>
                  Keep recording
                </Button>
              </>
            ) : (
              <Button variant="ghost" onClick={() => setConfirmDiscard(true)}>
                Discard
              </Button>
            )}
          </div>
        </article>
      ) : (
        <article className="glass-subtle settings-card meetings-start" aria-label="New recording">
          <div className="segmented-control" role="group" aria-label="Where the conversation happens">
            <button
              type="button"
              aria-pressed={mode === "call"}
              disabled={support?.supported === false}
              onClick={() => setMode("call")}
            >
              On a call
            </button>
            <button type="button" aria-pressed={mode === "room"} onClick={() => setMode("room")}>
              In the room
            </button>
          </div>
          <p className="muted">
            {mode === "call"
              ? "Records your microphone and the sound of the call (Zoom, Teams, Meet, FaceTime, a phone app), so your words and theirs are told apart."
              : "Records the microphone only, for a conversation in the same room."}
          </p>
          {support?.supported === false && support.reason ? (
            <p className="muted meetings-hint">{support.reason}</p>
          ) : null}
          <label className="field-stack">
            <span className="setting-title">Name</span>
            <input
              type="text"
              value={title}
              maxLength={80}
              placeholder={mode === "call" ? "e.g. Client call with Anna" : "e.g. Session with Ben"}
              onChange={(event) => setTitle(event.target.value)}
            />
          </label>
          <p className="meetings-consent">
            Let everyone know you are recording. In many places the law requires their consent.
          </p>
          {mode === "call" ? (
            <p className="muted">
              Headphones give the cleanest result: with loudspeakers, your microphone also hears the other side.
            </p>
          ) : null}
          {error ? (
            <p className="error-text" role="alert">
              {readableError(error)}
            </p>
          ) : null}
          <div className="meetings-actions">
            <Button variant="primary" icon="microphone" busy={busy === "start"} onClick={() => void start()}>
              Start recording
            </Button>
          </div>
        </article>
      )}

      <article className="glass-subtle settings-card" aria-label="Speaker names">
        <h3>Who said what</h3>
        <p className="muted">
          Your microphone is always you. The call&apos;s sound is the other side
          {pro
            ? ", and Murmela tells the people on it apart."
            : ". With Murmela Pro, Murmela also tells the people on it apart."}{" "}
          For their names, add the browser helper: during Google Meet, Microsoft Teams or Zoom calls in the
          browser, it tells Murmela who is speaking. It sends names and timings only, and only to Murmela on this
          computer.
        </p>
        {helperError ? <p className="warning-text">{helperError}</p> : null}
        <div className="meetings-actions">
          <Button
            variant="secondary"
            icon="folder"
            onClick={() => {
              setHelperError(null);
              revealBrowserHelper().catch((reason) =>
                setHelperError(describeError(reason, "The browser helper could not be shown.")),
              );
            }}
          >
            Show browser helper
          </Button>
          <span className="muted">Load it in chrome://extensions with Developer mode on.</span>
        </div>
      </article>

      {pending.length > 0 ? (
        <section aria-labelledby="pending-heading">
          <h2 id="pending-heading" className="section-title">
            Not transcribed yet
          </h2>
          <ul className="file-queue-list">
            {pending.map((recording) => (
              <li className="queue-item" key={recording.id}>
                <div className="queue-item-row">
                  <div className="queue-item-copy">
                    <h3>{recording.title}</h3>
                    <span className="queue-item-meta">
                      {recording.startedAt ? formatDateTime(recording.startedAt) + " · " : ""}
                      {formatListDuration(recording.durationMs)}
                      {recording.interrupted ? " · recovered after Murmela stopped" : ""}
                    </span>
                  </div>
                  <div className="queue-actions">
                    <ActionButton
                      icon="retry"
                      action={async () => {
                        await transcribePendingCallRecording(recording.id);
                        refreshPending();
                      }}
                      success="Queued"
                    >
                      Transcribe
                    </ActionButton>
                    <ActionButton
                      variant="ghost"
                      icon="trash"
                      action={async () => {
                        await deletePendingCallRecording(recording.id);
                        refreshPending();
                      }}
                      success="Deleted"
                    >
                      Delete
                    </ActionButton>
                  </div>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {callJobs.length > 0 ? (
        <section aria-labelledby="recent-heading">
          <h2 id="recent-heading" className="section-title">
            Recent recordings
          </h2>
          <ul className="file-queue-list">
            {callJobs.map((item) => {
              const working = WORKING.includes(item.stage);
              return (
                <li className={"queue-item stage-" + item.stage} key={item.id}>
                  <div className="queue-item-row">
                    <div className="queue-item-copy">
                      <h3>{item.sourceFile.originalName.replace(/\.wav$/i, "")}</h3>
                      <span className="queue-item-meta">
                        {item.sourceFile.durationMs ? formatListDuration(item.sourceFile.durationMs) : ""}
                      </span>
                      {item.stage === "failed" && item.errorMessage ? (
                        <p className="error-text queue-item-note" role="alert">
                          {readableError(item.errorMessage)}
                        </p>
                      ) : null}
                    </div>
                    <div className="queue-item-status">
                      <span className="status-text" role="status">
                        {item.stage === "completed" ? "Transcript ready" : item.statusText || "Waiting"}
                      </span>
                      {working && item.stage !== "queued" ? (
                        <Progress value={item.progressPercent} label={"Transcribing " + item.sourceFile.originalName} />
                      ) : null}
                    </div>
                    <div className="queue-actions">
                      {item.stage === "failed" || item.stage === "canceled" ? (
                        <ActionButton icon="retry" action={() => onRetry(item.id)} success="Queued again">
                          Retry
                        </ActionButton>
                      ) : null}
                      {item.stage === "completed" || item.reviewRef ? (
                        <Button onClick={() => onReview(item)}>Open in Review</Button>
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

function LevelRow({
  label,
  detail,
  level,
  error,
}: {
  label: string;
  detail?: string;
  level: number;
  error?: string | null;
}) {
  return (
    <div className="meetings-level">
      <div className="meetings-level-copy">
        <span className="setting-title">{label}</span>
        {detail ? <span className="muted">{detail}</span> : null}
      </div>
      <div
        className="meetings-meter"
        role="meter"
        aria-label={`${label} level`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(Math.min(1, level) * 100)}
      >
        <span style={{ width: `${Math.round(Math.min(1, level) * 100)}%` }} />
      </div>
      {error ? <p className="warning-text">Reconnecting: {error}</p> : null}
    </div>
  );
}

function BrowserLine({ status }: { status: CallStatus }) {
  const browser = status.browser;
  if (!browser || browser.msSinceLastReport > 15_000) {
    return (
      <p className="muted meetings-hint" role="status">
        Browser helper not connected. Names come from it during browser calls; without it, speakers are labelled by
        side{status.bridgePort === null ? ". (Its local connection could not open.)" : "."}
      </p>
    );
  }
  const platform = PLATFORM_NAMES[browser.platform] ?? "the browser";
  return (
    <p className="meetings-hint" role="status">
      Names from {platform}
      {browser.participants.length ? `: ${browser.participants.join(", ")}` : ""}
      {browser.speaking.length ? ` · speaking: ${browser.speaking.join(", ")}` : ""}
    </p>
  );
}
