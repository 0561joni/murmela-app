import { useEffect, useState } from "react";
import { getGlobeKeyAction, openKeyboardSettings, type GlobeKeyAction } from "../lib/api";

const WHAT_MACOS_DOES: Record<Exclude<GlobeKeyAction, "do_nothing" | "unknown">, string> = {
  show_emoji: "opens the emoji picker",
  change_input_source: "switches the keyboard language",
  start_dictation: "starts Apple Dictation",
};

/**
 * With the 🌐 key as the dictation shortcut, macOS still reacts to a tap of it.
 * Suggests "Press 🌐 key to: Do Nothing" and links to Keyboard settings; it
 * never changes the system setting itself. Re-checks when the window regains focus.
 */
export function GlobeKeyNotice({ className = "" }: { className?: string }) {
  const [action, setAction] = useState<GlobeKeyAction | null>(null);
  useEffect(() => {
    let current = true;
    const check = () => {
      getGlobeKeyAction()
        .then((value) => current && setAction(value))
        .catch(() => undefined);
    };
    check();
    window.addEventListener("focus", check);
    return () => {
      current = false;
      window.removeEventListener("focus", check);
    };
  }, []);
  if (!action || action === "do_nothing" || action === "unknown") return null;
  return (
    <div className={`globe-key-notice ${className}`.trim()} role="note">
      <p>
        <strong>One setting to change:</strong> a tap on 🌐 also {WHAT_MACOS_DOES[action]}. In Keyboard settings, set
        “Press 🌐 key to” to <strong>Do Nothing</strong>.
      </p>
      <button type="button" className="button button-secondary" onClick={() => void openKeyboardSettings()}>
        Open Keyboard settings
      </button>
    </div>
  );
}
