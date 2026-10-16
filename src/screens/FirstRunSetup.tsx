import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import murmelaMark from "../assets/murmela-mark.png";
import { Button, Progress } from "../components/Feedback";
import { AppIcon } from "../components/IconButton";
import { ModelSummary } from "../components/ModelPicker";
import {
  cancelModelDownload,
  getModelDownloadStatuses,
  getModelsFreeSpace,
  listDownloadableModels,
  listenModelDownloadStatus,
  startModelDownload,
} from "../lib/api";
import { describeError, FN_KEY_SHORTCUT, formatBytes, formatShortcutForDisplay, isOneKeyShortcut, readableError } from "../lib/formatting";
import { GlobeKeyNotice } from "../components/GlobeKeyNotice";
import { formatWorkflows, getModelPresentation, isModelRecommended } from "../lib/modelPresentation";
import type {
  AppSettings,
  DictationReadiness,
  DownloadableModel,
  ModelDownloadStatus,
  ModelUseContext,
  QuickDictationStatusResponse,
} from "../types/domain";

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

type Step = 0 | 1 | 2;
const STEP_TITLES = ["Choose your engine", "Let Murmela listen and type", "Try it"] as const;

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
  onFinish: () => Promise<void>;
}

export function FirstRunSetup(props: FirstRunSetupProps) {
  const titleId = useId();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const [step, setStep] = useState<Step>(0);
  const [catalog, setCatalog] = useState<DownloadableModel[] | null>(null);
  const [freeSpace, setFreeSpace] = useState<number | null>(null);
  const [chosenId, setChosenId] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<Record<string, ModelDownloadStatus>>({});
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  // The model the user committed to; it becomes the shortcut model once installed.
  const [committedId, setCommittedId] = useState<string | null>(null);
  const pendingModel = useRef<string | null>(null);
  const appliedModel = useRef<string | null>(null);
  const onUseModel = useRef(props.onUseModel);
  onUseModel.current = props.onUseModel;

  useEffect(() => {
    headingRef.current?.focus();
  }, [step]);

  useEffect(() => {
    let disposed = false;
    void Promise.all([listDownloadableModels(), getModelsFreeSpace().catch(() => null)])
      .then(([models, free]) => {
        if (disposed) return;
        setCatalog(models);
        setFreeSpace(free);
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

  useEffect(() => {
    if (chosenId || candidates.length === 0) return;
    const firstFitting = candidates.find((model) => fits(model, freeSpace)) ?? candidates[0];
    setChosenId(firstFitting.id);
  }, [candidates, chosenId, freeSpace]);

  const committedModel = candidates.find((model) => model.id === committedId) ?? null;
  const committedDownload = committedId ? downloads[committedId] : undefined;
  const modelInstalled = Boolean(committedModel?.installed) || committedDownload?.state === "completed";

  async function continueWithModel() {
    const model = candidates.find((entry) => entry.id === chosenId);
    if (!model) return;
    setError("");
    setBusy(true);
    try {
      pendingModel.current = model.id;
      setCommittedId(model.id);
      if (model.installed) {
        await applyModel(model.id);
      } else if (downloads[model.id]?.state !== "downloading") {
        const status = await startModelDownload(model.id);
        setDownloads((current) => ({ ...current, [model.id]: status }));
      }
      setStep(1);
    } catch (reason) {
      pendingModel.current = null;
      setCommittedId(null);
      setError(messageOf(reason, "The download could not start."));
    } finally {
      setBusy(false);
    }
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

  return (
    <div className="first-run-backdrop">
      <div className="first-run" role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <header className="first-run-header">
          <img src={murmelaMark} alt="" className="first-run-logo" width={52} height={36} />
          <div className="first-run-heading">
            <p className="first-run-kicker">Welcome to Murmela · Step {step + 1} of 3</p>
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
          <EngineStep
            candidates={candidates}
            loading={catalog === null}
            freeSpace={freeSpace}
            chosenId={chosenId}
            downloads={downloads}
            onChoose={setChosenId}
          />
        ) : step === 1 ? (
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
            onOpenShortcutSettings={props.onOpenShortcutSettings}
          />
        )}

        {step === 1 && committedDownload?.state === "downloading" ? (
          <DownloadLine model={committedModel} status={committedDownload} />
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
            <Button variant="ghost" onClick={() => setStep((step - 1) as Step)} disabled={busy}>
              Back
            </Button>
          ) : (
            <span />
          )}
          {step === 0 ? (
            <Button
              variant="primary"
              busy={busy}
              disabled={!chosenId || !fits(candidates.find((model) => model.id === chosenId), freeSpace)}
              onClick={() => void continueWithModel()}
            >
              {candidates.find((model) => model.id === chosenId)?.installed ? "Use this engine" : "Download and continue"}
            </Button>
          ) : step === 1 ? (
            <Button variant="primary" onClick={() => setStep(2)}>
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

function EngineStep({
  candidates,
  loading,
  freeSpace,
  chosenId,
  downloads,
  onChoose,
}: {
  candidates: DownloadableModel[];
  loading: boolean;
  freeSpace: number | null;
  chosenId: string | null;
  downloads: Record<string, ModelDownloadStatus>;
  onChoose: (modelId: string) => void;
}) {
  const groupName = useId();
  if (loading) return <p className="first-run-lead">Looking for engines that run on this computer…</p>;
  if (candidates.length === 0) {
    return (
      <p className="first-run-lead">
        No dictation engine is available for this computer. You can still add one by hand later in Settings → Engines.
      </p>
    );
  }
  return (
    <div className="first-run-body">
      <p className="first-run-lead">
        Murmela turns speech into text on this computer; nothing is sent anywhere. Pick the engine that does the
        listening. You can add or switch engines later in Settings.
      </p>
      <div className="first-run-engines" role="radiogroup" aria-label="Dictation engine">
        {candidates.map((model) => {
          const presentation = getModelPresentation(model);
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
                onChange={() => onChoose(model.id)}
              />
              <span className="first-run-engine-body">
                <ModelSummary
                  presentation={presentation}
                  recommendation={
                    isModelRecommended(presentation, "shortcut_dictation") ? "Recommended" : null
                  }
                />
                <span className="first-run-engine-pitch">
                  {FIRST_RUN_PITCH[presentation.id] ?? presentation.description}
                </span>
                <span className="first-run-engine-facts">
                  <span>{model.installed ? "Installed" : `${formatBytes(model.sizeBytes)} download`}</span>
                  <span>Works for: {formatWorkflows(model.capabilities)}</span>
                </span>
                {!roomy ? (
                  <span className="notice-text">
                    Not enough free disk space ({formatBytes(freeSpace ?? 0)} free).
                  </span>
                ) : null}
                {status?.state === "downloading" ? (
                  <Progress value={status.progressPercent} label={`Downloading ${presentation.friendlyName}`} />
                ) : null}
              </span>
            </label>
          );
        })}
      </div>
      {freeSpace !== null ? (
        <p className="first-run-note">{formatBytes(freeSpace)} free on this disk.</p>
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
  onOpenShortcutSettings,
}: {
  settings: AppSettings | null;
  readiness: DictationReadiness | null;
  quickStatus: QuickDictationStatusResponse | null;
  model: DownloadableModel | null;
  download: ModelDownloadStatus | undefined;
  modelInstalled: boolean;
  onOpenShortcutSettings: () => Promise<void>;
}) {
  // Only dictations finished after this step opened count as the test.
  const baseline = useRef(quickStatus?.lastTranscriptId ?? null);
  const shortcutValue = quickStatus?.registeredShortcut ?? settings?.shortcut ?? FN_KEY_SHORTCUT;
  const shortcut = formatShortcutForDisplay(shortcutValue);
  const oneKey = isOneKeyShortcut(shortcutValue);
  const hold = (quickStatus?.shortcutMode ?? settings?.shortcutMode) !== "toggle";
  const finished =
    quickStatus &&
    (quickStatus.state === "inserted" || quickStatus.state === "clipboard_only") &&
    quickStatus.lastTranscriptId !== baseline.current &&
    Boolean(quickStatus.lastTranscriptText?.trim());
  const failed = quickStatus?.state === "error" && quickStatus.lastErrorMessage;
  const working = quickStatus?.state === "listening" || quickStatus?.state === "processing";

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
  return (
    <div className="first-run-body">
      <p className="first-run-lead">
        Click in the box below, {hold || oneKey ? "hold" : "press"} <kbd className="first-run-kbd">{shortcut}</kbd>
        {hold || oneKey ? ", say something and let go." : ", say something and press it again."} This works in any app.
        {oneKey ? " For longer dictations, double-tap it and talk hands-free." : null}
      </p>
      {shortcutValue === "Fn" ? <GlobeKeyNotice /> : null}
      <textarea
        className="first-run-try"
        rows={3}
        placeholder="Your words will appear here…"
        aria-label="Try dictation here"
      />
      <p className={finished ? "status-text is-success" : failed ? "error-text" : "first-run-note"} role="status">
        {finished
          ? `It works! Murmela heard: “${quickStatus?.lastTranscriptText?.trim()}”`
          : failed
            ? readableError(quickStatus?.lastErrorMessage ?? "")
            : working
              ? quickStatus?.state === "listening"
                ? "Listening…"
                : "Turning speech into text…"
              : "Waiting for your first words."}
      </p>
    </div>
  );
}

function DownloadLine({ model, status }: { model: DownloadableModel | null; status: ModelDownloadStatus }) {
  const name = model ? getModelPresentation(model).friendlyName : status.modelName;
  return (
    <div className="first-run-download">
      <span>
        Downloading {name}
        {status.progressPercent != null ? ` · ${Math.round(status.progressPercent)} %` : "…"}
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

function fits(model: DownloadableModel | undefined, freeSpace: number | null) {
  if (!model) return false;
  if (model.installed || freeSpace === null) return true;
  return model.sizeBytes + DISK_HEADROOM_BYTES <= freeSpace;
}

function messageOf(reason: unknown, fallback: string) {
  return describeError(reason, fallback);
}
