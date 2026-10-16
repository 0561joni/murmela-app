import type {
  CallCaptureSupport,
  CallMode,
  CallStatus,
  CallTranscriptionStarted,
  PendingCallRecording,
  LicenseStatus,
  NetworkActivity,
  NetworkActivityEntry,
  NetworkDestination,
  LivePairStatus,
  UpdateStatus,
  AppSettings,
  CreateVocabularyTermInput,
  DictationReadiness,
  DictationStats,
  MicrophonePermission,
  DownloadableModel,
  FileTranscriptionRequest,
  FileTranscriptionStatusEvent,
  FileTranscriptionResponse,
  HealthCheckResponse,
  InputDeviceOption,
  InstalledModel,
  ModelDownloadStatus,
  QuickDictationStatusResponse,
  RecordingResult,
  RecordingStatusResponse,
  SelectedSourceFile,
  SettingsPatch,
  StartFileTranscriptionResponse,
  StartupStatus,
  TranscriptionPreviewRequest,
  TranscriptionPreviewResponse,
  TranscriptSummary,
  TranscriptDetail,
  UpdateVocabularyTermInput,
  VocabularyTerm,
} from "../types/domain";

const mockSettings: AppSettings = {
  defaultMode: "quick_dictate",
  shortcut: "Fn",
  translationEnabled: false,
  translationCycleShortcut: "Ctrl+Alt+L",
  translationModelId: "translategemma-12b-q6-k",
  shortcutMode: "push_to_talk",
  languageMode: "auto",
  fixedLanguage: null,
  preferredInputDevice: null,
  insertBehavior: "paste",
  launchAtLoginEnabled: false,
  gpuEnabled: true,
  shortcutDictationModelProfile: "accurate",
  shortcutDictationSelectedModelId: "ggml-medium-bin",
  quickDictateModelProfile: "accurate",
  quickDictateSelectedModelId: "ggml-large-v3-turbo-q5_0-bin",
  fileTranscribeModelProfile: "accurate",
  fileTranscribeSelectedModelId: "ggml-large-v3-turbo-q5_0-bin",
  appearance: "system",
  motionPreference: "system",
  saveHistory: true,
  soundsEnabled: true,
  volumeDuckingEnabled: true,
  fileDiarizationEnabled: false,
  livePairChunkMs: 560,
  livePairKeepLoaded: true,
  pasteLastShortcut: "Ctrl+Alt+V",
  seriousMode: false,
  autoUpdateEnabled: true,
};

// Browser preview: add ?first-run to the URL to simulate a fresh install.
const mockSetup = (() => {
  const freshInstall =
    typeof window !== "undefined" &&
    new URLSearchParams(window.location.search).has("first-run");
  return {
    freshInstall,
    firstRunCompleted: !freshInstall,
    microphone: (freshInstall ? "not_determined" : "granted") as MicrophonePermission,
  };
})();

const mockTranscripts: TranscriptSummary[] = [];
const mockModels: InstalledModel[] = [
  {
    id: "ggml-small-bin",
    engine: "whisper.cpp",
    modelName: "ggml-small.bin",
    variant: "balanced",
    localPath: "mock://models/ggml-small.bin",
    sizeBytes: 487_601_967,
    isDefault: true,
    profile: "balanced",
  },
  {
    id: "ggml-medium-bin",
    engine: "whisper.cpp",
    modelName: "ggml-medium.bin",
    variant: "accurate",
    localPath: "mock://models/ggml-medium.bin",
    sizeBytes: 1_533_763_059,
    isDefault: false,
    profile: "accurate",
  },
  {
    id: "ggml-large-v3-turbo-bin",
    engine: "whisper.cpp",
    modelName: "ggml-large-v3-turbo.bin",
    variant: "accurate",
    localPath: "mock://models/ggml-large-v3-turbo.bin",
    sizeBytes: 1_624_555_275,
    isDefault: false,
    profile: "accurate",
  },
  {
    id: "ggml-large-v3-turbo-q5_0-bin",
    engine: "whisper.cpp",
    modelName: "ggml-large-v3-turbo-q5_0.bin",
    variant: "accurate",
    localPath: "mock://models/ggml-large-v3-turbo-q5_0.bin",
    sizeBytes: 574_041_195,
    isDefault: true,
    profile: "accurate",
  },
];
const mockDownloadableModels: DownloadableModel[] = [
  {
    id: "live-pair",
    engine: "fluidaudio-live-pair",
    modelName: "Live pair · Nemotron + Parakeet",
    description: "Experimental shortcut dictation with live text while you speak.",
    sizeBytes: 1_381_000_000,
    profile: "fast",
    availability: "available",
    availabilityReason: null,
    installed: false,
    requirements: "Apple Silicon · macOS 14+ · about 1.4 GB of memory while loaded",
    artifactCount: 6,
    capability: "asr",
    capabilities: {
      supportedContexts: ["shortcut_dictation"],
      nativeDiarization: false,
      timestampedSegments: false,
      contextSupport: true,
      languageControl: "automatic_and_fixed",
      maximumAudioDurationMs: 300_000,
      streamingTranscription: true,
    },
  },
  { id: "translategemma-12b-q6-k", engine: "llama.cpp-translation", modelName: "TranslateGemma 12B",
    description: "Best-quality offline translation for the Translate screen and for dictation.", sizeBytes: 9660827392, profile: "accurate",
    availability: "unsupported_platform", availabilityReason: "Use the Apple Silicon desktop app for local translation.",
    installed: false, requirements: "Apple Silicon · 16 GB RAM recommended · about 11 GB of free memory while translating", artifactCount: 1, capability: "translation" },
  { id: "translategemma-4b-q6-k", engine: "llama.cpp-translation", modelName: "TranslateGemma 4B",
    description: "Compact offline translation. Faster and needs a third of the memory, with lower quality than 12B.", sizeBytes: 3190755840, profile: "fast",
    availability: "unsupported_platform", availabilityReason: "Use the Apple Silicon desktop app for local translation.",
    installed: false, requirements: "Apple Silicon · works on 8–16 GB Macs · about 5 GB of free memory while translating", artifactCount: 1, capability: "translation" },
  {
    id: "ggml-small-bin",
    engine: "whisper.cpp",
    modelName: "ggml-small.bin",
    description:
      "Good balance when you want lower memory use and reliable everyday quality.",
    sizeBytes: 487_601_967,
    profile: "balanced",
    availability: "available",
    availabilityReason: null,
    installed: true,
    requirements: null,
    artifactCount: 1,
    capability: "asr",
    origin: "custom",
  },
  {
    id: "ggml-medium-bin",
    engine: "whisper.cpp",
    modelName: "ggml-medium.bin",
    description:
      "Strong default for shortcut dictation when you want better accuracy.",
    sizeBytes: 1_533_763_059,
    profile: "accurate",
    availability: "available",
    availabilityReason: null,
    installed: true,
    requirements: null,
    artifactCount: 1,
    capability: "asr",
    origin: "custom",
  },
  {
    id: "ggml-large-v3-turbo-bin",
    engine: "whisper.cpp",
    modelName: "ggml-large-v3-turbo.bin",
    description:
      "Best full-size turbo model when you want top quality and speed.",
    sizeBytes: 1_624_555_275,
    profile: "accurate",
    availability: "available",
    availabilityReason: null,
    installed: true,
    requirements: null,
    artifactCount: 1,
    capability: "asr",
    origin: "custom",
  },
  {
    id: "ggml-large-v3-turbo-q5_0-bin",
    engine: "whisper.cpp",
    modelName: "ggml-large-v3-turbo-q5_0.bin",
    description:
      "Quantized turbo model with lower memory use and a strong quality-speed tradeoff.",
    sizeBytes: 574_041_195,
    profile: "accurate",
    availability: "available",
    availabilityReason: null,
    installed: true,
    requirements: null,
    artifactCount: 1,
    capability: "asr",
  },
  {
    id: "qwen3-asr-1.7b-bf16",
    engine: "qwen3_asr_c",
    modelName: "Qwen3-ASR-1.7B",
    description:
      "High-quality multilingual and code-switch transcription with dictionary-aware spelling prompts. CPU-only.",
    sizeBytes: 4_703_041_355,
    profile: "accurate",
    availability: "available",
    availabilityReason: null,
    installed: true,
    requirements: "macOS or Linux · 16 GB RAM recommended · CPU-only",
    artifactCount: 7,
    capability: "asr",
  },
  {
    id: "moss-transcribe-diarize-0.9b-f16",
    engine: "moss-transcribe-cpp",
    modelName: "MOSS Transcribe + Diarize 0.9B F16",
    description:
      "Long-form multilingual transcription with built-in speakers, timestamps, and vocabulary hotwords.",
    sizeBytes: 1_833_647_104,
    profile: "accurate",
    availability: "available",
    availabilityReason: null,
    installed: false,
    requirements:
      "CPU-only · up to 90 minutes · speaker identification built in",
    artifactCount: 1,
    capability: "asr",
    capabilities: {
      supportedContexts: [
        "shortcut_dictation",
        "quick_dictate",
        "file_transcription",
      ],
      nativeDiarization: true,
      timestampedSegments: true,
      contextSupport: true,
      languageControl: "automatic_only",
      maximumAudioDurationMs: 5_400_000,
    },
  },
  {
    id: "vibevoice-asr-8bit-mlx",
    engine: "vibevoice-mlx",
    modelName: "VibeVoice-ASR 8-bit MLX",
    description:
      "The most accurate engine for files: long recordings with speaker labels, timestamps, mixed languages and your vocabulary.",
    sizeBytes: 9_521_624_407,
    profile: "accurate",
    availability: "available",
    availabilityReason: null,
    installed: false,
    requirements:
      "Apple Silicon · macOS 14+ · at least 18 GB of memory (about 14 GB while transcribing) · file transcription only",
    artifactCount: 4,
    capability: "asr",
    capabilities: {
      supportedContexts: ["file_transcription"],
      nativeDiarization: true,
      timestampedSegments: true,
      contextSupport: true,
      languageControl: "automatic_only",
      maximumAudioDurationMs: 3_600_000,
    },
  },
];
// Browser preview: ?translate-demo also makes the translation models downloadable
// (slowly, so the download progress can be seen).
const mockTranslateDemo =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).has("translate-demo");
if (mockTranslateDemo) {
  mockDownloadableModels.forEach((model) => {
    if (model.capability === "translation") {
      model.availability = "available";
      model.availabilityReason = null;
    }
  });
}

