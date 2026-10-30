#!/usr/bin/env python3
"""Offline, one-folder PyInstaller entry point for VibeVoice-ASR MLX."""

import json
import os
import re
import sys
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
# It can also repeat a longer phrase or a whole segment, timestamps included.
# Real output never repeats itself token for token this long.
PHRASE_LOOP_WINDOW_TOKENS = 240
PHRASE_LOOP_MAX_PERIOD = 60
# Or keep writing without ever finishing a segment. VibeVoice cuts at speaker
# turns and pauses; even a long monologue finishes one well before this.
STALL_TOKENS = 2000
# A loop is fed by what the model wrote before it, so the next pass starts
# fresh where the last finished segment ended. When a pass gets stuck again
# this soon, the audio there is the problem: the pass is retried once with a
# repetition penalty (that got the 40-minute meeting past its loop), and then
# a short stretch is left out and reported, so Murmela can fill it with
# another engine. A stretch stuck again right after is left out longer.
MIN_PROGRESS_SECONDS = 20.0
RETRY_DECODING = {"repetition_penalty": 1.1, "repetition_context_size": 20}
SKIP_SECONDS = 15.0
MAX_SKIP_SECONDS = 120.0
# Less than this left at the end of the audio is not worth another pass.
MIN_PASS_SECONDS = 1.0
# A loop can also end inside one segment ("no, no, no, …" a hundred times)
# before it is noticed. People do say "no, no, no", but not seven times over.
REPEATED_RUN = re.compile(r"((?:\S+\s+){1,3}?)\1{6,}")
KEPT_REPEATS = 3


def without_repeated_runs(text):
    return REPEATED_RUN.sub(lambda match: match.group(1) * KEPT_REPEATS, text)


def log(message):
    # Murmela keeps the last lines of stderr to explain a failure.
    print(f"murmela: {message}", file=sys.stderr, flush=True)


class DecoderLoop(Exception):
    def __init__(self, kind, tokens):
        super().__init__(f"decoder {kind}")
        self.kind = kind
        self.tokens = tokens


def loop_kind(tokens, tokens_at_last_end):
    """Why decoding should stop now, or None. Checked every 50 tokens."""
    count = len(tokens)
    if count >= LOOP_WINDOW_TOKENS and len(set(tokens[-LOOP_WINDOW_TOKENS:])) <= LOOP_MAX_DISTINCT_TOKENS:
        return "repeated a word"
    if count >= PHRASE_LOOP_WINDOW_TOKENS:
        window = tokens[-PHRASE_LOOP_WINDOW_TOKENS:]
        for period in range(1, PHRASE_LOOP_MAX_PERIOD + 1):
            if all(window[i] == window[i - period] for i in range(period, len(window))):
                return "repeated a phrase"
    if count - tokens_at_last_end >= STALL_TOKENS:
        return "stopped finishing segments"
    return None


class Decoding:
    """Where decoding is, across all passes over one audio file."""

    def __init__(self):
        self.total_seconds = 0.0
        self.offset_seconds = 0.0
        self.reported = 1


def watch_decoding(model, decoding):
    """Stop a pass that loops, and report how far decoding got.

    VibeVoice decodes a recording in a single pass. Without progress, Murmela
    hears nothing between "model loaded" (1 %) and "done" (100 %), and a pass
    that takes longer than its ten-minute stall watchdog (a 10-minute chunk on
    an 18 GB Mac under memory pressure) is killed as if it had hung. The
    progress comes from the end time of the newest segment.
    """
    stream_generate = model.stream_generate

    def generate_and_watch(*args, **kwargs):
        tokens = []
        last_end = 0.0
        tokens_at_last_end = 0
        checked_at = time.monotonic()
        for token, logprobs in stream_generate(*args, **kwargs):
            tokens.append(token)
            # Stop at once instead of filling memory until max_tokens.
            if len(tokens) % 50 == 0:
                kind = loop_kind(tokens, tokens_at_last_end)
                if kind:
                    raise DecoderLoop(kind, tokens)
            now = time.monotonic()
            if now - checked_at >= PROGRESS_INTERVAL_SECONDS:
                checked_at = now
                try:
                    tail = model.tokenizer.decode(tokens[-PROGRESS_TAIL_TOKENS:], skip_special_tokens=True)
                    ends = SEGMENT_END.findall(tail)
                    if ends and float(ends[-1]) > last_end:
                        last_end = float(ends[-1])
                        tokens_at_last_end = len(tokens)
                        if decoding.total_seconds > 0:
                            done = decoding.offset_seconds + last_end
                            percent = min(99, 1 + int(98 * done / decoding.total_seconds))
                            if percent > decoding.reported:
                                decoding.reported = percent
                                emit({"type": "progress", "progress_percent": percent})
                except Exception:
                    # Progress is a courtesy; never let it break a transcription.
                    pass
            yield token, logprobs

    model.stream_generate = generate_and_watch


