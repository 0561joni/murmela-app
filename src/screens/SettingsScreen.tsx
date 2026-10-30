import { useEffect, useRef, useState } from "react";
import {
  FN_KEY_SHORTCUT,
  RIGHT_OPTION_SHORTCUT,
  describeError,
  formatShortcutForDisplay,
  isOneKeyShortcut,
  readableError,
  formatBytes,
} from "../lib/formatting";
import { ActionButton, Button, PageHeader } from "../components/Feedback";
import { AboutSection } from "../components/AboutSection";
import { LicenseCard } from "../components/LicenseCard";
import { NetworkActivityCard } from "../components/NetworkActivityCard";
import { ProHint } from "../components/ProHint";
import { useLicense } from "../hooks/useLicense";
import { GemmaTermsDialog } from "../components/GemmaTermsDialog";
import { GlobeKeyNotice } from "../components/GlobeKeyNotice";
import { IconButton } from "../components/IconButton";
import { QuickTranslateCard } from "../components/QuickTranslateCard";
import { OUTPUT_MODES } from "../lib/translationApi";
import {
  ModelInfoButton,
  ModelPicker,
  ModelSummary,
} from "../components/ModelPicker";
import {
  formatWorkflows,
  getFriendlyModelName,
  getModelPresentation,
} from "../lib/modelPresentation";
import {
  previewFeedbackSound,
  resetDictationStats,
  cancelModelDownload,
  cancelRecordingSession,
  deleteModel,
  getRecordingInputLevel,
  getModelDownloadStatuses,
  acceptModelTerms,
  getLivePairStatus,
  getModelTermsStatus,
  listDownloadableModels,
  listInputDevices,
  listenLivePairStatus,
  listenModelDownloadStatus,
  openModelsFolder,
  rescanModelsFolder,
  resumeShortcutCapture,
  startModelDownload,
  startRecordingSession,
  suspendShortcutCapture,
} from "../lib/api";
import { setMelaEnabled, useMelaEnabled } from "../lib/mela";
import { HelpHint } from "../components/HelpHint";
import type {
  AppSettings,
  DownloadableModel,
  InputDeviceOption,
  InstalledModel,
  LivePairStatus,
  ModelDownloadStatus,
  OutputMode,
  SettingsPatch,
} from "../types/domain";

const LIVE_PAIR_ID = "live-pair";

const LIVE_PAIR_STATE_LABELS: Record<LivePairStatus["state"], string> = {
  off: "Not loaded",
  preparing: "Preparing",
  ready: "Ready",
  unloaded: "Unloaded to free memory",
  error: "Needs attention",
};

interface SettingsScreenProps {
  initialSection?: string;
  onSectionChange?: (section: string) => void;
  settings: AppSettings | null;
  installedModels: InstalledModel[];
  onSave: (patch: SettingsPatch) => Promise<void>;
  onReloadModelState: () => Promise<void>;
  /** Opens the welcome setup again (engine, languages, practice). */
  onRunWelcomeSetup?: () => void;
}

const DEFAULT_SHORTCUT = "CmdOrCtrl+Shift+Space";
/** Control-Option-L; matches translation::DEFAULT_SHORTCUT in the backend. */
const DEFAULT_LANGUAGE_SHORTCUT = "Ctrl+Alt+L";
/** Control-Option-V; matches settings::DEFAULT_PASTE_LAST_SHORTCUT in the backend. */
const DEFAULT_PASTE_LAST_SHORTCUT = "Ctrl+Alt+V";

type CaptureField = "shortcut" | "translationCycleShortcut" | "pasteLastShortcut" | "quickTranslateShortcut";

/** Languages every dictation engine accepts as a fixed choice (Live dictation's list). */
const SPOKEN_LANGUAGES: Array<[string, string]> = [
  ["de", "German"],
  ["en", "English"],
  ["fr", "French"],
  ["es", "Spanish"],
  ["it", "Italian"],
  ["pt", "Portuguese"],
];

