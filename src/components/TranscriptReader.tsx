import { formatClock } from "../lib/formatting";
import type { TranscriptResult } from "../types/domain";

export function TranscriptReader({ result }: { result: TranscriptResult }) {
  const name = (id: string) =>
    result.speakers.find((speaker) => speaker.speakerId === id)?.displayName ??
    id;
  return (
    <div className="transcript-reader">
      {result.segments.length === 0 ? (
        <p className="transcript-body">{result.plainText}</p>
      ) : (
        result.segments.map((segment) => {
          const label =
            segment.speakerAttribution === "assigned" && segment.speakerId
              ? name(segment.speakerId)
              : segment.speakerAttribution === "likely" && segment.speakerId
                ? name(segment.speakerId) + "?"
                : segment.speakerAttribution === "overlap"
                  ? (segment.speakerIds ?? []).map(name).join(" + ") ||
                    "Overlapping speakers"
                  : segment.speakerAttribution === "uncertain"
                    ? "Uncertain speaker"
                    : "Unknown speaker";
          const speaker = result.speakers.find(
            (item) =>
              item.speakerId === (segment.speakerId ?? segment.speakerIds?.[0]),
          );
          return (
            <div className="transcript-paragraph" key={segment.id}>
              <div className="transcript-paragraph-meta">
                {result.speakers.length > 0 ? (
                  <span
                    className={
                      "speaker-label speaker-color-" +
                      ((speaker?.speakerOrder ?? 0) % 6)
                    }
                  >
                    {label}
                  </span>
                ) : null}
                <span className="speaker-time">
                  {formatClock(segment.startMs)}
                </span>
              </div>
              <p>{segment.text}</p>
            </div>
          );
        })
      )}
      {result.diarizationWarning ? (
        <p className="warning-text">{result.diarizationWarning}</p>
      ) : null}
      {result.warnings?.map((warning, index) => (
        <p className="warning-text" key={index}>
          {warning.reason}
        </p>
      ))}
    </div>
  );
}
