// Murmela's playful side: the Murmelmeter numbers, milestone messages and the
// short lines used while listening. Everything here is skipped in Serious mode,
// and none of it is ever used for errors or lost text.

/** Typing speed the "time saved" figure compares against. */
export const TYPING_WORDS_PER_MINUTE = 40;

/** Minutes saved by speaking instead of typing at 40 words per minute. */
export function minutesSaved(words: number, speakingMs: number) {
  return Math.max(0, words / TYPING_WORDS_PER_MINUTE - speakingMs / 60_000);
}

export function formatMinutesSaved(minutes: number) {
  if (minutes < 1) return "under a minute";
  if (minutes < 90) return `${Math.round(minutes)} min`;
  const hours = minutes / 60;
  return `${hours < 10 ? hours.toFixed(1) : Math.round(hours)} h`;
}

export function formatCount(value: number) {
  return value.toLocaleString("en-US");
}

/** Rough word counts of familiar texts, smallest first. */
const COMPARISONS: Array<[number, string]> = [
  [1, "Just warming up the vocal cords."],
  [100, "About a postcard's worth. Wish you were here."],
  [500, "A decent email. Possibly a strongly worded one."],
  [1_500, "A solid blog post. No one reads those to the end either."],
  [7_500, "A short story. Hemingway would approve. Briefly."],
  [17_000, "About The Little Prince. Draw me a sheep?"],
  [40_000, "A novella. Your keyboard sends its regards."],
  [77_000, "Roughly the first Harry Potter. Owl post pending."],
  [190_000, "About Moby-Dick. Call yourself a talker."],
  [580_000, "War and Peace territory. Tolstoy is getting nervous."],
];

export function wordComparison(totalWords: number) {
  let line = "Your Murmelmeter wakes up with your first dictation. No pressure.";
  for (const [threshold, text] of COMPARISONS) {
    if (totalWords >= threshold) line = text;
  }
  return line;
}

const MILESTONES: Array<[number, string]> = [
  [1_000, "1,000 words murmured. You're officially a talker."],
  [10_000, "10,000 words. Your keyboard sends its regards."],
  [50_000, "50,000 words. That's a novel. Where's the movie deal?"],
  [100_000, "100,000 words. Your vocal cords deserve a raise."],
  [250_000, "250,000 words. Some people journal. You broadcast."],
  [500_000, "500,000 words. Tolstoy is getting nervous."],
  [1_000_000, "One million words. Murmela bows deeply."],
];

/** The milestone message when a dictation pushes the total past one. */
export function crossedMilestone(previousTotal: number, nextTotal: number) {
  let message: string | null = null;
  for (const [threshold, text] of MILESTONES) {
    if (previousTotal < threshold && nextTotal >= threshold) message = text;
  }
  return message;
}

export const LISTENING_LINES = [
  "All ears",
  "Go on, I'm listening",
  "Recording your brilliance",
  "Mic's hot",
  "Murmur away",
];

export const PROCESSING_LINES = [
  "Turning blah into text",
  "Untangling your words",
  "Typing so you don't have to",
  "Polishing your prose",
  "Decoding your genius",
];

/** A stable pick per dictation, so the line does not flicker while it runs. */
export function pickLine(lines: readonly string[], seed: string | number | null | undefined) {
  const text = String(seed ?? "");
  let hash = 0;
  for (let index = 0; index < text.length; index += 1) {
    hash = (hash * 31 + text.charCodeAt(index)) >>> 0;
  }
  return lines[hash % lines.length];
}
