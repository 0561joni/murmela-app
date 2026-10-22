#!/usr/bin/env python3
"""Offline, one-folder PyInstaller entry point for VibeVoice-ASR MLX."""

import json
import os
import sys
import tempfile
import threading
import time

PROTOCOL_VERSION = 1
ORIGINAL_PARENT = os.getppid()


def emit(record):
    print(json.dumps(record, ensure_ascii=False), flush=True)


def value(item, *names, default=None):
    for name in names:
        if isinstance(item, dict) and name in item:
            return item[name]
        if hasattr(item, name):
            return getattr(item, name)
    return default


# The mlx-community VibeVoice-ASR weights ship without tokenizer files. mlx-audio
# then falls back to downloading the "Qwen/Qwen2.5-7B" tokenizer from Hugging
# Face, which can never work offline. Murmela bundles that tokenizer (pinned and
# verified by scripts/build-vibevoice-worker.mjs) and serves it locally.
UPSTREAM_TOKENIZER = "Qwen/Qwen2.5-7B"
TOKENIZER_DIRECTORY = "qwen2.5-7b-tokenizer"
TOKENIZER_FILES = ("tokenizer.json", "tokenizer_config.json", "vocab.json", "merges.txt")


def bundled_tokenizer_dir():
    candidates = []
    if os.environ.get("MURMELA_VIBEVOICE_TOKENIZER"):
        candidates.append(os.environ["MURMELA_VIBEVOICE_TOKENIZER"])
    if getattr(sys, "frozen", False):
        candidates.append(os.path.join(getattr(sys, "_MEIPASS", os.path.dirname(sys.executable)), TOKENIZER_DIRECTORY))
    here = os.path.dirname(os.path.abspath(__file__))
    candidates.append(os.path.join(here, "..", "..", "src-tauri", "target", "vibevoice-runtime", TOKENIZER_DIRECTORY))
    for candidate in candidates:
        if all(os.path.isfile(os.path.join(candidate, name)) for name in TOKENIZER_FILES):
            return os.path.abspath(candidate)
    raise RuntimeError("the bundled Qwen2.5 tokenizer for VibeVoice is missing; rebuild the VibeVoice worker")


def has_tokenizer(directory):
    return os.path.isfile(os.path.join(directory, "tokenizer_config.json")) and (
        os.path.isfile(os.path.join(directory, "tokenizer.json"))
        or os.path.isfile(os.path.join(directory, "vocab.json"))
    )


def use_bundled_tokenizer(model_path):
    """Route mlx-audio's tokenizer lookup to local files only."""
    from transformers import AutoTokenizer

    original = AutoTokenizer.from_pretrained
    if getattr(original, "_murmela_local", False):
        return
    local = model_path if has_tokenizer(model_path) else bundled_tokenizer_dir()

    def from_pretrained(name, *args, **kwargs):
        if str(name) in (UPSTREAM_TOKENIZER, str(model_path)):
            name = local
        return original(name, *args, **kwargs)

    from_pretrained._murmela_local = True
    AutoTokenizer.from_pretrained = from_pretrained


def handle(request):
    if request.get("protocolVersion") != PROTOCOL_VERSION:
        raise ValueError("unsupported worker protocol version")
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    from mlx_audio.stt.generate import generate_transcription
    from mlx_audio.stt.utils import load_model

    emit({"type": "progress", "progress_percent": 1})
    use_bundled_tokenizer(request["modelPath"])
    model = load_model(request["modelPath"])
    max_tokens = int(request.get("maxTokens", 32768))
    with tempfile.TemporaryDirectory(prefix="murmela-vibevoice-") as output_dir:
        transcription = generate_transcription(
            model=model,
            audio=request["audioPath"],
            output_path=os.path.join(output_dir, "transcript"),
            format="json",
            verbose=False,
            max_tokens=max_tokens,
            context=request.get("prompt") or None,
            # Pin decoding so results do not depend on library defaults:
            # greedy (temperature 0, no nucleus filtering) unless asked otherwise.
            temperature=0.0 if request.get("greedy", True) else 0.7,
            top_p=1.0,
        )
    raw_segments = value(transcription, "sentences", "segments", default=[]) or []
    segments = []
    for item in raw_segments:
        start = value(item, "start", "start_time", default=0)
        end = value(item, "end", "end_time", default=start)
        speaker = value(item, "speaker", "speaker_id")
        language = value(item, "language", "language_code")
        segments.append({
            "startMs": round(float(start) * 1000),
            "endMs": round(float(end) * 1000),
            "speaker": str(speaker) if speaker is not None else None,
            "text": str(value(item, "text", default="")),
            "languageCode": str(language) if language is not None else None,
        })
    text = str(value(transcription, "text", default=""))
    generated_tokens = int(value(transcription, "generation_tokens", default=0) or 0)
    truncated = bool(value(transcription, "truncated", "hit_token_limit", default=False)) or generated_tokens >= max_tokens
    emit({"type": "progress", "progress_percent": 100})
    emit({"type": "result", "result": {"text": text, "segments": segments, "truncated": truncated}})


def self_test():
    """Import everything a transcription touches, so a broken build fails at build time."""
    import miniaudio  # noqa: F401  (audio decoding)
    import mlx.core as mx

    # Run a real GPU computation: this loads MLX's Metal kernels (mlx.metallib),
    # which a plain import does not.
    result = mx.ones(4, stream=mx.gpu) * 2
    mx.eval(result)
    if result.sum().item() != 8:
        raise RuntimeError("MLX Metal self-test returned a wrong result")
    from mlx_audio.stt.generate import generate_transcription  # noqa: F401
    from mlx_audio.stt.utils import load_model  # noqa: F401
    import mlx_audio.stt.models.vibevoice_asr  # noqa: F401
    from mlx_lm.models.qwen2 import Qwen2Model  # noqa: F401
    from mlx_lm.generate import generate_step  # noqa: F401
    from mlx_lm.sample_utils import make_sampler  # noqa: F401
    from transformers import AutoTokenizer

    # Load the bundled tokenizer exactly as a transcription does (offline) and
    # check the special tokens VibeVoice repurposes for speech.
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    tokenizer = AutoTokenizer.from_pretrained(bundled_tokenizer_dir(), trust_remote_code=True)
    special = [tokenizer.convert_tokens_to_ids(token) for token in ("<|object_ref_start|>", "<|object_ref_end|>", "<|box_start|>")]
    if any(not isinstance(token_id, int) or token_id == tokenizer.unk_token_id for token_id in special):
        raise RuntimeError(f"bundled tokenizer lacks VibeVoice speech tokens: {special}")
    emit({"type": "selfTest", "ok": True})


def main():
    if "--self-test" in sys.argv[1:]:
        self_test()
        return

    def stop_if_parent_exits():
        while True:
            if os.getppid() != ORIGINAL_PARENT:
                os._exit(130)
            time.sleep(0.5)

    threading.Thread(target=stop_if_parent_exits, daemon=True).start()
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            handle(json.loads(line))
        except MemoryError:
            emit({"type": "error", "code": "MODEL_OUT_OF_MEMORY", "message": "VibeVoice ran out of unified memory"})
        except Exception as error:
            # MLX raises a full Metal working set as a RuntimeError.
            message = str(error)
            code = "MODEL_OUT_OF_MEMORY" if "insufficient memory" in message.lower() or "outofmemory" in message.lower() else "MODEL_WORKER_FAILED"
            emit({"type": "error", "code": code, "message": message})


if __name__ == "__main__":
    main()
