import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import murmelaMark from "../assets/murmela-mark.png";
import { Button, Progress } from "../components/Feedback";
import { AppIcon } from "../components/IconButton";
import {
  acceptModelTerms,
  cancelModelDownload,
  getModelDownloadStatuses,
  getModelsFreeSpace,
  getModelTermsStatus,
  listDownloadableModels,
  listenModelDownloadStatus,
  startModelDownload,
} from "../lib/api";
import { GemmaTermsDialog } from "../components/GemmaTermsDialog";
import { COMPACT_TRANSLATION_MODEL_ID, getTranslatorStatus } from "../lib/textTranslation";
import { getDictationOutputState, listenDictationOutput, OUTPUT_MODES } from "../lib/translationApi";
import { describeError, FN_KEY_SHORTCUT, formatBytes, formatShortcutForDisplay, isOneKeyShortcut, readableError } from "../lib/formatting";
import { GlobeKeyNotice } from "../components/GlobeKeyNotice";
import { getModelPresentation } from "../lib/modelPresentation";
import type {
  AppSettings,
  DictationOutputState,
  DictationReadiness,
  DownloadableModel,
  ModelDownloadStatus,
  ModelUseContext,
  OutputMode,
  QuickDictationStatusResponse,
  SettingsPatch,
} from "../types/domain";

type OutputLanguage = Exclude<OutputMode, "original">;
const OUTPUT_LANGUAGES = OUTPUT_MODES.filter((mode) => mode.value !== "original") as {
  value: OutputLanguage;
  label: string;
}[];

/** Space kept free beyond the model itself; matches the backend download check. */
const DISK_HEADROOM_BYTES = 1_000_000_000;

/** The engines offered for a first install, in display order, with one plain
 * sentence each. Slower file-oriented engines stay in Settings. */
const FIRST_RUN_PITCH: Record<string, string> = {
  "live-pair":
    "Words appear while you speak and are pasted the moment you let go. Made for the shortcut.",
  "qwen3-asr-1.7b-bf16":
    "Very accurate in German, English and French, also when you mix them. Takes a couple of seconds after you stop.",
  "ggml-large-v3-turbo-q5_0-bin":
    "The smallest download and works in almost any language, but makes more mistakes.",
};

const CURATED_ORDER = Object.keys(FIRST_RUN_PITCH);

type Step = 0 | 1 | 2 | 3;
const STEP_TITLES = [
  "What Murmela needs",
  "Translate as you speak",
  "Let Murmela listen and type",
  "Try it",
] as const;

export interface FirstRunSetupProps {
  settings: AppSettings | null;
  readiness: DictationReadiness | null;
  quickStatus: QuickDictationStatusResponse | null;
  isPollingAccessibility: boolean;
  onUseModel: (modelId: string) => Promise<void>;
  onRequestMicrophone: () => Promise<void>;
  onOpenMicrophoneSettings: () => Promise<void>;
  onResolveAccessibility: () => Promise<void>;
  onOpenShortcutSettings: () => Promise<void>;
  onSaveSettings: (patch: SettingsPatch) => Promise<void>;
  onFinish: () => Promise<void>;
}

