#!/usr/bin/env python3
"""Murmela NDJSON adapter for the separately linked moss-transcribe.cpp CLI."""

import json
import os
import pathlib
import subprocess
import sys
import threading
import time

PROTOCOL_VERSION = 1
ACTIVE_PROCESS = None
ORIGINAL_PARENT = os.getppid()


def emit(record):
    print(json.dumps(record, ensure_ascii=False), flush=True)


def worker_cli():
    override = os.environ.get("MURMELA_MOSS_CLI")
    if override:
        return pathlib.Path(override)
    # Frozen (PyInstaller) builds: the native CLI sits next to the executable,
    # while __file__ points into the bundle's internal folder.
    anchor = sys.executable if getattr(sys, "frozen", False) else __file__
    return pathlib.Path(anchor).resolve().parent / "moss-transcribe"


def handle(request):
    global ACTIVE_PROCESS
    if request.get("protocolVersion") != PROTOCOL_VERSION:
        raise ValueError("unsupported worker protocol version")
    model = pathlib.Path(request["modelPath"]) / "moss-transcribe-f16.gguf"
    audio = pathlib.Path(request["audioPath"])
    cli = worker_cli()
    if not cli.is_file():
        raise RuntimeError(f"MOSS native executable is missing: {cli}")
    if not model.is_file() or not audio.is_file():
        raise RuntimeError("MOSS model or prepared audio is incomplete")
    command = [str(cli), "transcribe", str(model), str(audio), "--max-new", str(request.get("maxTokens", 65536)), "--format", "json"]
    prompt = request.get("prompt")
    if prompt:
        command.extend(["--prompt", prompt])
    emit({"type": "progress", "progress_percent": 1})
    ACTIVE_PROCESS = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    stdout, stderr = ACTIVE_PROCESS.communicate()
    completed_returncode = ACTIVE_PROCESS.returncode
    ACTIVE_PROCESS = None
    if completed_returncode != 0:
        detail = stderr.strip() or f"exit code {completed_returncode}"
        raise RuntimeError(detail)
    parsed = json.loads(stdout)
    if isinstance(parsed, dict):
        parsed = parsed.get("segments", [])
    segments = []
    for item in parsed:
        segments.append({
            "startMs": round(float(item.get("start", 0)) * 1000),
            "endMs": round(float(item.get("end", 0)) * 1000),
            "speaker": str(item.get("speaker")) if item.get("speaker") is not None else None,
            "text": str(item.get("text", "")),
            "languageCode": item.get("language") or None,
        })
    emit({"type": "progress", "progress_percent": 100})
    stderr_lower = stderr.lower()
    truncated = "max" in stderr_lower and "token" in stderr_lower
    emit({"type": "result", "result": {"text": " ".join(item["text"] for item in segments), "segments": segments, "truncated": truncated}})


def self_test():
    """Check that the native CLI is present and runs, so a broken build fails at build time."""
    cli = worker_cli()
    if not cli.is_file() or not os.access(cli, os.X_OK):
        raise SystemExit(f"MOSS native executable is missing: {cli}")
    usage = subprocess.run([str(cli)], capture_output=True, text=True)
    if "usage" not in (usage.stdout + usage.stderr).lower():
        raise SystemExit(f"MOSS native executable did not start: {usage.stderr.strip()}")
    emit({"type": "selfTest", "ok": True})


def main():
    if "--self-test" in sys.argv[1:]:
        self_test()
        return

    def stop_if_parent_exits():
        global ACTIVE_PROCESS
        while True:
            if os.getppid() != ORIGINAL_PARENT:
                if ACTIVE_PROCESS is not None:
                    ACTIVE_PROCESS.kill()
                os._exit(130)
            time.sleep(0.5)

    threading.Thread(target=stop_if_parent_exits, daemon=True).start()
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            handle(json.loads(line))
        except MemoryError:
            emit({"type": "error", "code": "MODEL_OUT_OF_MEMORY", "message": "MOSS ran out of memory"})
        except Exception as error:
            emit({"type": "error", "code": "MODEL_WORKER_FAILED", "message": str(error)})


if __name__ == "__main__":
    main()
