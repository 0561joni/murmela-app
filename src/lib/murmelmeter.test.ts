import { describe, expect, it } from "vitest";
import {
  LISTENING_LINES,
  crossedMilestone,
  formatMinutesSaved,
  minutesSaved,
  pickLine,
  wordComparison,
} from "./murmelmeter";

describe("Murmelmeter", () => {
  it("counts time saved against typing at 40 words per minute", () => {
    expect(minutesSaved(400, 60_000)).toBe(9);
    expect(minutesSaved(10, 600_000)).toBe(0);
    expect(formatMinutesSaved(0.4)).toBe("under a minute");
    expect(formatMinutesSaved(42.4)).toBe("42 min");
    expect(formatMinutesSaved(150)).toBe("2.5 h");
  });

  it("compares the total with familiar texts", () => {
    expect(wordComparison(0)).toMatch(/first dictation/);
    expect(wordComparison(8_000)).toMatch(/short story/);
    expect(wordComparison(600_000)).toMatch(/Tolstoy/);
  });

  it("celebrates a milestone only when a dictation crosses it", () => {
    expect(crossedMilestone(9_990, 10_020)).toMatch(/^10,000 words/);
    expect(crossedMilestone(10_020, 10_400)).toBeNull();
    // Crossing two at once (e.g. after an import) names the larger one.
    expect(crossedMilestone(900, 10_500)).toMatch(/^10,000 words/);
  });

  it("picks a stable line per seed", () => {
    expect(pickLine(LISTENING_LINES, "session-a:1")).toBe(pickLine(LISTENING_LINES, "session-a:1"));
    expect(LISTENING_LINES).toContain(pickLine(LISTENING_LINES, null));
  });
});
