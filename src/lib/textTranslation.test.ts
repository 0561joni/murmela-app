import { describe, expect, it } from "vitest";
import {
  joinPieces,
  sameLanguage,
  segmentText,
  toneLabel,
  translatedFileName,
  translationTitle,
  wordAt,
} from "./textTranslation";

describe("segmentText", () => {
  it("splits paragraphs and puts the text back together exactly", () => {
    const text = "Hallo Lea,\n\ndanke für alles.\nBis bald.\n\n\nTschüss\n";
    const segmented = segmentText(text);
    expect(segmented.pieces.map((piece) => piece.text)).toEqual(["Hallo Lea,", "danke für alles.\nBis bald.", "Tschüss"]);
    expect(joinPieces(segmented, [])).toBe(text);
    expect(joinPieces(segmented, ["Hi Lea,", "thanks for everything.\nSee you soon.", "Bye"])).toBe(
      "Hi Lea,\n\nthanks for everything.\nSee you soon.\n\n\nBye\n",
    );
  });

  it("keeps code fences, front matter, links and number-only lines untranslated", () => {
    const text = "---\ntitle: Notizen\n---\n\nEinleitung\n```js\nconst a = 1;\n\nconst b = 2;\n```\n\nhttps://murmela.app\n\n12:30\n\nEnde";
    const segmented = segmentText(text);
    expect(segmented.pieces.map((piece) => [piece.text, piece.translatable])).toEqual([
      ["---\ntitle: Notizen\n---", false],
      ["Einleitung", true],
      ["```js\nconst a = 1;\n\nconst b = 2;\n```", false],
      ["https://murmela.app", false],
      ["12:30", false],
      ["Ende", true],
    ]);
    const outputs = segmented.pieces.map((piece) => (piece.translatable ? piece.text.toUpperCase() : "ignored"));
    expect(joinPieces(segmented, outputs)).toBe(text.replace("Einleitung", "EINLEITUNG").replace("Ende", "ENDE"));
  });

  it("normalizes Windows line breaks and keeps leading whitespace", () => {
    const segmented = segmentText("\n\n  Eins\r\n\r\nZwei");
    expect(segmented.before).toBe("\n\n");
    expect(segmented.pieces.map((piece) => piece.text)).toEqual(["  Eins", "Zwei"]);
  });

  it("handles an unclosed fence and empty text", () => {
    expect(segmentText("```\ncode").pieces).toEqual([{ text: "```\ncode", translatable: false, after: "" }]);
    expect(segmentText("").pieces).toEqual([]);
  });
});

describe("labels and names", () => {
  it("names tones with the target language's forms of address", () => {
    expect(toneLabel("formal", "fr")).toBe("Formal (vous)");
    expect(toneLabel("informal", "es-AR")).toBe("Informal (vos)");
    expect(toneLabel("auto", "de")).toBe("Match the source");
    expect(toneLabel("formal", "en")).toBe("Formal");
  });

  it("treats regional variants as the same language", () => {
    expect(sameLanguage("es", "es-AR")).toBe(true);
    expect(sameLanguage("de", "en")).toBe(false);
    expect(sameLanguage(null, "en")).toBe(false);
  });

  it("derives file names and Library titles", () => {
    expect(translatedFileName("notes.md", "fr")).toBe("notes.fr.md");
    expect(translatedFileName("Bericht.TXT", "en")).toBe("Bericht.en.txt");
    expect(translatedFileName(null, "pt-BR")).toBe("Translation.pt-BR.txt");
    expect(translationTitle(null, "\n# Wochenplan\nMontag")).toBe("Wochenplan");
    expect(translationTitle("a.txt", "x")).toBe("a.txt");
    expect(translationTitle(null, "a".repeat(80))).toHaveLength(60);
  });
});

describe("wordAt", () => {
  const text = "Die neue Funktion macht's leichter – l'ami, E-Mail.";
  it("finds the word under a character, or the one just before a gap", () => {
    expect(wordAt(text, 10)).toEqual({ start: 9, end: 17, word: "Funktion" });
    expect(wordAt(text, 17)).toEqual({ start: 9, end: 17, word: "Funktion" });
    expect(wordAt(text, 0)?.word).toBe("Die");
  });
  it("keeps apostrophes and hyphens inside a word", () => {
    expect(wordAt(text, text.indexOf("macht") + 2)?.word).toBe("macht's");
    expect(wordAt(text, text.indexOf("l'ami") + 3)?.word).toBe("l'ami");
    expect(wordAt(text, text.indexOf("E-Mail") + 1)?.word).toBe("E-Mail");
  });
  it("returns nothing on punctuation or spaces between words", () => {
    expect(wordAt(text, text.indexOf("–"))).toBeNull();
    expect(wordAt("  ", 1)).toBeNull();
  });
});
