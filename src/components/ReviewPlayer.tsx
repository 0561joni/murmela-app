import { describeError } from "../lib/formatting";
import {
  forwardRef,
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { Button } from "./Feedback";
import { pickAudioFiles } from "../lib/api";
import {
  releaseReviewAudio,
  resolveReviewAudio,
  ReviewApiError,
} from "../lib/reviewApi";
import { timestamp } from "../lib/speakerLabels";
import type { ReviewAudio, ReviewRef } from "../types/domain";
export interface ReviewPlayerHandle {
  seek(ms: number): void;
}
export const ReviewPlayer = memo(
  forwardRef<
    ReviewPlayerHandle,
    {
      reference: ReviewRef;
      durationMs: number | null;
      onTime: (ms: number) => void;
      onResolved: () => void;
    }
  >(function ReviewPlayer({ reference, durationMs, onTime, onResolved }, ref) {
    const audio = useRef<HTMLAudioElement>(null);
    const resource = useRef<ReviewAudio | null>(null);
    const generation = useRef(0);
    const fallbackTried = useRef(false);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState("");
    const [canRelink, setCanRelink] = useState(false);
    const [unverifiable, setUnverifiable] = useState(false);
    const [url, setUrl] = useState<string>();
    const [time, setTime] = useState(0);
    const [duration, setDuration] = useState((durationMs ?? 0) / 1000);
    const [playing, setPlaying] = useState(false);
    const [speed, setSpeed] = useState(1);
    const pendingSeek = useRef<number | null>(null);
    const desiredTime = useRef(0);
    const resumeAfterLoad = useRef(false);
    const play = useCallback(() => {
      void audio.current
        ?.play()
        .catch((e) =>
          setError(
            describeError(e, "Playback could not start."),
          ),
        );
    }, []);
    const seek = useCallback(
      (ms: number) => {
        const el = audio.current;
        if (!el || !resource.current) {
          setError("Load the original audio before playing a passage.");
          return;
        }
        const next = Math.max(0, ms / 1000);
        if (el.readyState < 1) {
          pendingSeek.current = next;
        } else
          el.currentTime = Math.min(
            next,
            Number.isFinite(el.duration) ? el.duration : next,
          );
        play();
      },
      [play],
    );
    useImperativeHandle(ref, () => ({ seek }), [seek]);
    const load = useCallback(
      async (replacementPath: string | null = null, fallback = false) => {
        const token = ++generation.current;
        setLoading(true);
        setError("");
        setUnverifiable(false);
        desiredTime.current = audio.current?.currentTime ?? 0;
        resumeAfterLoad.current = Boolean(
          audio.current && !audio.current.paused,
        );
        audio.current?.pause();
        try {
          const next = await resolveReviewAudio(
            reference,
            replacementPath,
            fallback,
          );
          if (token !== generation.current) {
            void releaseReviewAudio(next.token);
            return;
          }
          const previous = resource.current;
          resource.current = next;
          setUrl(next.url);
          setCanRelink(false);
          if (previous) void releaseReviewAudio(previous.token);
          onResolved();
        } catch (e) {
          if (token !== generation.current) return;
          setError(describeError(e));
          setUnverifiable(
            e instanceof ReviewApiError && e.code === "SOURCE_UNVERIFIABLE",
          );
          setCanRelink(
            e instanceof ReviewApiError &&
              ["SOURCE_FILE_REQUIRED", "SOURCE_FILE_MISMATCH"].includes(e.code),
          );
        } finally {
          if (token === generation.current) setLoading(false);
        }
      },
      [reference.kind, reference.id, onResolved],
    );
    useEffect(() => {
      fallbackTried.current = false;
      void load();
      return () => {
        generation.current++;
        audio.current?.pause();
        const current = resource.current;
        resource.current = null;
        if (current) void releaseReviewAudio(current.token);
      };
    }, [load]);
    const locate = async () => {
      try {
        const [source] = await pickAudioFiles();
        if (source) {
          fallbackTried.current = false;
          await load(source.filePath);
        }
      } catch (e) {
        setError(describeError(e));
      }
    };
    return (
      <section className="review-player surface" aria-label="Audio playback">
        <audio
          ref={audio}
          src={url}
          preload="metadata"
          onPlay={() => setPlaying(true)}
          onPause={() => setPlaying(false)}
          onEnded={() => setPlaying(false)}
          onLoadedMetadata={() => {
            const el = audio.current!;
            setDuration(
              Number.isFinite(el.duration)
                ? el.duration
                : (durationMs ?? 0) / 1000,
            );
            el.playbackRate = speed;
            const target = pendingSeek.current ?? desiredTime.current;
            if (target)
              el.currentTime = Math.min(target, el.duration || target);
            if (pendingSeek.current !== null || resumeAfterLoad.current) play();
            pendingSeek.current = null;
            resumeAfterLoad.current = false;
          }}
          onTimeUpdate={() => {
            const next = audio.current?.currentTime ?? 0;
            setTime(next);
            onTime(next * 1000);
          }}
          onError={() => {
            if (!resource.current) return;
            if (!fallbackTried.current) {
              fallbackTried.current = true;
              void load(null, true);
            } else
              setError(
                "This audio could not be played, including after conversion. You can still read and correct the transcript.",
              );
          }}
        />
        <div className="review-player-controls">
          <Button
            disabled={loading || !url}
            onClick={() => {
              if (playing) audio.current?.pause();
              else play();
            }}
          >
            {playing ? "Pause" : "Play"}
          </Button>
          <Button
            disabled={!url || loading}
            variant="ghost"
            onClick={() => {
              if (audio.current)
                audio.current.currentTime = Math.max(
                  0,
                  audio.current.currentTime - 10,
                );
            }}
            aria-label="Back 10 seconds"
          >
            −10s
          </Button>
          <Button
            disabled={!url || loading}
            variant="ghost"
            onClick={() => {
              if (audio.current)
                audio.current.currentTime = Math.min(
                  duration,
                  audio.current.currentTime + 10,
                );
            }}
            aria-label="Forward 10 seconds"
          >
            +10s
          </Button>
          <span className="review-playback-time" aria-label="Playback time">
            {timestamp(time * 1000)} / {timestamp(duration * 1000)}
          </span>
          <label className="review-speed">
            Speed{" "}
            <select
              aria-label="Playback speed"
              value={speed}
              onChange={(e) => {
                const next = Number(e.target.value);
                setSpeed(next);
                if (audio.current) audio.current.playbackRate = next;
              }}
            >
              {[0.5, 0.75, 1, 1.25, 1.5, 2].map((v) => (
                <option key={v} value={v}>
                  {v}×
                </option>
              ))}
            </select>
          </label>
        </div>
        <input
          type="range"
          className="review-seek"
          aria-label="Seek audio"
          aria-valuetext={`${timestamp(time * 1000)} of ${timestamp(duration * 1000)}`}
          min={0}
          max={duration || 1}
          step={0.1}
          value={Math.min(time, duration || 1)}
          disabled={!url || loading}
          onChange={(e) => {
            const next = Number(e.target.value);
            setTime(next);
            if (audio.current) audio.current.currentTime = next;
            onTime(next * 1000);
          }}
        />
        {loading ? (
          <p className="muted" role="status">
            {fallbackTried.current
              ? "Preparing audio for playback…"
              : "Checking original audio…"}
          </p>
        ) : null}
        {error ? (
          <div className="review-audio-error">
            <p role="status">{error}</p>
            {canRelink ? (
              <Button onClick={() => void locate()}>
                Locate original audio
              </Button>
            ) : !url && !unverifiable ? (
              <Button onClick={() => void load()}>
                Try loading audio again
              </Button>
            ) : null}
          </div>
        ) : null}
        {!loading && !error ? (
          <p className="muted review-audio-note">
            Plays from your original recording. Audio stays on this device.
          </p>
        ) : null}
      </section>
    );
  }),
);
