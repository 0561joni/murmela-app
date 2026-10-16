import { formatClock } from "./formatting";
import type { TranscriptSegment, TranscriptSpeaker } from "../types/domain";
export const speakerMap = (speakers: TranscriptSpeaker[]) =>
  new Map(speakers.map((s) => [s.speakerId, s]));
export function speakerLabel(
  segment: TranscriptSegment,
  speakers: Map<string, TranscriptSpeaker>,
): string {
  const name = (id: string) =>
    speakers.get(id)?.displayName ?? "Unknown speaker";
  const ids = segment.speakerIds ?? [];
  switch (segment.speakerAttribution) {
    case "assigned":
      return segment.speakerId ? name(segment.speakerId) : "Unknown speaker";
    case "likely":
      return segment.speakerId
        ? `${name(segment.speakerId)} (likely)`
        : "Likely speaker";
    case "overlap":
      return ids.length ? ids.map(name).join(" + ") : "Overlapping speakers";
    case "uncertain":
      return ids.length
        ? `Uncertain: ${ids.map(name).join(" / ")}`
        : "Uncertain speaker";
    default:
      return speakers.size ? "Unknown speaker" : "";
  }
}
export function needsSpeakerReview(
  segment: TranscriptSegment,
  manual: Set<string>,
): boolean {
  return !manual.has(segment.id) && segment.speakerAttribution !== "assigned";
}
export function timestamp(ms: number): string {
  return formatClock(ms);
}
