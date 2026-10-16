import type { ModelCapabilities, ModelProfile } from "../types/domain";

export type ModelPickerContext =
  | "shortcut_dictation"
  | "quick_dictate"
  | "file_transcription";

export interface PresentableModel {
  id: string;
  engine: string;
  modelName: string;
  sizeBytes: number;
  profile: ModelProfile;
  description?: string;
  requirements?: string | null;
  variant?: string;
  capabilities?: ModelCapabilities;
  licenseUrl?: string | null;
}

export interface ModelPresentation {
  id: string;
  friendlyName: string;
  technicalName: string;
  sizeBytes: number;
  speed: number;
  accuracy: number;
  description: string;
  technicalDetails: string;
  requirements: string | null;
  recommendedFor: ModelPickerContext[];
}

interface CatalogEntry {
  friendlyName: string;
  speed: number;
  accuracy: number;
  description: string;
  technicalDetails: string;
  recommendedFor: ModelPickerContext[];
  technicalNames: string[];
}

// Ratings are relative, spread over 1–5 in half steps (0 means "not rated"),
// from Murmela's benchmark of 55 real recordings (German, English, French,
// mixed; one speaker, M3 Pro):
// - accuracy: linear in word error rate, best model 5, worst 1;
// - speed: linear in log(typical wait for a ~10 s dictation), fastest 5,
//   slowest 1. Dictation models use the measured wait after release;
//   file-only models use real-time factor × 10 s.
// Re-run the benchmark and recompute these when models change.
const MODEL_CATALOG: Record<string, CatalogEntry> = {
  "live-pair": {
    friendlyName: "Live dictation · Experimental", speed: 5, accuracy: 4,
    description: "Words appear while you speak. When you let go, a second pass rereads the whole recording and the text is pasted almost instantly. The most accurate dictation engine in Murmela's tests for German and for mixing German and English. Works with the dictation shortcut only.",
    technicalDetails: "Nemotron 3.5 ASR Streaming 0.6B (live) + Parakeet TDT 0.6B v3 (final) · CoreML · about 1.4 GB memory while loaded · five-minute limit",
    recommendedFor: ["shortcut_dictation"], technicalNames: ["Live pair · Nemotron + Parakeet"],
  },
  "qwen3-asr-1.7b-bf16": {
    friendlyName: "Qwen",
    speed: 2.5,
    accuracy: 4,
    description: "Top accuracy in German, English and French, and handles switching between languages well. A short dictation takes about 2.5 seconds after you stop.",
    technicalDetails: "Qwen3-ASR 1.7B · BF16 · CPU inference",
    recommendedFor: ["quick_dictate", "file_transcription"],
    technicalNames: ["Qwen3-ASR-1.7B", "qwen3-asr-1.7b-bf16"],
  },
  "moss-transcribe-diarize-0.9b-f16": {
    friendlyName: "MOSS with speakers",
    speed: 1,
    accuracy: 4,
    description: "Long recordings with speaker labels, timestamps and sound events, and it uses your vocabulary. Accurate, but takes about as long as the recording itself, so it suits files rather than dictation.",
    technicalDetails: "MOSS Transcribe-Diarize 0.9B · F16 GGUF · isolated CPU worker",
    recommendedFor: [],
    technicalNames: ["MOSS Transcribe + Diarize 0.9B F16"],
  },
  "vibevoice-asr-8bit-mlx": {
    friendlyName: "VibeVoice · Experimental",
    speed: 1.5,
    accuracy: 5,
    description: "The most accurate engine in Murmela's tests, also for mixed-language audio, with speaker labels and your vocabulary, for recordings up to 60 minutes. Transcribes about twice as fast as real time but needs about 14 GB of memory, so it is for files only and needs a Mac with 18 GB or more.",
    technicalDetails: "VibeVoice-ASR by Microsoft (MIT, published for research and development) · 8-bit MLX · Apple Silicon",
    recommendedFor: ["file_transcription"],
    technicalNames: ["VibeVoice-ASR 8-bit MLX"],
  },
  "ggml-large-v3-turbo-q5_0-bin": {
    friendlyName: "Whisper Turbo Compact",
    speed: 2.5,
    accuracy: 1.5,
    description: "The smallest download and a dependable fallback for almost any language. Less accurate than Qwen or Live dictation, and weak when you switch languages.",
    technicalDetails: "Whisper large-v3-turbo · Q5_0 quantized · whisper.cpp",
    recommendedFor: [],
    technicalNames: ["ggml-large-v3-turbo-q5_0.bin"],
  },
  "ggml-large-v3-turbo-bin": {
    friendlyName: "Whisper Turbo",
    speed: 2.5,
    accuracy: 2,
    description: "Solid for single-language dictation. Tends to drop or translate words when you switch languages mid-recording.",
    technicalDetails: "Whisper large-v3-turbo · F16 · whisper.cpp",
    recommendedFor: [],
    technicalNames: ["ggml-large-v3-turbo.bin"],
  },
  "ggml-medium-bin": {
    friendlyName: "Whisper Medium",
    speed: 3,
    accuracy: 1,
    description: "Good on short single-language dictation, but less accurate on long recordings and weak when you switch languages.",
    technicalDetails: "Whisper medium · F16 · whisper.cpp",
    recommendedFor: [],
    technicalNames: ["ggml-medium.bin"],
  },
  "ggml-small-bin": {
    friendlyName: "Whisper Small",
    speed: 4,
    accuracy: 1.5,
    description: "Small and usually fast, but noticeably less accurate, and occasionally slow on short clips.",
    technicalDetails: "Whisper small · F16 · whisper.cpp",
    recommendedFor: [],
    technicalNames: ["ggml-small.bin"],
  },
};