if (mockSetup.freshInstall) {
  mockModels.length = 0;
  mockDownloadableModels.forEach((model) => {
    model.installed = false;
  });
  mockSettings.shortcutDictationSelectedModelId = null;
  mockSettings.quickDictateSelectedModelId = null;
  mockSettings.fileTranscribeSelectedModelId = null;
}

const mockModelDownloadListeners = new Set<
  (status: ModelDownloadStatus) => void
>();
const mockModelDownloadStatuses = new Map<string, ModelDownloadStatus>(
  mockDownloadableModels.map((model) => [
    model.id,
    {
      modelId: model.id,
      modelName: model.modelName,
      state: "idle" as const,
      downloadedBytes: 0,
      totalBytes: model.sizeBytes,
      progressPercent: 0,
      errorMessage: null,
      currentArtifact: null,
      artifactIndex: null,
      artifactCount: model.artifactCount,
    },
  ]),
);
const mockVocabularyTerms: VocabularyTerm[] = [
  {
    id: "builtin-linkedin",
    canonical: "LinkedIn",
    normalizedCanonical: "linkedin",
    matchMode: "exact_only",
    isBuiltin: true,
    createdAt: "2026-03-14T00:00:00Z",
    updatedAt: "2026-03-14T00:00:00Z",
    aliases: [
      {
        id: "builtin-linkedin-linked-in",
        alias: "linked in",
        normalizedAlias: "linked in",
      },
      {
        id: "builtin-linkedin-linken",
        alias: "linken",
        normalizedAlias: "linken",
      },
    ],
  },
];
const mockSelectedFiles: SelectedSourceFile[] = [
  {
    filePath: "mock://uploads/demo-interview.m4a",
    originalName: "demo-interview.m4a",
    mimeType: "audio/mp4",
    sizeBytes: 2_460_000,
    durationMs: null,
    sha256: null,
  },
  {
    filePath: "mock://uploads/customer-call.mp3",
    originalName: "customer-call.mp3",
    mimeType: "audio/mpeg",
    sizeBytes: 4_820_000,
    durationMs: null,
    sha256: null,
  },
];
const mockInputDevices: InputDeviceOption[] = [
  { id: "Built-in Microphone", name: "Built-in Microphone", isDefault: true },
  { id: "AirPods Pro", name: "AirPods Pro", isDefault: false },
];

let mockRecordingStatus: RecordingStatusResponse = {
  state: "idle",
  currentSessionId: null,
  activeInputDevice: "Browser preview microphone",
  lastRecordingPath: null,
  lastErrorMessage: null,
  durationMs: null,
  sampleRateHz: null,
  channels: null,
};

let mockRecordingStartedAt = 0;
let mockRecordingAccumulatedMs = 0;
let mockQuickDictationStatus: QuickDictationStatusResponse = {
  state: "idle",
  registeredShortcut: mockSettings.shortcut,
  shortcutMode: mockSettings.shortcutMode,
  isRegistered: false,
  lastTranscriptText: null,
  lastTranscriptId: null,
  lastRecordingPath: null,
  lastErrorMessage:
    "Global shortcuts only work in the Tauri desktop app, not in the browser preview.",
  lastModelName: null,
  lastInsertOutcome: null,
  lastDurationMs: null,
};
const mockFileTranscriptionListeners = new Set<
  (event: FileTranscriptionStatusEvent) => void
>();
const mockFileTranscriptionStatuses = new Map<
  string,
  FileTranscriptionStatusEvent
>();
let mockStartupStatus: StartupStatus = {
  phase: "workspace",
  step: 6,
  totalSteps: 6,
};
const mockStartupListeners = new Set<(status: StartupStatus) => void>();

function isTauriRuntime() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function invoke<T>(command: string, payload?: Record<string, unknown>) {
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  return tauriInvoke<T>(command, payload);
}

export async function getHealthCheck(): Promise<HealthCheckResponse> {
  if (!isTauriRuntime()) {
    return {
      appName: "Murmela",
      appVersion: "1.0.0",
      dbPath: "mock://speech_to_text.sqlite",
      tempDir: "mock://temp",
      modelsDir: "mock://models",
      startupNotices: [],
    };
  }
  return invoke<HealthCheckResponse>("health_check");
}

export async function quitApp(): Promise<void> {
  if (!isTauriRuntime()) {
    return;
  }
  return invoke<void>("quit_app");
}

export async function restartApp(): Promise<void> {
  if (!isTauriRuntime()) {
    return;
  }
  return invoke<void>("restart_app");
}

export async function getStartupStatus(): Promise<StartupStatus> {
  if (!isTauriRuntime()) {
    return mockStartupStatus;
  }
  return invoke<StartupStatus>("get_startup_status");
}

