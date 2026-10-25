/** `meeting`: a recorded call or in-person meeting. */
export type SourceType = "quick_dictate" | "file_upload" | "meeting";

export type TranscriptStatus =
  | "queued"
  | "recording"
  | "processing"
  | "completed"
  | "failed"
  | "canceled";

export type TranscriptQualityStatus = "clean" | "recovered" | "partial";

export type LanguageMode = "auto" | "fixed";
export type InsertBehavior = "paste" | "clipboard_only";
export type ModelProfile = "fast" | "balanced" | "accurate";
export type ModelCapability = "asr" | "vad" | "diarization" | "translation";
export type ModelUseContext =
  | "shortcut_dictation"
  | "quick_dictate"
  | "file_transcription";
export type ModelLanguageControl = "automatic_and_fixed" | "automatic_only";
export type DiarizationSource = "none" | "native_model" | "post_process";
export type DiarizationStatus =
  | "not_requested"
  | "pending"
  | "running"
  | "completed"
  | "completed_with_uncertainty"
  | "failed"
  | "canceled"
  | "not_enough_speech";
export type SpeakerAttribution =
  | "none"
  | "assigned"
  | "likely"
  | "uncertain"
  | "overlap";
export type ShortcutMode = "push_to_talk" | "toggle";

export type StartupPhase =
  | "files"
  | "models"
  | "audio"
  | "library"
  | "shortcuts"
  | "workspace"
  | "ready"
  | "failed";

export interface StartupStatus {
  phase: StartupPhase;
  step: number;
  totalSteps: 6;
  errorMessage?: string;
}

export interface HealthCheckResponse {
  appName: string;
  appVersion: string;
  dbPath: string;
  tempDir: string;
  modelsDir: string;
  startupNotices: string[];
}

export interface TranscriptSummary {
  id: string;
  createdAt: string;
  sourceType: SourceType;
  title: string;
  plainText: string;
  status: TranscriptStatus;
  detectedLanguages: string[];
  durationMs: number | null;
  modelName: string | null;
  qualityStatus: TranscriptQualityStatus;
  recoveredRegionCount: number;
  diarizationStatus: DiarizationStatus;
  speakerCount: number | null;
}

export interface AppSettings {
  defaultMode: "quick_dictate" | "file_transcribe";
  shortcut: string;
  translationEnabled: boolean;
  translationCycleShortcut: string;
  /** Output languages the language shortcut visits after Original, in shortcut order. */
  translationCycleLanguages: Exclude<OutputMode, "original">[];
  translationModelId: string;
  shortcutMode: ShortcutMode;
  languageMode: LanguageMode;
  fixedLanguage: string | null;
  preferredInputDevice: string | null;
  insertBehavior: InsertBehavior;
  launchAtLoginEnabled: boolean;
  gpuEnabled: boolean;
  shortcutDictationModelProfile: ModelProfile;
  shortcutDictationSelectedModelId: string | null;
  quickDictateModelProfile: ModelProfile;
  quickDictateSelectedModelId: string | null;
  fileTranscribeModelProfile: ModelProfile;
  fileTranscribeSelectedModelId: string | null;
  appearance: "system" | "light" | "dark";
  motionPreference: "system" | "reduced";
  saveHistory: boolean;
  soundsEnabled: boolean;
  volumeDuckingEnabled: boolean;
  fileDiarizationEnabled: boolean;
  livePairChunkMs: LivePairChunkMs;
  livePairKeepLoaded: boolean;
  pasteLastShortcut: string;
  seriousMode: boolean;
  autoUpdateEnabled: boolean;
}

export type LivePairChunkMs = 560 | 1120;

export interface LivePairStatus {
  state: "off" | "preparing" | "ready" | "unloaded" | "error";
  message: string | null;
  loadMs: number | null;
  rssBytes: number | null;
}

