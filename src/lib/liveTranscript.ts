const segmenter = typeof Intl.Segmenter === "function"
  ? new Intl.Segmenter(undefined, { granularity: "grapheme" })
  : null;

/** Move only the visual boundary; the committed/draft strings remain exact. */
export function splitLiveTranscript(committed: string, tentative: string) {
  if (!committed || !tentative) return { committed, tentative };
  const combined = committed + tentative;
  // Older webviews must not split a Unicode grapheme across styles.
  if (!segmenter) return { committed: "", tentative: combined };
  const crossing = segmenter.segment(combined).containing(committed.length - 1);
  if (crossing && crossing.index + crossing.segment.length > committed.length) {
    return { committed: combined.slice(0, crossing.index), tentative: combined.slice(crossing.index) };
  }
  return { committed, tentative };
}
