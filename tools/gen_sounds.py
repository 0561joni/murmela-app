#!/usr/bin/env python3
"""
Reproducible generator for Murmela UI sounds.

Run: python3 tools/gen_sounds.py

Renders the feedback cues to 16-bit PCM mono 44.1 kHz WAV files in
src-tauri/assets/sounds/ so they can be listened to (`afplay <file>`). The app
synthesises the same cues at the output device's rate in `sound.rs`
(`cue_samples`); keep both in step when changing the design.

Design: soft "wooden tap" cues.
- Each tap is a short marimba-like strike: 2 ms raised-cosine attack, a
  fundamental plus overtones at 3.93x and 9.2x that fade within a few
  milliseconds, so it knocks instead of rings.
- `listen_start`: rising pair A4 -> D5; `listen_stop`: falling pair D5 -> A4;
  `complete`: soft A-major arpeggio; `error`: low falling pair E4 -> C4.
- A one-pole low-pass around 3.5 kHz rounds off the strike; peak 0.10 of full
  scale keeps it quiet for many dictations a day.
"""

from __future__ import annotations

import math
import struct
import wave
from pathlib import Path

SAMPLE_RATE = 44_100
BIT_DEPTH = 16
PEAK = 0.10
TAP_SECONDS = 0.22
PARTIALS = [(1.0, 1.0, 0.060), (3.93, 0.22, 0.012), (9.2, 0.05, 0.004)]

REPO_ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = REPO_ROOT / "src-tauri" / "assets" / "sounds"

CUES = {
    "listen_start.wav": [(0.0, 440.00, 0.85), (0.075, 587.33, 1.0)],
    "listen_stop.wav": [(0.0, 587.33, 1.0), (0.075, 440.00, 0.8)],
    "complete.wav": [(0.0, 440.00, 0.8), (0.07, 554.37, 0.85), (0.14, 659.25, 0.9)],
    "error.wav": [(0.0, 329.63, 1.0), (0.11, 261.63, 0.9)],
}


def render(taps: list[tuple[float, float, float]]) -> list[float]:
    length = max(int((offset + TAP_SECONDS) * SAMPLE_RATE) for offset, _, _ in taps)
    out = [0.0] * length
    for offset, frequency, loudness in taps:
        start = int(offset * SAMPLE_RATE)
        for index in range(int(TAP_SECONDS * SAMPLE_RATE)):
            t = index / SAMPLE_RATE
            attack = min(t / 0.002, 1.0)
            attack = 0.5 - 0.5 * math.cos(attack * math.pi)
            value = sum(
                math.sin(t * frequency * ratio * 2 * math.pi) * gain * math.exp(-t / decay)
                for ratio, gain, decay in PARTIALS
            )
            if start + index < length:
                out[start + index] += value * attack * loudness
    alpha = 1.0 - math.exp(-2 * math.pi * 3_500.0 / SAMPLE_RATE)
    state = 0.0
    for index, value in enumerate(out):
        state += alpha * (value - state)
        out[index] = state
    peak = max(abs(v) for v in out) or 1.0
    out = [v * PEAK / peak for v in out]
    fade = int(0.005 * SAMPLE_RATE)
    for index in range(fade):
        out[length - fade + index] *= 1.0 - index / fade
    return out


def write_wav(path: Path, samples: list[float]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    max_int = 2 ** (BIT_DEPTH - 1) - 1
    frames = b"".join(
        struct.pack("<h", max(-max_int - 1, min(max_int, int(v * max_int))))
        for v in samples
    )
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(BIT_DEPTH // 8)
        wav.setframerate(SAMPLE_RATE)
        wav.writeframes(frames)


def main() -> None:
    for filename, taps in CUES.items():
        samples = render(taps)
        out = OUT_DIR / filename
        write_wav(out, samples)
        print(f"wrote {out} ({len(samples) / SAMPLE_RATE * 1000:.0f} ms)")


if __name__ == "__main__":
    main()
