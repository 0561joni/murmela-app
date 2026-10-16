import { useState } from "react";
import { downloadUpdateAsFree, installUpdateAndRestart, openProPage } from "../lib/api";
import { describeError, formatCalendarDay } from "../lib/formatting";
import type { UpdateStatus } from "../types/domain";

/** Release headlines usually have no full stop; the banner reads them as a sentence. */
function sentence(text: string) {
  const trimmed = text.trim();
  return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

/** Offers a downloaded release. Dismissing it keeps the update for the next quit. A release
 * that the Pro key doesn't cover isn't downloaded; the banner offers renewing or installing it as Free. */
export function UpdateBanner({ status }: { status: UpdateStatus | null }) {
  const [dismissedVersion, setDismissedVersion] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!status?.version || dismissedVersion === status.version) return null;
  if (status.state === "needs_renewal") return <RenewalBanner status={status} onLater={setDismissedVersion} />;
  if (status.state !== "ready") return null;

  async function restart() {
    setBusy(true);
    setError(null);
    try {
      await installUpdateAndRestart();
    } catch (reason) {
      setError(describeError(reason, "The update could not be installed."));
      setBusy(false);
    }
  }

  return (
    <div className="update-banner surface" role="status">
      <div>
        <strong>Murmela {status.version} is ready</strong>
        <p className="muted">
          {status.notes ? `${sentence(status.notes)} ` : ""}It installs when you restart or quit Murmela.
        </p>
        {error ? <p className="warning-text">{error}</p> : null}
      </div>
      <div className="update-banner-actions">
        <button
          type="button"
          className="button button-ghost"
          onClick={() => setDismissedVersion(status.version)}
          disabled={busy}
        >
          Later
        </button>
        <button
          type="button"
          className="button button-primary"
          onClick={() => void restart()}
          disabled={busy}
          aria-busy={busy || undefined}
        >
          Restart to update
        </button>
      </div>
    </div>
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