export function SettingsScreen({
  initialSection = "general",
  onSectionChange,
  settings,
  installedModels,
  onSave,
  onReloadModelState,
  onRunWelcomeSetup,
}: SettingsScreenProps) {
  const melaEnabled = useMelaEnabled();
  const [group, setGroup] = useState(initialSection);
  // The app can switch sections from outside, e.g. a Pro hint opening License.
  useEffect(() => setGroup(initialSection), [initialSection]);
  const { pro } = useLicense();
  const [savedField, setSavedField] = useState<string | null>(null);
  const [savingField, setSavingField] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [termsModelId, setTermsModelId] = useState<string | null>(null);
  const [isOpeningModelsFolder, setIsOpeningModelsFolder] = useState(false);
  const [isDownloadsExpanded, setIsDownloadsExpanded] = useState(false);
  const [isRecordEngineExpanded, setIsRecordEngineExpanded] = useState(false);
  const [confirmingDeleteId, setConfirmingDeleteId] = useState<string | null>(null);
  const [deletingModelId, setDeletingModelId] = useState<string | null>(null);
  const [isRescanningModels, setIsRescanningModels] = useState(false);
  const [downloadableModels, setDownloadableModels] = useState<
    DownloadableModel[]
  >([]);
  const [inputDevices, setInputDevices] = useState<InputDeviceOption[]>([]);
  const [modelDownloadStatuses, setModelDownloadStatuses] = useState<
    Record<string, ModelDownloadStatus>
  >({});
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [livePairStatus, setLivePairStatus] = useState<LivePairStatus | null>(null);
  const [isCapturingShortcut, setIsCapturingShortcut] = useState(false);
  const [capturingField, setCapturingField] = useState<CaptureField>("shortcut");
  const [isTestingMicrophone, setIsTestingMicrophone] = useState(false);
  const [microphoneTestLevel, setMicrophoneTestLevel] = useState(0);
  const [microphoneTestMessage, setMicrophoneTestMessage] = useState<
    string | null
  >(null);
  const microphoneTestPollerRef = useRef<number | null>(null);
  const isTestingMicrophoneRef = useRef(false);
  // Models whose download completed in this session count as installed until
  // the catalog says so (or until they are deleted).
  const completedModelsRef = useRef(new Set<string>());
  const reloadModelStateRef = useRef(onReloadModelState);
  reloadModelStateRef.current = onReloadModelState;

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    // Subscribe first so a slower snapshot never overwrites a newer event.
    void listenLivePairStatus((status) => {
      if (!disposed) setLivePairStatus(status);
    })
      .then(async (cleanup) => {
        if (disposed) {
          cleanup();
          return;
        }
        unlisten = cleanup;
        const snapshot = await getLivePairStatus();
        if (!disposed) setLivePairStatus((current) => current ?? snapshot);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    void listInputDevices()
      .then(setInputDevices)
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    let catalogRevision = 0;
    const liveStatuses = new Set<string>();
    const completedModels = completedModelsRef.current;
    const downloadStages = new Map<string, ModelDownloadStatus["state"]>();

    const refreshCatalog = async () => {
      const revision = ++catalogRevision;
      const models = await listDownloadableModels();
      if (!disposed && revision === catalogRevision) {
        setDownloadableModels(
          models.map((model) => ({
            ...model,
            installed: model.installed || completedModels.has(model.id),
          })),
        );
      }
    };

    const acceptStatus = async (status: ModelDownloadStatus, live = false) => {
      if (disposed) return;
      const previous = downloadStages.get(status.modelId);
      downloadStages.set(status.modelId, status.state);
      setModelDownloadStatuses((current) => ({
        ...current,
        [status.modelId]: status,
      }));
      if (status.state !== "completed" || previous === "completed") return;

      // A live completion follows verified installation. Historical statuses
      // need a fresh catalog check in case the files were removed meanwhile.
      if (live) {
        completedModels.add(status.modelId);
        setDownloadableModels((models) =>
          models.map((model) =>
            model.id === status.modelId ? { ...model, installed: true } : model,
          ),
        );
      }
      const results = await Promise.allSettled([
        refreshCatalog(),
        reloadModelStateRef.current(),
      ]);
      const failed = results.find((result) => result.status === "rejected");
      if (!disposed && failed?.status === "rejected") {
        setErrorMessage(
          `The engine was downloaded, but the list could not be refreshed. ${describeError(failed.reason)}`,
        );
      }
    };

    void refreshCatalog().catch((error) => {
      if (!disposed) {
        setErrorMessage(describeError(error, "Failed to read available models."));
      }
    });
    // Subscribe before taking the snapshot. A slower snapshot must never
    // overwrite a newer completion/progress event.
    void listenModelDownloadStatus((status) => {
      liveStatuses.add(status.modelId);
      void acceptStatus(status, true);
    }).then(async (cleanup) => {
      if (disposed) {
        cleanup();
        return;
      }
      unlisten = cleanup;
      const statuses = await getModelDownloadStatuses();
      await Promise.all(
        statuses.filter((status) => !liveStatuses.has(status.modelId))
          .map((status) => acceptStatus(status)),
      );
    }).catch((error) => {
      if (!disposed) {
        setErrorMessage(describeError(error, "Failed to follow model downloads."));
      }
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  async function persist(patch: SettingsPatch, field: string) {
    setSavedField(null);
    setSavingField(field);
    await onSave(patch);
    setSavedField(field);
  }

  function fieldFeedback(field: string) {
    return (
      <span className="field-feedback" role="status">
        {savingField === field
          ? "Saving…"
          : savedField === field
            ? "Saved"
            : ""}
      </span>
    );
  }

  async function handleChange<K extends keyof AppSettings>(
    key: K,
    value: AppSettings[K],
  ) {
    setIsSaving(true);
    setSavedField(null);
    setSavingField(key);
    setErrorMessage(null);
    try {
      await onSave({ [key]: value } as SettingsPatch);
      setSavedField(key);
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to save settings."),
      );
    } finally {
      setIsSaving(false);
      setSavingField(null);
    }
  }

  useEffect(() => {
    if (!isCapturingShortcut) {
      return;
    }

    function handleShortcutCapture(event: KeyboardEvent) {
      if (!isCapturingShortcut) {
        return;
      }

      if (
        event.key === "Escape" &&
        !event.metaKey &&
        !event.ctrlKey &&
        !event.altKey &&
        !event.shiftKey
      ) {
        event.preventDefault();
        event.stopPropagation();
        void cancelShortcutCapture();
        return;
      }

      const shortcut = acceleratorFromKeyboardEvent(event);
      if (!shortcut) {
        return;
      }

      event.preventDefault();
      event.stopPropagation();

      if (shortcut.kind === "unsupported") {
        setErrorMessage(shortcut.message);
        return;
      }

      setIsCapturingShortcut(false);
      void saveCapturedShortcut(shortcut.value);
    }

    window.addEventListener("keydown", handleShortcutCapture, true);
    return () =>
      window.removeEventListener("keydown", handleShortcutCapture, true);
  }, [isCapturingShortcut, settings?.shortcut, capturingField]);

  useEffect(() => {
    if (!isTestingMicrophone) {
      return;
    }

    const pollLevel = () => {
      void getRecordingInputLevel()
        .then((level) => setMicrophoneTestLevel(level))
        .catch(() => setMicrophoneTestLevel(0));
    };

    pollLevel();
    microphoneTestPollerRef.current = window.setInterval(pollLevel, 80);

    return () => {
      if (microphoneTestPollerRef.current !== null) {
        window.clearInterval(microphoneTestPollerRef.current);
        microphoneTestPollerRef.current = null;
      }
    };
  }, [isTestingMicrophone]);

  useEffect(() => {
    isTestingMicrophoneRef.current = isTestingMicrophone;
  }, [isTestingMicrophone]);

  useEffect(() => {
    return () => {
      if (microphoneTestPollerRef.current !== null) {
        window.clearInterval(microphoneTestPollerRef.current);
      }
      if (isTestingMicrophoneRef.current) {
        void cancelRecordingSession().catch(() => undefined);
      }
    };
  }, []);

  if (!settings) {
    return (
      <section className="screen">
        <div className="glass-panel">Loading settings…</div>
      </section>
    );
  }

  const displayedShortcut = formatShortcutForDisplay(settings.shortcut);
  const oneKey = isOneKeyShortcut(settings.shortcut);
  const selectedInputDeviceKnown =
    !settings.preferredInputDevice ||
    inputDevices.some((device) => device.id === settings.preferredInputDevice);
  const availableInputDevices = selectedInputDeviceKnown
    ? inputDevices
    : [
        ...inputDevices,
        {
          id: settings.preferredInputDevice ?? "__unavailable_input_device__",
          name: `${settings.preferredInputDevice ?? "Saved device"} (Unavailable)`,
          isDefault: false,
        },
      ];
  const diarizationModel = downloadableModels.find(
    (model) => model.capability === "diarization",
  );
  const speechModels = downloadableModels.filter(
    (model) => model.capability === "asr",
  );
  const translationModels = downloadableModels.filter((model) => model.capability === "translation");
  const installedTranslationModels = translationModels.filter((model) => model.installed);
  const translationAvailable = translationModels.some((model) => model.availability === "available");
  const cycleLanguages = settings.translationCycleLanguages ?? ["en", "de", "fr", "es-AR"];
  const cycleStops = OUTPUT_MODES.filter((mode) => mode.value === "original" || cycleLanguages.includes(mode.value as Exclude<OutputMode, "original">));
  const livePairInstalled = installedModels.some((model) => model.id === LIVE_PAIR_ID);
  const livePairSelected = settings.shortcutDictationSelectedModelId === LIVE_PAIR_ID;
  const dictationEngine = installedModels.find(
    (model) => model.id === settings.shortcutDictationSelectedModelId,
  );
  const recordEngine = installedModels.find(
    (model) => model.id === settings.quickDictateSelectedModelId,
  );
  // The record button normally follows the dictation engine; its own picker
  // only shows when they differ or when the user asks for it.
  const recordEngineDiffers = Boolean(
    recordEngine && dictationEngine && recordEngine.id !== dictationEngine.id,
  );
  const showRecordEngine = isRecordEngineExpanded || !recordEngine;
  const recordEngineSummary = !recordEngine
    ? "Choose an engine for the record button"
    : recordEngineDiffers
      ? `Uses ${getFriendlyModelName(recordEngine)}`
      : "Uses the dictation engine";
  /** Downloads that need licence terms (Gemma) ask for agreement first. */
  async function downloadWithTerms(modelId: string) {
    const terms = await getModelTermsStatus(modelId);
    if (terms.required && !terms.accepted) {
      setTermsModelId(modelId);
      return;
    }
    await startModelDownload(modelId);
  }
  const diarizationReady = diarizationModel?.installed === true;
  // Speaker labels are Pro: on the free version the switch shows off, whatever was saved before.
  const speakerLabelsOn = pro && settings.fileDiarizationEnabled;
  const diarizationStatus = diarizationModel
    ? modelDownloadStatuses[diarizationModel.id]
    : undefined;
  const diarizationDownloading = diarizationStatus?.state === "downloading";
  const anotherDownloadActive = Object.values(modelDownloadStatuses).some(
    (status) =>
      status.modelId !== diarizationModel?.id && status.state === "downloading",
  );
  const diarizationProgress =
    diarizationStatus?.progressPercent === null
      ? null
      : Math.round(diarizationStatus?.progressPercent ?? 0);

  async function beginShortcutCapture(field: CaptureField = "shortcut") {
    setCapturingField(field);
    setErrorMessage(null);
    try {
      await suspendShortcutCapture();
      setIsCapturingShortcut(true);
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to start shortcut capture."),
      );
    }
  }

  async function cancelShortcutCapture() {
    setIsCapturingShortcut(false);
    try {
      await resumeShortcutCapture();
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to restore the active shortcut."),
      );
    }
  }

  async function saveCapturedShortcut(shortcut: string) {
    setIsSaving(true);
    setErrorMessage(null);
    try {
      await resumeShortcutCapture();
      await persist({ [capturingField]: shortcut }, capturingField);
    } catch (error) {
      try {
        await resumeShortcutCapture();
      } catch {
        // Preserve the save error as the primary message.
      }
      setErrorMessage(
        describeError(error, "Failed to save settings."),
      );
    } finally {
      setIsSaving(false);
      setSavingField(null);
    }
  }

  async function handleQuickDictateModelChange(modelId: string) {
    const selectedModel =
      installedModels.find((model) => model.id === modelId) ?? null;
    if (!selectedModel) {
      return;
    }

    setIsSaving(true);
    setErrorMessage(null);
    try {
      await persist(
        {
          quickDictateSelectedModelId: selectedModel.id,
          quickDictateModelProfile: selectedModel.profile,
        },
        "quickDictateSelectedModelId",
      );
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to save settings."),
      );
    } finally {
      setIsSaving(false);
      setSavingField(null);
    }
  }

  async function handleDictationEngineChange(modelId: string) {
    const selectedModel =
      installedModels.find((model) => model.id === modelId) ?? null;
    if (!selectedModel) {
      return;
    }
    // The record button follows the dictation engine unless the user gave it
    // its own engine, or this engine cannot run it (e.g. Live dictation).
    const recordFollows =
      (!recordEngine || recordEngine.id === dictationEngine?.id) &&
      (!selectedModel.capabilities ||
        selectedModel.capabilities.supportedContexts.includes("quick_dictate"));

    setIsSaving(true);
    setErrorMessage(null);
    try {
      await persist(
        {
          shortcutDictationSelectedModelId: selectedModel.id,
          shortcutDictationModelProfile: selectedModel.profile,
          ...(recordFollows
            ? {
                quickDictateSelectedModelId: selectedModel.id,
                quickDictateModelProfile: selectedModel.profile,
              }
            : {}),
        },
        "shortcutDictationSelectedModelId",
      );
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to save settings."),
      );
    } finally {
      setIsSaving(false);
      setSavingField(null);
    }
  }

  async function handleFileTranscribeModelChange(modelId: string) {
    const selectedModel =
      installedModels.find((model) => model.id === modelId) ?? null;
    if (!selectedModel) {
      return;
    }

    setIsSaving(true);
    setErrorMessage(null);
    try {
      await persist(
        {
          fileTranscribeSelectedModelId: selectedModel.id,
          fileTranscribeModelProfile: selectedModel.profile,
        },
        "fileTranscribeSelectedModelId",
      );
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to save settings."),
      );
    } finally {
      setIsSaving(false);
      setSavingField(null);
    }
  }

  async function handleLanguageChange(value: string) {
    setIsSaving(true);
    setErrorMessage(null);
    try {
      await persist(
        value === "auto"
          ? { languageMode: "auto", fixedLanguage: null }
          : { languageMode: "fixed", fixedLanguage: value },
        "languageMode",
      );
    } catch (error) {
      setErrorMessage(describeError(error, "Failed to save settings."));
    } finally {
      setIsSaving(false);
      setSavingField(null);
    }
  }

  async function handleOpenModelsFolder() {
    setIsOpeningModelsFolder(true);
    setErrorMessage(null);
    try {
      await openModelsFolder();
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to open the models folder."),
      );
    } finally {
      setIsOpeningModelsFolder(false);
    }
  }

  async function handleDownloadModel(modelId: string) {
    setErrorMessage(null);
    try {
      await startModelDownload(modelId);
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to download the selected model."),
      );
    }
  }

  async function refreshModelLists() {
    const [models] = await Promise.all([
      listDownloadableModels(),
      reloadModelStateRef.current(),
    ]);
    setDownloadableModels(
      models.map((model) => ({
        ...model,
        installed: model.installed || completedModelsRef.current.has(model.id),
      })),
    );
  }

  async function handleDeleteModel(modelId: string) {
    setErrorMessage(null);
    setDeletingModelId(modelId);
    try {
      await deleteModel(modelId);
      completedModelsRef.current.delete(modelId);
      setModelDownloadStatuses((current) => {
        const next = { ...current };
        delete next[modelId];
        return next;
      });
      await refreshModelLists();
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to delete the model."),
      );
    } finally {
      setDeletingModelId(null);
      setConfirmingDeleteId(null);
    }
  }

  async function handleRescanModels() {
    setErrorMessage(null);
    setIsRescanningModels(true);
    try {
      await rescanModelsFolder();
      await refreshModelLists();
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to rescan the models folder."),
      );
    } finally {
      setIsRescanningModels(false);
    }
  }

  function deleteControl(model: DownloadableModel, name: string) {
    const busy = Object.values(modelDownloadStatuses).some(
      (status) => status.modelId === model.id && status.state === "downloading",
    );
    return (
      <DeleteModelControl
        name={name}
        sizeBytes={model.sizeBytes}
        confirming={confirmingDeleteId === model.id}
        deleting={deletingModelId === model.id}
        disabled={busy || (deletingModelId !== null && deletingModelId !== model.id)}
        onAsk={() => setConfirmingDeleteId(model.id)}
        onCancel={() => setConfirmingDeleteId(null)}
        onConfirm={() => void handleDeleteModel(model.id)}
      />
    );
  }

  async function handleCancelModelDownload(modelId: string) {
    setErrorMessage(null);
    try {
      await cancelModelDownload(modelId);
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to cancel the model download."),
      );
    }
  }

  async function stopMicrophoneTest(nextMessage?: string | null) {
    if (microphoneTestPollerRef.current !== null) {
      window.clearInterval(microphoneTestPollerRef.current);
      microphoneTestPollerRef.current = null;
    }
    if (isTestingMicrophone) {
      try {
        await cancelRecordingSession();
      } catch {
        // Ignore teardown failures when leaving test mode.
      }
    }
    setIsTestingMicrophone(false);
    setMicrophoneTestLevel(0);
    if (nextMessage !== undefined) {
      setMicrophoneTestMessage(nextMessage);
    }
  }

  async function toggleMicrophoneTest() {
    setErrorMessage(null);

    if (isTestingMicrophone) {
      await stopMicrophoneTest("Microphone test stopped.");
      return;
    }

    setMicrophoneTestLevel(0);
    setMicrophoneTestMessage("Starting microphone test…");
    try {
      await startRecordingSession(false);
      setIsTestingMicrophone(true);
      setMicrophoneTestMessage(
        "Speak now. Nothing is recorded or saved.",
      );
    } catch (error) {
      setIsTestingMicrophone(false);
      setMicrophoneTestLevel(0);
      setMicrophoneTestMessage(
        describeError(error, "Failed to start the microphone test."),
      );
    }
  }

  async function handlePreferredInputDeviceChange(nextValue: string) {
    if (isTestingMicrophone) {
      await stopMicrophoneTest(
        "Input device changed. Start the microphone test again.",
      );
    }

    await handleChange(
      "preferredInputDevice",
      nextValue.length === 0 ? null : nextValue,
    );
  }

  return (
    <section className="screen settings-screen">
      <PageHeader
        title="Settings"
        description="A few thoughtful adjustments. A smoother day."
      />
      <div className="settings-tabs" aria-label="Settings categories">
        {(
          [
            ["general", "General"],
            ["audio", "Dictation"],
            ["languages", "Languages"],
            ["models", "Engines"],
            ["appearance", "Appearance & feedback"],
            ["advanced", "Advanced"],
            ["license", "License"],
            ["about", "About"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            aria-pressed={group === id}
            onClick={() => {
              if (!isCapturingShortcut && !isTestingMicrophone) {
                setGroup(id);
                onSectionChange?.(id);
              }
            }}
            disabled={
              (isCapturingShortcut || isTestingMicrophone) && group !== id
            }
          >
            {label}
          </button>
        ))}
      </div>
      {errorMessage ? (
        <p className="error-text" role="alert">
          {errorMessage}
        </p>
      ) : null}
      <section
        className="settings-section"
        hidden={group !== "general"}
        aria-label="General"
      >
        <div className="settings-section-heading">
          <h2>General</h2>
          <p className="muted">Make Murmela fit your day.</p>
        </div>
        <div className="settings-grid">
          <article className="settings-card">
            <label className="field-stack">
              <span>Start in</span>
              <select
                value={settings.defaultMode}
                disabled={isSaving}
                onChange={(event) =>
                  void handleChange(
                    "defaultMode",
                    event.target.value as AppSettings["defaultMode"],
                  )
                }
              >
                <option value="quick_dictate">Dictate</option>
                <option value="file_transcribe">Transcribe files</option>
              </select>
            </label>
            {fieldFeedback("defaultMode")}
          </article>
          <article className="settings-card settings-card-wide">
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">Launch Murmela when you log in</p>
                <p className="muted">
                  Start the app automatically when your session begins.
                </p>
              </div>
              <div className="setting-control">
                <button
                  disabled={isSaving}
                  aria-label="Launch Murmela when you log in"
                  type="button"
                  className={
                    settings.launchAtLoginEnabled
                      ? "switch-button is-on"
                      : "switch-button"
                  }
                  aria-pressed={settings.launchAtLoginEnabled}
                  onClick={() =>
                    void handleChange(
                      "launchAtLoginEnabled",
                      !settings.launchAtLoginEnabled,
                    )
                  }
                >
                  <span className="switch-thumb" />
                </button>
                <span className="setting-state">
                  {settings.launchAtLoginEnabled ? "On at login" : "Off"}
                </span>
              </div>
              {fieldFeedback("launchAtLoginEnabled")}
            </div>
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">Save transcripts to the Library</p>
                <p className="muted">
                  Keep finished dictations and file transcripts on this computer.
                </p>
              </div>
              <div className="setting-control">
                <button
                  disabled={isSaving}
                  aria-label="Save transcripts to the Library"
                  type="button"
                  className={
                    settings.saveHistory
                      ? "switch-button is-on"
                      : "switch-button"
                  }
                  aria-pressed={settings.saveHistory}
                  onClick={() =>
                    void handleChange("saveHistory", !settings.saveHistory)
                  }
                >
                  <span className="switch-thumb" />
                </button>
                <span className="setting-state">
                  {settings.saveHistory ? "On" : "Off"}
                </span>
              </div>
              {fieldFeedback("saveHistory")}
            </div>
          </article>
          {onRunWelcomeSetup ? (
            <article className="settings-card settings-card-wide" aria-label="Welcome setup">
              <div className="setting-row">
                <div className="setting-copy">
                  <p className="setting-title">Welcome setup</p>
                  <p className="muted">Choose an engine and your translation languages again, and practise the dictation key.</p>
                </div>
                <div className="setting-control">
                  <Button variant="secondary" onClick={onRunWelcomeSetup}>
                    Run welcome setup
                  </Button>
                </div>
              </div>
            </article>
          ) : null}
          {group === "general" ? <NetworkActivityCard /> : null}
        </div>
      </section>
      <section
        className="settings-section"
        hidden={group !== "audio"}
        aria-label="Dictation"
      >
        <div className="settings-section-heading">
          <h2>Dictation</h2>
          <p className="muted">Your microphone, the shortcut, and what happens with your words.</p>
        </div>
        <div className="settings-grid">
          <article className="glass-subtle settings-card">
            <div className="field-stack">
              <span>Input device</span>
              <select
                aria-label="Input device"
                value={settings.preferredInputDevice || ""}
                disabled={inputDevices.length === 0}
                onChange={(event) =>
                  void handlePreferredInputDeviceChange(event.target.value)
                }
              >
                <option value="">
                  {inputDevices.length === 0
                    ? "No input devices found"
                    : "System default microphone"}
                </option>
                {availableInputDevices.map((device) => (
                  <option key={device.id} value={device.id}>
                    {device.isDefault
                      ? `${device.name} (Default)`
                      : device.name}
                  </option>
                ))}
              </select>
            </div>
            {fieldFeedback("preferredInputDevice")}
          </article>
          <article className="glass-subtle settings-card">
            <div className="field-stack">
              <span>Microphone test</span>
              <div className="microphone-test-panel">
                <p className="microphone-test-device">
                  {settings.preferredInputDevice ?? "System default microphone"}
                </p>
                <div
                  className={
                    isTestingMicrophone
                      ? "microphone-test-meter is-active"
                      : "microphone-test-meter"
                  }
                  aria-hidden="true"
                >
                  <div className="microphone-test-meter-track">
                    <div
                      className="microphone-test-meter-fill"
                      style={{
                        width: `${Math.max(0, microphoneTestLevel * 100)}%`,
                      }}
                    />
                  </div>
                  <span className="microphone-test-meter-label">
                    {isTestingMicrophone
                      ? `${Math.round(microphoneTestLevel * 100)}% input`
                      : "Idle"}
                  </span>
                </div>
                <p className="muted microphone-test-copy">
                  {microphoneTestMessage ??
                    "Speak and watch the meter. If it stays flat, Murmela can't hear this microphone."}
                </p>
                <IconButton
                  icon={isTestingMicrophone ? "stop" : "microphoneActive"}
                  label={
                    isTestingMicrophone
                      ? "Stop microphone test"
                      : "Start microphone test"
                  }
                  state={isTestingMicrophone ? "selected" : "default"}
                  onClick={() => {
                    void toggleMicrophoneTest();
                  }}
                />
              </div>
            </div>
          </article>
          <article className="glass-subtle settings-card settings-card-wide" aria-label="Dictation shortcut">
            <div className="field-stack">
              <span>Shortcut</span>
              <div className="shortcut-choice" role="radiogroup" aria-label="Dictation key">
                {([
                  [FN_KEY_SHORTCUT, "🌐 fn key", "Recommended · bottom-left key"],
                  [RIGHT_OPTION_SHORTCUT, "Right ⌥ Option", "For keyboards without 🌐"],
                  ["combination", "Key combination", "Your own shortcut"],
                ] as const).map(([value, title, detail]) => {
                  const checked = value === "combination" ? !oneKey : settings.shortcut === value;
                  return (
                    <button
                      key={value}
                      type="button"
                      role="radio"
                      aria-checked={checked}
                      className="shortcut-option"
                      disabled={isSaving || isCapturingShortcut}
                      onClick={() => {
                        if (checked) return;
                        void handleChange("shortcut", value === "combination" ? DEFAULT_SHORTCUT : value);
                      }}
                    >
                      <strong>{title}</strong>
                      <span>{detail}</span>
                    </button>
                  );
                })}
              </div>
              {oneKey ? (
                <>
                  <ul className="shortcut-gestures">
                    <li>Hold <kbd>{displayedShortcut}</kbd> to talk, let go to paste.</li>
                    <li>Double-tap it to talk hands-free, tap once more to finish.</li>
                    <li><kbd>Esc</kbd> cancels.{settings.translationEnabled ? <> Hold it and press <kbd>→</kbd> or <kbd>←</kbd> to switch language, and keep holding to dictate in it.</> : null}</li>
                  </ul>
                  {settings.shortcut === FN_KEY_SHORTCUT ? <GlobeKeyNotice /> : null}
                </>
              ) : (
              <>
              <div className="shortcut-field">
                <div className="shortcut-display">
                  {isCapturingShortcut && capturingField === "shortcut"
                    ? "Listening for shortcut… Press Esc to cancel."
                    : displayedShortcut}
                </div>
                <div className="shortcut-actions">
                  <IconButton
                    icon="keyboardEdit"
                    label={
                      isCapturingShortcut
                        ? "Listening for shortcut"
                        : "Set custom shortcut"
                    }
                    state={isCapturingShortcut ? "busy" : "default"}
                    disabled={isSaving || isCapturingShortcut}
                    onClick={() => {
                      void beginShortcutCapture();
                    }}
                  />
                  <IconButton
                    icon={isCapturingShortcut ? "xCircle" : "reset"}
                    label={
                      isCapturingShortcut
                        ? "Cancel shortcut capture"
                        : "Reset shortcut to default"
                    }
                    tone={isCapturingShortcut ? "danger" : "default"}
                    disabled={
                      isSaving ||
                      (!isCapturingShortcut &&
                        settings.shortcut === DEFAULT_SHORTCUT)
                    }
                    onClick={() => {
                      if (isCapturingShortcut) {
                        void cancelShortcutCapture();
                        return;
                      }
                      void handleChange("shortcut", DEFAULT_SHORTCUT);
                    }}
                  />
                </div>
              </div>
              <p className="muted shortcut-hint">Combine ⌘, ⌃, ⌥ or ⇧ with another key.</p>
              </>
              )}
            </div>
            {fieldFeedback("shortcut")}
          </article>
          {oneKey ? null : (
          <article className="settings-card">
            <label className="field-stack">
              <span>Shortcut behavior</span>
              <select
                value={settings.shortcutMode}
                disabled={isSaving}
                onChange={(event) =>
                  void handleChange(
                    "shortcutMode",
                    event.target.value as AppSettings["shortcutMode"],
                  )
                }
              >
                <option value="push_to_talk">Hold to speak</option>
                <option value="toggle">Press to start and stop</option>
              </select>
            </label>
            {fieldFeedback("shortcutMode")}
          </article>
          )}
          <article className="settings-card settings-card-wide">
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">
                  Auto paste after dictation
                  <HelpHint title="Pasting and Accessibility">
                    <p>
                      To put text where your cursor is, Murmela presses ⌘V for you. macOS only allows that for apps
                      you turn on under Privacy &amp; Security → Accessibility, and describes it as “control this
                      computer”.
                    </p>
                    <p>
                      Murmela uses it only to paste and to notice your dictation key. Without it, the text is copied
                      and you paste it yourself.
                    </p>
                  </HelpHint>
                </p>
                <p className="muted">Insert text directly while preserving your previous clipboard contents.</p>
              </div>
              <div className="setting-control">
                <button
                  disabled={isSaving}
                  aria-label="Auto paste after dictation"
                  type="button"
                  className={
                    settings.insertBehavior === "paste"
                      ? "switch-button is-on"
                      : "switch-button"
                  }
                  aria-pressed={settings.insertBehavior === "paste"}
                  onClick={() =>
                    void handleChange(
                      "insertBehavior",
                      settings.insertBehavior === "paste"
                        ? "clipboard_only"
                        : "paste",
                    )
                  }
                >
                  <span className="switch-thumb" />
                </button>
                <span className="setting-state">
                  {settings.insertBehavior === "paste"
                    ? "On when Accessibility allows it"
                    : "Off, copy to clipboard only"}
                </span>
              </div>
              {fieldFeedback("insertBehavior")}
            </div>
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">
                  Lower system audio during shortcut dictation
                </p>
                <p className="muted">
                  Drops output volume to 30% of its current level while
                  Murmela is listening, then restores it.
                </p>
              </div>
              <div className="setting-control">
                <button
                  disabled={isSaving}
                  aria-label="Lower system audio during dictation"
                  type="button"
                  className={
                    settings.volumeDuckingEnabled
                      ? "switch-button is-on"
                      : "switch-button"
                  }
                  aria-pressed={settings.volumeDuckingEnabled}
                  onClick={() =>
                    void handleChange(
                      "volumeDuckingEnabled",
                      !settings.volumeDuckingEnabled,
                    )
                  }
                >
                  <span className="switch-thumb" />
                </button>
                <span className="setting-state">
                  {settings.volumeDuckingEnabled
                    ? "30% of current volume"
                    : "Disabled"}
                </span>
              </div>
              {fieldFeedback("volumeDuckingEnabled")}
            </div>
          </article>
          <article className="glass-subtle settings-card settings-card-wide" aria-label="Paste last dictation">
            <div className="field-stack">
              <span>Paste last dictation</span>
              <div className="shortcut-field">
                <span className="shortcut-display">
                  {isCapturingShortcut && capturingField === "pasteLastShortcut"
                    ? "Press a shortcut… Esc to cancel"
                    : formatShortcutForDisplay(settings.pasteLastShortcut ?? DEFAULT_PASTE_LAST_SHORTCUT)}
                </span>
                <ActionButton
                  disabled={isCapturingShortcut || isSaving}
                  action={() => beginShortcutCapture("pasteLastShortcut")}
                >
                  Set paste-last shortcut
                </ActionButton>
                {isCapturingShortcut && capturingField === "pasteLastShortcut" ? (
                  <ActionButton action={cancelShortcutCapture}>Cancel capture</ActionButton>
                ) : (settings.pasteLastShortcut ?? DEFAULT_PASTE_LAST_SHORTCUT) !== DEFAULT_PASTE_LAST_SHORTCUT ? (
                  <IconButton
                    icon="reset"
                    label="Reset paste-last shortcut to default"
                    disabled={isSaving || isCapturingShortcut}
                    onClick={() => void handleChange("pasteLastShortcut", DEFAULT_PASTE_LAST_SHORTCUT)}
                  />
                ) : null}
              </div>
              <p className="muted">
                Pastes your last dictation again where your cursor is, for when the first paste landed in the wrong place. The menu-bar icon also has it, plus your recent dictations. Press Esc while dictating to cancel.
              </p>
            </div>
            {fieldFeedback("pasteLastShortcut")}
          </article>
        </div>
      </section>
      <section
        className="settings-section"
        hidden={group !== "languages"}
        aria-label="Languages"
      >
        <div className="settings-section-heading">
          <h2>Languages</h2>
          <p className="muted">
            The language you speak, and the languages Murmela can paste your words in.
          </p>
        </div>
        <div className="settings-grid">
          <article className="settings-card">
            <label className="field-stack">
              <span>Language you speak</span>
              <select
                value={settings.languageMode === "fixed" && settings.fixedLanguage ? settings.fixedLanguage : "auto"}
                disabled={isSaving}
                onChange={(event) => void handleLanguageChange(event.target.value)}
              >
                <option value="auto">Automatic (recommended)</option>
                {SPOKEN_LANGUAGES.map(([code, name]) => (
                  <option key={code} value={code}>{name}</option>
                ))}
              </select>
            </label>
            <p className="muted">
              Automatic also follows you when you switch languages mid-sentence. Pick one if short dictations come out in the wrong language. MOSS and VibeVoice always detect the language themselves.
            </p>
            {fieldFeedback("languageMode")}
          </article>
          {termsModelId ? (
            <GemmaTermsDialog
              onCancel={() => setTermsModelId(null)}
              onAccept={async () => {
                const modelId = termsModelId;
                await acceptModelTerms(modelId);
                setTermsModelId(null);
                await startModelDownload(modelId);
              }}
            />
          ) : null}
          <article className="glass-subtle settings-card settings-card-wide" aria-label="Translation">
            <h3>
              Translation <span className="muted">· Preview</span>
              <HelpHint title="Which translation engine?" topic="translation engines">
                <p>
                  Both are TranslateGemma by Google and run on this Mac. <strong>4B</strong> is the compact one: a
                  smaller download, fast, and fine for everyday text. <strong>12B</strong> translates more naturally
                  but needs about 13 GB of free memory while it works.
                </p>
                <p>When unsure, start with 4B. You can download the other one later.</p>
              </HelpHint>
            </h3>
            <p>Translate texts on the Translate screen, and dictate while Murmela pastes your words in any of nine languages.</p>
            <p className="muted">Needs an Apple Silicon Mac. Everything stays on this computer.</p>
            <ul className="translation-model-list" aria-label="Translation engines">
              {translationModels.map((model) => {
                const download = modelDownloadStatuses[model.id];
                const selected = settings.translationModelId === model.id || (model.installed && installedTranslationModels.length === 1);
                return (
                  <li key={model.id} className={"translation-model-row" + (model.installed && selected ? " is-selected" : "")}>
                    <div className="setting-copy">
                      <p className="setting-title">
                        {model.modelName}
                        {model.installed && installedTranslationModels.length > 1 && selected ? <span className="status-pill status-pill-success">In use</span> : null}
                      </p>
                      <p className="muted">{model.description} {formatBytes(model.sizeBytes)} download. {model.requirements}</p>
                    </div>
                    <div className="translation-download-actions">
                      {download?.state === "downloading" ? <>
                        <p role="status">Downloading · {Math.round(download.progressPercent ?? 0)}%</p>
                        <ActionButton action={() => cancelModelDownload(model.id).then(() => undefined)}>Cancel download</ActionButton>
                      </> : <>
                        {model.installed && installedTranslationModels.length > 1 && !selected ? (
                          <ActionButton variant="primary" disabled={isSaving} action={() => handleChange("translationModelId", model.id)}>Use this engine</ActionButton>
                        ) : null}
                        <ActionButton disabled={model.availability !== "available" || Object.values(modelDownloadStatuses).some((status) => status.state === "downloading")}
                          aria-label={(model.installed ? "Check and repair " : "Download ") + model.modelName}
                          action={() => downloadWithTerms(model.id)}>
                          {model.installed ? "Check and repair" : "Download"}
                        </ActionButton>
                        {model.installed ? deleteControl(model, model.modelName) : null}
                      </>}
                    </div>
                    {download?.errorMessage ? <p role="alert" className="warning-text">{readableError(download.errorMessage)}</p> : null}
                  </li>
                );
              })}
            </ul>
            {fieldFeedback("translationModelId")}
            {translationModels[0]?.availabilityReason ? <p className="muted">{translationModels[0].availabilityReason}</p> : null}
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">Translate while you dictate</p>
                <p className="muted">Adds the language shortcut below. The Translate screen works without it.</p>
              </div>
              <div className="setting-control">
                <button
                  disabled={isSaving || installedTranslationModels.length === 0 || !translationAvailable}
                  aria-label="Enable translation"
                  type="button"
                  className={settings.translationEnabled ? "switch-button is-on" : "switch-button"}
                  aria-pressed={settings.translationEnabled ?? false}
                  onClick={() => void handleChange("translationEnabled", !settings.translationEnabled)}
                >
                  <span className="switch-thumb" />
                </button>
              </div>
              {fieldFeedback("translationEnabled")}
            </div>
            <div className="field-stack">
              <span>Language shortcut</span>
              <div className="shortcut-field">
                <span className="shortcut-display">{isCapturingShortcut && capturingField === "translationCycleShortcut" ? "Press a shortcut… Esc to cancel" : formatShortcutForDisplay(settings.translationCycleShortcut ?? DEFAULT_LANGUAGE_SHORTCUT)}</span>
                <ActionButton disabled={isCapturingShortcut || isSaving} action={() => beginShortcutCapture("translationCycleShortcut")}>Set language shortcut</ActionButton>
                {isCapturingShortcut && capturingField === "translationCycleShortcut" ? (
                  <ActionButton action={cancelShortcutCapture}>Cancel capture</ActionButton>
                ) : settings.translationCycleShortcut !== DEFAULT_LANGUAGE_SHORTCUT ? (
                  <IconButton
                    icon="reset"
                    label="Reset language shortcut to default"
                    disabled={isSaving || isCapturingShortcut}
                    onClick={() => void handleChange("translationCycleShortcut", DEFAULT_LANGUAGE_SHORTCUT)}
                  />
                ) : null}
              </div>
              {settings.translationEnabled ? <>
                <p className="muted">Switches {cycleStops.map((mode) => mode.label).join(" → ")} between dictations. While translation is on, this key combination belongs to Murmela in every app.</p>
                {oneKey ? (
                  <p className="muted">Or hold <kbd>{displayedShortcut}</kbd> and press <kbd>→</kbd> or <kbd>←</kbd> in any app. Keep holding to dictate straight away in the new language.</p>
                ) : null}
              </> : (
                <p className="warning-text">Off. Turn on Translate while you dictate above to switch languages with this shortcut{oneKey ? <> or with <kbd>{displayedShortcut}</kbd> + <kbd>→</kbd></> : null}.</p>
              )}
            </div>
            {fieldFeedback("translationCycleShortcut")}
            <fieldset className="cycle-languages">
              <legend>Translate into</legend>
              <div className="cycle-language-options">
                {OUTPUT_MODES.filter((mode) => mode.value !== "original").map((mode) => {
                  const code = mode.value as Exclude<OutputMode, "original">;
                  const checked = cycleLanguages.includes(code);
                  return (
                    <label key={code} className="cycle-language">
                      <input
                        type="checkbox"
                        checked={checked}
                        disabled={isSaving || (checked && cycleLanguages.length === 1)}
                        onChange={() => void handleChange("translationCycleLanguages", checked ? cycleLanguages.filter((item) => item !== code) : [...cycleLanguages, code])}
                      />
                      <span>{mode.label}</span>
                    </label>
                  );
                })}
              </div>
              <p className="muted">🌐 + → and the language shortcut switch between Original and these. The Dictate screen offers every language.</p>
            </fieldset>
            {fieldFeedback("translationCycleLanguages")}
            <p className="muted">Uses Google&apos;s TranslateGemma (converted by mradermacher), subject to the <a href="https://ai.google.dev/gemma/terms" target="_blank" rel="noreferrer">Gemma terms</a> and <a href="https://ai.google.dev/gemma/prohibited_use_policy" target="_blank" rel="noreferrer">use restrictions</a>. Model information: <a href="https://huggingface.co/google/translategemma-12b-it" target="_blank" rel="noreferrer">12B</a>, <a href="https://huggingface.co/google/translategemma-4b-it" target="_blank" rel="noreferrer">4B</a></p>
          </article>
          <QuickTranslateCard
            settings={settings}
            isSaving={isSaving}
            hasEngine={installedTranslationModels.length > 0 && translationAvailable}
            capturing={isCapturingShortcut && capturingField === "quickTranslateShortcut"}
            onChange={handleChange}
            onCaptureShortcut={() => beginShortcutCapture("quickTranslateShortcut")}
            onCancelCapture={cancelShortcutCapture}
            feedback={fieldFeedback}
          />
        </div>
      </section>
      <section
        className="settings-section"
        hidden={group !== "models"}
        aria-label="Engines"
      >
        <div className="settings-section-heading">
          <h2>
            Engines
            <HelpHint title="Why several engines?" topic="engines">
              <p>
                An engine is the part that turns speech into text. Different jobs suit different engines, so Murmela
                lets you pick one for each:
              </p>
              <p>
                <strong>Dictation</strong>: the shortcut in any app. Speed matters most.
                <br />
                <strong>Record button</strong>: the button on the Dictate screen. It uses the dictation engine unless
                you choose another.
                <br />
                <strong>Files</strong>: audio and video files. Accuracy matters more than speed.
              </p>
              <p>You can use one engine for everything. More engines only cost disk space.</p>
            </HelpHint>
          </h2>
          <p className="muted">
            The speech engines that turn your voice into text. All of them run on this computer.
          </p>
        </div>
        <div className="settings-grid">
          <article className="glass-subtle settings-card settings-card-wide" aria-label="Dictation engine">
            <div className="field-stack">
              <ModelPicker
                label="Dictation engine"
                value={settings.shortcutDictationSelectedModelId ?? ""}
                models={installedModels}
                context="shortcut_dictation"
                disabled={isSaving}
                onChange={handleDictationEngineChange}
              />
              <p className="muted">Used for the dictation shortcut and, when it can, the record button on the Dictate screen.</p>
            </div>
            {fieldFeedback("shortcutDictationSelectedModelId")}
            <div className="record-engine">
              <button
                type="button"
                className="downloads-accordion-button record-engine-toggle"
                aria-expanded={showRecordEngine}
                onClick={() => setIsRecordEngineExpanded((current) => !current)}
              >
                <span className="downloads-accordion-copy">
                  <span>Record button</span>
                  <span className="muted">{recordEngineSummary}</span>
                </span>
                <span className={showRecordEngine ? "downloads-chevron is-open" : "downloads-chevron"}>
                  <svg viewBox="0 0 20 20" aria-hidden="true">
                    <path d="m6 8 4 4 4-4" />
                  </svg>
                </span>
              </button>
              {showRecordEngine ? (
                <div className="field-stack">
                  <ModelPicker
                    label="Record button engine"
                    value={settings.quickDictateSelectedModelId ?? ""}
                    models={installedModels}
                    context="quick_dictate"
                    disabled={isSaving}
                    onChange={handleQuickDictateModelChange}
                  />
                  {fieldFeedback("quickDictateSelectedModelId")}
                </div>
              ) : null}
            </div>
            {livePairSelected && !settings.launchAtLoginEnabled ? (
              <div className="setting-row">
                <p className="muted">
                  Tip: launch Murmela at login so Live dictation is ready before your first dictation.
                </p>
                <ActionButton
                  disabled={isSaving}
                  action={() => handleChange("launchAtLoginEnabled", true)}
                >
                  Launch at login
                </ActionButton>
              </div>
            ) : null}
          </article>
          <article className="glass-subtle settings-card settings-card-wide" aria-label="File engine">
            <div className="field-stack">
              <ModelPicker
                label="File engine"
                value={settings.fileTranscribeSelectedModelId ?? ""}
                models={installedModels}
                context="file_transcription"
                disabled={isSaving}
                onChange={handleFileTranscribeModelChange}
              />
              <p className="muted">Used for audio and video files. Accuracy matters more than speed here.</p>
            </div>
            {fieldFeedback("fileTranscribeSelectedModelId")}
          </article>
          <article className="glass-subtle settings-card settings-card-wide">
            <div className="field-stack">
              <button
                type="button"
                className="downloads-accordion-button"
                aria-expanded={isDownloadsExpanded}
                onClick={() => setIsDownloadsExpanded((current) => !current)}
              >
                <div className="downloads-accordion-copy">
                  <span>Download engines</span>
                  <p className="muted">
                    {formatDownloadSummary(
                      speechModels.filter((model) => (model.origin ?? "catalog") === "catalog"),
                      installedModels,
                      modelDownloadStatuses,
                    )}
                    {speechModels.some((model) => model.origin === "custom")
                      ? ` · ${speechModels.filter((model) => model.origin === "custom").length} more installed`
                      : ""}
                  </p>
                </div>
                <span
                  className={
                    isDownloadsExpanded
                      ? "downloads-chevron is-open"
                      : "downloads-chevron"
                  }
                >
                  <svg viewBox="0 0 20 20" aria-hidden="true">
                    <path d="m6 8 4 4 4-4" />
                  </svg>
                </span>
              </button>
              {isDownloadsExpanded ? (
                <div className="downloadable-models">
                  {speechModels.map((model) => {
                    const presentation = getModelPresentation(model);
                    const isInstalled =
                      model.installed ||
                      installedModels.some(
                        (installed) => installed.id === model.id,
                      );
                    const isUnavailable = model.availability !== "available";
                    const downloadStatus = modelDownloadStatuses[model.id];
                    const isDownloading =
                      downloadStatus?.state === "downloading";
                    const anotherDownloadActive = Object.values(
                      modelDownloadStatuses,
                    ).some(
                      (status) =>
                        status.modelId !== model.id &&
                        status.state === "downloading",
                    );
                    const progressLabel =
                      downloadStatus?.totalBytes &&
                      downloadStatus.downloadedBytes > 0
                        ? `${formatBytes(downloadStatus.downloadedBytes)} / ${formatBytes(downloadStatus.totalBytes)}`
                        : isDownloading
                          ? `${formatBytes(downloadStatus?.downloadedBytes ?? 0)} downloaded`
                          : null;
                    return (
                      <article
                        key={model.id}
                        className="downloadable-model-card"
                      >
                        <div className="downloadable-model-copy">
                          <div className="downloadable-model-heading">
                            <ModelSummary
                              presentation={presentation}
                              recommendation={presentation.recommendedFor.length > 0 ? "Recommended" : null}
                            />
                            <ModelInfoButton model={model} />
                          </div>
                          <p className="downloadable-model-meta">
                            {formatBytes(model.sizeBytes)} · Works for: {formatWorkflows(model.capabilities)}
                          </p>
                          {model.availabilityReason ? (
                            <p className="downloadable-model-meta">
                              {model.availabilityReason}
                            </p>
                          ) : null}
                          {model.origin === "custom" ? (
                            <p className="downloadable-model-meta warning-text">
                              Added to the models folder by hand · not verified by Murmela · use at your own risk
                            </p>
                          ) : null}
                          {model.licenseUrl ? <p className="downloadable-model-meta"><a href={model.licenseUrl} target="_blank" rel="noreferrer">Model licence</a></p> : null}
                          {isDownloading ? (
                            <div className="model-download-progress">
                              <div className="model-download-progress-track">
                                <div
                                  className={
                                    downloadStatus.progressPercent === null
                                      ? "model-download-progress-bar is-indeterminate"
                                      : "model-download-progress-bar"
                                  }
                                  style={
                                    downloadStatus.progressPercent === null
                                      ? undefined
                                      : {
                                          width: `${downloadStatus.progressPercent}%`,
                                        }
                                  }
                                />
                              </div>
                              <div className="model-download-progress-meta">
                                <span>
                                  {downloadStatus.progressPercent === null
                                    ? "Downloading…"
                                    : `${Math.round(downloadStatus.progressPercent)}%`}
                                </span>
                                {progressLabel ? (
                                  <span>{progressLabel}</span>
                                ) : null}
                              </div>
                              {downloadStatus.currentArtifact ? (
                                <span className="downloadable-model-meta">
                                  File {downloadStatus.artifactIndex ?? 1} of{" "}
                                  {downloadStatus.artifactCount}
                                </span>
                              ) : null}
                            </div>
                          ) : null}
                          {downloadStatus?.state === "failed" &&
                          downloadStatus.errorMessage ? (
                            <p className="error-text model-download-error">
                              {readableError(downloadStatus.errorMessage)}
                            </p>
                          ) : null}
                        </div>
                        <div className="downloadable-model-actions">
                          <span
                            className={
                              isInstalled
                                ? "status-pill status-pill-success"
                                : isDownloading
                                  ? "status-pill status-pill-processing"
                                  : downloadStatus?.state === "failed"
                                    ? "status-pill status-pill-error"
                                    : downloadStatus?.state === "completed"
                                      ? "status-pill status-pill-success"
                                      : "status-pill status-pill-idle"
                            }
                          >
                            {isInstalled
                              ? "Installed"
                              : isUnavailable
                                ? "Unavailable"
                                : isDownloading
                                  ? "Downloading"
                                  : downloadStatus?.state === "failed"
                                    ? "Failed"
                                    : downloadStatus?.state === "completed"
                                      ? "Downloaded"
                                      : "Available"}
                          </span>
                          {model.installed ? deleteControl(model, presentation.friendlyName) : null}
                          {!isUnavailable && !isInstalled ? (
                            <IconButton
                              icon={
                                isDownloading
                                  ? "xCircle"
                                  : downloadStatus?.state === "failed"
                                    ? "retry"
                                    : "download"
                              }
                              label={
                                isDownloading
                                  ? `Cancel download of ${presentation.friendlyName}`
                                  : downloadStatus?.state === "failed"
                                    ? `Retry download of ${presentation.friendlyName}`
                                    : `Download ${presentation.friendlyName}`
                              }
                              tone={isDownloading ? "danger" : "default"}
                              disabled={!isDownloading && anotherDownloadActive}
                              onClick={() => {
                                if (isDownloading) {
                                  void handleCancelModelDownload(model.id);
                                } else {
                                  void handleDownloadModel(model.id);
                                }
                              }}
                            />
                          ) : null}
                        </div>
                      </article>
                    );
                  })}
                </div>
              ) : null}
            </div>
          </article>
          <article className="glass-subtle settings-card settings-card-wide">
            <div className="field-stack">
              <span>Who said what</span>
              <p className="muted" style={{ margin: 0 }}>
                Labels the speakers in transcribed files. MOSS and VibeVoice
                label speakers on their own and don&apos;t need this.
              </p>
              {pro ? null : (
                <ProHint>Speaker labels are part of Murmela Pro.</ProHint>
              )}
              <div className="settings-option-list">
                <div className="setting-row">
                  <div className="setting-copy">
                    <p className="setting-title">Speaker identification</p>
                    <p className="muted">
                      {diarizationReady
                        ? "The local speaker model is installed."
                        : speakerLabelsOn
                          ? diarizationDownloading
                            ? `Installing the speaker model${diarizationProgress === null ? "" : ` — ${diarizationProgress}%`}`
                            : diarizationStatus?.state === "failed"
                              ? "The speaker model could not be installed."
                              : "Preparing the speaker model download…"
                          : `Downloads ${formatBytes(diarizationModel?.sizeBytes ?? 32_478_041)} the first time you turn this on.`}
                    </p>
                  </div>
                  <div className="setting-control">
                    <button
                      type="button"
                      className={
                        speakerLabelsOn
                          ? "switch-button is-on"
                          : "switch-button"
                      }
                      aria-pressed={speakerLabelsOn}
                      disabled={
                        !pro ||
                        isSaving ||
                        !diarizationModel ||
                        diarizationModel.availability !== "available" ||
                        (!settings.fileDiarizationEnabled &&
                          anotherDownloadActive)
                      }
                      onClick={() =>
                        void handleChange(
                          "fileDiarizationEnabled",
                          !settings.fileDiarizationEnabled,
                        )
                      }
                    >
                      <span className="switch-thumb" />
                    </button>
                    <span className="setting-state">
                      {speakerLabelsOn
                        ? diarizationReady
                          ? "On"
                          : diarizationDownloading
                            ? "Installing"
                            : diarizationStatus?.state === "failed"
                              ? "Retry needed"
                              : "Starting"
                        : "Off"}
                    </span>
                  </div>
                </div>
                {diarizationReady && diarizationModel ? (
                  <div className="setting-row">
                    <p className="muted">Speaker model · {formatBytes(diarizationModel.sizeBytes)} · deleting it turns speaker identification off</p>
                    {deleteControl(diarizationModel, "the speaker model")}
                  </div>
                ) : null}
                {settings.fileDiarizationEnabled && diarizationDownloading ? (
                  <div className="model-download-progress">
                    <div className="model-download-progress-track">
                      <div
                        className={
                          diarizationProgress === null
                            ? "model-download-progress-bar is-indeterminate"
                            : "model-download-progress-bar"
                        }
                        style={
                          diarizationProgress === null
                            ? undefined
                            : { width: `${diarizationProgress}%` }
                        }
                      />
                    </div>
                    <div className="model-download-progress-meta">
                      <span>
                        {diarizationProgress === null
                          ? "Installing…"
                          : `${diarizationProgress}%`}
                      </span>
                      {diarizationStatus?.totalBytes ? (
                        <span>
                          {formatBytes(diarizationStatus.downloadedBytes)} /{" "}
                          {formatBytes(diarizationStatus.totalBytes)}
                        </span>
                      ) : null}
                    </div>
                  </div>
                ) : null}
                {settings.fileDiarizationEnabled &&
                diarizationStatus?.state === "failed" ? (
                  <div className="setting-row">
                    <p className="error-text model-download-error">
                      {(diarizationStatus.errorMessage && readableError(diarizationStatus.errorMessage)) ??
                        "The speaker model download failed."}
                    </p>
                    <IconButton
                      icon="retry"
                      label="Retry speaker model download"
                      disabled={anotherDownloadActive}
                      onClick={() =>
                        diarizationModel &&
                        void handleDownloadModel(diarizationModel.id)
                      }
                    />
                  </div>
                ) : null}
                {!diarizationModel ||
                diarizationModel.availability !== "available" ? (
                  <p className="warning-text" style={{ margin: 0 }}>
                    {diarizationModel?.availabilityReason ??
                      "The speaker model is unavailable."}
                  </p>
                ) : null}
              </div>
            </div>
            {fieldFeedback("fileDiarizationEnabled")}
          </article>
        </div>
      </section>
      <section
        className="settings-section"
        hidden={group !== "appearance"}
        aria-label="Appearance & feedback"
      >
        <div className="settings-section-heading">
          <h2>Appearance & feedback</h2>
          <p className="muted">A workspace that feels like yours.</p>
        </div>
        <div className="settings-grid">
          <article className="settings-card">
            <label className="field-stack">
              <span>Appearance</span>
              <select
                value={settings.appearance}
                disabled={isSaving}
                onChange={(event) =>
                  void handleChange(
                    "appearance",
                    event.target.value as AppSettings["appearance"],
                  )
                }
              >
                <option value="system">Follow system</option>
                <option value="light">Light</option>
                <option value="dark">Dark</option>
              </select>
            </label>
            {fieldFeedback("appearance")}
          </article>
          <article className="settings-card">
            <label className="field-stack">
              <span>Motion</span>
              <select
                value={settings.motionPreference}
                disabled={isSaving}
                onChange={(event) =>
                  void handleChange(
                    "motionPreference",
                    event.target.value as AppSettings["motionPreference"],
                  )
                }
              >
                <option value="system">Follow system</option>
                <option value="reduced">Reduce motion</option>
              </select>
            </label>
            <p className="muted">
              System reduced-motion preferences are always respected.
            </p>
            {fieldFeedback("motionPreference")}
          </article>
          <article className="settings-card settings-card-wide">
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">Play feedback sounds</p>
                <p className="muted">
                  Quiet cues for recording, task completion, and errors.
                </p>
              </div>
              <div className="setting-control">
                <button
                  disabled={isSaving}
                  aria-label="Play feedback sounds"
                  type="button"
                  className={
                    settings.soundsEnabled
                      ? "switch-button is-on"
                      : "switch-button"
                  }
                  aria-pressed={settings.soundsEnabled}
                  onClick={() =>
                    void handleChange("soundsEnabled", !settings.soundsEnabled)
                  }
                >
                  <span className="switch-thumb" />
                </button>
                <span className="setting-state">
                  {settings.soundsEnabled ? "Enabled" : "Disabled"}
                </span>
              </div>
              {fieldFeedback("soundsEnabled")}
            </div>
            <div className="sound-preview-row" aria-label="Preview sounds">
              {(["start", "stop", "complete", "error"] as const).map((cue) => (
                <ActionButton
                  key={cue}
                  action={() => previewFeedbackSound(cue)}
                  success=""
                  disabled={!settings.soundsEnabled}
                >
                  Preview {cue}
                </ActionButton>
              ))}
            </div>
          </article>
          <article className="settings-card settings-card-wide" aria-label="Personality">
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">Serious mode</p>
                <p className="muted">
                  Turns off the Murmelmeter, playful messages and easter eggs.
                  Murmela never jokes about errors either way.
                </p>
              </div>
              <div className="setting-control">
                <button
                  disabled={isSaving}
                  aria-label="Serious mode"
                  type="button"
                  className={settings.seriousMode ? "switch-button is-on" : "switch-button"}
                  aria-pressed={settings.seriousMode ?? false}
                  onClick={() => void handleChange("seriousMode", !settings.seriousMode)}
                >
                  <span className="switch-thumb" />
                </button>
                <span className="setting-state">{settings.seriousMode ? "On" : "Off"}</span>
              </div>
              {fieldFeedback("seriousMode")}
            </div>
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">Mela</p>
                <p className="muted">
                  A small glass marmot that reacts to what Murmela is doing, in the dictation pill and in this window.
                  She is drawn inside the app, never sees your words and looks away when your notes are open.
                </p>
              </div>
              <div className="setting-control">
                <button
                  disabled={settings.seriousMode}
                  aria-label="Mela"
                  type="button"
                  className={melaEnabled && !settings.seriousMode ? "switch-button is-on" : "switch-button"}
                  aria-pressed={melaEnabled && !settings.seriousMode}
                  onClick={() => setMelaEnabled(!melaEnabled)}
                >
                  <span className="switch-thumb" />
                </button>
                <span className="setting-state">
                  {settings.seriousMode ? "Off in Serious mode" : melaEnabled ? "On" : "Off"}
                </span>
              </div>
            </div>
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">Murmelmeter</p>
                <p className="muted">
                  Counts words, speaking time and streaks on this computer. It stores numbers only, never what you said.
                </p>
              </div>
              <ActionButton action={() => resetDictationStats()} success="Reset">
                Reset Murmelmeter
              </ActionButton>
            </div>
          </article>
        </div>
      </section>
      <section
        className="settings-section"
        hidden={group !== "advanced"}
        aria-label="Advanced"
      >
        <div className="settings-section-heading">
          <h2>Advanced</h2>
          <p className="muted">Device integration and technical controls.</p>
        </div>
        <div className="settings-grid">
          <article className="glass-subtle settings-card settings-card-wide">
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">Models folder</p>
                <p className="muted">
                  Open the shared model directory in Finder to
                  add or manage model files. whisper.cpp models (.bin) you add
                  appear in the model lists after a rescan. Murmela does not
                  verify them, so use them at your own risk.
                </p>
              </div>
              <div className="setting-control">
                <IconButton
                  icon="retry"
                  label="Rescan models folder"
                  state={isRescanningModels ? "busy" : "default"}
                  disabled={isRescanningModels}
                  onClick={() => {
                    void handleRescanModels();
                  }}
                />
                <IconButton
                  icon="folder"
                  label="Open in Finder"
                  state={isOpeningModelsFolder ? "busy" : "default"}
                  disabled={isOpeningModelsFolder}
                  onClick={() => {
                    void handleOpenModelsFolder();
                  }}
                />
              </div>
            </div>
          </article>
          {livePairInstalled ? (
            <article className="glass-subtle settings-card settings-card-wide" aria-label="Live dictation tuning">
              <h3>
                Live dictation tuning
                <HelpHint title="Live dictation tuning">
                  <p>
                    <strong>Faster</strong> shows words sooner while you speak. <strong>Steadier</strong> waits a
                    little longer, so the preview changes less and German comes out better. The final text is pasted
                    either way.
                  </p>
                  <p>
                    <strong>Keep loaded</strong> holds the engine in memory (about 1.4 GB) so it is ready the moment
                    you press the key. When off, it loads on each press, which adds a short wait, and frees the
                    memory after a minute.
                  </p>
                </HelpHint>
              </h3>
              {livePairInstalled ? <>
                <p className="muted" role="status">
                  Live dictation: {LIVE_PAIR_STATE_LABELS[livePairStatus?.state ?? "off"]}
                  {livePairStatus?.state === "ready" && livePairStatus.loadMs != null
                    ? ` · loaded in ${(livePairStatus.loadMs / 1000).toFixed(1)} s`
                    : ""}
                  {" · about 1.4 GB of memory while loaded"}
                </p>
                {livePairStatus?.message ? (
                  <p className={livePairStatus.state === "error" ? "warning-text" : "muted"}>{livePairStatus.message}</p>
                ) : null}
                <label className="field-stack">
                  <span>Live preview</span>
                  <select
                    value={settings.livePairChunkMs ?? 560}
                    disabled={isSaving}
                    onChange={(event) =>
                      void handleChange("livePairChunkMs", Number(event.target.value) as AppSettings["livePairChunkMs"])
                    }
                  >
                    <option value={560}>Faster · words appear sooner</option>
                    <option value={1120}>Steadier · calmer preview, better for German</option>
                  </select>
                </label>
                {fieldFeedback("livePairChunkMs")}
                <div className="setting-row">
                  <div className="setting-copy">
                    <p className="setting-title">Keep Live dictation loaded</p>
                    <p className="muted">
                      Ready the moment you press the shortcut. When off, the models load on each press and are released after a minute.
                    </p>
                  </div>
                  <div className="setting-control">
                    <button
                      disabled={isSaving}
                      aria-label="Keep Live dictation loaded"
                      type="button"
                      className={settings.livePairKeepLoaded ? "switch-button is-on" : "switch-button"}
                      aria-pressed={settings.livePairKeepLoaded}
                      onClick={() => void handleChange("livePairKeepLoaded", !settings.livePairKeepLoaded)}
                    >
                      <span className="switch-thumb" />
                    </button>
                    <span className="setting-state">{settings.livePairKeepLoaded ? "Loaded" : "On demand"}</span>
                  </div>
                  {fieldFeedback("livePairKeepLoaded")}
                </div>
              </> : null}
            </article>
          ) : null}
          <article className="settings-card settings-card-wide">
            <div className="setting-row">
              <div className="setting-copy">
                <p className="setting-title">
                  Use the graphics chip (GPU)
                  <HelpHint title="Graphics chip (GPU)" topic="the graphics chip">
                    <p>
                      Engines that support it run on your Mac's graphics chip through Apple's Metal, which is usually
                      much faster than the main processor (CPU).
                    </p>
                    <p>If that does not work, Murmela falls back to the CPU on its own. Turn this off only to troubleshoot.</p>
                  </HelpHint>
                </p>
                <p className="muted">
                  Faster transcription where supported. Falls back to the main processor if not.
                </p>
              </div>
              <div className="setting-control">
                <button
                  disabled={isSaving}
                  aria-label="Use GPU acceleration"
                  type="button"
                  className={
                    settings.gpuEnabled
                      ? "switch-button is-on"
                      : "switch-button"
                  }
                  aria-pressed={settings.gpuEnabled}
                  onClick={() =>
                    void handleChange("gpuEnabled", !settings.gpuEnabled)
                  }
                >
                  <span className="switch-thumb" />
                </button>
                <span className="setting-state">
                  {settings.gpuEnabled
                    ? "Try Metal when available"
                    : "CPU only"}
                </span>
              </div>
              {fieldFeedback("gpuEnabled")}
            </div>
          </article>
        </div>
      </section>
      <section
        className="settings-section"
        hidden={group !== "license"}
        aria-label="License"
      >
        <div className="settings-section-heading">
          <h2>License</h2>
          <p className="muted">Pro or Lifetime on this Mac.</p>
        </div>
        {group === "license" ? <LicenseCard /> : null}
      </section>
      <section
        className="settings-section"
        hidden={group !== "about"}
        aria-label="About"
      >
        <div className="settings-section-heading">
          <h2>About</h2>
          <p className="muted">Updates, credits and licences.</p>
        </div>
        {group === "about" ? (
          <AboutSection
            autoUpdateEnabled={settings.autoUpdateEnabled ?? true}
            saving={isSaving}
            onToggleAutoUpdate={() => void handleChange("autoUpdateEnabled", !(settings.autoUpdateEnabled ?? true))}
          />
        ) : null}
      </section>
    </section>
  );
}