export interface SettingsPatch {
  defaultMode?: "quick_dictate" | "file_transcribe";
  shortcut?: string;
  translationEnabled?: boolean;
  translationCycleShortcut?: string;
  translationCycleLanguages?: Exclude<OutputMode, "original">[];
  translationModelId?: string;
  shortcutMode?: ShortcutMode;
  languageMode?: LanguageMode;
  fixedLanguage?: string | null;
  preferredInputDevice?: string | null;
  insertBehavior?: InsertBehavior;
  launchAtLoginEnabled?: boolean;
  gpuEnabled?: boolean;
  shortcutDictationModelProfile?: ModelProfile;
  shortcutDictationSelectedModelId?: string | null;
  quickDictateModelProfile?: ModelProfile;
  quickDictateSelectedModelId?: string | null;
  fileTranscribeModelProfile?: ModelProfile;
  fileTranscribeSelectedModelId?: string | null;
  appearance?: AppSettings["appearance"];
  motionPreference?: AppSettings["motionPreference"];
  saveHistory?: boolean;
  soundsEnabled?: boolean;
  volumeDuckingEnabled?: boolean;
  fileDiarizationEnabled?: boolean;
  livePairChunkMs?: LivePairChunkMs;
  livePairKeepLoaded?: boolean;
  pasteLastShortcut?: string;
  seriousMode?: boolean;
  autoUpdateEnabled?: boolean;
}

export interface InstalledModel {
  id: string;
  engine: string;
  modelName: string;
  variant: string;
  localPath: string;
  sizeBytes: number;
  isDefault: boolean;
  profile: ModelProfile;
  capabilities?: ModelCapabilities;
}

export interface ModelCapabilities {
  supportedContexts: ModelUseContext[];
  nativeDiarization: boolean;
  timestampedSegments: boolean;
  contextSupport: boolean;
  languageControl: ModelLanguageControl;
  maximumAudioDurationMs: number | null;
  streamingTranscription?: boolean;
}

export interface DownloadableModel {
  id: string;
  engine: string;
  modelName: string;
  description: string;
  sizeBytes: number;
  profile: ModelProfile;
  availability: "available" | "unsupported_platform";
  availabilityReason: string | null;
  installed: boolean;
  requirements: string | null;
  artifactCount: number;
  capability: ModelCapability;
  capabilities?: ModelCapabilities;
  licenseUrl?: string | null;
  /** catalog: offered for download · custom: added to the models folder by hand. */
  origin?: ModelOrigin;
}

export type ModelOrigin = "catalog" | "custom";

export type ModelDownloadState =
  | "idle"
  | "downloading"
  | "completed"
  | "canceled"
  | "failed";

export interface ModelDownloadStatus {
  modelId: string;
  modelName: string;
  state: ModelDownloadState;
  downloadedBytes: number;
  totalBytes: number | null;
  progressPercent: number | null;
  errorMessage: string | null;
  currentArtifact: string | null;
  artifactIndex: number | null;
  artifactCount: number;
}

export type PreviewSourceKind = "quick_dictate" | "file_upload";

export interface TranscriptSegment {
  id: string;
  startMs: number;
  endMs: number;
  text: string;
  languageCode: string;
  segmentOrder: number;
  confidence: number | null;
  speakerId: string | null;
  speakerIds: string[] | null;
  speakerAttribution: SpeakerAttribution;
  speakerConfidence: number | null;
}

export interface DiarizationTurn {
  id: string;
  startMs: number;
  endMs: number;
  speakerIds: string[];
  confidence: number | null;
  isOverlap: boolean;
  isUncertain: boolean;
  turnOrder: number;
}
export interface TranscriptSpeaker {
  speakerId: string;
  displayName: string;
  speakerOrder: number;
}

export interface TranscriptResult {
  jobId: string;
  modelName: string;
  fullText: string;
  plainText: string;
  timestampedText: string;
  detectedLanguages: string[];
  segments: TranscriptSegment[];
  qualityStatus: TranscriptQualityStatus;
  recoveredRegionCount: number;
  warnings: TranscriptWarning[];
  diarizationStatus: DiarizationStatus;
  diarizationModelId: string | null;
  diarizationSource?: DiarizationSource;
  diarizationWarning: string | null;
  diarizationPolicyVersion: number | null;
  diarizationClusteringThreshold: number | null;
  diarizationSpeakerCountHint: number | null;
  speakers: TranscriptSpeaker[];
  diarizationTurns: DiarizationTurn[];
}

