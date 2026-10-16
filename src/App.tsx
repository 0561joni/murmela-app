import murmelaMark from "./assets/murmela-mark.png";
import { useDictationTranslation } from "./hooks/useDictationTranslation";
import { setDictationOutputMode, retryDictationTranslation, retryStreamingDictation, listenTranslationErrors } from "./lib/translationApi";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  reportManualFeedback,
  dismissFileTranscription,
  invalidateFileReview,
  cancelFileTranscription,
  deleteAllTranscripts,
  deleteTranscript,
  getFileTranscriptionStatuses,
  getModelDownloadStatuses,
  listenModelDownloadStatus,
  getStartupStatus,
  getHealthCheck,
  getDictationReadiness,
  getQuickDictateStatus,
  getRecordingStatus,
  getSettings,
  openAccessibilitySettings,
  requestAccessibilityAccess,
  openMicrophoneSettings,
  requestMicrophoneAccess,
  completeFirstRun,
  getDictationStats,
  listenDictationStats,
  listInstalledModels,
  listDownloadableModels,
  listTranscripts,
  listVocabularyTerms,
  listenFileTranscriptionStatus,
  listenQuickDictateStatus,
  listenStartupStatus,
  pickAudioFiles,
  prepareDroppedAudioFiles,
  previewTranscription,
  startFileTranscription,
  startRecordingSession,
  stopRecordingSession,
  cancelRecordingSession,
  resetQuickDictation,
  updateSettings,
  createVocabularyTerm,
  updateVocabularyTerm,
  deleteVocabularyTerm,
  frontendStartupComplete,
  reportStartupFailure,
  listenLicenseLink,
  takePendingLicenseLink,
} from "./lib/api";
import { ReviewWorkspace } from "./screens/ReviewWorkspace";
import { useReviewJobs } from "./hooks/useReviewJobs";
import { reviewKey, isReviewJobActive } from "./lib/reviewApi";
import type { ReviewRef } from "./types/domain";
import { DictateScreen } from "./screens/DictateScreen";
import { FirstRunSetup } from "./screens/FirstRunSetup";
import { FilesScreen, isFileWorking } from "./screens/FilesScreen";
import { MeetingsScreen } from "./screens/MeetingsScreen";
import { applyAppearance, usePlayful } from "./lib/appearance";
import { crossedMilestone } from "./lib/murmelmeter";
import { SettingsScreen } from "./screens/SettingsScreen";
import { HistoryScreen } from "./screens/HistoryScreen";
import { VocabularyScreen } from "./screens/VocabularyScreen";
import { TranslateScreen, type TranslateOpenRequest } from "./screens/TranslateScreen";
import { AccessibilityExplainer } from "./components/AccessibilityExplainer";
import { UpdateBanner } from "./components/UpdateBanner";
import { ProBanner } from "./components/ProBanner";
import { useUpdateStatus } from "./hooks/useUpdateStatus";
import { activateFromLink, useLicense } from "./hooks/useLicense";
import { onOpenLicenseSettings } from "./components/ProHint";
import { AppIcon, IconButton, type AppIconName } from "./components/IconButton";
import { describeError, PASTE_SHORTCUT, readableError } from "./lib/formatting";
import { useAccessibilityReadinessPolling } from "./hooks/useAccessibilityReadinessPolling";
import type {
  AppSettings,
  DictationReadiness,
  DictationStats,
  FileQueueItem,
  FileTranscriptionStatusEvent,
  HealthCheckResponse,
  InstalledModel,
  ManualTranscriptionUiState,
  ModelDownloadStatus,
  ModelUseContext,
  QuickDictationStatusResponse,
  RecordingStatusResponse,
  SettingsPatch,
  TranscriptSummary,
  TranscriptionPreviewResponse,
  VocabularyTerm,
} from "./types/domain";

type ScreenId = "dictate" | "files" | "meetings" | "translate" | "settings" | "vocabulary" | "history";

type ToastKind = "success" | "info" | "error";

interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  hint?: string;
  durationMs: number;
}

const TERMINAL_DICTATION_STATES = new Set([
  "inserted",
  "clipboard_only",
  "error",
]);

const NAV_ITEMS: Array<{ id: ScreenId; label: string; icon: AppIconName }> = [
  { id: "dictate", label: "Dictate", icon: "microphone" },
  { id: "files", label: "Transcribe files", icon: "fileAudio" },
  { id: "meetings", label: "Meetings", icon: "headset" },
  { id: "translate", label: "Translate", icon: "languages" },
  { id: "history", label: "Library", icon: "library" },
  { id: "vocabulary", label: "Vocabulary", icon: "book" },
  { id: "settings", label: "Settings", icon: "gear" },
];

