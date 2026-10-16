// Passes speaker events from call pages to Murmela, which listens on 127.0.0.1
// only while it records a call. Nothing is sent anywhere else, and nothing is stored.
"use strict";

const PORTS = [47615, 47616, 47617, 47618, 47619];
let port = null;
let lastState = { connected: false, recording: false, platform: null, at: 0 };

async function findPort() {
  for (const candidate of PORTS) {
    try {
      const response = await fetch(`http://127.0.0.1:${candidate}/v1/status`, {
        signal: AbortSignal.timeout(800),
      });
      const status = await response.json();
      if (status && status.app === "murmela") return candidate;
    } catch (error) {
      // Not listening on this port.
    }
  }
  return null;
}

function setBadge(recording) {
  chrome.action.setBadgeText({ text: recording ? "REC" : "" });
  if (recording) chrome.action.setBadgeBackgroundColor({ color: "#c0392b" });
}

async function deliver(report) {
  if (port === null) port = await findPort();
  if (port === null) {
    lastState = { connected: false, recording: false, platform: report.platform, at: Date.now() };
    setBadge(false);
    return lastState;
  }
  try {
    // text/plain keeps this a simple request, with no CORS preflight.
    const response = await fetch(`http://127.0.0.1:${port}/v1/report`, {
      method: "POST",
      headers: { "Content-Type": "text/plain" },
      body: JSON.stringify(report),
    });
    const answer = await response.json();
    lastState = { connected: true, recording: Boolean(answer.accepted), platform: report.platform, at: Date.now() };
  } catch (error) {
    // Murmela stopped recording (the port closed) or quit.
    port = null;
    lastState = { connected: false, recording: false, platform: report.platform, at: Date.now() };
  }
  setBadge(lastState.recording);
  return lastState;
}

chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message && message.type === "report" && message.report) {
    deliver(message.report).then(reply);
    return true;
  }
  if (message && message.type === "state") {
    reply(lastState);
  }
  return false;
});
