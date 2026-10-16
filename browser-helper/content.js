// Runs in the call's page. Polls who is speaking and hands the events to the
// background worker, which passes them to Murmela on this computer.
(function () {
  "use strict";

  const detect = globalThis.MurmelaDetect;
  const platform = detect.platformFor(window.location);
  if (!platform) return;

  const POLL_MS = 250;
  const SEND_MS = 1000;
  // Without news, still say "connected" every few seconds so Murmela shows it.
  const KEEPALIVE_MS = 5000;

  const tracker = new detect.SpeakerTracker();
  let latest = { inCall: false, selfName: "", participants: [], speaking: [], meetingTitle: "" };
  let lastSent = 0;

  function poll() {
    try {
      const now = Date.now();
      latest = detect.snapshot(platform, document);
      if (!latest.inCall) return;
      tracker.setSelfName(latest.selfName);
      tracker.update(latest.speaking, now);
      tracker.captions(detect.captions(platform, document), now);
    } catch (error) {
      // A page change broke a selector; keep polling, the next layout may work again.
    }
  }

  function send() {
    if (!latest.inCall) return;
    const events = tracker.take();
    const now = Date.now();
    if (events.length === 0 && now - lastSent < KEEPALIVE_MS) return;
    lastSent = now;
    const report = {
      platform,
      meetingTitle: latest.meetingTitle || document.title || "",
      selfName: latest.selfName || "",
      participants: latest.participants,
      events,
    };
    try {
      chrome.runtime.sendMessage({ type: "report", report }, () => void chrome.runtime.lastError);
    } catch (error) {
      // The extension was reloaded; this page keeps the old script until it reloads.
    }
  }

  setInterval(poll, POLL_MS);
  setInterval(send, SEND_MS);
})();
