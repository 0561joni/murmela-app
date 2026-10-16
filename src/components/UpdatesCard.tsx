import { useState } from "react";
import { useUpdateStatus } from "../hooks/useUpdateStatus";
import { describeError } from "../lib/formatting";
import type { UpdateStatus } from "../types/domain";

function describeStatus(status: UpdateStatus | null): string {
  switch (status?.state) {
    case "checking":
      return "Checking for updates…";
    case "downloading":
      return `Downloading Murmela ${status.version ?? ""}…`;
    case "ready":
      return `Murmela ${status.version ?? ""} is downloaded. It installs when you restart or quit Murmela.`;
    case "up_to_date":
      return "Murmela is up to date.";
    case "failed":
      return status.message ?? "Murmela could not check for updates.";
    default:
      return "";
  }
}

export function UpdatesCard({
  autoUpdateEnabled,
  saving,
  onToggleAutoUpdate,
}: {
  autoUpdateEnabled: boolean;
  saving: boolean;
  onToggleAutoUpdate: () => void;
}) {
  const { status, checkNow } = useUpdateStatus();
  const [error, setError] = useState<string | null>(null);
  const busy = status?.state === "checking" || status?.state === "downloading";

  async function check() {
    setError(null);
    try {
      await checkNow();
    } catch (reason) {
      setError(describeError(reason, "Murmela could not check for updates."));
    }
  }

  return (
    <article className="glass-subtle settings-card settings-card-wide" aria-label="Updates">
      <h3>Updates</h3>
      <div className="setting-row">
        <div className="setting-copy">
          <p className="setting-title">Check for updates automatically</p>
          <p className="muted">
            Murmela asks GitHub every few hours whether a new version exists and downloads it in the background. No
            recordings, text or personal data are sent.
          </p>
        </div>
        <div className="setting-control">
          <button
            disabled={saving}
            aria-label="Check for updates automatically"
            type="button"
            className={autoUpdateEnabled ? "switch-button is-on" : "switch-button"}
            aria-pressed={autoUpdateEnabled}
            onClick={onToggleAutoUpdate}
          >
            <span className="switch-thumb" />
          </button>
          <span className="setting-state">{autoUpdateEnabled ? "On" : "Off"}</span>
        </div>
      </div>
      <div className="setting-row">
        <div className="setting-copy">
          <p className="setting-title">Version {status?.currentVersion ?? ""}</p>
          <p className="muted" role="status">{describeStatus(status)}</p>
          {error ? <p className="warning-text">{error}</p> : null}
        </div>
        <div className="setting-control">
          <button
            type="button"
            className="button button-secondary"
            onClick={() => void check()}
            disabled={busy || status?.state === "ready"}
            aria-busy={busy || undefined}
          >
            Check now
          </button>
        </div>
      </div>
    </article>
  );
}
