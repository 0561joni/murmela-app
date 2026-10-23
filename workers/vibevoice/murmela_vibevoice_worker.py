#!/usr/bin/env python3
"""Offline, one-folder PyInstaller entry point for VibeVoice-ASR MLX."""

import json
import os
import re
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


# VibeVoice writes each segment as JSON with its end time, e.g. "End time": 12.34.
SEGMENT_END = re.compile(r'"end(?:[ _]?time)?"\s*:\s*"?(\d+(?:\.\d+)?)', re.IGNORECASE)
# The model resamples to 24 kHz; mlx-audio's own duration uses the same rate.
MODEL_SAMPLE_RATE = 24000
PROGRESS_INTERVAL_SECONDS = 2.0
PROGRESS_TAIL_TOKENS = 256
# Greedy decoding can get stuck writing the same token or two forever
# ("a, a, a, …", seen in a 40-minute meeting). Real output, with its JSON keys,
# timestamps and words, never has this few distinct tokens in a window.
LOOP_WINDOW_TOKENS = 200
LOOP_MAX_DISTINCT_TOKENS = 6
# A second pass that discourages repeating the last few tokens gets past such
# a spot (the same meeting then transcribed cleanly). Only used after a loop,
# so normal decoding stays untouched.
RETRY_DECODING = {"repetition_penalty": 1.1, "repetition_context_size": 20}


class DecoderLoop(Exception):
    def __init__(self, at_seconds):
        super().__init__(f"decoder repeated itself after {at_seconds:.0f} s of audio")
        self.at_seconds = at_seconds


def report_decoding_progress(model):
    """Report how far decoding got, from the end time of the newest segment.

    VibeVoice decodes a recording in a single pass. Without this, Murmela hears
    nothing between "model loaded" (1 %) and "done" (100 %), and a pass that
    takes longer than its ten-minute stall watchdog (a 10-minute chunk on an
    18 GB Mac under memory pressure) is killed as if it had hung.
    """
    audio_seconds = [0.0]
    preprocess_audio = model._preprocess_audio
    stream_generate = model.stream_generate

    def preprocess_and_measure(*args, **kwargs):
        audio = preprocess_audio(*args, **kwargs)
        try:
            audio_seconds[0] = audio.shape[-1] / MODEL_SAMPLE_RATE
        except Exception:
            audio_seconds[0] = 0.0
        return audio

    def generate_and_report(*args, **kwargs):
        tokens = []
        reported = 1
        last_end = 0.0
        checked_at = time.monotonic()
        for token, logprobs in stream_generate(*args, **kwargs):
            tokens.append(token)
            # Stop at once instead of filling memory until max_tokens.
            if (
                len(tokens) % 50 == 0
                and len(tokens) >= LOOP_WINDOW_TOKENS
                and len(set(tokens[-LOOP_WINDOW_TOKENS:])) <= LOOP_MAX_DISTINCT_TOKENS
            ):
                raise DecoderLoop(last_end)
            now = time.monotonic()
            if audio_seconds[0] > 0 and now - checked_at >= PROGRESS_INTERVAL_SECONDS:
                checked_at = now
                try:
                    tail = model.tokenizer.decode(tokens[-PROGRESS_TAIL_TOKENS:], skip_special_tokens=True)
                    ends = SEGMENT_END.findall(tail)
                    if ends:
                        last_end = max(last_end, float(ends[-1]))
                        percent = min(99, 1 + int(98 * float(ends[-1]) / audio_seconds[0]))
                        if percent > reported:
                            reported = percent
                            emit({"type": "progress", "progress_percent": percent})
                except Exception:
                    # Progress is a courtesy; never let it break a transcription.
                    pass
            yield token, logprobs

    model._preprocess_audio = preprocess_and_measure
    model.stream_generate = generate_and_report


def handle(request):
    if request.get("protocolVersion") != PROTOCOL_VERSION:
        raise ValueError("unsupported worker protocol version")
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    from mlx_audio.stt.generate import generate_transcription
    from mlx_audio.stt.utils import load_model

    use_bundled_tokenizer(request["modelPath"])
    model = load_model(request["modelPath"])
    report_decoding_progress(model)
    # The first progress tells Murmela the model is loaded and decoding begins.
    emit({"type": "progress", "progress_percent": 1})
    max_tokens = int(request.get("maxTokens", 32768))

    def transcribe(**decoding):
        with tempfile.TemporaryDirectory(prefix="murmela-vibevoice-") as output_dir:
            return generate_transcription(
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
                **decoding,
            )

    try:
        transcription = transcribe()
    except DecoderLoop as loop:
        print(f"{loop}; transcribing again with a repetition penalty", file=sys.stderr, flush=True)
        import mlx.core as mx

        mx.clear_cache()  # the looping pass's cache would otherwise linger
        try:
            transcription = transcribe(**RETRY_DECODING)
        except DecoderLoop as again:
            emit({"type": "error", "code": "DECODER_REPETITION", "message": f"VibeVoice {again} twice"})
            return
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
