import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { applyAppearance } from "../lib/appearance";
import { Murmelmeter } from "./Murmelmeter";

const stats = {
  todayWords: 312,
  todayDictations: 9,
  totalWords: 18_420,
  totalDictations: 611,
  totalDurationMs: 7_800_000,
  streakDays: 6,
};

describe("Murmelmeter card", () => {
  afterEach(() => applyAppearance({ appearance: "system", motionPreference: "system", seriousMode: false }));

  it("shows words, time saved and the streak with a comparison", () => {
    render(<Murmelmeter stats={stats} />);
    expect(screen.getByRole("heading", { name: "Murmelmeter" })).toBeTruthy();
    expect(screen.getByText("18,420")).toBeTruthy();
    expect(screen.getByText("5.5 h")).toBeTruthy();
    expect(screen.getByText(/The Little Prince/)).toBeTruthy();
  });

  it("invites the first dictation and disappears in Serious mode", () => {
    render(<Murmelmeter stats={{ ...stats, todayWords: 0, totalWords: 0, totalDurationMs: 0, streakDays: 0 }} />);
    expect(screen.getByText(/wakes up with your first dictation/)).toBeTruthy();
    act(() => applyAppearance({ appearance: "system", motionPreference: "system", seriousMode: true }));
    expect(screen.queryByRole("heading", { name: "Murmelmeter" })).toBeNull();
  });
});