const CATALOG_BY_TECHNICAL_NAME = new Map<string, [string, CatalogEntry]>();
for (const [id, entry] of Object.entries(MODEL_CATALOG)) {
  for (const technicalName of entry.technicalNames) {
    CATALOG_BY_TECHNICAL_NAME.set(normalizeTechnicalName(technicalName), [id, entry]);
  }
}

export function getModelPresentation(
  model: PresentableModel,
): ModelPresentation {
  const directEntry = MODEL_CATALOG[model.id];
  const technicalEntry = CATALOG_BY_TECHNICAL_NAME.get(normalizeTechnicalName(model.modelName));
  const entry = directEntry ?? technicalEntry?.[1];
  const canonicalId = directEntry ? model.id : technicalEntry?.[0] ?? model.id;

  if (entry) {
    return {
      id: canonicalId,
      friendlyName: entry.friendlyName,
      technicalName: model.modelName,
      sizeBytes: model.sizeBytes,
      speed: entry.speed,
      accuracy: entry.accuracy,
      description: entry.description,
      technicalDetails: entry.technicalDetails,
      requirements: model.requirements ?? null,
      recommendedFor: entry.recommendedFor,
    };
  }

  const ratings = ratingsForProfile(model.profile);
  return {
    id: model.id,
    friendlyName: humanizeTechnicalName(model.modelName, model.engine),
    technicalName: model.modelName,
    sizeBytes: model.sizeBytes,
    speed: ratings.speed,
    accuracy: ratings.accuracy,
    description: "Added to the models folder by hand. Murmela has not verified it; use it at your own risk.",
    technicalDetails: [model.engine, model.variant].filter(Boolean).join(" · "),
    requirements: model.requirements ?? null,
    recommendedFor: [],
  };
}

export function getFriendlyModelName(
  model: Pick<PresentableModel, "id" | "engine" | "modelName" | "sizeBytes" | "profile"> | string | null | undefined,
): string {
  if (!model) return "Missing model";
  if (typeof model !== "string") return getModelPresentation(model).friendlyName;

  const entry = CATALOG_BY_TECHNICAL_NAME.get(normalizeTechnicalName(model));
  if (entry) return entry[1].friendlyName;
  return humanizeTechnicalName(model, "");
}

export function isModelRecommended(
  presentation: ModelPresentation,
  context: ModelPickerContext,
): boolean {
  return presentation.recommendedFor.includes(context);
}

/** Ratings run 0–5 in half steps; 0 means "not rated". */
export type RatingCircle = "full" | "half" | "empty";

export function normalizeRating(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(5, Math.round(value * 2) / 2));
}

export function ratingCircles(value: number): RatingCircle[] {
  const rating = normalizeRating(value);
  return Array.from({ length: 5 }, (_, index) => {
    if (rating >= index + 1) return "full";
    if (rating >= index + 0.5) return "half";
    return "empty";
  });
}

export function formatRatingValue(label: string, value: number): string {
  if (value === 0) return `${label} not rated`;
  return `${label} ${normalizeRating(value)} of 5`;
}

export function formatRatingLine(presentation: Pick<ModelPresentation, "speed" | "accuracy">) {
  return `${formatRatingValue("Speed", presentation.speed)}, ${formatRatingValue("Accuracy", presentation.accuracy)}`;
}

export function recommendationLabel(context: ModelPickerContext) {
  switch (context) {
    case "shortcut_dictation":
      return "the dictation shortcut";
    case "quick_dictate":
      return "the record button";
    case "file_transcription":
      return "files";
  }
}

const WORKFLOW_LABELS: Record<ModelPickerContext, string> = {
  shortcut_dictation: "Shortcut",
  quick_dictate: "Record button",
  file_transcription: "Files",
};

/** "Shortcut · Record button · Files": where an engine can be used. Engines
 * without capability data run everywhere. */
export function formatWorkflows(capabilities: ModelCapabilities | undefined) {
  const contexts: ModelPickerContext[] = capabilities?.supportedContexts ?? [
    "shortcut_dictation",
    "quick_dictate",
    "file_transcription",
  ];
  return contexts.map((context) => WORKFLOW_LABELS[context]).join(" · ");
}

function ratingsForProfile(profile: ModelProfile) {
  switch (profile) {
    case "fast":
      return { speed: 5, accuracy: 2 };
    case "balanced":
      return { speed: 4, accuracy: 3 };
    case "accurate":
      return { speed: 3, accuracy: 4 };
  }
}

function normalizeTechnicalName(value: string) {
  return value.trim().toLocaleLowerCase();
}

function humanizeTechnicalName(modelName: string, engine: string) {
  const fileName = modelName.split(/[\\/]/).pop() ?? modelName;
  const cleaned = fileName
    .replace(/\.bin$/i, "")
    .replace(/^ggml[-_]/i, "")
    .replace(/\.en$/i, " EN")
    .replace(/[-_]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  const titled = cleaned
    .split(" ")
    .map((part) => {
      if (/^(q\d|f\d|bf\d|fp\d|en)$/i.test(part)) return part.toUpperCase();
      return `${part.charAt(0).toUpperCase()}${part.slice(1)}`;
    })
    .join(" ");
  if (engine.toLocaleLowerCase().includes("whisper") && !/^whisper\b/i.test(titled)) {
    return `Whisper ${titled}`;
  }
  return titled || modelName;
}