export function App() {
  const [reviewTarget, setReviewTarget] = useState<{
    reference: ReviewRef;
    originLabel: string;
    scroll: number;
    anchor: HTMLElement | null;
  } | null>(null);
  const [screen, setScreen] = useState<ScreenId>("dictate");
  const [settingsSection, setSettingsSection] = useState("general");
  const { pro } = useLicense();
  const [downloadCount, setDownloadCount] = useState(0);
  const downloadStages = useRef(new Map<string, string>());
  const currentSettingsSection = useRef(settingsSection);
  currentSettingsSection.current = settingsSection;
  const [libraryVisited, setLibraryVisited] = useState(false);
  const [translateVisited, setTranslateVisited] = useState(false);
  const [translateOpenRequest, setTranslateOpenRequest] = useState<TranslateOpenRequest | null>(null);
  const [isTranslateDragActive, setIsTranslateDragActive] = useState(false);
  const [translationsVersion, setTranslationsVersion] = useState(0);
  const [sidebarExpanded, setSidebarExpanded] = useState(true);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [outputState, setOutputState] = useDictationTranslation(settings?.translationEnabled);
  const [health, setHealth] = useState<HealthCheckResponse | null>(null);
  const { jobs: reviewJobs, accept: acceptReviewJob } = useReviewJobs(
    Boolean(health),
  );
  const [speakerModelReady, setSpeakerModelReady] = useState<boolean | null>(
    null,
  );
  const [installedModels, setInstalledModels] = useState<InstalledModel[]>([]);
  const modelRefreshInFlight = useRef<Promise<void> | null>(null);
  const modelRefreshRevision = useRef(0);
  const [transcripts, setTranscripts] = useState<TranscriptSummary[]>([]);
  const [vocabularyTerms, setVocabularyTerms] = useState<VocabularyTerm[]>([]);
  const [preview, setPreview] = useState<TranscriptionPreviewResponse | null>(
    null,
  );
  const [recordingStatus, setRecordingStatus] =
    useState<RecordingStatusResponse | null>(null);
  const [manualTranscriptionState, setManualTranscriptionState] =
    useState<ManualTranscriptionUiState>({
      stage: "idle",
      statusText: "",
      startedAt: null,
      errorMessage: null,
    });
  const [quickDictationStatus, setQuickDictationStatus] =
    useState<QuickDictationStatusResponse | null>(null);
  const [dictationError, setDictationError] = useState<string | null>(null);
  const [readiness, setReadiness] = useState<DictationReadiness | null>(null);
  const [firstRunOpen, setFirstRunOpen] = useState(false);
  const [accessibilityExplainerOpen, setAccessibilityExplainerOpen] = useState(false);
  const { status: updateStatus } = useUpdateStatus();
  const [dictationStats, setDictationStats] = useState<DictationStats | null>(null);
  const playful = usePlayful();
  const playfulRef = useRef(playful);
  const statsRef = useRef<DictationStats | null>(null);
  playfulRef.current = playful;
  const firstRunChecked = useRef(false);
  const microphonePoll = useRef<number | null>(null);
  const [fileQueueItems, setFileQueueItems] = useState<FileQueueItem[]>([]);
  const [isFileDragActive, setIsFileDragActive] = useState(false);
  const [speakerCountHint, setSpeakerCountHint] = useState<number | null>(null);
  const speakerCountHintRef = useRef<number | null>(null);
  speakerCountHintRef.current = speakerCountHint;
  const currentScreenRef = useRef<ScreenId>("dictate");
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastIdRef = useRef(0);
  const mainRef = useRef<HTMLElement>(null);
  const manualGeneration = useRef(0);
  const fileEventStages = useRef(new Map<string, string>());
  const fileModelReady = useRef(false);
  fileModelReady.current = installedModels.some(
    (model) =>
      !model.capabilities ||
      model.capabilities.supportedContexts.includes("file_transcription"),
  );
  const statusRefreshBusy = useRef(false);
  useEffect(
    () =>
      onOpenLicenseSettings(() => {
        setReviewTarget(null);
        setSettingsSection("license");
        setScreen("settings");
      }),
    [],
  );
  // Activation links (murmela://activate?key=…): show the License screen and activate there.
  const licenseLinkOpened = useRef(false);
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const collect = async () => {
      // A taken key is gone from the backend, so use it even if this effect was torn down meanwhile.
      const key = await takePendingLicenseLink().catch(() => null);
      if (!key) return;
      licenseLinkOpened.current = true;
      setReviewTarget(null);
      setSettingsSection("license");
      setScreen("settings");
      void activateFromLink(key);
    };
    void listenLicenseLink(() => void collect()).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    void collect();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
  useEffect(() => {
    let disposed = false;
    void listDownloadableModels()
      .then((models) => {
        if (!disposed)
          setSpeakerModelReady(
            models.find((m) => m.capability === "diarization")?.installed ??
              false,
          );
      })
      .catch(() => {});
    return () => {
      disposed = true;
    };
  }, [installedModels, settings?.fileDiarizationEnabled]);
  const fileHints = useRef(new Map<string, number | null>());
  const lastDictationStateRef = useRef<string | null>(null);

  const dismissToast = useCallback((id: number) => {
    setToasts((current) => current.filter((toast) => toast.id !== id));
  }, []);

  const pushToast = useCallback(
    (toast: Omit<Toast, "id">) => {
      const id = ++toastIdRef.current;
      setToasts((current) => [...current, { ...toast, id }]);
      if (toast.durationMs > 0) {
        window.setTimeout(() => dismissToast(id), toast.durationMs);
      }
    },
    [dismissToast],
  );

  const showDroppedAudioError = useCallback(
    (error: unknown) => {
      const message = errorMessage(
        error,
        "Drop audio (WAV, MP3, M4A, OPUS) or video (MP4, MOV, M4V, MKV, WEBM, AVI) files to transcribe.",
      );
      console.error(message);
      pushToast({
        kind: "error",
        message: "Unsupported file",
        hint: message,
        durationMs: 5000,
      });
    },
    [pushToast],
  );

  useEffect(() => {
    let disposed = false;
    let cleanup: (() => void) | undefined;
    void listenTranslationErrors((message) => {
      if (!disposed) pushToast({ kind: "error", message: "Translation needs attention", hint: message, durationMs: 6000 });
    }).then((unlisten) => {
      if (disposed) unlisten(); else cleanup = unlisten;
    }).catch(() => undefined);
    return () => { disposed = true; cleanup?.(); };
  }, [pushToast]);

  useEffect(() => {
    if (outputState?.lastOutput?.transcriptId) {
      void listTranscripts("").then(setTranscripts).catch(() => undefined);
    }
  }, [outputState?.lastOutput?.transcriptId, outputState?.lastOutput?.status]);

  const refreshReadiness =
    useCallback(async (): Promise<DictationReadiness | null> => {
      try {
        const nextReadiness = await getDictationReadiness();
        setReadiness(nextReadiness);
        return nextReadiness;
      } catch (error) {
        console.error(
          errorMessage(error, "Failed to check dictation readiness."),
        );
        return null;
      }
    }, []);

  const {
    isPolling: isPollingAccessibility,
    startPolling: startAccessibilityPolling,
    stopPolling: stopAccessibilityPolling,
  } = useAccessibilityReadinessPolling(refreshReadiness);

  useEffect(() => {
    let disposed = false;
    let startupStarted = false;
    let unlisten: (() => void) | null = null;

    const handleStartupStatus = async (
      status: Awaited<ReturnType<typeof getStartupStatus>>,
    ) => {
      if (
        disposed ||
        startupStarted ||
        (status.phase !== "workspace" && status.phase !== "ready")
      ) {
        return;
      }
      startupStarted = true;
      try {
        await loadAppState();
        if (!disposed) {
          await frontendStartupComplete();
        }
      } catch (error) {
        const message = errorMessage(
          error,
          "Failed to load the Murmela workspace.",
        );
        console.error(message);
        await reportStartupFailure(message).catch(() => undefined);
      }
    };

    void listenStartupStatus((status) => {
      void handleStartupStatus(status);
    }).then((cleanup) => {
      if (disposed) {
        cleanup();
      } else {
        unlisten = cleanup;
      }
    });
    void getStartupStatus()
      .then(handleStartupStatus)
      .catch(async (error) => {
        const message = errorMessage(error, "Could not read startup status.");
        console.error(message);
        await reportStartupFailure(message).catch(() => undefined);
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (
      readiness &&
      (!readiness.accessibilityRequired || readiness.accessibilityGranted)
    ) {
      stopAccessibilityPolling();
    }
  }, [
    readiness?.accessibilityGranted,
    readiness?.accessibilityRequired,
    stopAccessibilityPolling,
  ]);

  useEffect(() => {
    if (
      recordingStatus?.state !== "listening" &&
      quickDictationStatus?.state !== "listening"
    ) {
      return;
    }
    const interval = window.setInterval(() => {
      void getRecordingStatus()
        .then(setRecordingStatus)
        .catch(() => undefined);
    }, 400);
    return () => window.clearInterval(interval);
  }, [recordingStatus?.state, quickDictationStatus?.state]);

  useEffect(() => {
    currentScreenRef.current = screen;
    mainRef.current?.scrollTo?.({ top: 0 });
    if (screen === "history") setLibraryVisited(true);
    if (screen === "translate") setTranslateVisited(true);
    if (screen !== "files") {
      setIsFileDragActive(false);
    }
  }, [screen]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listenQuickDictateStatus(async (nextStatus) => {
      setQuickDictationStatus(nextStatus);
      void getRecordingStatus()
        .then(setRecordingStatus)
        .catch(() => undefined);
      if (nextStatus.state === "error")
        setDictationError(nextStatus.lastErrorMessage ?? "Dictation failed.");
      else if (nextStatus.state === "listening") { setDictationError(null); setPreview(null); }

      const previousState = lastDictationStateRef.current;
      lastDictationStateRef.current = nextStatus.state;
      if (
        previousState !== null &&
        previousState !== nextStatus.state &&
        currentScreenRef.current !== "dictate" &&
        document.visibilityState === "visible" &&
        TERMINAL_DICTATION_STATES.has(nextStatus.state)
      ) {
        switch (nextStatus.state) {
          case "inserted":
            pushToast({
              kind: "success",
              message: "Pasted ✓",
              durationMs: 1800,
            });
            break;
          case "clipboard_only":
            pushToast({
              kind: "info",
              message: "Copied to clipboard",
              hint: nextStatus.lastInsertWarning ?? `Press ${PASTE_SHORTCUT} to paste`,
              durationMs: nextStatus.lastInsertWarning ? 7000 : 3500,
            });
            break;
          case "error":
            pushToast({
              kind: "error",
              message: "Dictation failed",
              hint: nextStatus.lastErrorMessage
                ? readableError(nextStatus.lastErrorMessage)
                : undefined,
              durationMs: 0, // persistent until dismissed
            });
            break;
        }
      }

      if (
        nextStatus.lastTranscriptId ||
        nextStatus.state === "inserted" ||
        nextStatus.state === "clipboard_only"
      ) {
        try {
          setTranscripts(await listTranscripts(""));
        } catch (error) {
          console.error(
            errorMessage(error, "Failed to refresh shortcut transcripts."),
          );
        }
      }
    }).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => {
      unlisten?.();
    };
  }, [pushToast]);

  useEffect(() => {
    const refreshVisibleState = () => {
      void refreshShortcutOutputs();
      void refreshFileStatuses();
      void refreshReadiness();
    };

    const handleVisibilityChange = () => {
      if (document.visibilityState === "visible") {
        refreshVisibleState();
      }
    };
    document.addEventListener("visibilitychange", handleVisibilityChange);
    window.addEventListener("focus", refreshVisibleState);

    let disposed = false;
    let unlistenFocus: (() => void) | null = null;

    if ("__TAURI_INTERNALS__" in window) {
      void import("@tauri-apps/api/window")
        .then(async ({ getCurrentWindow }) => {
          const cleanup = await getCurrentWindow().onFocusChanged(
            ({ payload }) => {
              if (disposed) {
                return;
              }
              if (payload) {
                refreshVisibleState();
              }
            },
          );

          if (disposed) {
            cleanup();
            return;
          }
          unlistenFocus = cleanup;
        })
        .catch(() => undefined);
    }

    return () => {
      disposed = true;
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.removeEventListener("focus", refreshVisibleState);
      unlistenFocus?.();
    };
  }, [refreshReadiness]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listenFileTranscriptionStatus(async (event) => {
      const previousStage = fileEventStages.current.get(event.jobId);
      fileEventStages.current.set(event.jobId, event.stage);
      if (
        previousStage &&
        previousStage !== event.stage &&
        (event.stage === "completed" || event.stage === "failed") &&
        currentScreenRef.current !== "files"
      ) {
        pushToast({
          kind: event.stage === "failed" ? "error" : "success",
          message:
            event.stage === "failed"
              ? "File needs attention"
              : "Transcript ready",
          hint: event.sourceFile.originalName,
          durationMs: event.stage === "failed" ? 0 : 4000,
        });
      }
      setFileQueueItems((current) => mergeFileStatusIntoQueue(current, event));
      if (event.result?.savedTranscript) {
        setTranscripts((current) =>
          prependTranscriptUnique(current, event.result!.savedTranscript!),
        );
      }
    }).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  // Listen for drag-drop events emitted from the Rust backend via on_webview_event.
  // This bypasses Tauri's onDragDropEvent JS API which has known issues on macOS,
  // and uses the same global event system that file-transcription-status events use.
  useEffect(() => {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
      return;
    }

    let disposed = false;
    const unlisteners: (() => void)[] = [];

    void import("@tauri-apps/api/event")
      .then(async ({ listen }) => {
        if (disposed) return;

        unlisteners.push(
          await listen("app://file-drag-enter", () => {
            if (currentScreenRef.current === "files") {
              setIsFileDragActive(true);
            }
            if (currentScreenRef.current === "translate") {
              setIsTranslateDragActive(true);
            }
          }),
        );

        unlisteners.push(
          await listen("app://file-drag-leave", () => {
            setIsFileDragActive(false);
            setIsTranslateDragActive(false);
          }),
        );

        unlisteners.push(
          await listen<string[]>("app://file-drop", async (event) => {
            setIsFileDragActive(false);
            setIsTranslateDragActive(false);
            if (currentScreenRef.current === "translate" && event.payload[0]) {
              setTranslateOpenRequest({ kind: "file", path: event.payload[0], nonce: Date.now() });
              return;
            }
            if (currentScreenRef.current !== "files") return;
            try {
              const files = await prepareDroppedAudioFiles(event.payload);
              enqueueSelectedFiles(files, speakerCountHintRef.current);
            } catch (error) {
              showDroppedAudioError(error);
            }
          }),
        );
      })
      .catch((error) => {
        console.error(
          "[drag-drop] Failed to set up drag-drop listeners:",
          error,
        );
      });

    return () => {
      disposed = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [showDroppedAudioError]);

  const hasActiveFiles = fileQueueItems.some((item) =>
    isFileWorking(item.stage),
  );
  useEffect(() => {
    if (!hasActiveFiles) return;
    const interval = window.setInterval(() => {
      void refreshFileStatuses();
    }, 2000);
    return () => window.clearInterval(interval);
  }, [hasActiveFiles]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const liveStatuses = new Set<string>();
    const updateCount = () =>
      setDownloadCount(
        [...downloadStages.current.values()].filter(
          (stage) => stage === "downloading",
        ).length,
      );
    const acceptStatus = (status: ModelDownloadStatus) => {
      if (disposed) return;
      const previous = downloadStages.current.get(status.modelId);
      downloadStages.current.set(status.modelId, status.state);
      updateCount();
      if (
        previous === "downloading" &&
        (status.state === "completed" || status.state === "failed") &&
        (currentScreenRef.current !== "settings" ||
          currentSettingsSection.current !== "models")
      ) {
        pushToast({
          kind: status.state === "failed" ? "error" : "success",
          message:
            status.state === "failed"
              ? "Model download needs attention"
              : "Model installed",
          hint: status.errorMessage ? readableError(status.errorMessage) : status.modelName,
          durationMs: status.state === "failed" ? 0 : 4000,
        });
      }
      if (status.state === "completed" && previous !== "completed") {
        void reloadModelState().catch((error) => {
          if (disposed || currentScreenRef.current === "settings") return;
          pushToast({
            kind: "error",
            message: "Model installed; list refresh needs attention",
            hint: errorMessage(
              error,
              "Open Settings → Engines to refresh the list.",
            ),
            durationMs: 0,
          });
        });
      }
    };
    void listenModelDownloadStatus((status) => {
      liveStatuses.add(status.modelId);
      acceptStatus(status);
    })
      .then(async (cleanup) => {
        if (disposed) cleanup();
        else {
          unlisten = cleanup;
          const statuses = await getModelDownloadStatuses();
          statuses.filter((status) => !liveStatuses.has(status.modelId))
            .forEach(acceptStatus);
        }
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [pushToast]);

  async function loadAppState() {
    const [
      nextHealth,
      nextSettings,
      nextInstalledModels,
      nextTranscripts,
      nextVocabularyTerms,
      nextRecordingStatus,
      nextQuickDictateStatus,
      nextFileStatuses,
    ] = await Promise.all([
      getHealthCheck(),
      getSettings(),
      listInstalledModels(),
      listTranscripts(""),
      listVocabularyTerms(),
      getRecordingStatus(),
      getQuickDictateStatus(),
      getFileTranscriptionStatuses(),
    ]);

    setHealth(nextHealth);
    setSettings(nextSettings);
    setInstalledModels(nextInstalledModels);
    // An activation link that opened the app wins over the usual start screen.
    if (!licenseLinkOpened.current)
      setScreen(
        nextSettings.defaultMode === "file_transcribe" ? "files" : "dictate",
      );
    applyAppearance(nextSettings);
    lastDictationStateRef.current = nextQuickDictateStatus.state;
    setTranscripts(nextTranscripts);
    setVocabularyTerms(nextVocabularyTerms);
    setRecordingStatus(nextRecordingStatus);
    setQuickDictationStatus(nextQuickDictateStatus);
    nextFileStatuses.forEach((event) =>
      fileEventStages.current.set(event.jobId, event.stage),
    );
    setFileQueueItems(mergeFileStatusesIntoQueue([], nextFileStatuses));
    for (const notice of nextHealth.startupNotices) {
      pushToast({
        kind: "info",
        message: "Murmela updated a setting",
        hint: notice,
        durationMs: 8000,
      });
    }
    void refreshReadiness();
  }

  async function refreshShortcutOutputs() {
    try {
      const [nextQuickDictateStatus, nextTranscripts] = await Promise.all([
        getQuickDictateStatus(),
        listTranscripts(""),
      ]);
      setQuickDictationStatus(nextQuickDictateStatus);
      setTranscripts(nextTranscripts);
    } catch (error) {
      console.error(
        errorMessage(error, "Failed to refresh shortcut output state."),
      );
    }
  }

  async function refreshFileStatuses() {
    if (statusRefreshBusy.current) return;
    statusRefreshBusy.current = true;
    try {
      const statuses = await getFileTranscriptionStatuses();
      setFileQueueItems((current) =>
        mergeFileStatusesIntoQueue(current, statuses),
      );
      for (const status of statuses) {
        if (status.result?.savedTranscript) {
          setTranscripts((current) =>
            prependTranscriptUnique(current, status.result!.savedTranscript!),
          );
        }
      }
    } catch (error) {
      console.error(
        errorMessage(error, "Failed to refresh file transcription statuses."),
      );
    } finally {
      statusRefreshBusy.current = false;
    }
  }

  async function saveSettings(patch: SettingsPatch) {
    const nextSettings = await updateSettings(patch);
    setSettings(nextSettings);
    applyAppearance(nextSettings);
    void getQuickDictateStatus()
      .then(setQuickDictationStatus)
      .catch(() => undefined);
    void refreshReadiness();
  }

  const reloadModelState = useCallback((): Promise<void> => {
    // The workspace and Models view observe the same completion event. Share
    // simultaneous requests, but read again if another completion arrives while
    // the read is pending so its models cannot be lost to an older snapshot.
    modelRefreshRevision.current += 1;
    if (modelRefreshInFlight.current) return modelRefreshInFlight.current;
    const refresh = (async () => {
      await Promise.resolve();
      try {
        while (true) {
          const revision = modelRefreshRevision.current;
          const results = await Promise.allSettled([
            getSettings().then(setSettings),
            listInstalledModels().then(setInstalledModels),
          ]);
          if (revision !== modelRefreshRevision.current) continue;
          void refreshReadiness();
          const failed = results.find((result) => result.status === "rejected");
          if (failed?.status === "rejected") throw failed.reason;
          return;
        }
      } finally {
        modelRefreshInFlight.current = null;
      }
    })();
    modelRefreshInFlight.current = refresh;
    return refresh;
  }, [refreshReadiness]);

  // Murmelmeter totals, and a toast when a dictation crosses a milestone.
  useEffect(() => {
    if (!health) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenDictationStats((next) => {
      if (disposed) return;
      const previous = statsRef.current;
      const milestone = previous
        ? crossedMilestone(previous.totalWords, next.totalWords)
        : null;
      if (milestone && playfulRef.current) {
        pushToast({ kind: "success", message: milestone, hint: "Murmelmeter", durationMs: 7000 });
      }
      statsRef.current = next;
      setDictationStats(next);
    })
      .then((cleanup) => {
        if (disposed) cleanup();
        else unlisten = cleanup;
      })
      .catch(() => undefined);
    void getDictationStats()
      .then((stats) => {
        if (disposed || statsRef.current) return;
        statsRef.current = stats;
        setDictationStats(stats);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [health, pushToast]);

  // A fresh install whose shortcut cannot work yet starts with the guided setup.
  useEffect(() => {
    if (!readiness || firstRunChecked.current) return;
    firstRunChecked.current = true;
    if (readiness.firstRunCompleted === false && readiness.shortcutModelReady === false) {
      setFirstRunOpen(true);
    }
  }, [readiness]);

  // Poll briefly after asking for the microphone, until the user has answered
  // the prompt or changed the setting in System Settings.
  const pollMicrophone = useCallback(
    (done: (state: DictationReadiness["microphone"]) => boolean) => {
      if (microphonePoll.current !== null) window.clearTimeout(microphonePoll.current);
      const expiresAt = Date.now() + 120_000;
      const check = async () => {
        const next = await refreshReadiness();
        if ((next && done(next.microphone)) || Date.now() >= expiresAt) {
          microphonePoll.current = null;
          return;
        }
        microphonePoll.current = window.setTimeout(check, 1000);
      };
      microphonePoll.current = window.setTimeout(check, 500);
    },
    [refreshReadiness],
  );
  useEffect(
    () => () => {
      if (microphonePoll.current !== null) window.clearTimeout(microphonePoll.current);
    },
    [],
  );

  const handleRequestMicrophone = useCallback(async () => {
    const state = await requestMicrophoneAccess();
    if (state === "not_determined") pollMicrophone((next) => next !== "not_determined");
    else void refreshReadiness();
  }, [pollMicrophone, refreshReadiness]);

  const handleOpenMicrophoneSettings = useCallback(async () => {
    await openMicrophoneSettings();
    pollMicrophone((next) => next === "granted");
  }, [pollMicrophone]);

  // Explain the permission before macOS shows its "control this computer" prompt.
  const handleResolveAccessibility = useCallback(async () => {
    if (isPollingAccessibility) {
      await refreshReadiness();
      return;
    }
    setAccessibilityExplainerOpen(true);
  }, [isPollingAccessibility, refreshReadiness]);

  const handleContinueAccessibility = useCallback(async () => {
    await requestAccessibilityAccess();
    await openAccessibilitySettings();
    startAccessibilityPolling();
    setAccessibilityExplainerOpen(false);
  }, [startAccessibilityPolling]);

  /** Makes a first-run engine the shortcut model, and the in-app and file
   * model too when those have nothing usable yet. */
  const handleUseFirstRunModel = useCallback(async (modelId: string) => {
    const [models, current] = await Promise.all([listInstalledModels(), getSettings()]);
    setInstalledModels(models);
    const model = models.find((entry) => entry.id === modelId);
    if (!model) throw new Error("The engine is not installed yet.");
    const runs = (candidate: InstalledModel | undefined, context: ModelUseContext) =>
      Boolean(
        candidate &&
          (!candidate.capabilities || candidate.capabilities.supportedContexts.includes(context)),
      );
    const usable = (id: string | null, context: ModelUseContext) =>
      runs(models.find((entry) => entry.id === id), context);
    const patch: SettingsPatch = {
      shortcutDictationSelectedModelId: model.id,
      shortcutDictationModelProfile: model.profile,
    };
    if (runs(model, "quick_dictate") && !usable(current.quickDictateSelectedModelId, "quick_dictate")) {
      patch.quickDictateSelectedModelId = model.id;
      patch.quickDictateModelProfile = model.profile;
    }
    if (
      runs(model, "file_transcription") &&
      !usable(current.fileTranscribeSelectedModelId, "file_transcription")
    ) {
      patch.fileTranscribeSelectedModelId = model.id;
      patch.fileTranscribeModelProfile = model.profile;
    }
    await saveSettings(patch);
  }, []);

  const closeFirstRun = useCallback(async () => {
    await completeFirstRun();
    setFirstRunOpen(false);
    void refreshReadiness();
  }, [refreshReadiness]);

  const handleResolveReadiness = useCallback(
    async (
      item: "model" | "engine" | "translation" | "microphone" | "shortcut" | "accessibility",
    ) => {
      if (item === "engine") {
        setFirstRunOpen(true);
        return;
      }
      if (item === "microphone") {
        if (readiness?.microphone === "not_determined") await handleRequestMicrophone();
        else await handleOpenMicrophoneSettings();
        return;
      }
      if (item === "accessibility") {
        await handleResolveAccessibility();
        return;
      }
      setSettingsSection(item === "model" ? "models" : item === "translation" ? "languages" : "audio");
      setScreen("settings");
    },
    [handleOpenMicrophoneSettings, handleRequestMicrophone, handleResolveAccessibility, readiness?.microphone],
  );

  async function removeTranscript(transcriptId: string) {
    await deleteTranscript(transcriptId);
    setTranscripts((current) =>
      current.filter((item) => item.id !== transcriptId),
    );
  }

  async function removeAllTranscripts() {
    await deleteAllTranscripts();
    setTranscripts([]);
    setTranslationsVersion((value) => value + 1);
  }

  async function beginManualRecording() {
    manualGeneration.current += 1;
    try {
      const nextRecording = await startRecordingSession();
      setRecordingStatus(nextRecording);
      setPreview(null);
      setManualTranscriptionState({
        stage: "idle",
        statusText: "",
        startedAt: null,
        errorMessage: null,
      });
    } catch (error) {
      void reportManualFeedback(crypto.randomUUID(), true).catch(
        () => undefined,
      );
      throw error;
    }
  }

  async function stopAndPreviewManualRecording() {
    if (!settings) {
      return;
    }
    const generation = manualGeneration.current;
    const startedAt = Date.now();
    setPreview(null);
    setManualTranscriptionState({
      stage: "processing",
      statusText: "Finishing your recording…",
      startedAt,
      errorMessage: null,
    });
    setRecordingStatus({
      state: "processing",
      currentSessionId: recordingStatus?.currentSessionId ?? null,
      activeInputDevice: recordingStatus?.activeInputDevice ?? null,
      lastRecordingPath: recordingStatus?.lastRecordingPath ?? null,
      lastErrorMessage: null,
      durationMs: recordingStatus?.durationMs ?? null,
      sampleRateHz: recordingStatus?.sampleRateHz ?? null,
      channels: recordingStatus?.channels ?? null,
    });

    try {
      const result = await stopRecordingSession();
      if (generation !== manualGeneration.current) return;
      setManualTranscriptionState({
        stage: "processing",
        statusText: "Transcribing your recording locally…",
        startedAt,
        errorMessage: null,
      });
      setRecordingStatus({
        state: "processing",
        currentSessionId: result.sessionId,
        activeInputDevice: recordingStatus?.activeInputDevice ?? null,
        lastRecordingPath: result.filePath,
        lastErrorMessage: null,
        durationMs: result.durationMs,
        sampleRateHz: result.sampleRateHz,
        channels: result.channels,
      });

      const nextPreview = await previewTranscription({
        sourceKind: "quick_dictate",
        sessionId: result.sessionId,
        profile: settings.quickDictateModelProfile,
        selectedModelId: settings.quickDictateSelectedModelId,
        languageMode: settings.languageMode,
        fixedLanguage: settings.fixedLanguage,
        timestamps: true,
        preferGpu: settings.gpuEnabled,
        filePath: result.filePath,
      });
      if (generation !== manualGeneration.current) return;
      setPreview(nextPreview);
      if (nextPreview.dictationOutput?.transcriptId) setTranscripts(await listTranscripts(""));
      void reportManualFeedback(
        result.sessionId,
        Boolean(nextPreview.error),
      ).catch(() => undefined);
      if (nextPreview.error) {
        setManualTranscriptionState({
          stage: "failed",
          statusText: "Manual dictation failed.",
          startedAt,
          errorMessage: `${nextPreview.error.code}: ${nextPreview.error.message}`,
        });
      } else {
        setManualTranscriptionState({
          stage: "idle",
          statusText: "",
          startedAt: null,
          errorMessage: null,
        });
      }
    } catch (error) {
      if (generation !== manualGeneration.current) return;
      void reportManualFeedback(
        recordingStatus?.currentSessionId ?? crypto.randomUUID(),
        true,
      ).catch(() => undefined);
      setPreview(null);
      setManualTranscriptionState({
        stage: "failed",
        statusText: "Manual dictation failed.",
        startedAt,
        errorMessage: errorMessage(error, "Manual dictation failed."),
      });
    } finally {
      const status = await getRecordingStatus().catch(() => null);
      if (status && generation === manualGeneration.current)
        setRecordingStatus(status);
    }
  }

  async function cancelManualRecording() {
    const status = await cancelRecordingSession();
    manualGeneration.current += 1;
    setPreview(null);
    setManualTranscriptionState({
      stage: "idle",
      statusText: "Recording canceled",
      startedAt: null,
      errorMessage: null,
    });
    setRecordingStatus(status);
  }

  async function resetDictation() {
    await resetQuickDictation().then((status) => {
      manualGeneration.current += 1;
      setDictationError(null);
      setManualTranscriptionState({
        stage: "idle",
        statusText: "Dictation reset",
        startedAt: null,
        errorMessage: null,
      });
      setQuickDictationStatus(status);
    });
    setRecordingStatus(await getRecordingStatus());
  }

  async function createVocabulary(
    input: Parameters<typeof createVocabularyTerm>[0],
  ) {
    const term = await createVocabularyTerm(input);
    setVocabularyTerms((current) =>
      [...current, term].sort((left, right) =>
        left.canonical.localeCompare(right.canonical),
      ),
    );
  }

  async function updateVocabulary(
    termId: string,
    input: Parameters<typeof updateVocabularyTerm>[1],
  ) {
    const updated = await updateVocabularyTerm(termId, input);
    setVocabularyTerms((current) =>
      current
        .map((term) => (term.id === termId ? updated : term))
        .sort((left, right) => left.canonical.localeCompare(right.canonical)),
    );
  }

  async function removeVocabulary(termId: string) {
    await deleteVocabularyTerm(termId);
    setVocabularyTerms((current) =>
      current.filter((term) => term.id !== termId),
    );
  }

  async function enqueueFiles(speakerCountHint: number | null) {
    const files = await pickAudioFiles();
    enqueueSelectedFiles(files, speakerCountHint);
  }

  async function retryFile(itemId: string) {
    const item = fileQueueItems.find((entry) => entry.id === itemId);
    if (!item) return;
    let files: FileQueueItem["sourceFile"][];
    try {
      files = await prepareDroppedAudioFiles([item.sourceFile.filePath]);
    } catch {
      files = await pickAudioFiles();
      if (!files.length)
        throw new Error("Choose the original audio file to retry.");
    }
    enqueueSelectedFiles(
      files.slice(0, 1),
      fileHints.current.get(itemId) ?? null,
    );
  }

  async function cancelFile(itemId: string) {
    await cancelFileTranscription(itemId);
    await refreshFileStatuses();
  }

  function enqueueSelectedFiles(
    files: FileQueueItem["sourceFile"][],
    speakerCountHint: number | null,
  ) {
    if (files.length === 0) {
      return;
    }
    if (!fileModelReady.current) {
      pushToast({
        kind: "error",
        message: "A speech model is needed",
        hint: "Open Settings → Engines to download an engine for files.",
        durationMs: 0,
      });
      return;
    }
    const queuedItems = files.map((sourceFile) =>
      createQueuedFileItem(crypto.randomUUID(), sourceFile),
    );
    setFileQueueItems((current) =>
      mergeFileStatusesIntoQueue([...queuedItems, ...current], []),
    );

    for (const item of queuedItems) {
      fileHints.current.set(item.id, speakerCountHint);
      fileEventStages.current.set(item.id, "queued");
      void startFileTranscription({
        jobId: item.id,
        sourceFile: item.sourceFile,
        speakerCountHint,
      }).catch((error) => {
        const message = errorMessage(
          error,
          "Failed to transcribe the selected file.",
        );
        setFileQueueItems((current) =>
          current.map((entry) =>
            entry.id === item.id
              ? {
                  ...entry,
                  stage: "failed",
                  statusText: "File transcription failed.",
                  errorMessage: message,
                }
              : entry,
          ),
        );
      });
    }
  }

  async function handleDroppedFiles(
    fileList: FileList,
    speakerCountHint: number | null,
  ) {
    // Extract file paths from the HTML5 FileList.
    // In Tauri's webview, File objects from drag-and-drop carry the native path.
    // In a plain browser, we fall back to the file name.
    const paths: string[] = [];
    for (let i = 0; i < fileList.length; i++) {
      const file = fileList[i];
      // Tauri webview exposes the native file path on the File object
      const filePath = (file as File & { path?: string }).path || file.name;
      paths.push(filePath);
    }
    try {
      const files = await prepareDroppedAudioFiles(paths);
      enqueueSelectedFiles(files, speakerCountHint);
    } catch (error) {
      showDroppedAudioError(error);
    }
  }

  const openReview = (reference: ReviewRef, originLabel: string) => {
    setReviewTarget({
      reference,
      originLabel,
      scroll: mainRef.current?.scrollTop ?? 0,
      anchor: document.activeElement as HTMLElement,
    });
    requestAnimationFrame(() => {
      if (mainRef.current) mainRef.current.scrollTop = 0;
    });
  };
  const closeReview = () => {
    const previous = reviewTarget;
    setReviewTarget(null);
    requestAnimationFrame(() => {
      if (mainRef.current) mainRef.current.scrollTop = previous?.scroll ?? 0;
      previous?.anchor?.focus({ preventScroll: true });
    });
  };
  const reviewUpdated = useCallback((summary: TranscriptSummary) => {
    setTranscripts((current) => {
      const previous = current.find((s) => s.id === summary.id);
      if (previous && JSON.stringify(previous) === JSON.stringify(summary))
        return current;
      return prependTranscriptUnique(current, summary);
    });
    invalidateFileReview({ kind: "saved", id: summary.id });
  }, []);
  const resolvedFileModel =
    installedModels.find(
      (model) => model.id === settings?.fileTranscribeSelectedModelId,
    ) ??
    installedModels.find(
      (model) =>
        model.profile === settings?.fileTranscribeModelProfile &&
        model.isDefault,
    ) ??
    installedModels.find(
      (model) => model.profile === settings?.fileTranscribeModelProfile,
    );
  const activeReviewFile = reviewTarget
    ? fileQueueItems.find((item) => {
        const reference =
          item.reviewRef ??
          (item.result?.savedTranscript
            ? { kind: "saved" as const, id: item.result.savedTranscript.id }
            : { kind: "session" as const, id: item.id });
        return reviewKey(reference) === reviewKey(reviewTarget.reference);
      })
    : undefined;

  return (
    <div className="app-scene">
      <ToastStack toasts={toasts} onDismiss={dismissToast} />
      {firstRunOpen ? (
        <FirstRunSetup
          settings={settings}
          readiness={readiness}
          quickStatus={quickDictationStatus}
          isPollingAccessibility={isPollingAccessibility}
          onUseModel={handleUseFirstRunModel}
          onRequestMicrophone={handleRequestMicrophone}
          onOpenMicrophoneSettings={handleOpenMicrophoneSettings}
          onResolveAccessibility={handleResolveAccessibility}
          onOpenShortcutSettings={async () => {
            await closeFirstRun();
            setSettingsSection("audio");
            setScreen("settings");
          }}
          onSaveSettings={saveSettings}
          onFinish={closeFirstRun}
        />
      ) : null}
      {accessibilityExplainerOpen ? (
        <AccessibilityExplainer
          onContinue={handleContinueAccessibility}
          onCancel={() => setAccessibilityExplainerOpen(false)}
        />
      ) : null}
      <div
        className={
          "app-shell" +
          (sidebarExpanded ? "" : " sidebar-collapsed")
        }
      >
        <aside className="sidebar">
          <div className="sidebar-titlebar" data-tauri-drag-region />
          <div className="sidebar-brand" data-tauri-drag-region>
            <img className="brand-mark" src={murmelaMark} alt="" width={26} height={18} />
            <span className="brand-wordmark">Murmela</span>
            <IconButton
              className="sidebar-toggle"
              size="compact"
              icon={sidebarExpanded ? "chevronLeft" : "chevronRight"}
              label={sidebarExpanded ? "Collapse sidebar" : "Expand sidebar"}
              tooltipPlacement="bottom"
              onClick={() => setSidebarExpanded((current) => !current)}
            />
          </div>

          <nav className="nav-list" aria-label="Main navigation">
            {NAV_ITEMS.map((item) => (
              <button
                key={item.id}
                className={
                  "nav-item" +
                  (screen === item.id ? " active" : "") +
                  (item.id === "settings" ? " nav-settings" : "")
                }
                aria-current={screen === item.id ? "page" : undefined}
                onClick={() => {
                  if (item.id === "settings") setSettingsSection("general");
                  setReviewTarget(null);
                  setScreen(item.id);
                }}
                aria-label={item.label}
                title={item.label}
              >
                <span className="nav-icon">
                  <AppIcon name={item.icon} />
                </span>
                <span className="nav-item-label">{item.label}</span>
                {item.id === "settings" && downloadCount > 0 ? (
                  <span
                    className="nav-count"
                    aria-label="Model downloads in progress"
                  >
                    {downloadCount}
                  </span>
                ) : null}
                {item.id === "files" &&
                fileQueueItems.some((entry) => isFileWorking(entry.stage)) ? (
                  <span className="nav-count" aria-label="Files in progress">
                    {
                      fileQueueItems.filter((entry) =>
                        isFileWorking(entry.stage),
                      ).length
                    }
                  </span>
                ) : null}
                {item.id === "dictate" &&
                (recordingStatus?.state === "listening" ||
                  manualTranscriptionState.stage === "processing" ||
                  quickDictationStatus?.state === "listening" ||
                  quickDictationStatus?.state === "processing") ? (
                  <span
                    className="nav-activity"
                    aria-label="Dictation in progress"
                  />
                ) : null}
              </button>
            ))}
          </nav>
        </aside>

        <main className="main-content" ref={mainRef}>
          <div className="content-frame">
            <UpdateBanner status={updateStatus} />
            <ProBanner hidden={screen === "settings" && settingsSection === "license"} />
            {!reviewTarget
              ? reviewJobs.filter(isReviewJobActive).map((job) => (
                  <div
                    className="review-job surface"
                    key={job.jobId}
                    role="status"
                  >
                    <div>
                      <strong>
                        Identifying speakers ·{" "}
                        {transcripts.find((t) => t.id === job.reference.id)
                          ?.title ?? "Session transcript"}
                      </strong>
                      <p className="muted">{job.statusText}</p>
                    </div>
                    <button
                      className="secondary-inline-button"
                      onClick={() =>
                        openReview(
                          job.reference,
                          {
                            dictate: "Dictate",
                            files: "Files",
                            meetings: "Meetings",
                            translate: "Translate",
                            history: "Library",
                            vocabulary: "Vocabulary",
                            settings: "Settings",
                          }[screen],
                        )
                      }
                    >
                      Review and manage
                    </button>
                  </div>
                ))
              : null}
            {reviewTarget ? (
              <ReviewWorkspace
                key={reviewKey(reviewTarget.reference)}
                reference={reviewTarget.reference}
                originLabel={reviewTarget.originLabel}
                onBack={closeReview}
                onUpdated={reviewUpdated}
                onDelete={removeTranscript}
                jobs={reviewJobs}
                onJobStarted={acceptReviewJob}
                initialJob={activeReviewFile}
                onStopInitial={
                  activeReviewFile
                    ? () => cancelFile(activeReviewFile.id)
                    : undefined
                }
                onResolveModel={() => {
                  setReviewTarget(null);
                  setSettingsSection("models");
                  setScreen("settings");
                }}
              />
            ) : null}
            <div hidden={Boolean(reviewTarget)}>
              {screen === "dictate" ? (
                <DictateScreen
                  outputState={outputState}
                  onRetryLive={async () => {
                    const status = await retryStreamingDictation();
                    setQuickDictationStatus(status); setDictationError(null); setPreview(null);
                    if (status.lastTranscriptId) setTranscripts(await listTranscripts(""));
                  }}
                  onOutputModeChange={async (mode) => { setOutputState(await setDictationOutputMode(mode)); }}
                  onRetryTranslation={async () => {
                    const output = await retryDictationTranslation();
                    setOutputState((current) => current ? { ...current, busy: false, lastOutput: output } : current);
                    setPreview((current) => current ? { ...current, dictationOutput: output, error: null } : current);
                    if (output.transcriptId) setTranscripts(await listTranscripts(""));
                    if (output.status !== "completed") throw new Error(output.errorMessage ?? "Translation failed.");
                    setDictationError(null);
                    setQuickDictationStatus((current) => current?.state === "error" ? { ...current, state: "idle", lastErrorMessage: null } : current);
                    setManualTranscriptionState((current) => ({ ...current, stage: "idle", statusText: "Translation ready", errorMessage: null }));
                  }}
                  settings={settings}
                  preview={preview}
                  recordingStatus={recordingStatus}
                  manualTranscriptionState={manualTranscriptionState}
                  quickDictationStatus={quickDictationStatus}
                  dictationError={dictationError}
                  readiness={readiness}
                  isPollingAccessibility={isPollingAccessibility}
                  onResolveReadiness={handleResolveReadiness}
                  onStartRecording={beginManualRecording}
                  recentDictations={settings?.saveHistory ? transcripts : []}
                  stats={dictationStats}
                  onOpenTranscript={(id) => openReview({ kind: "saved", id }, "Dictate")}
                  onStopAndTranscribeRecording={stopAndPreviewManualRecording}
                  onCancelRecording={cancelManualRecording}
                  onResetDictation={resetDictation}
                />
              ) : null}
              {screen === "files" ? (
                <FilesScreen
                  modelReady={fileModelReady.current}
                  onResolveModel={() => {
                    setSettingsSection("models");
                    setScreen("settings");
                  }}
                  items={fileQueueItems}
                  dragging={isFileDragActive}
                  speakerCountHint={speakerCountHint}
                  showSpeakerOptions={Boolean(
                    pro &&
                      settings?.fileDiarizationEnabled &&
                      !resolvedFileModel?.capabilities?.nativeDiarization,
                  )}
                  onSpeakerCountHintChange={setSpeakerCountHint}
                  onDragChange={setIsFileDragActive}
                  onPick={() => enqueueFiles(speakerCountHint)}
                  onDrop={(files) =>
                    handleDroppedFiles(files, speakerCountHint)
                  }
                  speakerMode={
                    resolvedFileModel?.capabilities?.nativeDiarization
                      ? "Speakers: built into model"
                      : pro && settings?.fileDiarizationEnabled
                        ? speakerModelReady === false
                          ? "Speakers: on · model not ready"
                          : "Speakers: on"
                        : "Speakers: off"
                  }
                  onOpenSpeakerSettings={() => {
                    setSettingsSection("models");
                    setScreen("settings");
                  }}
                  onReview={(item) =>
                    openReview(
                      item.reviewRef ??
                        (item.result?.savedTranscript
                          ? {
                              kind: "saved",
                              id: item.result.savedTranscript.id,
                            }
                          : { kind: "session", id: item.id }),
                      "Files",
                    )
                  }
                  onDismiss={async (id) => {
                    await dismissFileTranscription(id);
                    setFileQueueItems((current) =>
                      current.filter((item) => item.id !== id),
                    );
                    fileHints.current.delete(id);
                  }}
                  onCancel={cancelFile}
                  onRetry={retryFile}
                />
              ) : null}

              {screen === "meetings" ? (
                <MeetingsScreen
                  items={fileQueueItems}
                  pro={pro}
                  onRetry={retryFile}
                  onReview={(item) =>
                    openReview(
                      item.reviewRef ??
                        (item.result?.savedTranscript
                          ? { kind: "saved", id: item.result.savedTranscript.id }
                          : { kind: "session", id: item.id }),
                      "Meetings",
                    )
                  }
                />
              ) : null}

              {translateVisited || screen === "translate" ? (
                <div hidden={screen !== "translate"}>
                  <TranslateScreen
                    settings={settings}
                    transcripts={settings?.saveHistory ? transcripts : []}
                    openRequest={translateOpenRequest}
                    dragging={isTranslateDragActive}
                    onOpenSettings={() => {
                      setSettingsSection("languages");
                      setScreen("settings");
                    }}
                    onLibraryChanged={() => setTranslationsVersion((value) => value + 1)}
                  />
                </div>
              ) : null}

              {screen === "settings" ? (
                <SettingsScreen
                  initialSection={settingsSection}
                  onSectionChange={setSettingsSection}
                  settings={settings}
                  installedModels={installedModels}
                  onSave={saveSettings}
                  onReloadModelState={reloadModelState}
                  onRunWelcomeSetup={() => setFirstRunOpen(true)}
                />
              ) : null}

              {screen === "vocabulary" ? (
                <VocabularyScreen
                  vocabularyTerms={vocabularyTerms}
                  onCreateVocabularyTerm={createVocabulary}
                  onUpdateVocabularyTerm={updateVocabulary}
                  onDeleteVocabularyTerm={removeVocabulary}
                />
              ) : null}

              {libraryVisited || screen === "history" ? (
                <div hidden={screen !== "history"}>
                  <HistoryScreen
                    transcripts={transcripts}
                    onReview={(id) =>
                      openReview({ kind: "saved", id }, "Library")
                    }
                    onDeleteAll={removeAllTranscripts}
                    translationsVersion={translationsVersion}
                    onOpenTranslation={(id) => {
                      setTranslateOpenRequest({ kind: "saved", id, nonce: Date.now() });
                      setScreen("translate");
                    }}
                  />
                </div>
              ) : null}
            </div>
          </div>
        </main>
      </div>
    </div>
  );
}

function ToastStack({
  toasts,
  onDismiss,
}: {
  toasts: Toast[];
  onDismiss: (id: number) => void;
}) {
  if (toasts.length === 0) return null;
  return (
    <div role="region" aria-label="Notifications" className="toast-stack">
      {toasts.map((toast) => (
        <ToastChip key={toast.id} toast={toast} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

function ToastChip({
  toast,
  onDismiss,
}: {
  toast: Toast;
  onDismiss: (id: number) => void;
}) {
  return (
    <div
      role={toast.kind === "error" ? "alert" : "status"}
      className={"toast-chip toast-" + toast.kind}
    >
      <AppIcon
        name={
          toast.kind === "error"
            ? "info"
            : toast.kind === "success"
              ? "check"
              : "info"
        }
      />
      <div className="toast-copy">
        <strong>{toast.message}</strong>
        {toast.hint ? <p>{toast.hint}</p> : null}
      </div>
      <IconButton
        icon="xmark"
        size="compact"
        label="Dismiss notification"
        onClick={() => onDismiss(toast.id)}
      />
    </div>
  );
}

function errorMessage(error: unknown, fallback: string) {
  return describeError(error, fallback);
}

function createQueuedFileItem(
  id: string,
  sourceFile: FileQueueItem["sourceFile"],
): FileQueueItem {
  return {
    id,
    sourceFile,
    stage: "queued",
    progressPercent: null,
    processedMs: null,
    totalMs: null,
    etaSeconds: null,
    statusText: "Queued for local transcription.",
    result: null,
    errorMessage: null,
    startedAt: null,
  };
}

function mergeFileStatusesIntoQueue(
  current: FileQueueItem[],
  statuses: FileTranscriptionStatusEvent[],
): FileQueueItem[] {
  const merged = new Map<string, FileQueueItem>();
  const currentById = new Map(current.map((item) => [item.id, item]));

  for (const status of statuses) {
    const existing = currentById.get(status.jobId);
    merged.set(status.jobId, mergeStatusWithQueueItem(existing, status));
  }

  for (const item of current) {
    if (!merged.has(item.id)) {
      merged.set(item.id, item);
    }
  }

  const next = Array.from(merged.values()).sort((left, right) => {
    const rightStarted = right.startedAt ?? 0;
    const leftStarted = left.startedAt ?? 0;
    return rightStarted - leftStarted;
  });
  return next.length === current.length &&
    next.every((item, index) => item === current[index])
    ? current
    : next;
}

function mergeFileStatusIntoQueue(
  current: FileQueueItem[],
  status: FileTranscriptionStatusEvent,
): FileQueueItem[] {
  return mergeFileStatusesIntoQueue(current, [status]);
}

function mergeStatusWithQueueItem(
  existing: FileQueueItem | undefined,
  status: FileTranscriptionStatusEvent,
): FileQueueItem {
  if (
    existing?.updatedAt &&
    (status.updatedAtMs < existing.updatedAt ||
      (status.updatedAtMs === existing.updatedAt &&
        existing.result === status.result))
  )
    return existing;
  return {
    reviewRef: status.reviewRef,
    resultRevision: status.resultRevision,
    updatedAt: status.updatedAtMs,
    id: status.jobId,
    sourceFile: status.result?.sourceFile ?? status.sourceFile,
    stage: status.stage,
    progressPercent: status.progressPercent,
    processedMs: status.processedMs,
    totalMs: status.totalMs,
    etaSeconds: status.etaSeconds,
    statusText: status.statusText,
    result: status.result ?? existing?.result ?? null,
    errorMessage: status.errorMessage,
    startedAt: status.startedAtMs,
  };
}

function prependTranscriptUnique(
  current: TranscriptSummary[],
  next: TranscriptSummary,
): TranscriptSummary[] {
  return [next, ...current.filter((item) => item.id !== next.id)];
}
