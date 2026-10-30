import { useEffect, useState, type ReactNode } from "react";
import { ActionButton } from "./Feedback";
import { HelpHint } from "./HelpHint";
import { IconButton } from "./IconButton";
import { formatShortcutForDisplay } from "../lib/formatting";
import {
  DOUBLE_COPY,
  describeApps,
  pickApp,
  systemTranslationLanguage,
  type AppInfo,
} from "../lib/quickTranslateApi";
import { TRANSLATION_LANGUAGES, languageName } from "../lib/textTranslation";
import type { AppSettings } from "../types/domain";

/** A suggestion when switching from ⌘C twice to a key combination. */
const SUGGESTED_SHORTCUT = "Ctrl+Alt+T";

/**
 * Settings → Languages: translating selected text in any app with ⌘C twice
 * (or a key combination), where the translation appears, the user's own
 * language, and apps where ⌘C twice stays an ordinary copy.
 */
export function QuickTranslateCard({
  settings,
  isSaving,
  hasEngine,
  capturing,
  onChange,
  onCaptureShortcut,
  onCancelCapture,
  feedback,
}: {
  settings: AppSettings;
  isSaving: boolean;
  /** A translation engine is installed. */
  hasEngine: boolean;
  /** The key-combination field is waiting for keys. */
  capturing: boolean;
  onChange: <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => Promise<void>;
  onCaptureShortcut: () => Promise<void>;
  onCancelCapture: () => Promise<void>;
  feedback: (key: keyof AppSettings) => ReactNode;
}) {
  const [systemLanguage, setSystemLanguage] = useState<string | null>(null);
  const [apps, setApps] = useState<AppInfo[]>([]);
  const ignored = settings.quickTranslateIgnoredApps ?? [];
  const enabled = settings.quickTranslateEnabled ?? true;
  const shortcut = settings.quickTranslateShortcut ?? DOUBLE_COPY;
  const doubleCopy = shortcut === DOUBLE_COPY;

  useEffect(() => {
    void systemTranslationLanguage().then(setSystemLanguage).catch(() => setSystemLanguage(null));
  }, []);

  useEffect(() => {
    let current = true;
    void describeApps(ignored)
      .then((described) => {
        if (current) setApps(described);
      })
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, [ignored.join(",")]); // eslint-disable-line react-hooks/exhaustive-deps

  async function addApp() {
    const app = await pickApp();
    if (app && !ignored.includes(app.bundleId)) {
      await onChange("quickTranslateIgnoredApps", [...ignored, app.bundleId]);
    }
  }

  return (
    <article className="glass-subtle settings-card settings-card-wide quick-translate-settings" aria-label="Translate selected text">
      <h3>
        Translate selected text
        <HelpHint title="How does ⌘C twice work?" topic="translating selected text">
          <p>
            Select text in any app, hold <kbd>⌘</kbd> and press <kbd>C</kbd> twice. A small window opens by the pointer with
            the translation. <strong>Replace</strong> puts it in place of your selection, <strong>Copy</strong> copies it,
            and <strong>Edit in Murmela</strong> opens it on the Translate screen.
          </p>
          <p>
            Murmela notices the second copy on the clipboard, so this needs no permission. Only Replace asks for
            Accessibility, the first time you use it. Nothing leaves your Mac.
          </p>
        </HelpHint>
      </h3>
      <p>Select text in any app and press <kbd>⌘</kbd> <kbd>C</kbd> <kbd>C</kbd>. The translation opens right there, ready to copy or put in place of your selection.</p>
      {!hasEngine ? <p className="warning-text">Download a translation engine above first. Until then, ⌘C twice stays a normal copy.</p> : null}

      <div className="setting-row">
        <div className="setting-copy">
          <p className="setting-title">On in every app</p>
          <p className="muted">Text from another language goes into yours; text in your language goes into the last language you picked.</p>
        </div>
        <div className="setting-control">
          <button
            disabled={isSaving}
            aria-label="Translate selected text"
            type="button"
            className={enabled ? "switch-button is-on" : "switch-button"}
            aria-pressed={enabled}
            onClick={() => void onChange("quickTranslateEnabled", !enabled)}
          >
            <span className="switch-thumb" />
          </button>
        </div>
        {feedback("quickTranslateEnabled")}
      </div>

      <div className="field-stack">
        <span>Shortcut</span>
        <div className="segmented-control" role="group" aria-label="Shortcut for translating selected text">
          <button
            type="button"
            aria-pressed={doubleCopy}
            disabled={isSaving || !enabled}
            onClick={() => void onChange("quickTranslateShortcut", DOUBLE_COPY)}
          >
            ⌘C twice
          </button>
          <button
            type="button"
            aria-pressed={!doubleCopy}
            disabled={isSaving || !enabled}
            onClick={() => {
              if (doubleCopy) void onChange("quickTranslateShortcut", SUGGESTED_SHORTCUT);
            }}
          >
            Key combination
          </button>
        </div>
        {doubleCopy ? (
          <p className="muted">Like DeepL. Needs no permission.</p>
        ) : (
          <>
            <div className="shortcut-field">
              <span className="shortcut-display">
                {capturing ? "Press a shortcut… Esc to cancel" : formatShortcutForDisplay(shortcut)}
              </span>
              <ActionButton disabled={capturing || isSaving || !enabled} action={onCaptureShortcut}>
                Set shortcut
              </ActionButton>
              {capturing ? <ActionButton action={onCancelCapture}>Cancel capture</ActionButton> : null}
            </div>
            <p className="muted">Copies the selection for you and puts your clipboard back afterwards. Needs Accessibility.</p>
          </>
        )}
        {feedback("quickTranslateShortcut")}
      </div>

      <div className="quick-translate-fields">
        <label className="field-stack">
          <span>Show the translation in</span>
          <select
            value={settings.quickTranslateDisplay ?? "popup"}
            disabled={isSaving || !enabled}
            onChange={(event) => void onChange("quickTranslateDisplay", event.target.value as AppSettings["quickTranslateDisplay"])}
          >
            <option value="popup">A small window by the pointer</option>
            <option value="app">Murmela’s Translate screen</option>
          </select>
          {feedback("quickTranslateDisplay")}
        </label>
        <label className="field-stack">
          <span>Your language</span>
          <select
            value={settings.quickTranslateMyLanguage ?? "auto"}
            disabled={isSaving || !enabled}
            onChange={(event) => void onChange("quickTranslateMyLanguage", event.target.value === "auto" ? null : event.target.value)}
          >
            <option value="auto">{systemLanguage ? `Automatic (${languageName(systemLanguage)}, from macOS)` : "Automatic (from macOS)"}</option>
            {TRANSLATION_LANGUAGES.map((language) => (
              <option key={language.code} value={language.code}>{language.name}</option>
            ))}
          </select>
          {feedback("quickTranslateMyLanguage")}
        </label>
      </div>

      <div className="setting-row">
        <div className="setting-copy">
          <p className="setting-title">Keep quick translations in the Library</p>
          <p className="muted">
            {settings.saveHistory
              ? "Off by default, so quick lookups don't crowd your Library."
              : "Turn on Save transcripts to the Library under General first."}
          </p>
        </div>
        <div className="setting-control">
          <button
            disabled={isSaving || !enabled || !settings.saveHistory}
            aria-label="Keep quick translations in the Library"
            type="button"
            className={settings.quickTranslateSaveHistory ? "switch-button is-on" : "switch-button"}
            aria-pressed={settings.quickTranslateSaveHistory ?? false}
            onClick={() => void onChange("quickTranslateSaveHistory", !settings.quickTranslateSaveHistory)}
          >
            <span className="switch-thumb" />
          </button>
        </div>
        {feedback("quickTranslateSaveHistory")}
      </div>

      <div className="field-stack">
        <span>Apps where ⌘C twice stays a normal copy</span>
        <ul className="quick-translate-apps" aria-label="Ignored apps">
          {ignored.map((bundleId) => {
            const name = apps.find((app) => app.bundleId === bundleId)?.name;
            return (
              <li key={bundleId} className={name ? undefined : "is-missing"}>
                <span title={bundleId}>{name ?? bundleId}</span>
                <IconButton
                  icon="xmark"
                  size="compact"
                  label={`Stop ignoring ${name ?? bundleId}`}
                  disabled={isSaving}
                  onClick={() => void onChange("quickTranslateIgnoredApps", ignored.filter((id) => id !== bundleId))}
                />
              </li>
            );
          })}
        </ul>
        <div>
          <ActionButton disabled={isSaving} action={addApp}>Add app…</ActionButton>
        </div>
        <p className="muted">Password managers are on this list from the start. Copies they mark as secret are never translated anywhere.</p>
        {feedback("quickTranslateIgnoredApps")}
      </div>
    </article>
  );
}
