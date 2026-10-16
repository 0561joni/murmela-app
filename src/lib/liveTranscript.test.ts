import { describe, expect, it } from "vitest";
import { splitLiveTranscript } from "./liveTranscript";

describe("live transcript styling boundary", () => {
  it.each([
    ["", "Grö", "", "Grö"],
    ["Größe", " über", "Größe", " über"],
    ["Cafe", "\u0301 now", "Caf", "e\u0301 now"],
    ["Emoji 👩", "\u200d💻", "Emoji ", "👩\u200d💻"],
    ["Flag 🇩", "🇪", "Flag ", "🇩🇪"],
    ["done", "", "done", ""],
  ])("preserves exact bytes across %s / %s", (committed, tentative, stableStyle, draftStyle) => {
    const split = splitLiveTranscript(committed, tentative);
    expect(split).toEqual({ committed: stableStyle, tentative: draftStyle });
    expect(split.committed + split.tentative).toBe(committed + tentative);
  });
});