export interface TranscriptDetail extends TranscriptSummary {
  translation?: DictationOutput | null;
  manualSegmentIds?: string[];
  fullText: string;
  timestampedText: string;
  transcriptionWarnings: TranscriptWarning[];
  diarizationModelId: string | null;
  diarizationSource?: DiarizationSource;
  diarizationWarning: string | null;
  diarizationPolicyVersion: number | null;
  diarizationClusteringThreshold: number | null;
  diarizationSpeakerCountHint: number | null;
  segments: TranscriptSegment[];
  speakers: TranscriptSpeaker[];
  diarizationTurns: DiarizationTurn[];
}

export type TranscriptExportFormat = "txt" | "md" | "srt" | "vtt" | "json";
export type TranscriptCopyVariant = "speaker_aware" | "plain" | "translation";
export interface TranscriptExportResult {
  path: string | null;
}

export interface TranscriptWarning {
  startMs: number;
  endMs: number;
  reason: string;
  attempts: number;
  outcome: string;
}

export interface EngineErrorPayload {
  code: string;
  message: string;
}

export interface TranscriptionPreviewRequest {
  sessionId?: string;
  sourceKind: PreviewSourceKind;
  profile: ModelProfile;
  selectedModelId?: string | null;
  languageMode: LanguageMode;
  fixedLanguage: string | null;
  timestamps: boolean;
  preferGpu: boolean;
  filePath?: string | null;
}

export interface TranscriptionPreviewResponse {
  dictationOutput?: DictationOutput | null;
  sourceKind: PreviewSourceKind;
  resolvedModel: InstalledModel | null;
  result: TranscriptResult | null;
  error: EngineErrorPayload | null;
}

export interface SelectedSourceFile {
  filePath: string;
  originalName: string;
  mimeType: string;
  sizeBytes: number;
  durationMs: number | null;
  sha256: string | null;
}

export interface InputDeviceOption {
  id: string;
  name: string;
  isDefault: boolean;
}

export interface FileTranscriptionRequest {
  jobId: string;
  sourceFile: SelectedSourceFile;
  speakerCountHint: number | null;
}

export type FileTranscriptionJobStage =
  | "queued"
  | "preparing"
  | "transcribing"
  | "diarizing"
  | "saving"
  | "completed"
  | "failed"
  | "canceled";

export interface StartFileTranscriptionResponse {
  jobId: string;
}

export interface FileTranscriptionResponse {
  sourceFile: SelectedSourceFile;
  resolvedModel: InstalledModel | null;
  result: TranscriptResult;
  savedTranscript: TranscriptSummary | null;
}

export interface FileTranscriptionStatusEvent {
  reviewRef?: ReviewRef | null;
  resultRevision?: number;
  jobId: string;
  sourceFile: SelectedSourceFile;
  stage: FileTranscriptionJobStage;
  progressPercent: number | null;
  processedMs: number | null;
  totalMs: number | null;
  etaSeconds: number | null;
  statusText: string;
  result: FileTranscriptionResponse | null;
  errorMessage: string | null;
  startedAtMs: number;
  updatedAtMs: number;
}


export interface FileQueueItem {
  reviewRef?: ReviewRef | null;
  resultRevision?: number;
  updatedAt?: number;
  id: string;
  sourceFile: SelectedSourceFile;
  stage: FileTranscriptionJobStage;
  progressPercent: number | null;
  processedMs: number | null;
  totalMs: number | null;
  etaSeconds: number | null;
  statusText: string;
  result: FileTranscriptionResponse | null;
  errorMessage: string | null;
  startedAt: number | null;
}

export type RecordingOverlayState =
  | "idle"
  | "listening"
  | "processing"
  | "success"
  | "error";

export interface RecordingStatusResponse {
  state: RecordingOverlayState;
  currentSessionId: string | null;
  activeInputDevice: string | null;
  lastRecordingPath: string | null;
  lastErrorMessage: string | null;
  durationMs: number | null;
  sampleRateHz: number | null;
  channels: number | null;
}

