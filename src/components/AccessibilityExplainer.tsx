import { useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";

/**
 * Shown before macOS asks for Accessibility access. Apple's own prompt says
 * the app "would like to control this computer", which sounds alarming
 * without context, so Murmela explains what it does with the permission first.
 */
export function AccessibilityExplainer({
  onContinue,
  onCancel,
}: {
  onContinue: () => Promise<void>;
  onCancel: () => void;
}) {
  const titleId = useId();
  const continueRef = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    continueRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onCancel();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [busy, onCancel]);

  function trapFocus(event: ReactKeyboardEvent<HTMLDivElement>) {
    if (event.key !== "Tab") return;
    const focusable = Array.from(event.currentTarget.querySelectorAll<HTMLElement>("button:not([disabled])"));
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

  async function proceed() {
    setBusy(true);
    try {
      await onContinue();
    } finally {
      setBusy(false);
    }
  }

  return createPortal(
    <div className="model-info-backdrop is-above-setup">
      <div
        className="model-info-dialog glass-panel terms-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onKeyDown={trapFocus}
      >
        <h2 id={titleId}>Why Murmela asks for Accessibility</h2>
        <p className="model-info-description">
          To put your words where your cursor is, Murmela needs the macOS permission called Accessibility.
        </p>
        <div className="terms-dialog-summary">
          <strong>Murmela uses it only to</strong>
          <ul>
            <li>notice when you hold your dictation key, such as 🌐</li>
            <li>press ⌘V for you, so the text appears in the app you are using</li>
          </ul>
          <p className="muted">
            It does not read your screen or keep what you type. All other keys pass straight through, and nothing
            leaves your Mac.
          </p>
        </div>
        <p>
          Next, macOS asks whether Murmela may “control this computer”. That is Apple&apos;s wording for this
          permission. Choose <strong>Open System Settings</strong> and switch on Murmela.
        </p>
        <p className="muted terms-dialog-notice">
          You can turn it off any time under Privacy &amp; Security → Accessibility. Without it, Murmela copies the
          text and you paste it yourself.
        </p>
        <div className="terms-dialog-actions">
          <button type="button" className="button button-ghost" onClick={onCancel} disabled={busy}>
            Not now
          </button>
          <button
            ref={continueRef}
            type="button"
            className="button button-primary"
            onClick={() => void proceed()}
            disabled={busy}
            aria-busy={busy || undefined}
          >
            Continue
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
