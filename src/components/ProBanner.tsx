import { useState } from "react";
import { useLicense } from "../hooks/useLicense";
import { openProPage } from "../lib/api";
import { formatCalendarDay } from "../lib/formatting";
import { openLicenseSettings } from "./ProHint";

/** A slim invitation to Murmela Pro on the free version. Closing it hides it until the next launch. */
export function ProBanner({ hidden = false }: { hidden?: boolean }) {
  const { status } = useLicense();
  const [closed, setClosed] = useState(false);

  // Wait for the license status, so Pro users never see it flash.
  if (hidden || closed || !status || status.pro) return null;
  // A Pro key whose update year ended before this version came out.
  const ended = status.tier === "pro" && status.updatesUntil ? formatCalendarDay(status.updatesUntil) : null;

  return (
    <div className="pro-banner" role="region" aria-label="Murmela Pro">
      {ended ? (
        <p>
          <strong>Pro is off in this version.</strong>{" "}
          <span className="muted">
            Your Pro updates ended on {ended}, before this version came out. Renew Pro for another year or upgrade to
            Lifetime to turn it back on.
          </span>
        </p>
      ) : (
        <p>
          <strong>Get the most out of Murmela with Pro.</strong>{" "}
          <span className="muted">
            Speaker labels, document translation and word alternatives. Pay once, and support independent, private
            software.
          </span>
        </p>
      )}
      <div className="pro-banner-actions">
        <button type="button" className="link-button" onClick={openLicenseSettings}>
          {ended ? "Enter a new key" : "I have a key"}
        </button>
        <button type="button" className="button button-primary pro-banner-cta" onClick={() => void openProPage()}>
          {ended ? "Renew" : "Get Pro"}
        </button>
        <button
          type="button"
          className="pro-banner-close"
          aria-label="Hide until next launch"
          title="Hide until next launch"
          onClick={() => setClosed(true)}
        >
          ×
        </button>
      </div>
    </div>
  );
}