export interface RecordingResult {
  sessionId: string;
  filePath: string;
  durationMs: number;
  sampleRateHz: number;
  channels: number;
  sampleCount: number;
}

export type ManualTranscriptionStage = "idle" | "processing" | "failed";

export interface ManualTranscriptionUiState {
  stage: ManualTranscriptionStage;
  statusText: string;
  startedAt: number | null;
  errorMessage: string | null;
}

export type QuickDictationState =
  | "idle"
  | "listening"
  | "processing"
  | "inserted"
  | "clipboard_only"
  | "error";

export type InsertionOutcome = "pasted" | "clipboard_only";

export interface QuickDictationStatusResponse {
  canRetryStreaming?: boolean;
  state: QuickDictationState;
  registeredShortcut: string | null;
  shortcutMode: ShortcutMode;
  isRegistered: boolean;
  lastTranscriptText: string | null;
  lastTranscriptId: string | null;
  lastRecordingPath: string | null;
  lastErrorMessage: string | null;
  lastModelName: string | null;
  lastInsertOutcome: InsertionOutcome | null;
  lastInsertWarning?: string | null;
  lastDurationMs: number | null;
}

export type MicrophonePermission =
  | "granted"
  | "denied"
  | "restricted"
  | "not_determined";

/** Totals behind the Murmelmeter (no text is stored). */
export interface DictationStats {
  todayWords: number;
  todayDictations: number;
  totalWords: number;
  totalDictations: number;
  totalDurationMs: number;
  streakDays: number;
}

/** Records a call (microphone and the call's sound) or a meeting in the room (microphone only). */
export type CallMode = "call" | "room";

export interface CallCaptureSupport {
  supported: boolean;
  reason: string | null;
}

export interface CallChannelStatus {
  label: string;
  level: number;
  error: string | null;
}

/** What the browser helper reports from Google Meet, Teams or Zoom in the browser. */
export interface CallBrowserStatus {
  platform: string;
  meetingTitle: string | null;
  participants: string[];
  speaking: string[];
  msSinceLastReport: number;
}

export interface CallStatus {
  recording: boolean;
  callId: string | null;
  mode: CallMode | null;
  title: string | null;
  elapsedMs: number;
  limitMs: number;
  microphone: CallChannelStatus | null;
  system: CallChannelStatus | null;
  browser: CallBrowserStatus | null;
  bridgePort: number | null;
}

export interface CallTranscriptionStarted {
  jobId: string;
  sourceFile: SelectedSourceFile;
}

/** A recording not transcribed yet: its transcription failed, or Murmela quit while recording. */
export interface PendingCallRecording {
  id: string;
  title: string;
  startedAt: string;
  durationMs: number;
  mode: CallMode;
  interrupted: boolean;
}

export type NetworkDestinationId = "hugging_face" | "github" | "lemon_squeezy";

/** One of the only places Murmela ever contacts. */
export interface NetworkDestination {
  id: NetworkDestinationId;
  name: string;
  host: string;
  reason: string;
}

/** A connection Murmela made, newest first. */
export interface NetworkActivityEntry {
  id: number;
  startedAt: string;
  destination: NetworkDestinationId;
  host: string;
  purpose: string;
  /** Bytes sent and received; null when the updater doesn't report it. */
  bytes: number | null;
  outcome: "completed" | "failed" | "canceled";
}

export interface NetworkActivity {
  destinations: NetworkDestination[];
  entries: NetworkActivityEntry[];
}

export type UpdateState =
  | "idle"
  | "checking"
  | "up_to_date"
  /** A newer release exists; it downloads when the user asks. */
  | "available"
  | "downloading"
  | "ready"
  /** The downloaded release couldn't replace this copy of Murmela. */
  | "cannot_install"
  | "failed"
  /** A newer release exists, but this Mac's Pro key doesn't cover it. */
  | "needs_renewal";

export interface UpdateStatus {
  state: UpdateState;
  currentVersion: string;
  /** The release being downloaded or ready to install. */
  version: string | null;
  notes: string | null;
  message: string | null;
  lastCheckedMs: number | null;
  /** While downloading: percent done, when the size is known. */
  progress: number | null;
  /** With `needs_renewal`: the last day (YYYY-MM-DD) of the Pro key's update year. */
  proUpdatesUntil: string | null;
}

