import { useState } from "react";
import {
  downloadUpdate,
  downloadUpdateAsFree,
  installUpdateAndRestart,
  installUpdateInHomeApplications,
  openProPage,
} from "../lib/api";
import { describeError, formatCalendarDay } from "../lib/formatting";
import type { UpdateStatus } from "../types/domain";
import { Progress } from "./Feedback";

/** Release headlines usually have no full stop; the banner reads them as a sentence. */
function sentence(text: string) {
  const trimmed = text.trim();
  return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

/** Announces a new release and walks through downloading and installing it. Nothing
 * downloads until the user asks. Dismissing a downloaded release keeps it for the next quit.
 * A release that the Pro key doesn't cover offers renewing or installing it as Free. */
export function UpdateBanner({ status }: { status: UpdateStatus | null }) {
  const [dismissedVersion, setDismissedVersion] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!status?.version || dismissedVersion === status.version) return null;
  if (status.state === "needs_renewal") return <RenewalBanner status={status} onLater={setDismissedVersion} />;
  if (status.state === "cannot_install") return <CannotInstallBanner status={status} />;
  if (!["available", "downloading", "ready"].includes(status.state)) return null;

  async function run(action: () => Promise<unknown>, fallback: string) {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (reason) {
      setError(describeError(reason, fallback));
    } finally {
      setBusy(false);
    }
  }

  const notes = status.notes ? `${sentence(status.notes)} ` : "";
  return (
    <div className="update-banner surface" role="status">
      <div className="update-banner-copy">
        {status.state === "available" ? (
          <>
            <strong>Murmela {status.version} is available</strong>
            <p className="muted">{notes}Download it now, or later from Settings → About.</p>
          </>
        ) : status.state === "downloading" ? (
          <>
            <strong>
              Downloading Murmela {status.version}
              {status.progress != null ? ` · ${Math.round(status.progress)} %` : "…"}
            </strong>
            <Progress value={status.progress} label={`Downloading Murmela ${status.version}`} />
          </>
        ) : (
          <>
            <strong>Murmela {status.version} is ready</strong>
            <p className="muted">{notes}It installs when you restart or quit Murmela.</p>
          </>
        )}
        {error || status.message ? <p className="warning-text">{error ?? status.message}</p> : null}
      </div>
      {status.state !== "downloading" ? (
        <div className="update-banner-actions">
          <button
            type="button"
            className="button button-ghost"
            onClick={() => setDismissedVersion(status.version)}
            disabled={busy}
          >
            Later
          </button>
          {status.state === "available" ? (
            <button
              type="button"
              className="button button-primary"
              onClick={() => void run(downloadUpdate, "The update could not be downloaded.")}
              disabled={busy}
              aria-busy={busy || undefined}
            >
              Download update
            </button>
          ) : (
            <button
              type="button"
              className="button button-primary"
              onClick={() => void run(installUpdateAndRestart, "The update could not be installed.")}
              disabled={busy}
              aria-busy={busy || undefined}
            >
              Restart to update
            </button>
          )}
        </div>
      ) : null}
    </div>
  );
}

