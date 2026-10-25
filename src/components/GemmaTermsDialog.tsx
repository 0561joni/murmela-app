import { useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";

export const GEMMA_TERMS_URL = "https://ai.google.dev/gemma/terms";
export const GEMMA_PROHIBITED_USE_URL = "https://ai.google.dev/gemma/prohibited_use_policy";
/** Required notice text from the Gemma Terms of Use, section 3.1. */
export const GEMMA_NOTICE =
  "Gemma is provided under and subject to the Gemma Terms of Use found at ai.google.dev/gemma/terms";

/**
 * Shown before TranslateGemma is downloaded. Accepting records the user's
 * agreement (the backend refuses the download without it), which is how the
 * Gemma use restrictions become part of the user's terms with Murmela.
 */
export function GemmaTermsDialog({
  onAccept,
  onCancel,
}: {
  onAccept: () => Promise<void>;
  onCancel: () => void;
}) {
  const titleId = useId();
  const checkboxRef = useRef<HTMLInputElement>(null);
  const [agreed, setAgreed] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    checkboxRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onCancel();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [busy, onCancel]);

  function trapFocus(event: ReactKeyboardEvent<HTMLDivElement>) {
    if (event.key !== "Tab") return;
    const focusable = Array.from(
      event.currentTarget.querySelectorAll<HTMLElement>("button:not([disabled]), [href], input:not([disabled])"),
    );
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  async function accept() {
    setBusy(true);
    try {
      await onAccept();
    } finally {
      setBusy(false);
    }
  }

  // Above the welcome setup too, which asks for these terms while it is open.
  return createPortal(
    <div className="model-info-backdrop is-above-setup">
      <div
        className="model-info-dialog glass-panel terms-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onKeyDown={trapFocus}
      >
        <h2 id={titleId}>Gemma Terms of Use</h2>
        <p className="model-info-description">
          The translation engine, TranslateGemma, is a Google model. It is provided under the Gemma Terms of Use,
          including Google&apos;s Prohibited Use Policy. To download and use it, you need to agree to those terms.
        </p>
        <div className="terms-dialog-summary">
          <strong>In short, you may not use it to</strong>
          <ul>
            <li>break the law or infringe other people&apos;s rights, including copyright</li>
            <li>create spam, scams, phishing or malware</li>
            <li>harass, defame or incite harm against anyone</li>
            <li>create sexually explicit content</li>
            <li>impersonate people, or pass off generated text as human-written to deceive</li>
            <li>make automated decisions about credit, jobs, housing, insurance, healthcare or similar</li>
          </ul>
          <p className="muted">This is a summary. The full terms and policy apply.</p>
        </div>
        <p className="terms-dialog-links">
          <a href={GEMMA_TERMS_URL} target="_blank" rel="noreferrer">Read the Gemma Terms of Use</a>
          <a href={GEMMA_PROHIBITED_USE_URL} target="_blank" rel="noreferrer">Read the Prohibited Use Policy</a>
        </p>
        <p className="muted terms-dialog-notice">
          {GEMMA_NOTICE}. Murmela is not affiliated with or endorsed by Google.
        </p>
        <label className="terms-dialog-agree">
          <input
            ref={checkboxRef}
            type="checkbox"
            checked={agreed}
            onChange={(event) => setAgreed(event.target.checked)}
          />
          <span>I agree to the Gemma Terms of Use and the Gemma Prohibited Use Policy.</span>
        </label>
        <div className="terms-dialog-actions">
          <button type="button" className="button button-ghost" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
          <button
            type="button"
            className="button button-primary"
            onClick={() => void accept()}
            disabled={!agreed || busy}
            aria-busy={busy || undefined}
          >
            Agree and download
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
