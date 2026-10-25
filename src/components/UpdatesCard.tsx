import { useState } from "react";
import { useUpdateStatus } from "../hooks/useUpdateStatus";
import { downloadUpdate } from "../lib/api";
import { describeError, formatCalendarDay } from "../lib/formatting";
import type { UpdateStatus } from "../types/domain";

function describeStatus(status: UpdateStatus | null): string {
  switch (status?.state) {
    case "checking":
      return "Checking for updates…";
    case "available":
      return `Murmela ${status.version ?? ""} is available.`;
    case "downloading":
      return `Downloading Murmela ${status.version ?? ""}${status.progress != null ? ` · ${Math.round(status.progress)} %` : "…"}`;
    case "ready":
      return `Murmela ${status.version ?? ""} is downloaded. It installs when you restart or quit Murmela.`;
    case "cannot_install":
      return `Murmela ${status.version ?? ""} couldn't be installed, because this Mac account may not change the Applications folder. The banner at the top shows what to do.`;
    case "up_to_date":
      return "Murmela is up to date.";
    case "failed":
      return status.message ?? "Murmela could not check for updates.";
    case "needs_renewal":
      return `Murmela ${status.version ?? ""} is out. Your Pro updates ended${
        status.proUpdatesUntil ? ` on ${formatCalendarDay(status.proUpdatesUntil)}` : ""
      }, so Pro features in it need a renewal or Lifetime. Renew Pro, or install it as Free from the banner.`;
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

  const available = status?.state === "available";

  async function checkOrDownload() {
    setError(null);
    try {
      if (available) await downloadUpdate();
      else await checkNow();
    } catch (reason) {
      setError(describeError(reason, available ? "The update could not be downloaded." : "Murmela could not check for updates."));
    }
  }

  return (
    <article className="glass-subtle settings-card settings-card-wide" aria-label="Updates">
      <h3>Updates</h3>
      <div className="setting-row">
        <div className="setting-copy">
          <p className="setting-title">Check for updates automatically</p>
          <p className="muted">
            Murmela asks GitHub every few hours whether a new version exists and tells you. It downloads only when
            you choose to. No recordings, text or personal data are sent.
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
            className={available ? "button button-primary" : "button button-secondary"}
            onClick={() => void checkOrDownload()}
            disabled={busy || status?.state === "ready" || status?.state === "cannot_install"}
            aria-busy={busy || undefined}
          >
            {available ? "Download update" : "Check now"}
          </button>
        </div>
      </div>
    </article>
  );
}
