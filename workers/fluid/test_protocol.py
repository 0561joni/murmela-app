"""Protocol probes for the live-pair helper.

The first group needs no model weights. The second group runs only when the
pinned models are present (MURMELA_FLUID_MODELS, default
src-tauri/target/fluid-models) and checks session ordering and error codes
against real inference.
"""
import json
import os
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKER = Path(os.environ.get("MURMELA_FLUID_WORKER", ROOT / "src-tauri/bundle/fluid/murmela-fluid-worker"))
MODELS = Path(os.environ.get("MURMELA_FLUID_MODELS", ROOT / "src-tauri/target/fluid-models"))
NEMOTRON = MODELS / "nemotron/latin/560ms"
PARAKEET = MODELS / "parakeet"


def request(kind, session="s", sequence=0, **fields):
    return json.dumps(dict(version=1, type=kind, sessionId=session, sequence=sequence, **fields)) + "\n"


class Worker:
    def __init__(self):
        self.process = subprocess.Popen(
            [str(WORKER)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
        )

    def send(self, kind, session="s", sequence=0, **fields):
        self.process.stdin.write(request(kind, session, sequence, **fields))
        self.process.stdin.flush()
        reply = json.loads(self.process.stdout.readline())
        self.assert_envelope(reply, session, sequence)
        return reply

    @staticmethod
    def assert_envelope(reply, session, sequence):
        assert reply["version"] == 1, reply
        assert reply["sessionId"] == session, reply
        assert reply["sequence"] == sequence + 1, reply
        assert isinstance(reply["rssBytes"], int) and isinstance(reply["peakRssBytes"], int), reply

    def close(self):
        self.process.stdin.close()
        code = self.process.wait(timeout=30)
        stderr = self.process.stderr.read()
        self.process.stdout.close()
        self.process.stderr.close()
        return code, stderr


class ProtocolFailures(unittest.TestCase):
    def reject(self, frame):
        run = subprocess.run([str(WORKER)], input=frame, text=True, capture_output=True, timeout=10)
        self.assertNotEqual(run.returncode, 0)
        messages = [json.loads(line) for line in run.stdout.splitlines()]
        self.assertEqual(len(messages), 1)
        self.assertEqual(messages[0]["type"], "error")
        self.assertEqual(messages[0]["code"], "FLUID_PROTOCOL")
        self.assertNotIn("private test text", run.stderr)

    def test_malformed_and_truncated_json(self):
        self.reject("{broken}\n")
        self.reject('{"private test text":')
        self.reject('{"version":1,"type":"ping","sessionId":"s","sequence":0} trailing\n')

    def test_unknown_version_type_and_bad_fields_are_fatal(self):
        self.reject(json.dumps(dict(version=9, type="ping", sessionId="s", sequence=0)) + "\n")
        self.reject(request("dance"))
        self.reject(json.dumps(dict(version=1, type="ping", sessionId="", sequence=0)) + "\n")
        self.reject(json.dumps(dict(version=1, type="ping", sessionId="s", sequence=-1)) + "\n")
        self.reject(json.dumps(dict(version=1, type="ping", sessionId="s", sequence=True)) + "\n")
        self.reject(json.dumps(dict(version=1, type="ping", sessionId="x" * 129, sequence=0)) + "\n")

    def test_oversized_frame_is_bounded(self):
        self.reject("x" * (1024 * 1024 + 1))

    def test_clean_idle_eof_releases_process(self):
        run = subprocess.run([str(WORKER)], input="", capture_output=True, timeout=10)
        self.assertEqual(run.returncode, 0)
        self.assertEqual(run.stdout, b"")


class ResidentBehaviour(unittest.TestCase):
    def test_session_errors_keep_the_worker_alive(self):
        worker = Worker()
        self.assertEqual(worker.send("ping", "c1")["loaded"], False)
        self.assertEqual(worker.send("warmup", "c2")["code"], "FLUID_NOT_LOADED")
        self.assertEqual(worker.send("start", "s1", language="auto", chunkMs=560)["code"], "FLUID_NOT_LOADED")
        missing = worker.send("load", "c3", nemotronPath="/nonexistent/n", parakeetPath="/nonexistent/p")
        self.assertEqual(missing["code"], "FLUID_MODEL_MISSING")
        stale = worker.send("audio", "old", 4, startSample=0, samples=[0.0])
        self.assertEqual((stale["type"], stale["code"]), ("error", "FLUID_STALE_SESSION"))
        self.assertEqual(worker.send("cancel", "old", 5)["type"], "canceled")
        self.assertEqual(worker.send("unload", "c4")["type"], "unloaded")
        self.assertEqual(worker.send("ping", "c5")["loaded"], False)
        code, stderr = worker.close()
        self.assertEqual(code, 0)
        self.assertNotIn("nonexistent", stderr)


@unittest.skipUnless((NEMOTRON / "metadata.json").exists() and (PARAKEET / "Encoder_v2.mlmodelc").exists(),
                     "pinned live-pair models are not present")
class SessionOrdering(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.worker = Worker()
        ready = cls.worker.send("load", "c", nemotronPath=str(NEMOTRON), parakeetPath=str(PARAKEET))
        assert ready["type"] == "ready", ready
        assert ready["chunkMs"] == 560, ready
        assert cls.worker.send("warmup", "w")["type"] == "warm"

    @classmethod
    def tearDownClass(cls):
        code, _ = cls.worker.close()
        assert code == 0

    def start(self, session):
        reply = self.worker.send("start", session, language="auto", chunkMs=560)
        self.assertEqual(reply["type"], "started")

    def test_gap_duplicate_and_count_mismatch_are_rejected(self):
        self.start("gap")
        ok = self.worker.send("audio", "gap", 1, startSample=0, samples=[0.0] * 8960)
        self.assertEqual((ok["type"], ok["ackSample"]), ("progress", 8960))
        gap = self.worker.send("audio", "gap", 2, startSample=9000, samples=[0.0] * 100)
        self.assertEqual(gap["code"], "FLUID_SAMPLES_MISMATCH")
        # The rejected session is gone; later frames for it are stale.
        self.assertEqual(self.worker.send("finish", "gap", 3, expectedSamples=8960)["code"], "FLUID_STALE_SESSION")

        self.start("dup")
        self.worker.send("audio", "dup", 1, startSample=0, samples=[0.0] * 100)
        self.assertEqual(self.worker.send("audio", "dup", 2, startSample=0, samples=[0.0] * 100)["code"],
                         "FLUID_SAMPLES_MISMATCH")

        self.start("count")
        self.worker.send("audio", "count", 1, startSample=0, samples=[0.0] * 100)
        self.assertEqual(self.worker.send("finish", "count", 2, expectedSamples=99)["code"], "FLUID_SAMPLES_MISMATCH")

    def test_unordered_sequence_and_chunk_mismatch(self):
        self.start("order")
        self.assertEqual(self.worker.send("audio", "order", 3, startSample=0, samples=[0.0])["code"],
                         "FLUID_STALE_SESSION")
        self.assertEqual(self.worker.send("start", "x", language="auto", chunkMs=1120)["code"], "FLUID_CHUNK_MISMATCH")
        self.assertEqual(self.worker.send("start", "y", language="xx-XX", chunkMs=560)["code"],
                         "FLUID_LANGUAGE_UNSUPPORTED")
        self.assertEqual(self.worker.send("audio", "z", 1, startSample=0, samples=[2.0])["code"], "FLUID_STALE_SESSION")

    def test_final_carries_stream_and_final_text_and_new_start_supersedes(self):
        self.start("first")
        self.worker.send("audio", "first", 1, startSample=0, samples=[0.0] * 100)
        # A new start drops the unfinished session without a reload.
        self.start("second")
        self.assertEqual(self.worker.send("finish", "first", 2, expectedSamples=100)["code"], "FLUID_STALE_SESSION")
        progress = self.worker.send("audio", "second", 1, startSample=0, samples=[0.0] * 16000)
        self.assertEqual(progress["committedText"], "")
        final = self.worker.send("finish", "second", 2, expectedSamples=16000)
        self.assertEqual(final["type"], "final")
        for key in ("streamText", "finalText", "language", "streamError", "finalError"):
            self.assertIsInstance(final[key], str)
        self.assertEqual(final["finalError"], "")
        self.assertEqual(set(final["timings"]), {"audioMs", "flushMs", "finalMs", "finalWaitMs"})

    def test_ping_reports_both_models(self):
        pong = self.worker.send("ping", "p")
        self.assertEqual((pong["type"], pong["loaded"]), ("pong", True))
        self.assertIsInstance(pong["finalLoaded"], bool)

    def test_cancel_is_immediate_and_idempotent(self):
        self.start("cancel")
        self.worker.send("audio", "cancel", 1, startSample=0, samples=[0.0] * 100)
        self.assertEqual(self.worker.send("cancel", "cancel", 2)["type"], "canceled")
        self.assertEqual(self.worker.send("cancel", "cancel", 3)["type"], "canceled")
        self.assertEqual(self.worker.send("finish", "cancel", 4, expectedSamples=100)["code"], "FLUID_STALE_SESSION")
        self.assertEqual(self.worker.send("ping", "p")["loaded"], True)


if __name__ == "__main__":
    unittest.main()


@unittest.skipUnless((NEMOTRON / "metadata.json").exists() and (PARAKEET / "Encoder_v2.mlmodelc").exists(),
                     "pinned live-pair models are not present")
class StagedLoading(unittest.TestCase):
    def test_streaming_starts_before_the_final_model_and_finish_waits_for_it(self):
        worker = Worker()
        try:
            ready = worker.send("load", "c", nemotronPath=str(NEMOTRON), parakeetPath=str(PARAKEET))
            self.assertEqual(ready["type"], "ready")
            self.assertIsInstance(ready["finalLoaded"], bool)
            # No warmup and no wait: a session may begin while Parakeet loads.
            self.assertEqual(worker.send("start", "s", language="auto", chunkMs=560)["type"], "started")
            worker.send("audio", "s", 1, startSample=0, samples=[0.0] * 16000)
            final = worker.send("finish", "s", 2, expectedSamples=16000)
            self.assertEqual((final["type"], final["finalError"]), ("final", ""))
            self.assertGreaterEqual(final["timings"]["finalWaitMs"], 0)
            self.assertTrue(worker.send("ping", "p")["finalLoaded"])
        finally:
            code, _ = worker.close()
            self.assertEqual(code, 0)