def finished_segments(model, text):
    """The segments a pass completed before it was stopped.

    The output is a JSON array; a stopped pass leaves it unterminated, so the
    complete objects are read one by one. Trailing copies of the same segment
    belong to the loop and are dropped.
    """
    start = text.find("[")
    if start < 0:
        return []
    decoder = json.JSONDecoder()
    items = []
    position = start + 1
    while True:
        position = text.find("{", position)
        if position < 0:
            break
        try:
            item, position = decoder.raw_decode(text, position)
        except ValueError:
            break
        if isinstance(item, dict):
            items.append(item)
    segments = model.parse_transcription(json.dumps(items)) if items else []
    while len(segments) >= 2 and str(segments[-1].get("text", "")).strip() == str(segments[-2].get("text", "")).strip():
        segments.pop()
    return segments


def segment_seconds(item, name, default=0.0):
    try:
        return float(value(item, name, f"{name}_time", default=default))
    except (TypeError, ValueError):
        return default


def handle(request):
    if request.get("protocolVersion") != PROTOCOL_VERSION:
        raise ValueError("unsupported worker protocol version")
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    import mlx.core as mx
    from mlx_audio.stt.utils import load_audio, load_model

    use_bundled_tokenizer(request["modelPath"])
    model = load_model(request["modelPath"])
    # Passes after a loop get the audio as samples, which mlx-audio levels to
    # -25 dB FS; a file path is not levelled. Keep every pass like the path.
    model._normalize_audio = lambda audio, *args, **kwargs: audio
    decoding = Decoding()
    watch_decoding(model, decoding)
    # The first progress tells Murmela the model is loaded and decoding begins.
    emit({"type": "progress", "progress_percent": 1})
    max_tokens = int(request.get("maxTokens", 32768))
    audio = load_audio(request["audioPath"], sr=MODEL_SAMPLE_RATE)
    total = audio.shape[-1] / MODEL_SAMPLE_RATE
    decoding.total_seconds = total

    def transcribe(offset, **extra):
        decoding.offset_seconds = offset
        return model.generate(
            audio[int(offset * MODEL_SAMPLE_RATE):],
            sampling_rate=MODEL_SAMPLE_RATE,
            max_tokens=max_tokens,
            context=request.get("prompt") or None,
            # Pin decoding so results do not depend on library defaults:
            # greedy (temperature 0, no nucleus filtering) unless asked otherwise.
            temperature=0.0 if request.get("greedy", True) else 0.7,
            top_p=1.0,
            **extra,
        )

    segments = []
    gaps = []
    truncated = False
    offset = 0.0
    stuck_here = 0
    skip = SKIP_SECONDS
    while total - offset >= MIN_PASS_SECONDS:
        try:
            output = transcribe(offset, **(RETRY_DECODING if stuck_here == 1 else {}))
        except DecoderLoop as loop:
            mx.clear_cache()  # the stopped pass's cache would otherwise linger
            text = model.tokenizer.decode(loop.tokens, skip_special_tokens=True)
            done = finished_segments(model, text)
            reached = max((segment_seconds(item, "end") for item in done), default=0.0)
            log(f"pass from {offset:.1f} s {loop.kind} after {offset + reached:.1f} s "
                f"({len(loop.tokens)} tokens, ends with {text[-80:]!r})")
            for item in done:
                item["start"] = offset + segment_seconds(item, "start")
                item["end"] = offset + segment_seconds(item, "end")
            segments.extend(done)
            if reached >= MIN_PROGRESS_SECONDS:
                offset += reached
                stuck_here = 0
                skip = SKIP_SECONDS
                continue
            offset += reached
            stuck_here += 1
            if stuck_here < 2:
                continue
            until = min(total, offset + skip)
            log(f"leaving out {offset:.1f}–{until:.1f} s")
            gaps.append({"startMs": round(offset * 1000), "endMs": round(until * 1000), "reason": loop.kind})
            offset = until
            stuck_here = 0
            skip = min(MAX_SKIP_SECONDS, skip * 2)
            continue
        pass_segments = list(value(output, "segments", "sentences", default=[]) or [])
        for item in pass_segments:
            item["start"] = offset + segment_seconds(item, "start")
            item["end"] = offset + segment_seconds(item, "end", segment_seconds(item, "start"))
        segments.extend(pass_segments)
        generated_tokens = int(value(output, "generation_tokens", default=0) or 0)
        truncated = truncated or generated_tokens >= max_tokens
        break

    result_segments = []
    for item in segments:
        start = segment_seconds(item, "start")
        end = segment_seconds(item, "end", start)
        speaker = value(item, "speaker", "speaker_id")
        language = value(item, "language", "language_code")
        result_segments.append({
            "startMs": round(start * 1000),
            "endMs": round(end * 1000),
            "speaker": str(speaker) if speaker is not None else None,
            "text": without_repeated_runs(str(value(item, "text", default=""))),
            "languageCode": str(language) if language is not None else None,
        })
    text = " ".join(segment["text"].strip() for segment in result_segments if segment["text"].strip())
    emit({"type": "progress", "progress_percent": 100})
    emit({"type": "result", "result": {"text": text, "segments": result_segments, "truncated": truncated, "gaps": gaps}})


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
    from mlx_audio.stt.utils import load_audio, load_model  # noqa: F401
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
