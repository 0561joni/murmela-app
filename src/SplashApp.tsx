import { useEffect, useRef, useState } from "react";
import murmelaMark from "./assets/murmela-mark.png";
import {
  completeStartupHandoff,
  getStartupStatus,
  listenStartupStatus,
  quitApp,
  restartApp,
} from "./lib/api";
import { usePlayful, useReducedMotion } from "./lib/appearance";
import { AppIcon } from "./components/IconButton";
import { ActionButton } from "./components/Feedback";
import type { StartupPhase, StartupStatus } from "./types/domain";

export const STARTUP_PHASE_COPY: Record<
  Exclude<StartupPhase, "ready" | "failed">,
  { headline: string; detail: string }
> = {
  files: {
    headline: "Opening Murmela",
    detail: "Preparing your local workspace",
  },
  models: {
    headline: "Preparing speech models",
    detail: "Checking the models on your device",
  },
  audio: {
    headline: "Getting ready to listen",
    detail: "Connecting audio and desktop controls",
  },
  library: {
    headline: "Opening your library",
    detail: "Loading transcripts, settings, and vocabulary",
  },
  shortcuts: {
    headline: "Setting up your shortcut",
    detail: "Connecting your keyboard controls",
  },
  workspace: {
    headline: "Almost ready",
    detail: "Bringing your workspace into view",
  },
};

/** Detail lines in a lighter voice; the headline keeps saying what happens. */
const PLAYFUL_DETAIL: Record<Exclude<StartupPhase, "ready" | "failed">, string> = {
  files: "Stretching the vocal cords",
  models: "Teaching the robots to listen",
  audio: "Tuning the ears",
  library: "Dusting off your words",
  shortcuts: "Polishing the magic keys",
  workspace: "Clearing its throat",
};

export function SplashApp() {
  const playful = usePlayful();
  const [status, setStatus] = useState<StartupStatus>({
    phase: "files",
    step: 1,
    totalSteps: 6,
  });
  const [elapsed, setElapsed] = useState(0);
  const [finishing, setFinishing] = useState(false);
  const mountedAt = useRef(Date.now());
  const reduced = useReducedMotion();
  const failed = status.phase === "failed";
  const copy =
    status.phase === "failed"
      ? {
          headline: "Murmela couldn’t start",
          detail: "Try restarting to reopen your workspace.",
        }
      : status.phase === "ready"
        ? {
            headline: "Ready for your words",
            detail: "Your workspace is ready",
          }
        : playful
          ? { ...STARTUP_PHASE_COPY[status.phase], detail: PLAYFUL_DETAIL[status.phase] }
          : STARTUP_PHASE_COPY[status.phase];

  useEffect(() => {
    let disposed = false;
    let receivedEvent = false;
    let unlisten: (() => void) | undefined;
    void listenStartupStatus((next) => {
      receivedEvent = true;
      if (!disposed) setStatus(next);
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    });
    void getStartupStatus()
      .then((next) => {
        if (!disposed && !receivedEvent) setStatus(next);
      })
      .catch((error) => {
        if (!disposed)
          setStatus({
            phase: "failed",
            step: 1,
            totalSteps: 6,
            errorMessage:
              error instanceof Error ? error.message : String(error),
          });
      });
    const timer = window.setInterval(
      () => setElapsed(Math.floor((Date.now() - mountedAt.current) / 1000)),
      1000,
    );
    return () => {
      disposed = true;
      window.clearInterval(timer);
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (status.phase !== "ready") return;
    let handoffTimer = 0;
    const timer = window.setTimeout(
      () => {
        setFinishing(true);
        handoffTimer = window.setTimeout(
          () => {
            void completeStartupHandoff().catch((error) => {
              setFinishing(false);
              setStatus({
                phase: "failed",
                step: 6,
                totalSteps: 6,
                errorMessage:
                  error instanceof Error
                    ? error.message
                    : "Could not open the workspace.",
              });
            });
          },
          reduced ? 0 : 180,
        );
      },
      Math.max(0, 900 - (Date.now() - mountedAt.current)),
    );
    return () => {
      window.clearTimeout(timer);
      window.clearTimeout(handoffTimer);
    };
  }, [status.phase, reduced]);

  return (
    <main
      className={"splash-root" + (finishing ? " is-finishing" : "")}
      aria-label="Murmela startup"
    >
      <header className="splash-brand">
        <strong>Murmela</strong>
      </header>
      <div
        className={
          "splash-symbol" +
          (failed ? " is-failed" : status.phase === "ready" ? " is-ready" : "")
        }
      >
        {failed || status.phase === "ready" ? (
          <AppIcon name={failed ? "info" : "check"} />
        ) : (
          <img className="splash-logo" src={murmelaMark} alt="" width={64} height={44} />
        )}
      </div>
      <section className="startup-copy" role="status" aria-live="polite">
        <h1>{copy.headline}</h1>
        <p>{copy.detail}</p>
      </section>
      {failed ? (
        <section className="startup-recovery">
          <details>
            <summary>Technical details</summary>
            <p>{status.errorMessage ?? "An unknown error occurred."}</p>
          </details>
          <RecoveryActions />
        </section>
      ) : (
        <footer className="startup-progress">
          <div
            className="milestone-track"
            role="progressbar"
            aria-label={
              "Startup step " + status.step + " of " + status.totalSteps
            }
            aria-valuemin={1}
            aria-valuemax={6}
            aria-valuenow={status.step}
          >
            {Array.from({ length: 6 }, (_, index) => (
              <span
                className={
                  "milestone" + (index < status.step ? " is-complete" : "")
                }
                key={index}
              />
            ))}
          </div>
          <span className="step-label">
            {status.step} of {status.totalSteps}
          </span>
          {elapsed >= 15 ? (
            <p className="slow-note">
              Still preparing your local engine. This may take a moment.
            </p>
          ) : null}
          {elapsed >= 30 ? <RecoveryActions /> : null}
        </footer>
      )}
    </main>
  );
}
function RecoveryActions() {
  return (
    <div className="recovery-actions">
      <ActionButton action={restartApp} success="">
        Restart Murmela
      </ActionButton>
      <ActionButton action={quitApp} success="" variant="ghost">
        Quit
      </ActionButton>
    </div>
  );
}
