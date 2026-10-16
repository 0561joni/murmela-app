// Reads who is speaking in a browser call (Google Meet, Microsoft Teams, Zoom)
// from the page, and turns it into events for Murmela. Only names and timings
// leave this script, and only to Murmela on this computer (see background.js).
//
// The call apps change their pages often, so every signal has a fallback:
// - Google Meet: the speaking indicator on participant tiles, and live captions.
// - Microsoft Teams: live captions (Teams shows no reliable speaking indicator).
// - Zoom: the active-speaker tile in speaker view.
// Captions are the most dependable source of names; turn them on in the call.
(function (root) {
  "use strict";

  const MEET_SPEAKING_CLASSES = ["Oaajhc", "HX2H7", "wEsLMd", "OgVli", "speaking", "active-speaker", "speaker-active"];
  // How call apps label your own captions or tile, in Murmela's languages.
  const SELF_LABELS = new Set(["you", "du", "sie", "ich", "vous", "moi", "tú", "tu", "usted", "yo", "voi", "io", "você", "eu", "jij", "u", "ik", "ty", "ja"]);
  const TEAMS_NAME_SUFFIX = /\s*\((unverified|guest|external|extern|invité|invitée|gast|invitado|ospite|convidado|gość)\)\s*$/i;

  function text(element) {
    return (element && element.textContent ? element.textContent : "").replace(/\s+/g, " ").trim();
  }

  function cleanName(name) {
    const cleaned = String(name || "")
      .replace(TEAMS_NAME_SUFFIX, "")
      .replace(/\s+/g, " ")
      .trim();
    return cleaned.length > 0 && cleaned.length <= 80 ? cleaned : "";
  }

  function isSelfLabel(name) {
    return SELF_LABELS.has(String(name || "").trim().toLowerCase());
  }

  function platformFor(location) {
    const host = location.hostname;
    if (host === "meet.google.com") return "meet";
    if (host === "teams.microsoft.com" || host === "teams.live.com" || host.endsWith("teams.cloud.microsoft")) return "teams";
    if (host === "zoom.us" || host.endsWith(".zoom.us")) return "zoom";
    return null;
  }

  // ---------- Google Meet ----------

  function meetTileName(tile) {
    return cleanName(text(tile.querySelector("span.notranslate")) || tile.getAttribute("data-self-name") || "");
  }

  function meetSelfName(doc) {
    const marker = doc.querySelector("[data-self-name]");
    const fromTile = marker ? cleanName(marker.getAttribute("data-self-name")) : "";
    return fromTile && !isSelfLabel(fromTile) ? fromTile : "";
  }

  function meetTileSpeaking(tile) {
    const hasClass = (element) => MEET_SPEAKING_CLASSES.some((name) => element.classList && element.classList.contains(name));
    if (hasClass(tile)) return true;
    for (const element of tile.querySelectorAll("*")) {
      if (hasClass(element)) return true;
    }
    return Boolean(tile.querySelector('[data-audio-level]:not([data-audio-level="0"])'));
  }

  function meetSnapshot(doc) {
    const tiles = Array.from(doc.querySelectorAll("[data-participant-id]")).filter(
      (tile) => !tile.parentElement || !tile.parentElement.closest("[data-participant-id]"),
    );
    const participants = [];
    const speaking = [];
    let selfName = meetSelfName(doc);
    for (const tile of tiles) {
      const name = meetTileName(tile);
      if (!name) continue;
      const self = tile.querySelector("[data-self-name]") !== null || tile.hasAttribute("data-self-name");
      if (self && !selfName && !isSelfLabel(name)) selfName = name;
      if (!participants.includes(name)) participants.push(name);
      if (meetTileSpeaking(tile) && !speaking.includes(name)) speaking.push(name);
    }
    const inCall = Array.from(doc.querySelectorAll(".google-symbols")).some((icon) => text(icon) === "call_end");
    return { inCall: inCall || tiles.length > 0, selfName, participants, speaking, meetingTitle: "" };
  }

  /**
   * Meet's caption region holds one block per turn: a name element, then the text
   * element. Blocks are found by that shape rather than by class names, which Meet
   * rotates; the innermost match wins.
   */
  function meetCaptions(doc) {
    const region = doc.querySelector('div[role="region"][tabindex="0"]');
    if (!region) return [];
    const candidates = Array.from(region.querySelectorAll("*")).filter((element) => {
      if (element.children.length !== 2) return false;
      const name = cleanName(text(element.children[0]));
      const words = text(element.children[1]);
      return name !== "" && words !== "" && words !== name;
    });
    return candidates
      .filter((block) => !candidates.some((other) => other !== block && block.contains(other)))
      .map((block) => ({
        name: cleanName(text(block.children[0])),
        element: block.children[1],
        length: text(block.children[1]).length,
      }));
  }

  // ---------- Microsoft Teams ----------

  function teamsSnapshot(doc) {
    const participants = [];
    for (const stream of doc.querySelectorAll("[data-stream-type][data-tid]")) {
      const name = cleanName(stream.getAttribute("data-tid"));
      // Skip machine ids such as "video-stream-2".
      if (name && !/^(video|screen|audio)[-_]|stream[-_]?\d/i.test(name) && !participants.includes(name)) {
        participants.push(name);
      }
    }
    const inCall = doc.querySelector("#hangup-button, [data-tid='hangup-main-btn'], [data-tid='call-hangup']") !== null;
    return { inCall, selfName: "", participants, speaking: [], meetingTitle: "" };
  }

  /** Teams captions: authors and texts in document order, paired. */
  function teamsCaptions(doc) {
    const wrapper = doc.querySelector('[data-tid="closed-caption-renderer-wrapper"]');
    if (!wrapper) return [];
    const authors = Array.from(wrapper.querySelectorAll('[data-tid="author"]'));
    const texts = Array.from(wrapper.querySelectorAll('[data-tid="closed-caption-text"]'));
    const pairs = [];
    for (let index = 0; index < Math.min(authors.length, texts.length); index += 1) {
      const name = cleanName(text(authors[index]));
      if (name) pairs.push({ name, element: texts[index], length: text(texts[index]).length });
    }
    return pairs;
  }

  // ---------- Zoom (web client) ----------

  const ZOOM_ACTIVE_FRAMES = [
    ".speaker-active-container__video-frame",
    ".speaker-bar-container__video-frame--active",
    '[class*="single-"][class*="-container__video-frame"]',
  ];

  function zoomDocument(doc) {
    const frame = doc.querySelector("#webclient");
    try {
      return (frame && frame.contentDocument) || doc;
    } catch (error) {
      return doc;
    }
  }

  function zoomSnapshot(doc) {
    const zoom = zoomDocument(doc);
    const participants = [];
    for (const footer of zoom.querySelectorAll(".video-avatar__avatar-footer")) {
      const name = cleanName(text(footer));
      if (name && !participants.includes(name)) participants.push(name);
    }
    const speaking = [];
    for (const selector of ZOOM_ACTIVE_FRAMES) {
      const frame = zoom.querySelector(selector);
      const name = frame ? cleanName(text(frame.querySelector(".video-avatar__avatar-footer"))) : "";
      if (name) {
        speaking.push(name);
        break;
      }
    }
    const inCall = zoom.querySelector("#audioOptionMenu, .footer__leave-btn-container") !== null;
    return { inCall, selfName: "", participants, speaking, meetingTitle: "" };
  }

  function snapshot(platform, doc) {
    if (platform === "meet") return meetSnapshot(doc);
    if (platform === "teams") return teamsSnapshot(doc);
    if (platform === "zoom") return zoomSnapshot(doc);
    return { inCall: false, selfName: "", participants: [], speaking: [], meetingTitle: "" };
  }

  // ---------- Turning observations into events ----------

  /**
   * Speaking indicators flicker, so a name must be seen (or missed) on two polls in
   * a row before it starts (or stops). While someone keeps speaking, "speaking" is
   * repeated every few seconds so Murmela can close spans if a stop is ever missed.
   */
  class SpeakerTracker {
    constructor(options = {}) {
      this.heartbeatMs = options.heartbeatMs ?? 3000;
      this.captionGapMs = options.captionGapMs ?? 1000;
      this.selfName = "";
      this.seen = new Map(); // name -> consecutive polls seen
      this.missed = new Map(); // name -> consecutive polls missed while active
      this.active = new Map(); // name -> last time "speaking" was sent
      this.lastCaption = new Map();
      this.captionLengths = new WeakMap();
      this.events = [];
    }

    setSelfName(name) {
      if (name) this.selfName = name;
    }

    isSelf(name) {
      return isSelfLabel(name) || (this.selfName !== "" && name === this.selfName);
    }

    push(name, kind, t) {
      this.events.push({ t, name, kind, isSelf: this.isSelf(name) });
    }

    update(speakingNames, t) {
      const current = new Set(speakingNames.map(cleanName).filter(Boolean));
      for (const name of current) {
        const seen = (this.seen.get(name) || 0) + 1;
        this.seen.set(name, seen);
        this.missed.delete(name);
        if (!this.active.has(name)) {
          if (seen >= 2) {
            this.active.set(name, t);
            this.push(name, "speaking", t);
          }
        } else if (t - this.active.get(name) >= this.heartbeatMs) {
          this.active.set(name, t);
          this.push(name, "speaking", t);
        }
      }
      for (const name of Array.from(this.seen.keys())) {
        if (current.has(name)) continue;
        this.seen.delete(name);
      }
      for (const name of Array.from(this.active.keys())) {
        if (current.has(name)) continue;
        const missed = (this.missed.get(name) || 0) + 1;
        this.missed.set(name, missed);
        if (missed >= 2) {
          this.active.delete(name);
          this.missed.delete(name);
          this.push(name, "stopped", t);
        }
      }
    }

    /** A caption by `name` grew. Reported at most once a second per name. */
    caption(name, t) {
      const cleaned = cleanName(name);
      if (!cleaned) return;
      if (t - (this.lastCaption.get(cleaned) ?? -Infinity) < this.captionGapMs) return;
      this.lastCaption.set(cleaned, t);
      this.push(cleaned, "caption", t);
    }

    /** Reports captions whose text grew since the last poll. */
    captions(pairs, t) {
      for (const pair of pairs) {
        const before = this.captionLengths.get(pair.element);
        this.captionLengths.set(pair.element, pair.length);
        if (before === undefined ? pair.length > 0 : pair.length > before) this.caption(pair.name, t);
      }
    }

    take() {
      const events = this.events;
      this.events = [];
      return events;
    }
  }

  root.MurmelaDetect = {
    platformFor,
    snapshot,
    captions(platform, doc) {
      if (platform === "meet") return meetCaptions(doc);
      if (platform === "teams") return teamsCaptions(doc);
      return [];
    },
    cleanName,
    isSelfLabel,
    SpeakerTracker,
  };
})(globalThis);
