# Murmela speaker names (browser helper)

A small browser extension for Chrome, Edge, Brave and Arc. While Murmela records
a call, it tells Murmela who is speaking in **Google Meet**, **Microsoft Teams** or
**Zoom** in the browser, so the transcript says "Ada Lovelace" instead of
"Speaker 2".

## Install

1. In Murmela, open **Meetings** and choose **Show browser helper**. The folder opens.
2. In the browser, open `chrome://extensions` (Edge: `edge://extensions`).
3. Turn on **Developer mode** and choose **Load unpacked**, then select the folder.

## What it reads and sends

- **Google Meet:** the speaking indicator on participant tiles, plus live captions.
  Each caption block names its speaker.
- **Microsoft Teams:** live captions. Teams has no reliable speaking indicator in
  the page, so **turn on live captions** from the More menu in the call.
- **Zoom (web client):** the active speaker's tile in speaker view.

It sends participant names and the times they speak to `http://127.0.0.1`. That is
Murmela on this computer, and Murmela listens only while it records a call. It
sends nothing anywhere else and stores nothing. Caption text is only checked for new
words, to notice who is speaking; it never leaves the page.

Murmela also records the call's sound, so it doesn't depend on this helper. Without
the helper, the other participants are labelled "Other side". With Murmela Pro they
are labelled Speaker 1, 2 and so on, and you can rename them in Review.

## When names are wrong or missing

Meet, Teams and Zoom change their pages often, and a change can stop the helper
reading names until it is updated. Turning on the call's captions gives the most
dependable names. If no names arrive, Murmela's Meetings screen shows that the
helper isn't connected; the recording itself is unaffected.

## Development

`detect.js` holds the page logic and is tested with jsdom
(`npx vitest run browser-helper`). `content.js` polls the page four times a
second; `background.js` delivers events to Murmela.
