import { useState } from "react";
import { installUpdateAndRestart } from "../lib/api";
import { describeError } from "../lib/formatting";
import type { UpdateStatus } from "../types/domain";

/** Release headlines usually have no full stop; the banner reads them as a sentence. */
function sentence(text: string) {
  const trimmed = text.trim();
  return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

/** Offers a downloaded release. Dismissing it keeps the update for the next quit. */
export function UpdateBanner({ status }: { status: UpdateStatus | null }) {
  const [dismissedVersion, setDismissedVersion] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (status?.state !== "ready" || !status.version || dismissedVersion === status.version) return null;

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