export async function listenStartupStatus(
  handler: (status: StartupStatus) => void,
): Promise<() => void> {
  if (!isTauriRuntime()) {
    mockStartupListeners.add(handler);
    return () => mockStartupListeners.delete(handler);
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<StartupStatus>("app://startup-status", (event) => {
    handler(event.payload);
  });
}

export async function frontendStartupComplete(): Promise<void> {
  if (!isTauriRuntime()) {
    mockStartupStatus = { phase: "ready", step: 6, totalSteps: 6 };
    mockStartupListeners.forEach((listener) => listener(mockStartupStatus));
    return;
  }
  return invoke<void>("frontend_startup_complete");
}

export async function reportStartupFailure(message: string): Promise<void> {
  if (!isTauriRuntime()) {
    mockStartupStatus = {
      phase: "failed",
      step: mockStartupStatus.step,
      totalSteps: 6,
      errorMessage: message,
    };
    mockStartupListeners.forEach((listener) => listener(mockStartupStatus));
    return;
  }
  return invoke<void>("report_startup_failure", { message });
}

export async function completeStartupHandoff(): Promise<void> {
  if (!isTauriRuntime()) {
    return;
  }
  return invoke<void>("complete_startup_handoff");
}

export async function openModelsFolder(): Promise<void> {
  if (!isTauriRuntime()) {
    return;
  }
  return invoke("open_models_folder");
}

export async function listDownloadableModels(): Promise<DownloadableModel[]> {
  if (!isTauriRuntime()) {
    return mockDownloadableModels
      .map((model) => ({
        ...model,
        installed:
          model.installed ||
          mockModels.some((installed) => installed.id === model.id),
      }));
  }
  return invoke<DownloadableModel[]>("list_downloadable_models");
}

/** Deletes an installed model's files; selections that used it fall back to
 * another installed model. */
export async function deleteModel(modelId: string): Promise<void> {
  if (!isTauriRuntime()) {
    const index = mockModels.findIndex((model) => model.id === modelId);
    if (index >= 0) mockModels.splice(index, 1);
    const entry = mockDownloadableModels.find((model) => model.id === modelId);
    if (entry) entry.installed = false;
    return;
  }
  return invoke<void>("delete_model", { modelId });
}

/** Re-reads the models folder so files added or removed by hand show up. */
export async function rescanModelsFolder(): Promise<InstalledModel[]> {
  if (!isTauriRuntime()) {
    return mockModels;
  }
  return invoke<InstalledModel[]>("rescan_models_folder");
}

export async function getModelDownloadStatuses(): Promise<
  ModelDownloadStatus[]
> {
  if (!isTauriRuntime()) {
    return Array.from(mockModelDownloadStatuses.values());
  }
  return invoke<ModelDownloadStatus[]>("get_model_download_statuses");
}

export async function startModelDownload(
  modelId: string,
): Promise<ModelDownloadStatus> {
  if (!isTauriRuntime()) {
    const model = mockDownloadableModels.find((entry) => entry.id === modelId);
    if (!model) {
      throw new Error("Unsupported model download.");
    }
    if (model.availability !== "available") {
      throw new Error("This model is not available on this Mac.");
    }
    const current = mockModelDownloadStatuses.get(modelId);
    if (current?.state === "downloading") {
      return current;
    }

    const activeDownload = Array.from(mockModelDownloadStatuses.values()).find(
      (status) => status.state === "downloading",
    );
    if (activeDownload) {
      throw new Error("Another model download is already in progress.");
    }

    const totalBytes = model.sizeBytes;
    const initialStatus: ModelDownloadStatus = {
      modelId: model.id,
      modelName: model.modelName,
      state: "downloading",
      downloadedBytes: 0,
      totalBytes,
      progressPercent: 0,
      errorMessage: null,
      currentArtifact:
        model.artifactCount > 1 ? "config.json" : model.modelName,
      artifactIndex: 1,
      artifactCount: model.artifactCount,
    };
    mockModelDownloadStatuses.set(model.id, initialStatus);
    emitMockModelDownloadStatus(initialStatus);

    const steps = 8;
    for (let step = 1; step <= steps; step += 1) {
      window.setTimeout(() => {
        if (mockModelDownloadStatuses.get(model.id)?.state === "canceled") {
          return;
        }
        const isFinal = step === steps;
        const nextStatus: ModelDownloadStatus = {
          modelId: model.id,
          modelName: model.modelName,
          state: isFinal ? "completed" : "downloading",
          downloadedBytes: Math.round((totalBytes * step) / steps),
          totalBytes,
          progressPercent: Math.round((100 * step) / steps),
          errorMessage: null,
          currentArtifact: isFinal ? null : model.modelName,
          artifactIndex: isFinal ? model.artifactCount : 1,
          artifactCount: model.artifactCount,
        };
        if (
          isFinal &&
          !mockModels.some((installed) => installed.id === model.id)
        ) {
          mockModels.push({
            id: model.id,
            engine: model.engine,
            modelName: model.modelName,
            variant: model.profile,
            localPath: `mock://models/${model.modelName}`,
            sizeBytes: model.sizeBytes,
            isDefault: false,
            profile: model.profile,
            capabilities: model.capabilities,
          });
        }
        mockModelDownloadStatuses.set(model.id, nextStatus);
        emitMockModelDownloadStatus(nextStatus);
      }, step * (mockTranslateDemo && model.capability === "translation" ? 2500 : 280));
    }

    return initialStatus;
  }
  return invoke<ModelDownloadStatus>("start_model_download", { modelId });
}

export interface LicenceDocument {
  name: string;
  sizeBytes: number;
}

/** The licence texts that ship with the app (Settings → About). */
export async function listLicenceDocuments(): Promise<LicenceDocument[]> {
  if (!isTauriRuntime()) {
    return [
      { name: "THIRD_PARTY_NOTICES.md", sizeBytes: 9_000 },
      { name: "Apache-2.0.txt", sizeBytes: 11_900 },
      { name: "THIRD-PARTY-LIBRARIES.txt", sizeBytes: 471_000 },
    ];
  }
  return invoke<LicenceDocument[]>("list_licence_documents");
}

export async function readLicenceDocument(name: string): Promise<string> {
  if (!isTauriRuntime()) {
    return `${name}\n\nThe full licence text is shown here in the Murmela app.`;
  }
  return invoke<string>("read_licence_document", { name });
}

export interface ModelTermsStatus {
  required: boolean;
  accepted: boolean;
  version?: string;
}

/** Browser preview only: terms accepted in this session. */
const mockAcceptedTerms = new Set<string>();

/** Whether a model needs licence terms accepted before it can be downloaded. */
export async function getModelTermsStatus(modelId: string): Promise<ModelTermsStatus> {
  if (!isTauriRuntime()) {
    const model = mockDownloadableModels.find((entry) => entry.id === modelId);
    const required = model?.capability === "translation";
    return { required, accepted: !required || mockAcceptedTerms.has(modelId) };
  }
  return invoke<ModelTermsStatus>("model_terms_status", { modelId });
}

/** Browser preview only: `?pro` shows the app as if a Pro license were active. */
let mockLicense: LicenseStatus =
  typeof window !== "undefined" && new URLSearchParams(window.location.search).has("pro")
    ? { pro: true, keyHint: "••••4D51", licensedTo: "Ada Lovelace", activatedAt: "2026-10-14T12:00:00Z" }
    : { pro: false, keyHint: null, licensedTo: null, activatedAt: null };

export async function getLicenseStatus(): Promise<LicenseStatus> {
  if (!isTauriRuntime()) return mockLicense;
  return invoke<LicenseStatus>("get_license_status");
}

export async function activateLicense(licenseKey: string): Promise<LicenseStatus> {
  if (!isTauriRuntime()) {
    const key = licenseKey.trim();
    if (!key) throw new Error("Paste the license key from your receipt email.");
    mockLicense = { pro: true, keyHint: `••••${key.slice(-4).toUpperCase()}`, licensedTo: "Preview buyer", activatedAt: new Date().toISOString() };
    return mockLicense;
  }
  return invoke<LicenseStatus>("activate_license", { licenseKey });
}

export async function deactivateLicense(): Promise<LicenseStatus> {
  if (!isTauriRuntime()) {
    mockLicense = { pro: false, keyHint: null, licensedTo: null, activatedAt: null };
    return mockLicense;
  }
  return invoke<LicenseStatus>("deactivate_license");
}

/** Browser preview only: `?license-link=KEY` behaves as if Murmela was opened from an activation link. */
let mockLicenseLink: string | null =
  typeof window !== "undefined" ? new URLSearchParams(window.location.search).get("license-link") : null;

/** The key from a `murmela://activate?key=…` link that arrived, at most once. */
export async function takePendingLicenseLink(): Promise<string | null> {
  if (!isTauriRuntime()) {
    const key = mockLicenseLink;
    mockLicenseLink = null;
    return key;
  }
  return invoke<string | null>("take_pending_license_link");
}

/** Opens murmela.com's pricing in the default browser. */
export async function openProPage(): Promise<void> {
  if (!isTauriRuntime()) {
    window.open("https://murmela.com/#pricing", "_blank", "noopener");
    return;
  }
  return invoke<void>("open_pro_page");
}

export async function listenLicenseLink(handler: () => void): Promise<() => void> {
  if (!isTauriRuntime()) return () => undefined;
  const { listen } = await import("@tauri-apps/api/event");
  return listen("app://license-link", () => handler());
}

export async function acceptModelTerms(modelId: string): Promise<void> {
  if (!isTauriRuntime()) {
    mockAcceptedTerms.add(modelId);
    return;
  }
  return invoke<void>("accept_model_terms", { modelId });
}

export async function cancelModelDownload(
  modelId: string,
): Promise<ModelDownloadStatus> {
  if (!isTauriRuntime()) {
    const current = mockModelDownloadStatuses.get(modelId);
    if (!current || current.state !== "downloading") {
      throw new Error("No active download exists for this model.");
    }
    const canceled: ModelDownloadStatus = {
      ...current,
      state: "canceled",
      currentArtifact: null,
      artifactIndex: null,
    };
    mockModelDownloadStatuses.set(modelId, canceled);
    emitMockModelDownloadStatus(canceled);
    return canceled;
  }
  return invoke<ModelDownloadStatus>("cancel_model_download", { modelId });
}

export async function listenModelDownloadStatus(
  handler: (status: ModelDownloadStatus) => void,
): Promise<() => void> {
  if (!isTauriRuntime()) {
    mockModelDownloadListeners.add(handler);
    return () => {
      mockModelDownloadListeners.delete(handler);
    };
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<ModelDownloadStatus>("model-download-status", (event) => {
    handler(event.payload);
  });
}

export async function getLivePairStatus(): Promise<LivePairStatus> {
  if (!isTauriRuntime()) {
    return { state: "off", message: null, loadMs: null, rssBytes: null };
  }
  return invoke<LivePairStatus>("get_live_pair_status");
}

export async function listenLivePairStatus(
  handler: (status: LivePairStatus) => void,
): Promise<() => void> {
  if (!isTauriRuntime()) {
    return () => undefined;
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<LivePairStatus>("live-pair-status", (event) => {
    handler(event.payload);
  });
}

export async function getSettings(): Promise<AppSettings> {
  if (!isTauriRuntime()) {
    return mockSettings;
  }
  return invoke<AppSettings>("get_settings");
}

export async function listInputDevices(): Promise<InputDeviceOption[]> {
  if (!isTauriRuntime()) {
    return mockInputDevices;
  }
  return invoke<InputDeviceOption[]>("list_input_devices");
}

export async function updateSettings(
  patch: SettingsPatch,
): Promise<AppSettings> {
  if (!isTauriRuntime()) {
    Object.assign(mockSettings, patch);
    if (patch.fileDiarizationEnabled === true) {
      const diarizationModel = mockDownloadableModels.find(
        (model) => model.capability === "diarization",
      );
      if (
        diarizationModel &&
        !mockModels.some((model) => model.id === diarizationModel.id)
      ) {
        await startModelDownload(diarizationModel.id);
      }
    } else if (patch.fileDiarizationEnabled === false) {
      const diarizationStatus = Array.from(
        mockModelDownloadStatuses.values(),
      ).find(
        (status) =>
          status.state === "downloading" &&
          mockDownloadableModels.find((model) => model.id === status.modelId)
            ?.capability === "diarization",
      );
      if (diarizationStatus) {
        await cancelModelDownload(diarizationStatus.modelId);
      }
    }
    mockQuickDictationStatus = {
      ...mockQuickDictationStatus,
      registeredShortcut: mockSettings.shortcut,
      shortcutMode: mockSettings.shortcutMode,
    };
    return mockSettings;
  }
  return invoke<AppSettings>("update_settings", { patch });
}

export async function suspendShortcutCapture(): Promise<void> {
  if (!isTauriRuntime()) {
    mockQuickDictationStatus = {
      ...mockQuickDictationStatus,
      isRegistered: false,
      lastErrorMessage: null,
    };
    return;
  }
  await invoke("suspend_shortcut_capture");
}

export async function resumeShortcutCapture(): Promise<void> {
  if (!isTauriRuntime()) {
    mockQuickDictationStatus = {
      ...mockQuickDictationStatus,
      isRegistered: true,
      registeredShortcut: mockSettings.shortcut,
      shortcutMode: mockSettings.shortcutMode,
      lastErrorMessage: null,
    };
    return;
  }
  await invoke("resume_shortcut_capture");
}

export async function listTranscripts(
  query: string,
): Promise<TranscriptSummary[]> {
  if (!isTauriRuntime()) {
    const normalized = query.trim().toLowerCase();
    return normalized.length === 0
      ? mockTranscripts
      : mockTranscripts.filter((item) =>
          [item.title, item.plainText].some((field) =>
            field.toLowerCase().includes(normalized),
          ),
        );
  }
  return invoke<TranscriptSummary[]>("list_transcripts", {
    query: query.trim().length === 0 ? null : query,
  });
}

export async function deleteTranscript(transcriptId: string): Promise<void> {
  if (!isTauriRuntime()) {
    const index = mockTranscripts.findIndex((item) => item.id === transcriptId);
    if (index >= 0) {
      mockTranscripts.splice(index, 1);
    }
    return;
  }
  return invoke("delete_transcript", { transcriptId });
}

export async function deleteAllTranscripts(): Promise<void> {
  if (!isTauriRuntime()) {
    mockTranscripts.splice(0, mockTranscripts.length);
    return;
  }
  return invoke("delete_all_transcripts");
}

export async function getTranscript(
  transcriptId: string,
): Promise<TranscriptDetail> {
  if (!isTauriRuntime()) {
    const summary = mockTranscripts.find((item) => item.id === transcriptId);
    if (!summary) throw new Error("Transcript not found.");
    return {
      ...summary,
      fullText: summary.plainText,
      timestampedText: summary.plainText,
      transcriptionWarnings: [],
      diarizationModelId: null,
      diarizationWarning: null,
      diarizationPolicyVersion: null,
      diarizationClusteringThreshold: null,
      diarizationSpeakerCountHint: null,
      segments: [
        {
          id: `${summary.id}:0`,
          startMs: 0,
          endMs: summary.durationMs ?? 0,
          text: summary.plainText,
          languageCode: summary.detectedLanguages[0] ?? "und",
          segmentOrder: 0,
          confidence: null,
          speakerId: null,
          speakerIds: null,
          speakerAttribution: "none",
          speakerConfidence: null,
        },
      ],
      speakers: [],
      diarizationTurns: [],
    };
  }
  return invoke<TranscriptDetail>("get_transcript", { transcriptId });
}

export async function renameTranscript(
  transcriptId: string,
  title: string,
): Promise<TranscriptSummary> {
  if (!isTauriRuntime()) {
    const transcript = mockTranscripts.find((item) => item.id === transcriptId);
    if (!transcript) throw new Error("Transcript not found.");
    transcript.title = title.trim();
    return transcript;
  }
  return invoke<TranscriptSummary>("rename_transcript", {
    transcriptId,
    title,
  });
}

export async function copyTextToClipboard(text: string): Promise<void> {
  if (!isTauriRuntime()) {
    await navigator.clipboard.writeText(text);
    return;
  }
  return invoke("copy_text_to_clipboard", { text });
}

export async function listInstalledModels(): Promise<InstalledModel[]> {
  if (!isTauriRuntime()) {
    return mockModels;
  }
  return invoke<InstalledModel[]>("list_installed_models");
}

export async function previewTranscription(
  request: TranscriptionPreviewRequest,
): Promise<TranscriptionPreviewResponse> {
  if (!isTauriRuntime()) {
    return {
      sourceKind: request.sourceKind,
      resolvedModel:
        mockModels.find((model) => model.id === request.selectedModelId) ??
        mockModels.find((model) => model.profile === request.profile) ??
        null,
      result: null,
      error: {
        code: "browser_preview",
        message: "Use the Tauri app to run real transcription.",
      },
    };
  }
  return invoke<TranscriptionPreviewResponse>("preview_transcription", {
    request,
  });
}

export async function pickAudioFiles(): Promise<SelectedSourceFile[]> {
  if (!isTauriRuntime()) {
    return mockSelectedFiles;
  }
  return invoke<SelectedSourceFile[]>("pick_audio_files");
}

export async function prepareDroppedAudioFiles(
  paths: string[],
): Promise<SelectedSourceFile[]> {
  if (!isTauriRuntime()) {
    const files = paths
      .filter(isSupportedAudioPath)
      .map((filePath) => buildMockSelectedFile(filePath));
    if (files.length === 0) {
      throw new Error(
        "Drop audio (WAV, MP3, M4A, OPUS) or video (MP4, MOV, M4V, MKV, WEBM, AVI) files to transcribe.",
      );
    }
    return files;
  }
  return invoke<SelectedSourceFile[]>("prepare_dropped_audio_files", { paths });
}

export async function startFileTranscription(
  request: FileTranscriptionRequest,
): Promise<StartFileTranscriptionResponse> {
  if (!isTauriRuntime()) {
    const jobId = request.jobId;
    const sourceFile = request.sourceFile;
    const transcript: TranscriptSummary | null = mockSettings.saveHistory
      ? {
          id: crypto.randomUUID(),
          createdAt: new Date().toISOString(),
          sourceType: "file_upload",
          title: sourceFile.originalName,
          plainText:
            "Hallo Lena, LinkedIn bleibt LinkedIn und Empanadas bleiben Empanadas.",
          status: "completed",
          detectedLanguages: ["de", "en", "es"],
          durationMs: 8_400,
          modelName: "browser-preview",
          qualityStatus: "clean",
          recoveredRegionCount: 0,
          diarizationStatus: "not_requested",
          speakerCount: null,
        }
      : null;

    if (transcript) {
      mockTranscripts.unshift(transcript);
    }

    const result: FileTranscriptionResponse = {
      sourceFile: {
        ...sourceFile,
        durationMs: 8_400,
        sha256: "mock-sha256",
      },
      resolvedModel: null,
      result: {
        jobId: crypto.randomUUID(),
        modelName: "browser-preview",
        fullText:
          "Hallo Lena, LinkedIn bleibt LinkedIn und Empanadas bleiben Empanadas.",
        plainText:
          "Hallo Lena, LinkedIn bleibt LinkedIn und Empanadas bleiben Empanadas.",
        timestampedText:
          "[00:00 - 00:04] de: Hallo Lena, LinkedIn bleibt LinkedIn.\n[00:04 - 00:08] es: Und Empanadas bleiben Empanadas.",
        detectedLanguages: ["de", "en", "es"],
        qualityStatus: "clean",
        recoveredRegionCount: 0,
        warnings: [],
        diarizationStatus: "not_requested",
        diarizationModelId: null,
        diarizationWarning: null,
        diarizationPolicyVersion: null,
        diarizationClusteringThreshold: null,
        diarizationSpeakerCountHint: request.speakerCountHint,
        speakers: [],
        diarizationTurns: [],
        segments: [
          {
            id: crypto.randomUUID(),
            startMs: 0,
            endMs: 4000,
            text: "Hallo Lena, LinkedIn bleibt LinkedIn.",
            languageCode: "de",
            segmentOrder: 0,
            confidence: 0.89,
            speakerId: null,
            speakerIds: null,
            speakerAttribution: "none",
            speakerConfidence: null,
          },
          {
            id: crypto.randomUUID(),
            startMs: 4000,
            endMs: 8400,
            text: "Und Empanadas bleiben Empanadas.",
            languageCode: "es",
            segmentOrder: 1,
            confidence: 0.9,
            speakerId: null,
            speakerIds: null,
            speakerAttribution: "none",
            speakerConfidence: null,
          },
        ],
      },
      savedTranscript: transcript,
    };
    queueMicrotask(() => {
      emitMockFileTranscriptionStatus({
        jobId,
        sourceFile,
        stage: "queued",
        progressPercent: null,
        processedMs: null,
        totalMs: null,
        etaSeconds: null,
        statusText: "Queued for local transcription.",
        result: null,
        errorMessage: null,
        startedAtMs: Date.now(),
        updatedAtMs: Date.now(),
      });
      window.setTimeout(() => {
        emitMockFileTranscriptionStatus({
          jobId,
          sourceFile,
          stage: "preparing",
          progressPercent: null,
          processedMs: null,
          totalMs: null,
          etaSeconds: null,
          statusText: "Preparing audio and estimating workload…",
          result: null,
          errorMessage: null,
          startedAtMs: Date.now(),
          updatedAtMs: Date.now(),
        });
      }, 120);
      window.setTimeout(() => {
        emitMockFileTranscriptionStatus({
          jobId,
          sourceFile,
          stage: "transcribing",
          progressPercent: 42,
          processedMs: 3500,
          totalMs: 8400,
          etaSeconds: 4,
          statusText: `Transcribing ${sourceFile.originalName}...`,
          result: null,
          errorMessage: null,
          startedAtMs: Date.now(),
          updatedAtMs: Date.now(),
        });
      }, 420);
      window.setTimeout(() => {
        emitMockFileTranscriptionStatus({
          jobId,
          sourceFile,
          stage: "saving",
          progressPercent: 100,
          processedMs: 8400,
          totalMs: 8400,
          etaSeconds: 0,
          statusText: "Saving to the Library…",
          result: null,
          errorMessage: null,
          startedAtMs: Date.now(),
          updatedAtMs: Date.now(),
        });
      }, 920);
      window.setTimeout(() => {
        emitMockFileTranscriptionStatus({
          jobId,
          sourceFile,
          stage: "completed",
          progressPercent: 100,
          processedMs: 8400,
          totalMs: 8400,
          etaSeconds: 0,
          statusText: "Done.",
          result,
          errorMessage: null,
          startedAtMs: Date.now(),
          updatedAtMs: Date.now(),
        });
      }, 1200);
    });
    return { jobId };
  }
  return invoke<StartFileTranscriptionResponse>("start_file_transcription", {
    request,
  });
}

export async function getFileTranscriptionStatuses(): Promise<
  FileTranscriptionStatusEvent[]
> {
  if (!isTauriRuntime()) {
    return Array.from(mockFileTranscriptionStatuses.values()).sort(
      (left, right) => right.startedAtMs - left.startedAtMs,
    );
  }
  const statuses = await invoke<FileTranscriptionStatusEvent[]>(
    "get_file_transcription_statuses",
  );
  return Promise.all(statuses.map(hydrateFileStatus));
}

export async function cancelFileTranscription(jobId: string): Promise<void> {
  if (!isTauriRuntime()) {
    const current = mockFileTranscriptionStatuses.get(jobId);
    if (current) {
      emitMockFileTranscriptionStatus({
        ...current,
        stage: "canceled",
        progressPercent: current.progressPercent,
        statusText: "File transcription canceled.",
        errorMessage: "The file transcription was canceled by the user.",
        result: null,
      });
    }
    return;
  }
  return invoke<void>("cancel_file_transcription", { jobId });
}

function buildMockSelectedFile(filePath: string): SelectedSourceFile {
  const matched = mockSelectedFiles.find((file) => file.filePath === filePath);
  if (matched) {
    return matched;
  }
  const originalName = filePath.split("/").pop() || "upload.wav";
  const extension = originalName.split(".").pop()?.toLowerCase();
  return {
    filePath,
    originalName,
    mimeType: MOCK_MIME_TYPES[extension ?? ""] ?? "audio/wav",
    sizeBytes: 1_024_000,
    durationMs: null,
    sha256: null,
  };
}

const MOCK_MIME_TYPES: Record<string, string> = {
  wav: "audio/wav",
  mp3: "audio/mpeg",
  m4a: "audio/mp4",
  opus: "audio/ogg",
  mp4: "video/mp4",
  mov: "video/quicktime",
  m4v: "video/x-m4v",
  mkv: "video/x-matroska",
  webm: "video/webm",
  avi: "video/x-msvideo",
};

function isSupportedAudioPath(filePath: string) {
  const extension = filePath.split(".").pop()?.toLowerCase() ?? "";
  return Object.prototype.hasOwnProperty.call(MOCK_MIME_TYPES, extension);
}

export async function getRecordingStatus(): Promise<RecordingStatusResponse> {
  if (!isTauriRuntime()) {
    if (mockRecordingStatus.state === "listening") {
      mockRecordingStatus = {
        ...mockRecordingStatus,
        durationMs:
          mockRecordingAccumulatedMs + (Date.now() - mockRecordingStartedAt),
      };
    }
    return mockRecordingStatus;
  }
  return invoke<RecordingStatusResponse>("get_recording_status");
}

export async function getRecordingInputLevel(): Promise<number> {
  if (!isTauriRuntime()) {
    if (mockRecordingStatus.state !== "listening") {
      return 0;
    }
    const elapsed = Date.now() - mockRecordingStartedAt;
    return Math.max(0, Math.min(1, (Math.sin(elapsed / 180) + 1) / 2));
  }
  return invoke<number>("get_recording_input_level");
}

export async function startRecordingSession(
  feedback = true,
): Promise<RecordingStatusResponse> {
  if (!isTauriRuntime()) {
    mockRecordingStartedAt = Date.now();
    mockRecordingAccumulatedMs = 0;
    mockRecordingStatus = {
      state: "listening",
      currentSessionId: crypto.randomUUID(),
      activeInputDevice: "Browser preview microphone",
      lastRecordingPath: mockRecordingStatus.lastRecordingPath,
      lastErrorMessage: null,
      durationMs: 0,
      sampleRateHz: 48000,
      channels: 2,
    };
    return mockRecordingStatus;
  }
  return invoke<RecordingStatusResponse>("start_recording_session", {
    feedback,
    purpose: feedback ? "dictation" : "microphone_test",
  });
}

export async function stopRecordingSession(): Promise<RecordingResult> {
  if (!isTauriRuntime()) {
    const durationMs =
      mockRecordingStatus.state === "listening"
        ? mockRecordingAccumulatedMs + (Date.now() - mockRecordingStartedAt)
        : mockRecordingAccumulatedMs;
    const result: RecordingResult = {
      sessionId: mockRecordingStatus.currentSessionId ?? crypto.randomUUID(),
      filePath: "mock://temp/browser-preview.wav",
      durationMs,
      sampleRateHz: 16000,
      channels: 1,
      sampleCount: 32000,
    };
    mockRecordingStatus = {
      state: "success",
      currentSessionId: result.sessionId,
      activeInputDevice: "Browser preview microphone",
      lastRecordingPath: result.filePath,
      lastErrorMessage: null,
      durationMs: result.durationMs,
      sampleRateHz: result.sampleRateHz,
      channels: result.channels,
    };
    mockRecordingAccumulatedMs = 0;
    return result;
  }
  return invoke<RecordingResult>("stop_recording_session");
}

export async function cancelRecordingSession(): Promise<RecordingStatusResponse> {
  if (!isTauriRuntime()) {
    mockRecordingAccumulatedMs = 0;
    mockRecordingStatus = {
      ...mockRecordingStatus,
      state: "idle",
      currentSessionId: null,
      durationMs: null,
      sampleRateHz: null,
      channels: null,
      lastErrorMessage: null,
    };
    return mockRecordingStatus;
  }
  return invoke<RecordingStatusResponse>("cancel_recording_session");
}

export async function getQuickDictateStatus(): Promise<QuickDictationStatusResponse> {
  if (!isTauriRuntime()) {
    return mockQuickDictationStatus;
  }
  return invoke<QuickDictationStatusResponse>("get_quick_dictate_status");
}

/**
 * Force dictation back to a clean Idle state. Recovers a wedged audio worker,
 * hides the overlay, restores volume, and re-arms the shortcut. Surfaced in the
 * UI as the "Reset" action when dictation looks unresponsive.
 */
export async function resetQuickDictation(): Promise<QuickDictationStatusResponse> {
  if (!isTauriRuntime()) {
    return mockQuickDictationStatus;
  }
  return invoke<QuickDictationStatusResponse>("reset_quick_dictation");
}

/** Snapshot of the prerequisites for shortcut dictation (model, shortcut,
 * Accessibility). Drives the Home readiness checklist. */
export async function getDictationReadiness(): Promise<DictationReadiness> {
  if (!isTauriRuntime()) {
    const shortcutModel = mockModels.find(
      (model) => model.id === mockSettings.shortcutDictationSelectedModelId,
    );
    return {
      hasModel: mockModels.length > 0,
      shortcutModelReady: Boolean(
        shortcutModel &&
          (!shortcutModel.capabilities ||
            shortcutModel.capabilities.supportedContexts.includes("shortcut_dictation")),
      ),
      microphone: mockSetup.microphone,
      firstRunCompleted: mockSetup.firstRunCompleted,
      shortcutRegistered: true,
      autoPasteEnabled: true,
      accessibilityRequired: mockSetup.freshInstall,
      accessibilityGranted: !mockSetup.freshInstall,
    };
  }
  return invoke<DictationReadiness>("get_dictation_readiness");
}

let mockStats: DictationStats = {
  todayWords: 312,
  todayDictations: 9,
  totalWords: 18_420,
  totalDictations: 611,
  totalDurationMs: 7_800_000,
  streakDays: 6,
};
const mockStatsListeners = new Set<(stats: DictationStats) => void>();

const mockNetworkDestinations: NetworkDestination[] = [
  {
    id: "hugging_face",
    name: "Hugging Face",
    host: "huggingface.co",
    reason: "Downloads the speech, translation and voice-activity models you choose. Nothing is uploaded.",
  },
  {
    id: "github",
    name: "GitHub",
    host: "github.com",
    reason:
      "Checks for a new version of Murmela and downloads it, only while automatic updates are on or when you press Check now.",
  },
  {
    id: "lemon_squeezy",
    name: "Lemon Squeezy",
    host: "api.lemonsqueezy.com",
    reason:
      "Activates Murmela Pro once with your license key, and frees this Mac's slot if you deactivate it. Pro then works offline.",
  },
];
let mockNetworkEntries: NetworkActivityEntry[] = [
  { id: 3, startedAt: "2026-10-14T09:12:00Z", destination: "github", host: "github.com", purpose: "Update check", bytes: null, outcome: "completed" },
  { id: 2, startedAt: "2026-10-13T18:40:00Z", destination: "lemon_squeezy", host: "api.lemonsqueezy.com", purpose: "Activate Murmela Pro", bytes: 1_284, outcome: "completed" },
  { id: 1, startedAt: "2026-10-13T18:02:00Z", destination: "hugging_face", host: "huggingface.co", purpose: "Download Whisper Large v3 Turbo", bytes: 1_624_555_275, outcome: "completed" },
];

/** The only places Murmela can contact, and every connection it made. */
export async function getNetworkActivity(): Promise<NetworkActivity> {
  if (!isTauriRuntime()) return { destinations: mockNetworkDestinations, entries: mockNetworkEntries };
  return invoke<NetworkActivity>("get_network_activity");
}

export async function clearNetworkActivity(): Promise<void> {
  if (!isTauriRuntime()) {
    mockNetworkEntries = [];
    return;
  }
  return invoke<void>("clear_network_activity");
}

const idleCallStatus: CallStatus = {
  recording: false,
  callId: null,
  mode: null,
  title: null,
  elapsedMs: 0,
  limitMs: 6 * 60 * 60 * 1000,
  microphone: null,
  system: null,
  browser: null,
  bridgePort: null,
};
/** Browser preview only: a simulated recording. */
let mockCall: { mode: CallMode; title: string; startedAt: number } | null = null;
let mockPendingCalls: PendingCallRecording[] = [];

export async function getCallCaptureSupport(): Promise<CallCaptureSupport> {
  if (!isTauriRuntime()) return { supported: true, reason: null };
  return invoke<CallCaptureSupport>("get_call_capture_support");
}

export async function getCallStatus(): Promise<CallStatus> {
  if (!isTauriRuntime()) {
    if (!mockCall) return idleCallStatus;
    const elapsedMs = Date.now() - mockCall.startedAt;
    const wave = (offset: number) => Math.max(0, Math.sin(elapsedMs / 400 + offset)) * 0.7;
    return {
      ...idleCallStatus,
      recording: true,
      callId: "preview",
      mode: mockCall.mode,
      title: mockCall.title,
      elapsedMs,
      microphone: { label: "MacBook Pro Microphone", level: wave(0), error: null },
      system: mockCall.mode === "call" ? { label: "Sound from other apps", level: wave(2), error: null } : null,
      browser:
        mockCall.mode === "call"
          ? {
              platform: "meet",
              meetingTitle: "Weekly check-in",
              participants: ["Ada Lovelace", "Ben Weber"],
              speaking: elapsedMs % 8000 < 4000 ? ["Ada Lovelace"] : [],
              msSinceLastReport: 400,
            }
          : null,
      bridgePort: mockCall.mode === "call" ? 47615 : null,
    };
  }
  return invoke<CallStatus>("get_call_status");
}

export async function startCallRecording(mode: CallMode, title: string | null): Promise<CallStatus> {
  if (!isTauriRuntime()) {
    mockCall = { mode, title: title?.trim() || (mode === "call" ? "Call" : "Meeting"), startedAt: Date.now() };
    return getCallStatus();
  }
  return invoke<CallStatus>("start_call_recording", { mode, title });
}

/** Stops recording and queues the transcription like a dropped file. */
export async function stopCallRecording(speakerCountHint: number | null): Promise<CallTranscriptionStarted> {
  if (!isTauriRuntime()) {
    const title = mockCall?.title ?? "Call";
    const durationMs = mockCall ? Date.now() - mockCall.startedAt : 0;
    mockCall = null;
    return {
      jobId: crypto.randomUUID(),
      sourceFile: {
        filePath: `/preview/meetings/${crypto.randomUUID()}/call.wav`,
        originalName: `${title}.wav`,
        mimeType: "audio/wav",
        sizeBytes: Math.round(durationMs * 32),
        durationMs,
        sha256: null,
      },
    };
  }
  return invoke<CallTranscriptionStarted>("stop_call_recording", { speakerCountHint });
}

export async function discardCallRecording(): Promise<void> {
  if (!isTauriRuntime()) {
    mockCall = null;
    return;
  }
  return invoke<void>("discard_call_recording");
}

export async function listPendingCallRecordings(): Promise<PendingCallRecording[]> {
  if (!isTauriRuntime()) return mockPendingCalls;
  return invoke<PendingCallRecording[]>("list_pending_call_recordings");
}

export async function transcribePendingCallRecording(id: string): Promise<CallTranscriptionStarted> {
  if (!isTauriRuntime()) {
    mockPendingCalls = mockPendingCalls.filter((recording) => recording.id !== id);
    return stopCallRecording(null);
  }
  return invoke<CallTranscriptionStarted>("transcribe_pending_call_recording", { id, speakerCountHint: null });
}

export async function deletePendingCallRecording(id: string): Promise<void> {
  if (!isTauriRuntime()) {
    mockPendingCalls = mockPendingCalls.filter((recording) => recording.id !== id);
    return;
  }
  return invoke<void>("delete_pending_call_recording", { id });
}

/** Opens the browser helper's folder so it can be loaded as an unpacked extension. */
export async function revealBrowserHelper(): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke<string>("reveal_browser_helper");
}

/** True for the file a call or meeting recording is transcribed from. */
export function isCallRecordingPath(path: string): boolean {
  return /[\\/]meetings[\\/][^\\/]+[\\/]call\.wav$/.test(path);
}

export async function getDictationStats(): Promise<DictationStats> {
  if (!isTauriRuntime()) return mockSetup.freshInstall ? { ...mockStats, todayWords: 0, todayDictations: 0, totalWords: 0, totalDictations: 0, totalDurationMs: 0, streakDays: 0 } : mockStats;
  return invoke<DictationStats>("get_dictation_stats");
}

export async function resetDictationStats(): Promise<DictationStats> {
  if (!isTauriRuntime()) {
    mockStats = { todayWords: 0, todayDictations: 0, totalWords: 0, totalDictations: 0, totalDurationMs: 0, streakDays: 0 };
    mockStatsListeners.forEach((listener) => listener(mockStats));
    return mockStats;
  }
  return invoke<DictationStats>("reset_dictation_stats");
}

/** Fires after every finished dictation with the new Murmelmeter totals. */
export async function listenDictationStats(
  handler: (stats: DictationStats) => void,
): Promise<() => void> {
  if (!isTauriRuntime()) {
    mockStatsListeners.add(handler);
    return () => {
      mockStatsListeners.delete(handler);
    };
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<DictationStats>("dictation-stats", (event) => handler(event.payload));
}

/** Shows the system microphone prompt if the user has not decided yet. */
export async function requestMicrophoneAccess(): Promise<MicrophonePermission> {
  if (!isTauriRuntime()) {
    mockSetup.microphone = "granted";
    return mockSetup.microphone;
  }
  return invoke<MicrophonePermission>("request_microphone_access");
}

/** Opens the OS pane where a denied microphone permission can be changed. */
export async function openMicrophoneSettings(): Promise<void> {
  if (!isTauriRuntime()) return;
  return invoke<void>("open_microphone_settings");
}

/** Records that the first-run setup was finished or skipped. */
export async function completeFirstRun(): Promise<void> {
  if (!isTauriRuntime()) {
    mockSetup.firstRunCompleted = true;
    return;
  }
  return invoke<void>("complete_first_run");
}

/** Free bytes on the volume that holds the models folder, if known. */
export async function getModelsFreeSpace(): Promise<number | null> {
  if (!isTauriRuntime()) return 48_000_000_000;
  return invoke<number | null>("get_models_free_space");
}

export type GlobeKeyAction = "do_nothing" | "change_input_source" | "show_emoji" | "start_dictation" | "unknown";

/** What macOS does on a tap of the 🌐 key ("Press 🌐 key to"). */
export async function getGlobeKeyAction(): Promise<GlobeKeyAction> {
  if (!isTauriRuntime()) return "show_emoji";
  return invoke<GlobeKeyAction>("get_globe_key_action");
}

/** Opens System Settings → Keyboard (macOS). */
export async function openKeyboardSettings(): Promise<void> {
  if (!isTauriRuntime()) return;
  return invoke<void>("open_keyboard_settings");
}

/** Shows macOS's Accessibility prompt, which also lists Murmela in System
 * Settings. Only call after explaining the permission. */
export async function requestAccessibilityAccess(): Promise<void> {
  if (!isTauriRuntime()) return;
  return invoke<void>("request_accessibility_access");
}

// Browser previews: `?update-ready` shows a downloaded release.
const mockUpdateStatus = (): UpdateStatus => {
  const ready = typeof window !== "undefined" && new URLSearchParams(window.location.search).has("update-ready");
  return {
    state: ready ? "ready" : "up_to_date",
    currentVersion: "1.0.0",
    version: ready ? "0.2.0" : null,
    notes: ready ? "Faster live dictation and a clearer permissions setup." : null,
    message: null,
    lastCheckedMs: Date.now(),
  };
};

export async function getUpdateStatus(): Promise<UpdateStatus> {
  if (!isTauriRuntime()) return mockUpdateStatus();
  return invoke<UpdateStatus>("get_update_status");
}

/** Looks for a newer release now and downloads it if there is one. */
export async function checkForUpdates(): Promise<UpdateStatus> {
  if (!isTauriRuntime()) return mockUpdateStatus();
  return invoke<UpdateStatus>("check_for_updates");
}

export async function installUpdateAndRestart(): Promise<void> {
  if (!isTauriRuntime()) return;
  return invoke<void>("install_update_and_restart");
}

export async function listenUpdateStatus(handler: (status: UpdateStatus) => void): Promise<() => void> {
  if (!isTauriRuntime()) return () => undefined;
  const { listen } = await import("@tauri-apps/api/event");
  return listen<UpdateStatus>("update-status", (event) => handler(event.payload));
}

/** Open the OS pane where the user grants Accessibility access (macOS). */
export async function openAccessibilitySettings(): Promise<void> {
  if (!isTauriRuntime()) {
    return;
  }
  return invoke<void>("open_accessibility_settings");
}

export async function listenQuickDictateStatus(
  handler: (status: QuickDictationStatusResponse) => void,
): Promise<() => void> {
  if (!isTauriRuntime()) {
    return () => undefined;
  }

  const { listen } = await import("@tauri-apps/api/event");
  return listen<QuickDictationStatusResponse>(
    "quick-dictate-status",
    (event) => {
      handler(event.payload);
    },
  );
}

export async function listenFileTranscriptionStatus(
  handler: (status: FileTranscriptionStatusEvent) => void,
) {
  if (!isTauriRuntime()) {
    mockFileTranscriptionListeners.add(handler);
    return () => {
      mockFileTranscriptionListeners.delete(handler);
    };
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<FileTranscriptionStatusEvent>(
    "file-transcription-status",
    (event) => {
      void hydrateFileStatus(event.payload)
        .then(handler)
        .catch(() => handler(event.payload));
    },
  );
}

function emitMockFileTranscriptionStatus(event: FileTranscriptionStatusEvent) {
  const current = mockFileTranscriptionStatuses.get(event.jobId);
  mockFileTranscriptionStatuses.set(event.jobId, {
    ...event,
    startedAtMs: current?.startedAtMs ?? event.startedAtMs,
    updatedAtMs: Date.now(),
  });
  const nextEvent = mockFileTranscriptionStatuses.get(event.jobId)!;
  for (const listener of mockFileTranscriptionListeners) {
    listener(nextEvent);
  }
}

function emitMockModelDownloadStatus(status: ModelDownloadStatus) {
  for (const listener of mockModelDownloadListeners) {
    listener(status);
  }
}

export async function listVocabularyTerms(): Promise<VocabularyTerm[]> {
  if (!isTauriRuntime()) {
    return [...mockVocabularyTerms];
  }
  return invoke<VocabularyTerm[]>("list_vocabulary_terms");
}

export async function createVocabularyTerm(
  input: CreateVocabularyTermInput,
): Promise<VocabularyTerm> {
  if (!isTauriRuntime()) {
    const now = new Date().toISOString();
    const term: VocabularyTerm = {
      id: crypto.randomUUID(),
      canonical: input.canonical,
      normalizedCanonical: input.canonical.trim().toLowerCase(),
      matchMode: input.matchMode,
      isBuiltin: false,
      createdAt: now,
      updatedAt: now,
      aliases: input.aliases
        .filter((alias) => alias.trim().length > 0)
        .map((alias) => ({
          id: crypto.randomUUID(),
          alias,
          normalizedAlias: alias.trim().toLowerCase(),
        })),
    };
    mockVocabularyTerms.push(term);
    return term;
  }
  return invoke<VocabularyTerm>("create_vocabulary_term", { input });
}

export async function updateVocabularyTerm(
  termId: string,
  input: UpdateVocabularyTermInput,
): Promise<VocabularyTerm> {
  if (!isTauriRuntime()) {
    const index = mockVocabularyTerms.findIndex((term) => term.id === termId);
    if (index < 0) {
      throw new Error(`Vocabulary term ${termId} not found`);
    }
    const current = mockVocabularyTerms[index];
    const updated: VocabularyTerm = {
      ...current,
      canonical: input.canonical,
      normalizedCanonical: input.canonical.trim().toLowerCase(),
      matchMode: input.matchMode,
      updatedAt: new Date().toISOString(),
      aliases: input.aliases
        .filter((alias) => alias.trim().length > 0)
        .map((alias) => ({
          id: crypto.randomUUID(),
          alias,
          normalizedAlias: alias.trim().toLowerCase(),
        })),
    };
    mockVocabularyTerms[index] = updated;
    return updated;
  }
  return invoke<VocabularyTerm>("update_vocabulary_term", { termId, input });
}

export async function deleteVocabularyTerm(termId: string): Promise<void> {
  if (!isTauriRuntime()) {
    const index = mockVocabularyTerms.findIndex((term) => term.id === termId);
    if (index >= 0) {
      mockVocabularyTerms.splice(index, 1);
    }
    return;
  }
  return invoke("delete_vocabulary_term", { termId });
}

export async function previewFeedbackSound(
  cue: "start" | "stop" | "complete" | "error",
): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke("preview_feedback_sound", { cue });
}

export async function reportManualFeedback(
  operationId: string,
  failed: boolean,
): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke("report_manual_feedback", { operationId, failed });
}

// Full text is fetched once per result revision. Heartbeats and polling only
// carry lightweight metadata; concurrent event/poll reads share one promise.
const fileResults = new Map<
  string,
  {
    revision: number;
    reference: import("../types/domain").ReviewRef;
    result: Promise<FileTranscriptionResponse>;
  }
>();
async function hydrateFileStatus(
  status: FileTranscriptionStatusEvent,
): Promise<FileTranscriptionStatusEvent> {
  if (!status.reviewRef || !status.resultRevision || status.result)
    return status;
  let entry = fileResults.get(status.jobId);
  if (!entry || entry.revision !== status.resultRevision) {
    const result = invoke<FileTranscriptionResponse>(
      "get_file_transcription_result",
      { jobId: status.jobId },
    );
    entry = {
      revision: status.resultRevision,
      reference: status.reviewRef,
      result,
    };
    fileResults.set(status.jobId, entry);
    const expected = entry;
    void result.catch(() => {
      if (fileResults.get(status.jobId) === expected)
        fileResults.delete(status.jobId);
    });
  }
  return { ...status, result: await entry.result };
}
export function invalidateFileReview(
  reference: import("../types/domain").ReviewRef,
) {
  // Edits do not change a file job's processing stage, but its next read must
  // reflect the latest projection. The review view owns its own revision data.
  for (const [id, entry] of fileResults)
    if (
      entry.reference.kind === reference.kind &&
      entry.reference.id === reference.id
    )
      fileResults.delete(id);
}
export async function dismissFileTranscription(jobId: string): Promise<void> {
  if (isTauriRuntime()) await invoke("dismiss_file_transcription", { jobId });
  else mockFileTranscriptionStatuses.delete(jobId);
  fileResults.delete(jobId);
}
