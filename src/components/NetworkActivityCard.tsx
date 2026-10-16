import { useEffect, useState } from "react";
import { clearNetworkActivity, exportNetworkActivity, getNetworkActivity } from "../lib/api";
import { describeError, formatBytes, formatDateTime } from "../lib/formatting";
import type { NetworkActivity, NetworkActivityEntry } from "../types/domain";
import { ActionButton } from "./Feedback";

const OUTCOME_LABEL: Record<NetworkActivityEntry["outcome"], string | null> = {
  completed: null,
  failed: "Failed",
  canceled: "Canceled",
};

/** The log as plain text: the allowed destinations, then every connection, newest first. */
export function networkActivityText(activity: NetworkActivity, exportedAt: Date = new Date()) {
  const names = new Map(activity.destinations.map((destination) => [destination.id, destination.name]));
  const lines = [
    "Murmela network activity",
    `Exported ${formatDateTime(exportedAt.toISOString())}`,
    "",
    "Places Murmela can contact (it refuses any other address):",
    ...activity.destinations.map((destination) => `- ${destination.name} (${destination.host}): ${destination.reason}`),
    "",
    `Connections made (${activity.entries.length}):`,
  ];
  if (activity.entries.length === 0) lines.push("None since the log started or was last cleared.");
  for (const entry of activity.entries) {
    const outcome = OUTCOME_LABEL[entry.outcome];
    lines.push(
      [
        formatDateTime(entry.startedAt),
        `${names.get(entry.destination) ?? entry.host} (${entry.host})`,
        entry.purpose + (outcome ? ` [${outcome}]` : ""),
        entry.bytes === null ? "size not reported" : formatBytes(entry.bytes),
      ].join(" · "),
    );
  }
  return lines.join("\n") + "\n";
}

/** Settings → General: the only places Murmela can contact, and every connection it actually made. */
export function NetworkActivityCard() {
  const [activity, setActivity] = useState<NetworkActivity | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [savedTo, setSavedTo] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    getNetworkActivity()
      .then((next) => current && setActivity(next))
      .catch((reason) => current && setError(describeError(reason, "The network activity log could not be read.")));
    return () => {
      current = false;
    };
  }, []);

  async function exportLog() {
    if (!activity) return;
    const day = new Date().toISOString().slice(0, 10);
    setSavedTo(await exportNetworkActivity(`Murmela network activity ${day}.txt`, networkActivityText(activity)));
  }

  async function clear() {
    await clearNetworkActivity();
    setActivity((previous) => (previous ? { ...previous, entries: [] } : previous));
  }

  const names = new Map(activity?.destinations.map((destination) => [destination.id, destination.name]));
  const entries = activity?.entries ?? [];

  return (
    <article className="glass-subtle settings-card settings-card-wide" aria-label="Network activity">
      <h3>Network activity</h3>
      <p className="muted">
        Murmela works offline. These are the only places it can ever contact, and it refuses any other address.
      </p>
      {error ? <p className="warning-text">{error}</p> : null}
      {activity === null && !error ? <p className="muted">Loading…</p> : null}
      {activity ? (
        <>
          <ul className="network-destinations" aria-label="Places Murmela can contact">
            {activity.destinations.map((destination) => (
              <li key={destination.id}>
                <div className="network-destination-head">
                  <span>{destination.name}</span>
                  <code className="network-host">{destination.host}</code>
                </div>
                <p className="muted">{destination.reason}</p>
              </li>
            ))}
          </ul>
          <p className="setting-title">Connections made</p>
          {entries.length === 0 ? (
            <p className="muted network-log-empty">No connections since the log started or was last cleared.</p>
          ) : (
            <div className="network-log" tabIndex={0} role="region" aria-label="Connection log">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Date</th>
                    <th scope="col">Destination</th>
                    <th scope="col">Purpose</th>
                    <th scope="col" className="network-log-size">Size</th>
                  </tr>
                </thead>
                <tbody>
                  {entries.map((entry) => {
                    const outcome = OUTCOME_LABEL[entry.outcome];
                    return (
                      <tr key={entry.id}>
                        <td>{formatDateTime(entry.startedAt)}</td>
                        <td title={entry.host}>{names.get(entry.destination) ?? entry.host}</td>
                        <td>
                          {entry.purpose}
                          {outcome ? <span className="network-log-outcome"> · {outcome}</span> : null}
                        </td>
                        <td className="network-log-size">
                          {entry.bytes === null ? (
                            <span title="The updater doesn't report this size">—</span>
                          ) : (
                            formatBytes(entry.bytes)
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </>
      ) : null}
      <div className="setting-row">
        <div className="setting-copy">
          <p className="muted">No analytics, no crash reporting, no telemetry. Recordings and text never leave this computer.</p>
          {savedTo ? <p className="muted" role="status">Saved to {savedTo}</p> : null}
        </div>
        <div className="setting-control network-log-actions">
          <ActionButton action={exportLog} success="" disabled={activity === null}>
            Export…
          </ActionButton>
          <ActionButton action={clear} success="Cleared" disabled={entries.length === 0}>
            Clear log
          </ActionButton>
        </div>
      </div>
    </article>
  );
}
