import { useEffect, useState } from "react";
import { useLicense } from "../hooks/useLicense";
import { openProPage } from "../lib/api";
import { describeError } from "../lib/formatting";
import { ProTag } from "./ProHint";

function formatDate(iso: string | null) {
  if (!iso) return "";
  const date = new Date(iso);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
}

export function LicenseCard() {
  const { status, linkActivation, activate, deactivate } = useLicense();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"activate" | "deactivate" | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const linkBusy = linkActivation?.state === "activating";
  const shownError = error ?? (linkActivation?.state === "failed" ? linkActivation.message : null);

  // A link that failed (e.g. offline) leaves its key in the field, ready to try again.
  useEffect(() => {
    if (linkActivation?.state === "failed") setKey(linkActivation.key);
  }, [linkActivation]);

  async function run(action: "activate" | "deactivate") {
    setBusy(action);
    setError(null);
    try {
      if (action === "activate") {
        await activate(key);
        setKey("");
      } else {
        await deactivate();
        setConfirming(false);
      }
    } catch (reason) {
      setError(describeError(reason, "Something went wrong. Please try again."));
    } finally {
      setBusy(null);
    }
  }

  if (!status) {
    return <article className="glass-subtle settings-card settings-card-wide" aria-label="License" aria-busy />;
  }

  if (status.pro) {
    return (
      <article className="glass-subtle settings-card settings-card-wide" aria-label="License">
        <h3>
          Murmela Pro <ProTag />
        </h3>
        <div className="setting-row">
          <div className="setting-copy">
            <p className="setting-title">
              {status.licensedTo ? `Licensed to ${status.licensedTo}` : "Pro is active on this Mac"}
            </p>
            <p className="muted">
              Key {status.keyHint}
              {status.activatedAt ? ` · activated ${formatDate(status.activatedAt)}` : ""}. Thank you for supporting
              Murmela. Pro works offline and never checks in again.
            </p>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-copy">
            <p className="setting-title">Move to another Mac</p>
            <p className="muted">
              Your key works on 3 Macs. Deactivating frees this Mac's slot, and this Mac goes back to the free
              version. This needs the internet.
            </p>
            {error ? (
              <p className="warning-text" role="alert">
                {error}
              </p>
            ) : null}
          </div>
          <div className="setting-control">
            {confirming ? (
              <>
                <button
                  type="button"
                  className="button button-secondary"
                  onClick={() => setConfirming(false)}
                  disabled={busy !== null}
                >
                  Keep Pro
                </button>
                <button
                  type="button"
                  className="button button-danger"
                  onClick={() => void run("deactivate")}
                  disabled={busy !== null}
                  aria-busy={busy === "deactivate" || undefined}
                >
                  {busy === "deactivate" ? "Deactivating…" : "Deactivate"}
                </button>
              </>
            ) : (
              <button type="button" className="button button-secondary" onClick={() => setConfirming(true)}>
                Deactivate this Mac
              </button>
            )}
          </div>
        </div>
      </article>
    );
  }

  return (
    <article className="glass-subtle settings-card settings-card-wide" aria-label="License">
      <h3>Murmela Free</h3>
      {linkBusy ? (
        <p className="setting-title" role="status" aria-busy>
          Activating Murmela Pro from your link…
        </p>
      ) : null}
      <p className="muted">
        Dictation, translation and file transcription are free, for good. Pro adds speaker labels, document
        translation and word alternatives, and supports the work on Murmela.
      </p>
      <form
        className="license-form"
        onSubmit={(event) => {
          event.preventDefault();
          void run("activate");
        }}
      >
        <label className="setting-title" htmlFor="license-key">
          License key
        </label>
        <div className="license-input-row">
          <input
            id="license-key"
            value={key}
            onChange={(event) => setKey(event.target.value)}
            placeholder="XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX"
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="characters"
            spellCheck={false}
            disabled={busy !== null || linkBusy}
          />
          <button
            type="submit"
            className="button button-primary"
            disabled={busy !== null || linkBusy || key.trim() === ""}
            aria-busy={busy === "activate" || undefined}
          >
            {busy === "activate" ? "Activating…" : "Activate"}
          </button>
        </div>
        {shownError ? (
          <p className="warning-text" role="alert">
            {shownError}
          </p>
        ) : null}
        <p className="muted license-fineprint">
          The key is in your receipt email from Lemon Squeezy. Activating sends only the key and an anonymous device
          label, once. After that Pro works offline.{" "}
          <button type="button" className="link-button" onClick={() => void openProPage()}>
            Get Murmela Pro
          </button>
        </p>
      </form>
    </article>
  );
}
