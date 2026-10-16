import type { ReviewDocument } from "../types/domain";
import { resultFixture } from "./fixtures";
export function reviewFixture(count = 3): ReviewDocument {
  return {
    reference: { kind: "saved", id: "review-1" },
    revision: 1,
    manualSegmentIds: [],
    unmatchedSpeakerIds: [],
    canUndo: false,
    detail: {
      ...resultFixture,
      id: "review-1",
      createdAt: "2026-09-05T12:00:00Z",
      sourceType: "file_upload",
      title: "Weekly planning",
      status: "completed",
      durationMs: count * 6000,
      modelName: "Whisper Small",
      speakerCount: 2,
      transcriptionWarnings: [],
      diarizationStatus: "completed_with_uncertainty",
      diarizationSource: "post_process",
      diarizationModelId: "local-speakers",
      diarizationSpeakerCountHint: 2,
      speakers: [
        { speakerId: "a", displayName: "Maya", speakerOrder: 0 },
        { speakerId: "b", displayName: "Leo", speakerOrder: 1 },
      ],
      segments: Array.from({ length: count }, (_, i) => ({
        ...resultFixture.segments[0],
        id: `passage-${i}`,
        startMs: i * 6000,
        endMs: (i + 1) * 6000,
        segmentOrder: i,
        text: `Passage ${i + 1}: Let’s review this week’s progress and next steps.`,
        speakerId: i === 1 ? null : i % 2 ? "b" : "a",
        speakerIds: i === 1 ? ["a", "b"] : [i % 2 ? "b" : "a"],
        speakerAttribution: i === 1 ? "uncertain" : "assigned",
      })),
    },
  };
}
