import { useEffect, useState } from "react";
import murmelaMark from "../assets/murmela-mark.png";
import {
  getHealthCheck,
  listLicenceDocuments,
  readLicenceDocument,
  type LicenceDocument,
} from "../lib/api";
import { describeError } from "../lib/formatting";
import { GEMMA_NOTICE } from "./GemmaTermsDialog";
import { UpdatesCard } from "./UpdatesCard";

interface ModelCredit {
  name: string;
  use: string;
  maker: string;
  licence: string;
  url: string;
  note?: string;
}

/** Credits for every model Murmela can download; the licence files ship with the app. */
const MODEL_CREDITS: ModelCredit[] = [
  {
    name: "Nemotron 3.5 ASR Streaming 0.6B",
    use: "Live dictation · live text",
    maker: "NVIDIA",
    licence: "OpenMDW-1.1",
    url: "https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b",
    note: "CoreML conversion by FluidInference.",
  },
  {
    name: "Parakeet TDT 0.6B v3",
    use: "Live dictation · final pass",
    maker: "NVIDIA",
    licence: "CC BY 4.0",
    url: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
    note: "Converted to CoreML by FluidInference; used unchanged. NVIDIA does not endorse Murmela.",
  },
  {
    name: "Qwen3-ASR 1.7B",
    use: "Dictation and files",
    maker: "Qwen team, Alibaba Cloud",
    licence: "Apache-2.0",
    url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B",
  },
  {
    name: "MOSS Transcribe-Diarize 0.9B",
    use: "Files with speakers",
    maker: "OpenMOSS / MOSI.AI",
    licence: "Apache-2.0",
    url: "https://huggingface.co/OpenMOSS-Team/MOSS-Transcribe-Diarize",
  },
  {
    name: "VibeVoice-ASR",
    use: "Files with speakers · experimental",
    maker: "Microsoft",
    licence: "MIT",
    url: "https://huggingface.co/microsoft/VibeVoice-ASR",
    note: "MLX conversion by mlx-community. Microsoft publishes it for research and development.",
  },
  {
    name: "Whisper",
    use: "Dictation and files",
    maker: "OpenAI",
    licence: "MIT",
    url: "https://github.com/openai/whisper",
  },
  {
    name: "TranslateGemma 12B",
    use: "Translation",
    maker: "Google",
    licence: "Gemma Terms of Use",
    url: "https://ai.google.dev/gemma/terms",
    note: `${GEMMA_NOTICE}.`,
  },
  {
    name: "pyannote segmentation 3.0",
    use: "Speaker identification",
    maker: "pyannote",
    licence: "MIT",
    url: "https://huggingface.co/pyannote/segmentation-3.0",
  },
  {
    name: "ERes2Net (3D-Speaker)",
    use: "Speaker identification",
    maker: "Tongyi Lab, Alibaba",
    licence: "Apache-2.0",
    url: "https://github.com/modelscope/3D-Speaker",
  },
  {
    name: "Silero VAD",
    use: "Finding pauses in long recordings",
    maker: "Silero Team",
    licence: "MIT",
    url: "https://github.com/snakers4/silero-vad",
  },
];

function formatSize(bytes: number) {
  return bytes >= 100_000 ? `${Math.round(bytes / 1000)} KB` : `${(bytes / 1000).toFixed(1)} KB`;
}

function LicenceText({ document }: { document: LicenceDocument }) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open || text !== null) return;
    let current = true;
    readLicenceDocument(document.name)
      .then((value) => current && setText(value))
      .catch((reason) => current && setError(describeError(reason, "This licence text could not be read.")));
    return () => {
      current = false;
    };
  }, [open, text, document.name]);

  return (
    <details className="licence-document" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>
        <span>{document.name}</span>
        <span className="muted">{formatSize(document.sizeBytes)}</span>
      </summary>
      {error ? <p className="warning-text">{error}</p> : null}
      {open && text === null && !error ? <p className="muted">Loading…</p> : null}
      {text !== null ? <pre className="licence-text" tabIndex={0}>{text}</pre> : null}
    </details>
  );
}

/** Settings → About: who made Murmela's engines, and every licence text it ships. */
export function AboutSection({
  autoUpdateEnabled,
  saving,
  onToggleAutoUpdate,
}: {
  autoUpdateEnabled: boolean;
  saving: boolean;
  onToggleAutoUpdate: () => void;
}) {
  const [version, setVersion] = useState<string | null>(null);
  const [documents, setDocuments] = useState<LicenceDocument[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    getHealthCheck()
      .then((health) => current && setVersion(health.appVersion))
      .catch(() => undefined);
    listLicenceDocuments()
      .then((list) => current && setDocuments(list))
      .catch((reason) => current && setError(describeError(reason, "The licence texts could not be listed.")));
    return () => {
      current = false;
    };
  }, []);

  return (
    <div className="settings-grid">
      <article className="glass-subtle settings-card settings-card-wide about-brand">
        <img src={murmelaMark} alt="" width={64} height={44} />
        <div>
          <h3 className="about-name">Murmela</h3>
          <p className="muted">
            {version ? `Version ${version} · ` : ""}Speech to text that stays on this computer.
          </p>
        </div>
      </article>

      <UpdatesCard autoUpdateEnabled={autoUpdateEnabled} saving={saving} onToggleAutoUpdate={onToggleAutoUpdate} />

      <article className="glass-subtle settings-card settings-card-wide" aria-label="Engines and licences">
        <h3>Engines and their licences</h3>
        <p className="muted">
          Murmela runs these models locally. They are downloaded from their publishers when you choose them and keep
          their own licences.
        </p>
        <ul className="about-credits">
          {MODEL_CREDITS.map((credit) => (
            <li key={credit.name}>
              <div className="about-credit-head">
                <a href={credit.url} target="_blank" rel="noreferrer">{credit.name}</a>
                <span className="about-licence">{credit.licence}</span>
              </div>
              <p className="muted">{credit.maker} · {credit.use}</p>
              {credit.note ? <p className="muted about-credit-note">{credit.note}</p> : null}
            </li>
          ))}
        </ul>
      </article>

      <article className="glass-subtle settings-card settings-card-wide" aria-label="Licence texts">
        <h3>Licence texts</h3>
        <p className="muted">
          The full texts of every licence that applies to Murmela, including all open-source libraries built into the
          app.
        </p>
        {error ? <p className="warning-text">{error}</p> : null}
        {documents === null && !error ? <p className="muted">Loading…</p> : null}
        {documents?.length === 0 ? <p className="muted">No licence texts were found in this build.</p> : null}
        <div className="licence-documents">
          {documents?.map((document) => <LicenceText key={document.name} document={document} />)}
        </div>
      </article>
    </div>
  );
}