export function FirstRunSetup(props: FirstRunSetupProps) {
  const titleId = useId();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const [step, setStep] = useState<Step>(0);
  const [catalog, setCatalog] = useState<DownloadableModel[] | null>(null);
  const [freeSpace, setFreeSpace] = useState<number | null>(null);
  const [downloads, setDownloads] = useState<Record<string, ModelDownloadStatus>>({});
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  // The model the user committed to; it becomes the shortcut model once installed.
  const [committedId, setCommittedId] = useState<string | null>(null);
  const pendingModel = useRef<string | null>(null);
  // Murmela downloads one model at a time: the rest wait here, in order.
  const [queue, setQueue] = useState<string[]>([]);
  const activeDownload = useRef<string | null>(null);
  // Translation: the languages picked in this setup and the engine that serves them.
  const [languages, setLanguages] = useState<OutputLanguage[]>([]);
  // The answer to "translate what you dictate?"; null until asked.
  const [wantsTranslation, setWantsTranslation] = useState<boolean | null>(null);
  const [translationModelId, setTranslationModelId] = useState<string | null>(null);
  const [recommendCompact, setRecommendCompact] = useState(true);
  const [termsOpen, setTermsOpen] = useState(false);
  const appliedModel = useRef<string | null>(null);
  const onUseModel = useRef(props.onUseModel);
  onUseModel.current = props.onUseModel;

  useEffect(() => {
    headingRef.current?.focus();
  }, [step]);

  useEffect(() => {
    let disposed = false;
    void Promise.all([
      listDownloadableModels(),
      getModelsFreeSpace().catch(() => null),
      getTranslatorStatus().catch(() => null),
    ])
      .then(([models, free, translator]) => {
        if (disposed) return;
        setCatalog(models);
        setFreeSpace(free);
        // Same rule as the Translate screen: 12B only with room to run it.
        const large = models.find(
          (model) => model.capability === "translation" && model.id !== COMPACT_TRANSLATION_MODEL_ID,
        );
        const needed = (large?.sizeBytes ?? 9_660_827_392) + 3_100_000_000;
        setRecommendCompact(!(translator?.memoryAvailableBytes != null && translator.memoryAvailableBytes >= needed));
      })
      .catch((reason) => {
        if (!disposed) setError(messageOf(reason, "Could not load the list of engines."));
      });
    return () => {
      disposed = true;
    };
  }, []);

  const applyModel = useCallback(
    async (modelId: string) => {
      if (appliedModel.current === modelId) return;
      appliedModel.current = modelId;
      try {
        await onUseModel.current(modelId);
      } catch (reason) {
        appliedModel.current = null;
        setError(messageOf(reason, "Could not select the engine."));
      }
    },
    [],
  );

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const accept = (status: ModelDownloadStatus) => {
      if (disposed) return;
      setDownloads((current) => ({ ...current, [status.modelId]: status }));
      if (status.state === "downloading" && activeDownload.current === null) activeDownload.current = status.modelId;
      if (activeDownload.current === status.modelId && status.state !== "downloading") {
        activeDownload.current = null;
        setQueue((current) => [...current]);
      }
      if (status.state === "completed") {
        setCatalog((current) =>
          current?.map((model) =>
            model.id === status.modelId ? { ...model, installed: true } : model,
          ) ?? current,
        );
        if (pendingModel.current === status.modelId) void applyModel(status.modelId);
      }
    };
    void listenModelDownloadStatus(accept)
      .then(async (cleanup) => {
        if (disposed) {
          cleanup();
          return;
        }
        unlisten = cleanup;
        (await getModelDownloadStatuses()).forEach(accept);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [applyModel]);

  const candidates = useMemo(() => {
    if (!catalog) return [];
    return catalog
      .filter(
        (model) =>
          model.capability === "asr" &&
          supports(model, "shortcut_dictation") &&
          (model.installed ||
            (model.availability === "available" &&
              (model.origin ?? "catalog") === "catalog" &&
              model.id in FIRST_RUN_PITCH)),
      )
      .sort((left, right) => {
        const rank = (model: DownloadableModel) => {
          const curated = CURATED_ORDER.indexOf(model.id);
          return curated === -1 ? CURATED_ORDER.length : curated;
        };
        return rank(left) - rank(right) || left.sizeBytes - right.sizeBytes;
      });
  }, [catalog]);

  // An installed engine is used at once. Otherwise the recommended one (first
  // in the curated order) that fits on the disk is preselected, and nothing
  // downloads until the user agrees on the first step. More can be ticked.
  const [engineIds, setEngineIds] = useState<string[]>([]);
  const engineChosen = useRef(false);
  useEffect(() => {
    if (engineChosen.current || catalog === null || !props.readiness) return;
    engineChosen.current = true;
    if (props.readiness.shortcutModelReady) return;
    const installed = candidates.find((entry) => entry.installed);
    if (installed) {
      pendingModel.current = installed.id;
      setCommittedId(installed.id);
      void applyModel(installed.id);
      return;
    }
    const model = candidates.find((entry) => fits(entry, freeSpace));
    if (model) {
      setEngineIds([model.id]);
    } else if (candidates.length > 0) {
      setError("There is not enough free disk space for a dictation engine. Free up some space, then pick one in Settings → Engines.");
    }
  }, [applyModel, candidates, catalog, freeSpace, props.readiness]);
  /** The ticked engines still to download, in the curated order. */
  const chosenEngines = candidates.filter((model) => engineIds.includes(model.id) && !model.installed);
  const chosenBytes = chosenEngines.reduce((total, model) => total + model.sizeBytes, 0);
  const enginesQueued = chosenEngines.some((model) => queue.includes(model.id) || downloads[model.id]?.state === "downloading");

  /** Adds models to the download queue; they start one after the other. */
  function enqueue(ids: string[]) {
    setQueue((current) => [...current, ...ids.filter((id) => !current.includes(id))]);
  }

  // Starts the next queued download whenever nothing is downloading.
  useEffect(() => {
    if (activeDownload.current !== null || queue.length === 0) return;
    const [next, ...rest] = queue;
    activeDownload.current = next;
    setQueue(rest);
    startModelDownload(next)
      .then((status) => setDownloads((current) => ({ ...current, [next]: status })))
      .catch((reason) => {
        activeDownload.current = null;
        if (pendingModel.current === next) pendingModel.current = null;
        setError(messageOf(reason, "A download could not start."));
        setQueue((current) => [...current]);
      });
  }, [queue]);

  function continueWithEngines() {
    setError("");
    if (chosenEngines.length > 0 && !enginesQueued) {
      // The highest ticked engine becomes the dictation engine once it is installed.
      pendingModel.current = chosenEngines[0].id;
      setCommittedId(chosenEngines[0].id);
      enqueue(chosenEngines.map((model) => model.id));
    }
    setStep(1);
  }

  /** Space left once the speech engines that are queued or downloading are on disk.
   * Engines ticked but skipped with "Not now" don't count. */
  const comingBytes = candidates
    .filter((model) => !model.installed && (queue.includes(model.id) || downloads[model.id]?.state === "downloading"))
    .reduce((total, model) => total + model.sizeBytes, 0);
  const spaceAfterEngine = freeSpace === null ? null : freeSpace - comingBytes;

  const translationModels = useMemo(
    () => (catalog ?? []).filter((model) => model.capability === "translation" && model.availability === "available"),
    [catalog],
  );
  // Until the user picks one: an installed engine, else the recommended one if it
  // fits on the disk, else one that does.
  const translationPicked = useRef(false);
  useEffect(() => {
    if (translationPicked.current || translationModels.length === 0) return;
    const installed = translationModels.find((model) => model.installed);
    const recommended = translationModels.find(
      (model) => (model.id === COMPACT_TRANSLATION_MODEL_ID) === recommendCompact,
    );
    const fitting = [recommended, ...translationModels].find((model) => model && fits(model, spaceAfterEngine));
    setTranslationModelId((installed ?? fitting ?? recommended ?? translationModels[0]).id);
  }, [recommendCompact, spaceAfterEngine, translationModels]);
  const translationModel = translationModels.find((model) => model.id === translationModelId) ?? null;
  const translationDownload = translationModelId ? downloads[translationModelId] : undefined;
  const translationComing =
    Boolean(translationModel?.installed) ||
    translationDownload?.state === "downloading" ||
    (translationModelId !== null && queue.includes(translationModelId));
  /** No translation engine fits on the disk: the languages are saved for later instead. */
  const noTranslationRoom =
    translationModels.length > 0 && !translationModels.some((model) => fits(model, spaceAfterEngine));

  const committedModel = candidates.find((model) => model.id === committedId) ?? null;
  const committedDownload = committedId ? downloads[committedId] : undefined;
  const modelInstalled = Boolean(committedModel?.installed) || committedDownload?.state === "completed";


  async function continueWithLanguages() {
    setError("");
    if (languages.length === 0) {
      setStep(2);
      return;
    }
    setBusy(true);
    try {
      await props.onSaveSettings({ translationCycleLanguages: languages });
      const model = translationModel;
      if (model?.installed) {
        await props.onSaveSettings({ translationEnabled: true, translationModelId: model.id });
      } else if (model && !translationComing && !noTranslationRoom) {
        const terms = await getModelTermsStatus(model.id);
        if (terms.required && !terms.accepted) {
          setTermsOpen(true);
          return;
        }
        await startTranslationDownload(model.id);
      }
      setStep(2);
    } catch (reason) {
      setError(messageOf(reason, "Could not set up translation."));
    } finally {
      setBusy(false);
    }
  }

  async function startTranslationDownload(modelId: string) {
    enqueue([modelId]);
  }

  async function finish() {
    setBusy(true);
    try {
      await props.onFinish();
    } catch (reason) {
      setError(messageOf(reason, "Could not finish setup."));
      setBusy(false);
    }
  }

  const activeStatus = Object.values(downloads).find((status) => status.state === "downloading") ?? null;

  return (
    <div className="first-run-backdrop">
      <div className="first-run" role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <header className="first-run-header">
          <img src={murmelaMark} alt="" className="first-run-logo" width={52} height={36} />
          <div className="first-run-heading">
            <p className="first-run-kicker">Welcome to Murmela · Step {step + 1} of {STEP_TITLES.length}</p>
            <h1 id={titleId} ref={headingRef} tabIndex={-1}>
              {STEP_TITLES[step]}
            </h1>
          </div>
          <Button variant="ghost" onClick={() => void finish()} disabled={busy}>
            Skip setup
          </Button>
        </header>
        <ol className="first-run-steps" aria-label="Setup progress">
          {STEP_TITLES.map((title, index) => (
            <li
              key={title}
              className={index < step ? "is-done" : index === step ? "is-current" : undefined}
              aria-current={index === step ? "step" : undefined}
            >
              {title}
            </li>
          ))}
        </ol>

        {step === 0 ? (
          <NeedsStep
            ready={Boolean(props.readiness?.shortcutModelReady) || committedModel?.installed === true}
            loading={catalog === null}
            candidates={candidates}
            chosenIds={engineIds}
            onToggle={(id) =>
              setEngineIds((current) => (current.includes(id) ? current.filter((entry) => entry !== id) : [...current, id]))
            }
            locked={enginesQueued}
            freeSpace={freeSpace}
            downloads={downloads}
          />
        ) : step === 1 && wantsTranslation !== true ? (
          <TranslationQuestion
            onAnswer={(yes) => {
              setWantsTranslation(yes);
              if (!yes) {
                setLanguages([]);
                setStep(2);
              }
            }}
          />
        ) : step === 1 ? (
          <LanguagesStep
            languages={languages}
            onChangeLanguages={setLanguages}
            models={translationModels}
            chosenId={translationModelId}
            onChooseModel={(id) => {
              translationPicked.current = true;
              setTranslationModelId(id);
            }}
            recommendCompact={recommendCompact}
            freeSpace={spaceAfterEngine}
            downloads={downloads}
            noRoom={noTranslationRoom}
            needsDownload={Boolean(translationModel) && !translationComing}
          />
        ) : step === 2 ? (
          <PermissionsStep
            readiness={props.readiness}
            isPollingAccessibility={props.isPollingAccessibility}
            onRequestMicrophone={props.onRequestMicrophone}
            onOpenMicrophoneSettings={props.onOpenMicrophoneSettings}
            onResolveAccessibility={props.onResolveAccessibility}
          />
        ) : (
          <TryStep
            settings={props.settings}
            readiness={props.readiness}
            quickStatus={props.quickStatus}
            model={committedModel}
            download={committedDownload}
            modelInstalled={modelInstalled}
            translating={languages.length > 0 || Boolean(props.settings?.translationEnabled)}
            translationDownload={translationDownload}
            onOpenShortcutSettings={props.onOpenShortcutSettings}
          />
        )}

        {termsOpen && translationModel ? (
          <GemmaTermsDialog
            onCancel={() => setTermsOpen(false)}
            onAccept={async () => {
              await acceptModelTerms(translationModel.id);
              setTermsOpen(false);
              await startTranslationDownload(translationModel.id);
              setStep(2);
            }}
          />
        ) : null}

        {step > 0 && step < 3 && activeStatus ? (
          <DownloadLine
            model={catalog?.find((model) => model.id === activeStatus.modelId) ?? null}
            status={activeStatus}
            waiting={queue.length}
          />
        ) : null}
        {translationDownload?.state === "failed" ? (
          <p className="error-text" role="alert">
            {translationModel?.modelName ?? "The translation engine"} could not be downloaded:{" "}
            {readableError(translationDownload.errorMessage ?? "unknown error")}
          </p>
        ) : null}
        {committedDownload?.state === "failed" ? (
          <p className="error-text" role="alert">
            {committedModel ? getModelPresentation(committedModel).friendlyName : "The engine"} could not be
            downloaded: {readableError(committedDownload.errorMessage ?? "unknown error")}
          </p>
        ) : null}
        {error ? (
          <p className="error-text" role="alert">
            {readableError(error)}
          </p>
        ) : null}

        <footer className="first-run-footer">
          {step > 0 ? (
            <Button
              variant="ghost"
              onClick={() =>
                // From the language setup, Back returns to the translate question.
                step === 1 && wantsTranslation ? setWantsTranslation(null) : setStep((step - 1) as Step)
              }
              disabled={busy}
            >
              Back
            </Button>
          ) : (
            <span />
          )}
          {step === 0 ? (
            <span className="first-run-footer-actions">
              {chosenEngines.length > 0 && !enginesQueued ? (
                <Button variant="ghost" onClick={() => setStep(1)} disabled={busy}>
                  Not now
                </Button>
              ) : null}
              <Button
                variant="primary"
                busy={busy}
                disabled={!fitsBytes(chosenBytes, freeSpace)}
                onClick={continueWithEngines}
              >
                {chosenEngines.length > 0 && !enginesQueued
                  ? `Download ${formatBytes(chosenBytes)} and continue`
                  : "Continue"}
              </Button>
            </span>
          ) : step === 1 && wantsTranslation !== true ? (
            <Button variant="ghost" onClick={() => setStep(2)}>
              Skip this step
            </Button>
          ) : step === 1 ? (
            <Button
              variant="primary"
              busy={busy}
              disabled={
                languages.length > 0 &&
                translationModel !== null &&
                !translationComing &&
                !noTranslationRoom &&
                !fits(translationModel, spaceAfterEngine)
              }
              onClick={() => void continueWithLanguages()}
            >
              {languages.length === 0
                ? "Skip this step"
                : !translationModel || translationComing
                  ? "Continue"
                  : noTranslationRoom
                    ? "Save and continue"
                    : `Download ${formatBytes(translationModel.sizeBytes)} and continue`}
            </Button>
          ) : step === 2 ? (
            <Button variant="primary" onClick={() => setStep(3)}>
              Continue
            </Button>
          ) : (
            <Button variant="primary" busy={busy} onClick={() => void finish()}>
              {props.readiness?.shortcutModelReady ? "Start talking" : "Finish setup"}
            </Button>
          )}
        </footer>
      </div>
    </div>
  );
}

function NeedsStep({
  ready,
  loading,
  candidates,
  chosenIds,
  onToggle,
  locked,
  freeSpace,
  downloads,
}: {
  ready: boolean;
  loading: boolean;
  candidates: DownloadableModel[];
  chosenIds: string[];
  onToggle: (modelId: string) => void;
  /** The chosen engines are already queued; the choice can't change any more. */
  locked: boolean;
  freeSpace: number | null;
  downloads: Record<string, ModelDownloadStatus>;
}) {
  const offered = candidates.filter((model) => !model.installed);
  const recommendedId = offered.find((model) => fits(model, freeSpace))?.id;
  const chosenBytes = offered
    .filter((model) => chosenIds.includes(model.id))
    .reduce((total, model) => total + model.sizeBytes, 0);
  return (
    <div className="first-run-body">
      <p className="first-run-lead">
        Murmela turns your speech into text on this Mac. Your voice is never sent anywhere. To work, it needs:
      </p>
      <ul className="first-run-needs">
        <li>
          <strong>A speech engine</strong>
          <span>
            {ready
              ? "Already installed."
              : "The part that understands what you say. It is downloaded once from Hugging Face and stored on this Mac; after that, Murmela works offline."}
          </span>
        </li>
        <li>
          <strong>Microphone access</strong>
          <span>So Murmela can hear you. macOS asks in step 3.</span>
        </li>
        <li>
          <strong>Accessibility access</strong>
          <span>So Murmela can paste where your cursor is and notice your dictation key. Also in step 3.</span>
        </li>
      </ul>
      {ready ? null : loading ? (
        <p className="first-run-note">Looking up the speech engines…</p>
      ) : offered.length > 0 ? (
        <>
          <p className="first-run-note">
            Which speech engines should Murmela download? Tick one or more; they download one after the other.
            With several, dictation uses the highest one in this list.
            {freeSpace !== null ? ` ${formatBytes(freeSpace)} free on this Mac.` : ""} You can add or swap engines
            any time in Settings → Engines.
          </p>
          <div className="first-run-engines" role="group" aria-label="Speech engines to download">
            {offered.map((model) => {
              const chosen = chosenIds.includes(model.id);
              // An unticked engine must fit next to the ones already ticked.
              const roomy = chosen || fitsBytes(chosenBytes + model.sizeBytes, freeSpace);
              const status = downloads[model.id];
              const forDictation = offered.find((entry) => chosenIds.includes(entry.id))?.id === model.id;
              return (
                <label
                  key={model.id}
                  className={[
                    "first-run-engine",
                    chosen ? "is-selected" : "",
                    roomy && !locked ? "" : "is-disabled",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                >
                  <input
                    type="checkbox"
                    value={model.id}
                    aria-label={`${getModelPresentation(model).friendlyName}, ${formatBytes(model.sizeBytes)}`}
                    checked={chosen}
                    disabled={!roomy || locked}
                    onChange={() => onToggle(model.id)}
                  />
                  <span className="first-run-engine-body">
                    <strong>
                      {getModelPresentation(model).friendlyName}
                      {model.id === recommendedId ? (
                        <span className="status-pill status-pill-success">Recommended</span>
                      ) : null}
                      {forDictation && chosenIds.length > 1 ? (
                        <span className="status-pill">Used for dictation</span>
                      ) : null}
                    </strong>
                    {FIRST_RUN_PITCH[model.id] ? (
                      <span className="first-run-engine-pitch">{FIRST_RUN_PITCH[model.id]}</span>
                    ) : null}
                    <span className="first-run-engine-facts">
                      <span>{formatBytes(model.sizeBytes)} download</span>
                    </span>
                    {!roomy ? (
                      <span className="notice-text">Not enough free disk space for this engine as well.</span>
                    ) : null}
                    {status?.state === "downloading" ? (
                      <Progress value={status.progressPercent} label={`Downloading ${model.modelName}`} />
                    ) : null}
                  </span>
                </label>
              );
            })}
          </div>
        </>
      ) : null}
    </div>
  );
}

function TranslationQuestion({ onAnswer }: { onAnswer: (yes: boolean) => void }) {
  return (
    <div className="first-run-body">
      <p className="first-run-question">Do you want Murmela to translate what you dictate?</p>
      <p className="first-run-lead">
        For example, speak German and have English text appear. Not sure? Choose No; you can turn translation on
        later in Settings → Languages.
      </p>
      <div className="first-run-choices">
        <button type="button" className="first-run-choice" onClick={() => onAnswer(false)}>
          <strong>No, just dictate</strong>
          <span>Text appears in the language you speak.</span>
        </button>
        <button type="button" className="first-run-choice" onClick={() => onAnswer(true)}>
          <strong>Yes, translate too</strong>
          <span>Choose languages and download a translation engine.</span>
        </button>
      </div>
    </div>
  );
}

function LanguagesStep({
  languages,
  onChangeLanguages,
  models,
  chosenId,
  onChooseModel,
  recommendCompact,
  freeSpace,
  downloads,
  noRoom,
  needsDownload,
}: {
  languages: OutputLanguage[];
  onChangeLanguages: (languages: OutputLanguage[]) => void;
  models: DownloadableModel[];
  chosenId: string | null;
  onChooseModel: (modelId: string) => void;
  recommendCompact: boolean;
  freeSpace: number | null;
  downloads: Record<string, ModelDownloadStatus>;
  /** No translation engine fits on the disk. */
  noRoom: boolean;
  /** The chosen engine still has to be downloaded. */
  needsDownload: boolean;
}) {
  const groupName = useId();
  const toggle = (code: OutputLanguage) =>
    onChangeLanguages(
      // Kept in shortcut order, like the setting.
      OUTPUT_LANGUAGES.map((mode) => mode.value).filter((value) =>
        value === code ? !languages.includes(code) : languages.includes(value),
      ),
    );
  return (
    <div className="first-run-body">
      <p className="first-run-lead">
        You speak as usual, and Murmela pastes the text in the language you choose. Translation runs on this Mac,
        like dictation, so nothing is sent anywhere. It needs a second engine, downloaded once.
      </p>
      <p className="first-run-lead">
        Pick the languages you want to translate into. While dictating, a key switches between them and back to
        the language you spoke. You can change this any time in Settings → Languages.
      </p>
      <fieldset className="first-run-languages">
        <legend className="sr-only">Translate into</legend>
        {OUTPUT_LANGUAGES.map((mode) => (
          <label key={mode.value} className={languages.includes(mode.value) ? "is-selected" : undefined}>
            <input type="checkbox" checked={languages.includes(mode.value)} onChange={() => toggle(mode.value)} />
            <span>{mode.label}</span>
          </label>
        ))}
      </fieldset>
      {languages.length > 0 && models.length > 0 ? (
        <>
          <p className="first-run-note">Translation needs its own engine, TranslateGemma by Google:</p>
          <div className="first-run-engines" role="radiogroup" aria-label="Translation engine">
            {models.map((model) => {
              const compact = model.id === COMPACT_TRANSLATION_MODEL_ID;
              const roomy = fits(model, freeSpace);
              const status = downloads[model.id];
              return (
                <label
                  key={model.id}
                  className={[
                    "first-run-engine",
                    chosenId === model.id ? "is-selected" : "",
                    roomy ? "" : "is-disabled",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                >
                  <input
                    type="radio"
                    name={groupName}
                    value={model.id}
                    checked={chosenId === model.id}
                    disabled={!roomy}
                    onChange={() => onChooseModel(model.id)}
                  />
                  <span className="first-run-engine-body">
                    <strong>
                      {compact ? "Compact" : "Best quality"} · {model.modelName}
                      {compact === recommendCompact ? (
                        <span className="status-pill status-pill-success">Recommended for this Mac</span>
                      ) : null}
                    </strong>
                    <span className="first-run-engine-facts">
                      <span>{model.installed ? "Installed" : `${formatBytes(model.sizeBytes)} download`}</span>
                      {model.requirements ? <span>{model.requirements}</span> : null}
                    </span>
                    {!roomy ? (
                      <span className="notice-text">Not enough free disk space for this engine.</span>
                    ) : null}
                    {status?.state === "downloading" ? (
                      <Progress value={status.progressPercent} label={`Downloading ${model.modelName}`} />
                    ) : null}
                  </span>
                </label>
              );
            })}
          </div>
          {noRoom ? (
            <p className="notice-text" role="status">
              There isn't enough free disk space for a translation engine right now. Your languages are saved; free
              up some space and download an engine later in Settings → Languages.
            </p>
          ) : needsDownload ? (
            <p className="first-run-note">
              Before the download starts, you'll be asked to accept Google's Gemma Terms of Use.
            </p>
          ) : null}
        </>
      ) : languages.length > 0 ? (
        <p className="first-run-note">Translation needs an Apple Silicon Mac. Your choice is saved for later.</p>
      ) : null}
    </div>
  );
}

function PermissionsStep({
  readiness,
  isPollingAccessibility,
  onRequestMicrophone,
  onOpenMicrophoneSettings,
  onResolveAccessibility,
}: {
  readiness: DictationReadiness | null;
  isPollingAccessibility: boolean;
  onRequestMicrophone: () => Promise<void>;
  onOpenMicrophoneSettings: () => Promise<void>;
  onResolveAccessibility: () => Promise<void>;
}) {
  const microphone = readiness?.microphone ?? "granted";
  const accessibilityNeeded = Boolean(readiness?.accessibilityRequired);
  const accessibilityGranted = Boolean(readiness?.accessibilityGranted);
  return (
    <div className="first-run-body">
      <p className="first-run-lead">
        Murmela needs your permission for two things. Your recordings never leave this computer.
      </p>
      <ul className="first-run-checklist">
        <li className={microphone === "granted" ? "is-done" : undefined}>
          <CheckMark done={microphone === "granted"} />
          <span className="first-run-check-text">
            <strong>Microphone</strong>
            <span>
              {microphone === "granted"
                ? "Allowed. Murmela can hear you."
                : microphone === "not_determined"
                  ? "So Murmela can hear what you dictate."
                  : "Blocked. Turn on Murmela under Privacy & Security → Microphone, then come back."}
            </span>
          </span>
          {microphone === "not_determined" ? (
            <Button variant="secondary" onClick={() => void onRequestMicrophone()}>
              Allow microphone
            </Button>
          ) : microphone !== "granted" ? (
            <Button variant="secondary" onClick={() => void onOpenMicrophoneSettings()}>
              Open System Settings
            </Button>
          ) : null}
        </li>
        {accessibilityNeeded ? (
          <li className={accessibilityGranted ? "is-done" : undefined}>
            <CheckMark done={accessibilityGranted} />
            <span className="first-run-check-text">
              <strong>Paste into other apps</strong>
              <span>
                {accessibilityGranted
                  ? "Allowed. Text lands where your cursor is."
                  : "Lets Murmela paste where your cursor is and notice your dictation key. Without it, text is copied and you paste it yourself."}
              </span>
            </span>
            {!accessibilityGranted ? (
              <Button variant="secondary" onClick={() => void onResolveAccessibility()}>
                {isPollingAccessibility ? "Check again" : "Allow pasting"}
              </Button>
            ) : null}
          </li>
        ) : null}
      </ul>
    </div>
  );
}

function TryStep({
  settings,
  readiness,
  quickStatus,
  model,
  download,
  modelInstalled,
  translating,
  translationDownload,
  onOpenShortcutSettings,
}: {
  settings: AppSettings | null;
  readiness: DictationReadiness | null;
  quickStatus: QuickDictationStatusResponse | null;
  model: DownloadableModel | null;
  download: ModelDownloadStatus | undefined;
  modelInstalled: boolean;
  translating: boolean;
  translationDownload: ModelDownloadStatus | undefined;
  onOpenShortcutSettings: () => Promise<void>;
}) {
  // Only dictations finished after this step opened count as the test.
  const baseline = useRef({ id: quickStatus?.lastTranscriptId ?? null, text: quickStatus?.lastTranscriptText ?? null });
  const [heard, setHeard] = useState(false);
  // The last practice dictation; stays once the status moves on to idle.
  const [heardText, setHeardText] = useState<string | null>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  const [output, setOutput] = useState<DictationOutputState | null>(null);
  const baselineMode = useRef<OutputMode | null>(null);
  const [switched, setSwitched] = useState(false);
  const shortcutValue = quickStatus?.registeredShortcut ?? settings?.shortcut ?? FN_KEY_SHORTCUT;
  const shortcut = formatShortcutForDisplay(shortcutValue);
  const oneKey = isOneKeyShortcut(shortcutValue);
  const hold = (quickStatus?.shortcutMode ?? settings?.shortcutMode) !== "toggle";
  const finished = heardText !== null;
  const failed = quickStatus?.state === "error" && quickStatus.lastErrorMessage;
  const working = quickStatus?.state === "listening" || quickStatus?.state === "processing";

  useEffect(() => {
    if (quickStatus?.state === "listening") setHeard(true);
  }, [quickStatus?.state]);

  const doneText = quickStatus?.lastTranscriptText?.trim() ?? "";
  const doneState = quickStatus?.state === "inserted" || quickStatus?.state === "clipboard_only";
  const isNew =
    quickStatus?.lastTranscriptId !== baseline.current.id || quickStatus?.lastTranscriptText !== baseline.current.text;
  useEffect(() => {
    if (!doneState || !isNew || !doneText) return;
    baseline.current = { id: quickStatus?.lastTranscriptId ?? null, text: quickStatus?.lastTranscriptText ?? null };
    setHeard(true);
    setHeardText(doneText);
    // The paste lands in this box when Murmela is in front; if it did not
    // (focus elsewhere, no paste permission), put the words there directly.
    const field = box.current;
    if (field && !field.value.includes(doneText)) {
      field.value = field.value.trim() ? `${field.value.trimEnd()} ${doneText}` : doneText;
    }
  }, [doneState, doneText, isNew, quickStatus?.lastTranscriptId, quickStatus?.lastTranscriptText]);

  // The language switch is done once the output language moves away from where it started.
  useEffect(() => {
    if (!translating) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const accept = (state: DictationOutputState) => {
      if (disposed) return;
      setOutput(state);
      if (baselineMode.current === null) baselineMode.current = state.outputMode;
      else if (state.outputMode !== baselineMode.current) setSwitched(true);
    };
    void getDictationOutputState().then(accept).catch(() => undefined);
    void listenDictationOutput(accept)
      .then((cleanup) => (disposed ? cleanup() : (unlisten = cleanup)))
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [translating]);

  if (model && !modelInstalled) {
    return (
      <div className="first-run-body">
        <p className="first-run-lead">
          Almost there. {getModelPresentation(model).friendlyName} is still downloading; you can try it as soon as
          it is ready.
        </p>
        {download?.state === "downloading" ? <DownloadLine model={model} status={download} /> : null}
      </div>
    );
  }
  if (!model && readiness && !readiness.shortcutModelReady) {
    return (
      <div className="first-run-body">
        <p className="first-run-lead">
          Murmela needs a speech engine before you can dictate. Go back to the first step to download one, or pick
          one later in Settings → Engines.
        </p>
      </div>
    );
  }
  if (readiness && !readiness.shortcutRegistered) {
    return (
      <div className="first-run-body">
        <p className="first-run-lead">
          The dictation shortcut is not set up yet. Choose one in Settings, then dictate from any app.
        </p>
        <Button variant="secondary" onClick={() => void onOpenShortcutSettings()}>
          Set a shortcut
        </Button>
      </div>
    );
  }
  const key = <kbd className="first-run-kbd">{shortcut}</kbd>;
  const languageKey = oneKey ? (
    <>
      hold {key} and press <kbd className="first-run-kbd">→</kbd>
    </>
  ) : (
    <>
      press <kbd className="first-run-kbd">{formatShortcutForDisplay(settings?.translationCycleShortcut ?? "Ctrl+Alt+L")}</kbd>
    </>
  );
  const translationReady = Boolean(output?.ready);
  const currentLanguage = OUTPUT_MODES.find((mode) => mode.value === output?.outputMode)?.label ?? "Original";
  return (
    <div className="first-run-body">
      <p className="first-run-lead">
        Click in the box below and follow the steps. It works the same in any app.
      </p>
      {shortcutValue === "Fn" ? <GlobeKeyNotice /> : null}
      <ol className="first-run-practice" aria-label="Practice">
        <PracticeItem done={heard || Boolean(finished)} active={!heard && !finished}>
          {hold || oneKey ? <>Hold {key} and say a sentence.</> : <>Press {key} and say a sentence.</>}
        </PracticeItem>
        <PracticeItem done={Boolean(finished)} active={heard && !finished}>
          {hold || oneKey ? "Let go. Your words appear where the cursor is." : <>Press {key} again. Your words appear where the cursor is.</>}
        </PracticeItem>
        {translating ? (
          <PracticeItem done={switched} active={Boolean(finished) && !switched && translationReady}>
            {translationReady ? (
              <>
                To switch language, {languageKey}. Now: <strong>{currentLanguage}</strong>.
                {oneKey ? " Keep holding and speak to dictate in it." : " Then dictate as before."}
              </>
            ) : (
              <>
                Switching language works once the translation engine is ready
                {translationDownload?.state === "downloading" && translationDownload.progressPercent != null
                  ? ` (downloading, ${Math.round(translationDownload.progressPercent)} %)`
                  : ""}
                .
              </>
            )}
          </PracticeItem>
        ) : null}
      </ol>
      <textarea
        ref={box}
        className="first-run-try"
        rows={3}
        placeholder="Your words will appear here…"
        aria-label="Try dictation here"
      />
      <p className={finished ? "status-text is-success" : failed ? "error-text" : "first-run-note"} role="status">
        {finished
          ? `It works! Murmela heard: “${heardText}”`
          : failed
            ? readableError(quickStatus?.lastErrorMessage ?? "")
            : working
              ? quickStatus?.state === "listening"
                ? "Listening…"
                : "Turning speech into text…"
              : "Waiting for your first words."}
      </p>
      <p className="first-run-note">
        {oneKey ? <>For longer dictations, double-tap {key} and talk hands-free; tap it once more to finish. </> : null}
        <kbd className="first-run-kbd">Esc</kbd> cancels a dictation.
      </p>
    </div>
  );
}

function PracticeItem({ done, active, children }: { done: boolean; active: boolean; children: ReactNode }) {
  return (
    <li className={done ? "is-done" : active ? "is-active" : undefined}>
      <CheckMark done={done} />
      <span>{children}</span>
      <span className="sr-only">{done ? " Done." : ""}</span>
    </li>
  );
}

function DownloadLine({
  model,
  status,
  waiting = 0,
}: {
  model: DownloadableModel | null;
  status: ModelDownloadStatus;
  /** Downloads queued after this one. */
  waiting?: number;
}) {
  const name = model ? getModelPresentation(model).friendlyName : status.modelName;
  return (
    <div className="first-run-download">
      <span>
        Downloading {name}
        {status.progressPercent != null ? ` · ${Math.round(status.progressPercent)} %` : "…"}
        {waiting > 0 ? ` · ${waiting} more after this` : ""}
      </span>
      <Progress value={status.progressPercent} label={`Downloading ${name}`} />
      <Button variant="ghost" onClick={() => void cancelModelDownload(status.modelId).catch(() => undefined)}>
        Cancel download
      </Button>
    </div>
  );
}

function CheckMark({ done }: { done: boolean }) {
  return (
    <span className={done ? "first-run-check is-done" : "first-run-check"} aria-hidden="true">
      {done ? <AppIcon name="check" /> : null}
    </span>
  );
}

function supports(model: DownloadableModel, context: ModelUseContext) {
  return !model.capabilities || model.capabilities.supportedContexts.includes(context);
}

function fitsBytes(bytes: number, freeSpace: number | null) {
  return freeSpace === null || bytes === 0 || bytes + DISK_HEADROOM_BYTES <= freeSpace;
}

function fits(model: DownloadableModel | undefined, freeSpace: number | null) {
  if (!model) return false;
  if (model.installed || freeSpace === null) return true;
  return model.sizeBytes + DISK_HEADROOM_BYTES <= freeSpace;
}

function messageOf(reason: unknown, fallback: string) {
  return describeError(reason, fallback);
}
