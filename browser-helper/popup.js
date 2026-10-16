"use strict";

const NAMES = { meet: "Google Meet", teams: "Microsoft Teams", zoom: "Zoom" };

chrome.runtime.sendMessage({ type: "state" }, (state) => {
  const label = document.getElementById("label");
  const hint = document.getElementById("hint");
  const fresh = state && Date.now() - state.at < 15000;
  if (fresh && state.recording) {
    document.getElementById("state").classList.add("on");
    label.textContent = `Sending names from ${NAMES[state.platform] || "this call"} to Murmela.`;
    hint.textContent = "Turn on the call's live captions for the most reliable names.";
  } else if (fresh && state.connected) {
    label.textContent = "Murmela is open but not recording this call.";
    hint.textContent = "Start a recording in Murmela → Meetings.";
  } else if (fresh) {
    label.textContent = "Murmela isn't recording.";
    hint.textContent = "Start a recording in Murmela → Meetings, then keep this call open.";
  } else {
    label.textContent = "No call open in this browser.";
    hint.textContent = "Works with Google Meet, Microsoft Teams and Zoom in the browser.";
  }
});