export interface DictationReadiness {
  hasModel: boolean;
  /** The saved shortcut model is installed and can run shortcut dictation. */
  shortcutModelReady: boolean;
  microphone: MicrophonePermission;
  firstRunCompleted: boolean;
  translationReady?: boolean;
  shortcutRegistered: boolean;
  autoPasteEnabled: boolean;
  accessibilityRequired: boolean;
  accessibilityGranted: boolean;
}

export interface VocabularyAlias {
  id: string;
  alias: string;
  normalizedAlias: string;
}

export type VocabularyMatchMode = "exact_only" | "exact_and_fuzzy";

export interface VocabularyTerm {
  id: string;
  canonical: string;
  normalizedCanonical: string;
  matchMode: VocabularyMatchMode;
  isBuiltin: boolean;
  createdAt: string;
  updatedAt: string;
  aliases: VocabularyAlias[];
}

export interface CreateVocabularyTermInput {
  canonical: string;
  aliases: string[];
  matchMode: VocabularyMatchMode;
}

export interface UpdateVocabularyTermInput {
  canonical: string;
  aliases: string[];
  matchMode: VocabularyMatchMode;
}

export type ReviewRef = { kind: "saved" | "session"; id: string };
export interface ReviewDocument {
  reference: ReviewRef;
  detail: TranscriptDetail;
  revision: number;
  manualSegmentIds: string[];
  unmatchedSpeakerIds: string[];
  canUndo: boolean;
}
export type ReviewEdit =
  | { type: "rename"; speakerId: string; name: string }
  | { type: "add_speaker"; name: string }
  | {
      type: "assign";
      segmentIds: string[];
      speakerIds: string[];
      newSpeakerName?: string | null;
    }
  | { type: "merge"; speakerIds: string[]; targetId: string }
  | { type: "undo" };
export interface ReviewJobStatus {
  speakerCount?: number | null;
  jobId: string;
  reference: ReviewRef;
  stage:
    | "queued"
    | "validating"
    | "diarizing"
    | "saving"
    | "canceling"
    | "completed"
    | "failed"
    | "canceled";
  statusText: string;
  error: { code: string; message: string } | null;
  resultRevision: number | null;
  startedAtMs: number;
  updatedAtMs: number;
}
export interface ReviewAudio {
  url: string;
  token: string;
  durationMs: number | null;
}

export type OutputMode = "original" | "en" | "de" | "fr" | "es" | "es-AR" | "it" | "pt-BR" | "nl" | "pl";
export interface DictationOutput {
  sourceLanguages?: string[];
  sessionId: string;
  sourceText: string;
  outputText: string | null;
  targetLanguage: OutputMode;
  errorCode?: string | null;
  status: "pending" | "completed" | "failed" | "canceled";
  modelId: string | null;
  modelRevision: string | null;
  promptVersion: number | null;
  transcriptId: string | null;
  errorMessage: string | null;
}
export interface DictationOutputState {
  outputMode: OutputMode;
  busy: boolean;
  stage: string;
  statusText: string;
  ready: boolean;
  errorMessage: string | null;
  lastOutput: DictationOutput | null;
}

/** Murmela Pro license on this Mac. Activated once online, then checked only locally. */
/** Pro: one year of updates. Lifetime: updates for good. */
export type LicenseTier = "pro" | "lifetime";

export interface LicenseStatus {
  /** The Pro features are unlocked in this version of Murmela. */
  pro: boolean;
  /** The key on this Mac. A Pro key with `pro: false` means this version came out after its update year. */
  tier: LicenseTier | null;
  /** Last day (YYYY-MM-DD) whose releases a Pro key unlocks; null for Lifetime. */
  updatesUntil: string | null;
  /** The day this version of Murmela was released (YYYY-MM-DD). */
  releaseDate: string;
  keyHint: string | null;
  licensedTo: string | null;
  activatedAt: string | null;
}
