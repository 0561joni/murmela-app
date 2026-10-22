import { afterEach, describe, expect, it } from "vitest";
import { melaFlag, overlayMood, readMelaEnabled, setMelaEnabled, windowMood, type WindowMoodInput } from "./mela";

const rest: WindowMoodInput = {
  dictation: null,
  showingContent: false,
  translating: false,
  filesWorking: false,
  moment: null,
  offline: false,
  egg: false,
  idle: false,
};

describe("Mela moods", () => {
  it("looks away from saved words before anything else", () => {
    expect(windowMood({ ...rest, showingContent: true, dictation: "listening", filesWorking: true })).toBe("private");
  });

  it("puts live dictation ahead of moments and background work", () => {
    expect(windowMood({ ...rest, dictation: "listening", moment: "happy", filesWorking: true })).toBe("listen");
    expect(windowMood({ ...rest, translating: true, filesWorking: true })).toBe("think");
    expect(windowMood({ ...rest, moment: "lang", filesWorking: true })).toBe("lang");
    expect(windowMood({ ...rest, filesWorking: true, idle: true })).toBe("work");
  });

  it("rests last: egg, then offline, then sleep", () => {
    expect(windowMood({ ...rest, egg: true, offline: true })).toBe("egg");
    expect(windowMood({ ...rest, offline: true, idle: true })).toBe("offline");
    expect(windowMood({ ...rest, idle: true })).toBe("sleep");
    expect(windowMood(rest)).toBe("idle");
  });

  it("follows the pill and hides with it", () => {
    expect(overlayMood("listening")).toBe("listen");
    expect(overlayMood("processing")).toBe("think");
    expect(overlayMood("clipboard_only")).toBe("happy");
    expect(overlayMood("failed")).toBe("oops");
    expect(overlayMood("mode")).toBe("lang");
    expect(overlayMood("hidden")).toBe("hidden");
  });

  it("has no flag for the original language", () => {
    expect(melaFlag("original")).toBeNull();
    expect(melaFlag("es-AR")?.sun).toBe(true);
  });
});

describe("Mela preference", () => {
  afterEach(() => localStorage.clear());

  it("is off until switched on, and remembers the choice", () => {
    expect(readMelaEnabled()).toBe(false);
    setMelaEnabled(true);
    expect(readMelaEnabled()).toBe(true);
    setMelaEnabled(false);
    expect(readMelaEnabled()).toBe(false);
  });

  it("keeps Crystal on and turns the removed looks off", () => {
    localStorage.setItem("murmela-companion", "crystal");
    expect(readMelaEnabled()).toBe(true);
    localStorage.setItem("murmela-companion", "clay");
    expect(readMelaEnabled()).toBe(false);
  });
});
