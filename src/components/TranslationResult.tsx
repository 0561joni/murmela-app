import { readableError } from "../lib/formatting";
import { useEffect, useState } from "react";
import { ActionButton } from "./Feedback";
import { copyTextToClipboard } from "../lib/api";
import { outputLanguageLabel } from "../lib/translationApi";
import type { DictationOutput } from "../types/domain";

export function TranslationResult({ output, onRetry }: {
  output: DictationOutput;
  onRetry?: () => Promise<void>;
}) {
  const [original, setOriginal] = useState(false);
  useEffect(() => setOriginal(false), [output.sessionId, output.status]);
  const ready = output.status === "completed" && output.outputText !== null;
  return <section className="translation-result" aria-label="Translated dictation">
    <div className="translation-result-heading">
      <div className="segmented-control" role="group" aria-label="Text version">
        <button type="button" aria-pressed={!original} onClick={() => setOriginal(false)}>
          {outputLanguageLabel(output.targetLanguage)}
        </button>
        <button type="button" aria-pressed={original} onClick={() => setOriginal(true)}>Original</button>
      </div>
      {ready ? <ActionButton icon="copy" action={() => copyTextToClipboard(output.outputText!)} success="Copied">Copy translation</ActionButton> : null}
      <ActionButton icon="copy" action={() => copyTextToClipboard(output.sourceText)} success="Copied">Copy original</ActionButton>
    </div>
    {output.errorMessage ? <p role="alert" className="warning-text">{readableError(output.errorMessage)}</p> : null}
    <p className="transcript-body">{original || !ready ? output.sourceText : output.outputText}</p>
    {!ready && output.status !== "pending" && onRetry ? <ActionButton action={onRetry} success="Translation ready">Retry translation</ActionButton> : null}
    {!ready ? <p className="muted">The original is preserved. No translated text was pasted.</p> : null}
  </section>;
}
