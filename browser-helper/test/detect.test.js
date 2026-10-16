// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";

let detect;

beforeAll(async () => {
  await import("../detect.js");
  detect = globalThis.MurmelaDetect;
});

function page(html) {
  const doc = document.implementation.createHTMLDocument("call");
  doc.body.innerHTML = html;
  return doc;
}

describe("platformFor", () => {
  it("knows the three call apps and nothing else", () => {
    expect(detect.platformFor(new URL("https://meet.google.com/abc-defg-hij"))).toBe("meet");
    expect(detect.platformFor(new URL("https://teams.microsoft.com/v2/"))).toBe("teams");
    expect(detect.platformFor(new URL("https://teams.cloud.microsoft/"))).toBe("teams");
    expect(detect.platformFor(new URL("https://app.zoom.us/wc/123/start"))).toBe("zoom");
    expect(detect.platformFor(new URL("https://zoom.us.evil.example/"))).toBe(null);
    expect(detect.platformFor(new URL("https://example.com/"))).toBe(null);
  });
});

describe("Google Meet", () => {
  const meet = page(`
    <span class="google-symbols">call_end</span>
    <div data-participant-id="1" data-self-name="Jo Example"><span class="notranslate">Jo Example</span></div>
    <div data-participant-id="2"><div class="Oaajhc"></div><span class="notranslate">Ada Lovelace</span></div>
    <div data-participant-id="3"><span class="notranslate">Ben  Weber</span><div data-audio-level="0"></div></div>
    <div role="region" tabindex="0">
      <div>
        <div><div><img alt=""><span>Ada Lovelace</span></div><div>Thanks for joining today</div></div>
        <div><div>You</div><div>Happy to</div></div>
      </div>
      <button>Jump to bottom</button>
    </div>`);

  it("reads participants, the speaking tile and this user's name", () => {
    const snapshot = detect.snapshot("meet", meet);
    expect(snapshot.inCall).toBe(true);
    expect(snapshot.selfName).toBe("Jo Example");
    expect(snapshot.participants).toEqual(["Jo Example", "Ada Lovelace", "Ben Weber"]);
    expect(snapshot.speaking).toEqual(["Ada Lovelace"]);
  });

  it("pairs each caption block's name with its text", () => {
    const captions = detect.captions("meet", meet);
    expect(captions.map((caption) => caption.name)).toEqual(["Ada Lovelace", "You"]);
    expect(captions[0].length).toBe("Thanks for joining today".length);
  });
});

describe("Microsoft Teams", () => {
  const teams = page(`
    <button id="hangup-button"></button>
    <div data-stream-type="Video" data-tid="Ada Lovelace (Guest)"></div>
    <div data-stream-type="Video" data-tid="video-stream-2"></div>
    <div data-tid="closed-caption-renderer-wrapper">
      <div><span data-tid="author">Ada Lovelace (Guest)</span><span data-tid="closed-caption-text">Shall we start?</span></div>
      <div><span data-tid="author">Ben Weber</span><span data-tid="closed-caption-text">Yes.</span></div>
    </div>`);

  it("takes names from captions and stream tiles, without guest suffixes or machine ids", () => {
    const snapshot = detect.snapshot("teams", teams);
    expect(snapshot.inCall).toBe(true);
    expect(snapshot.participants).toEqual(["Ada Lovelace"]);
    expect(detect.captions("teams", teams).map((caption) => caption.name)).toEqual(["Ada Lovelace", "Ben Weber"]);
  });
});

describe("Zoom", () => {
  it("reads the active speaker in speaker view", () => {
    const zoom = page(`
      <div class="footer__leave-btn-container"></div>
      <div class="speaker-active-container__video-frame"><div class="video-avatar__avatar-footer">Ada Lovelace</div></div>
      <div class="gallery"><div class="video-avatar__avatar-footer">Ben Weber</div></div>`);
    const snapshot = detect.snapshot("zoom", zoom);
    expect(snapshot.inCall).toBe(true);
    expect(snapshot.speaking).toEqual(["Ada Lovelace"]);
    expect(snapshot.participants).toEqual(["Ada Lovelace", "Ben Weber"]);
  });
});

describe("SpeakerTracker", () => {
  it("ignores one-poll flicker, repeats while speaking and stops after two misses", () => {
    const tracker = new detect.SpeakerTracker({ heartbeatMs: 3000 });
    tracker.update(["Ada"], 0);
    tracker.update([], 250); // flicker: never confirmed
    expect(tracker.take()).toEqual([]);

    tracker.update(["Ada"], 1000);
    tracker.update(["Ada"], 1250);
    tracker.update(["Ada"], 4300);
    tracker.update([], 4550); // one miss is not a stop
    tracker.update(["Ada"], 4800);
    tracker.update([], 5050);
    tracker.update([], 5300);
    expect(tracker.take().map((event) => [event.kind, event.t])).toEqual([
      ["speaking", 1250],
      ["speaking", 4300],
      ["stopped", 5300],
    ]);
  });

  it("marks this user's own events and throttles captions", () => {
    const tracker = new detect.SpeakerTracker({ captionGapMs: 1000 });
    tracker.setSelfName("Jo Example");
    tracker.caption("You", 0);
    tracker.caption("Ada", 0);
    tracker.caption("Ada", 400);
    tracker.caption("Ada", 1200);
    tracker.update(["Jo Example"], 0);
    tracker.update(["Jo Example"], 250);
    const events = tracker.take();
    expect(events.map((event) => [event.name, event.kind, event.isSelf])).toEqual([
      ["You", "caption", true],
      ["Ada", "caption", false],
      ["Ada", "caption", false],
      ["Jo Example", "speaking", true],
    ]);
  });

  it("reports a caption only when its text grows", () => {
    const tracker = new detect.SpeakerTracker({ captionGapMs: 0 });
    const element = {};
    tracker.captions([{ name: "Ada", element, length: 5 }], 0);
    tracker.captions([{ name: "Ada", element, length: 5 }], 300);
    tracker.captions([{ name: "Ada", element, length: 12 }], 600);
    expect(tracker.take().map((event) => event.t)).toEqual([0, 600]);
  });
});