/** The release is downloaded, but this account may not replace Murmela where it is. */
function CannotInstallBanner({ status }: { status: UpdateStatus }) {
  const [busy, setBusy] = useState<"home" | "retry" | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function run(which: "home" | "retry", action: () => Promise<unknown>) {
    setBusy(which);
    setError(null);
    try {
      await action();
    } catch (reason) {
      setError(describeError(reason, "The update could not be installed."));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="update-banner update-banner-blocked surface" role="alert">
      <div className="update-banner-copy">
        <strong>Murmela {status.version} couldn't be installed</strong>
        <p className="muted">
          This Mac account isn't allowed to change apps in the Applications folder. That's common on company Macs.
          Choose one of these:
        </p>
      </div>
      <div className="update-choices">
        <div className="update-choice">
          <FoldersPicture />
          <strong>Install it in your own Applications folder</strong>
          <p className="muted">
            Your home folder has an Applications folder of its own, which you may change. Murmela puts the new
            version there and opens it. Future updates then install by themselves. You can delete the old copy
            later, or ask your IT team to.
          </p>
          <button
            type="button"
            className="button button-primary"
            onClick={() => void run("home", installUpdateInHomeApplications)}
            disabled={busy !== null}
            aria-busy={busy === "home" || undefined}
          >
            Install in my Applications folder
          </button>
        </div>
        <div className="update-choice">
          <PasswordPicture />
          <strong>Get admin rights, then try again</strong>
          <p className="muted">
            If your company lets you request admin rights, for example with Privileges or Self Service, do that
            first. macOS then asks for your password, and Murmela updates where it is.
          </p>
          <button
            type="button"
            className="button button-secondary"
            onClick={() => void run("retry", installUpdateAndRestart)}
            disabled={busy !== null}
            aria-busy={busy === "retry" || undefined}
          >
            Try again
          </button>
        </div>
      </div>
      {error ? <p className="warning-text">{error}</p> : null}
    </div>
  );
}

/** Two folders: the locked system Applications folder and the user's own. */
function FoldersPicture() {
  return (
    <svg className="update-picture" viewBox="0 0 220 64" role="img" aria-label="The Applications folder needs admin rights; the one in your home folder doesn't">
      <g>
        <path d="M8 14h22l5 5h33v33H8z" className="update-picture-folder is-locked" />
        <rect x="31" y="28" width="14" height="11" rx="2" className="update-picture-lock" />
        <path d="M34 28v-3a4 4 0 0 1 8 0v3" className="update-picture-stroke" />
        <text x="38" y="62" textAnchor="middle" className="update-picture-label">Applications</text>
      </g>
      <path d="M88 33h40m-6-5 6 5-6 5" className="update-picture-stroke" />
      <g>
        <path d="M146 14h22l5 5h33v33h-60z" className="update-picture-folder" />
        <path d="M168 33l5 5 9-10" className="update-picture-check" />
        <text x="176" y="62" textAnchor="middle" className="update-picture-label">~/Applications</text>
      </g>
    </svg>
  );
}

/** A sketch of macOS's administrator prompt. */
function PasswordPicture() {
  return (
    <svg className="update-picture" viewBox="0 0 220 64" role="img" aria-label="macOS asks for an administrator's password">
      <rect x="40" y="4" width="140" height="56" rx="8" className="update-picture-folder" />
      <rect x="54" y="14" width="18" height="15" rx="3" className="update-picture-lock" />
      <path d="M58 14v-3a5 5 0 0 1 10 0v3" className="update-picture-stroke" />
      <rect x="80" y="14" width="88" height="5" rx="2.5" className="update-picture-line" />
      <rect x="80" y="23" width="60" height="4" rx="2" className="update-picture-line" />
      <rect x="54" y="37" width="114" height="9" rx="3" className="update-picture-field" />
      <text x="60" y="44" className="update-picture-label">••••••••</text>
      <rect x="136" y="50" width="32" height="6" rx="3" className="update-picture-button" />
    </svg>
  );
}

function RenewalBanner({ status, onLater }: { status: UpdateStatus; onLater: (version: string | null) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const until = status.proUpdatesUntil ? formatCalendarDay(status.proUpdatesUntil) : null;

  async function installAsFree() {
    setBusy(true);
    setError(null);
    try {
      await downloadUpdateAsFree();
    } catch (reason) {
      setError(describeError(reason, "The update could not be downloaded."));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="update-banner surface" role="status">
      <div>
        <strong>Murmela {status.version} is out</strong>
        <p className="muted">
          {until ? `Your Pro updates ended on ${until}, so ` : "Your Pro updates have ended, so "}Pro features in{" "}
          {status.version} need a renewal or Lifetime. The version you have keeps Pro for good.
        </p>
        {error ? <p className="warning-text">{error}</p> : null}
      </div>
      <div className="update-banner-actions">
        <button type="button" className="button button-ghost" onClick={() => onLater(status.version)} disabled={busy}>
          Later
        </button>
        <button
          type="button"
          className="button button-secondary"
          onClick={() => void installAsFree()}
          disabled={busy}
          aria-busy={busy || undefined}
        >
          {busy ? "Downloading…" : "Install as Free"}
        </button>
        <button type="button" className="button button-primary" onClick={() => void openProPage()} disabled={busy}>
          Renew
        </button>
      </div>
    </div>
  );
}