function DeleteModelControl({
  name,
  sizeBytes,
  confirming,
  deleting,
  disabled,
  onAsk,
  onCancel,
  onConfirm,
}: {
  name: string;
  sizeBytes: number;
  confirming: boolean;
  deleting: boolean;
  disabled: boolean;
  onAsk: () => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  if (confirming || deleting) {
    return (
      <span className="model-delete-confirm" role="group" aria-label={`Delete ${name}`}>
        <span className="downloadable-model-meta">
          Delete {name} and free {formatBytes(sizeBytes)}?
        </span>
        <Button variant="danger" busy={deleting} onClick={onConfirm}>
          Delete
        </Button>
        <Button variant="ghost" disabled={deleting} onClick={onCancel}>
          Keep
        </Button>
      </span>
    );
  }
  return (
    <IconButton
      icon="trash"
      label={`Delete ${name}`}
      tone="danger"
      disabled={disabled}
      onClick={onAsk}
    />
  );
}

function formatDownloadSummary(
  downloadableModels: DownloadableModel[],
  installedModels: InstalledModel[],
  statuses: Record<string, ModelDownloadStatus>,
) {
  const modelIds = new Set(downloadableModels.map((model) => model.id));
  const installedCount = downloadableModels.filter(
    (model) =>
      model.installed ||
      installedModels.some(
        (installed) =>
          installed.id === model.id || installed.modelName === model.modelName,
      ),
  ).length;
  const activeStatus = Object.values(statuses).find(
    (status) => modelIds.has(status.modelId) && status.state === "downloading",
  );
  if (activeStatus) {
    const activeModel = downloadableModels.find(
      (model) => model.id === activeStatus.modelId,
    );
    const activeName = activeModel
      ? getModelPresentation(activeModel).friendlyName
      : activeStatus.modelName;
    return `${installedCount} of ${downloadableModels.length} installed · Downloading ${activeName}`;
  }
  return `${installedCount} of ${downloadableModels.length} installed`;
}

type CapturedShortcut =
  { kind: "valid"; value: string } | { kind: "unsupported"; message: string };

function acceleratorFromKeyboardEvent(event: KeyboardEvent): CapturedShortcut | null {
  if (
    event.key === "Fn" ||
    event.key === "Globe" ||
    event.code === "Fn" ||
    event.code === "Globe" ||
    event.getModifierState("Fn")
  ) {
    return {
      kind: "unsupported",
      message:
        "The Fn/Globe key can't be used. Combine Cmd, Ctrl, Alt or Shift with another key.",
    };
  }

  const key = normalizeShortcutKey(event);
  if (!key) {
    return null;
  }

  const modifiers: string[] = [];
  if (event.metaKey) modifiers.push("CmdOrCtrl");
  if (event.ctrlKey) modifiers.push("Ctrl");
  if (event.altKey) {
    modifiers.push("Alt");
  }
  if (event.shiftKey) {
    modifiers.push("Shift");
  }

  if (modifiers.length === 0) {
    return null;
  }

  return { kind: "valid", value: [...modifiers, key].join("+") };
}

function normalizeShortcutKey(event: KeyboardEvent) {
  const { code, key } = event;
  if (code.startsWith("Key")) {
    return code.slice(3).toUpperCase();
  }
  if (code.startsWith("Digit")) {
    return code.slice(5);
  }
  if (code.startsWith("Numpad") && code.length > "Numpad".length) {
    return code;
  }
  if (/^F\d{1,2}$/.test(key)) {
    return key.toUpperCase();
  }

  switch (code) {
    case "Backquote":
      return "Backquote";
    case "Backslash":
      return "Backslash";
    case "BracketLeft":
      return "BracketLeft";
    case "BracketRight":
      return "BracketRight";
    case "Comma":
      return "Comma";
    case "Equal":
      return "Equal";
    case "Minus":
      return "Minus";
    case "Period":
      return "Period";
    case "Quote":
      return "Quote";
    case "Semicolon":
      return "Semicolon";
    case "Slash":
      return "Slash";
    case "Space":
      return "Space";
    case "Enter":
      return "Enter";
    case "Tab":
      return "Tab";
    case "Backspace":
      return "Backspace";
    case "CapsLock":
      return "CapsLock";
    case "Delete":
      return "Delete";
    case "Escape":
      return "Escape";
    case "ArrowUp":
      return "Up";
    case "ArrowDown":
      return "Down";
    case "ArrowLeft":
      return "Left";
    case "ArrowRight":
      return "Right";
    case "Home":
    case "End":
    case "PageUp":
    case "PageDown":
    case "Insert":
    case "PrintScreen":
    case "ScrollLock":
    case "NumLock":
      return code;
    case "AudioVolumeDown":
      return "AudioVolumeDown";
    case "AudioVolumeUp":
      return "AudioVolumeUp";
    case "AudioVolumeMute":
      return "AudioVolumeMute";
    default:
      break;
  }

  if (["Meta", "Control", "Shift", "Alt"].includes(key)) {
    return null;
  }
  if (key.length === 1) {
    return key.toUpperCase();
  }

  return null;
}
