import { useEffect, useRef } from "react";

export type WaveformMode = "listening" | "processing" | "rest";

/** Smoothed microphone level shared with the pill's glow, 0…1. */
export type LevelSink = (level: number) => void;

function reducedMotion() {
  return (
    document.documentElement.dataset.motion === "reduced" ||
    Boolean(window.matchMedia?.("(prefers-reduced-motion: reduce)").matches)
  );
}

/**
 * The overlay's voice waveform. Levels arrive about ten times a second; the bars
 * are animated every frame so they flow instead of stepping. Each bar reads its
 * own slice of the brand gradient, so together they span cyan → magenta.
 */
export function OverlayWaveform({
  level,
  mode,
  bars = 13,
  onLevel,
}: {
  level: number;
  mode: WaveformMode;
  bars?: number;
  onLevel?: LevelSink;
}) {
  const container = useRef<HTMLDivElement>(null);
  const target = useRef({ level, mode });
  target.current = { level, mode };
  const sink = useRef(onLevel);
  sink.current = onLevel;

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    const spans = Array.from(element.children) as HTMLElement[];
    // Taller in the middle, like a voice print.
    const weights = spans.map((_, index) => 0.38 + 0.62 * Math.sin((Math.PI * (index + 0.5)) / spans.length));
    const phases = spans.map((_, index) => index * 1.7 + (index % 3) * 0.9);
    const heights = spans.map(() => 0.14);
    let smoothed = 0;

    const paint = (now: number) => {
      const { level: incoming, mode: current } = target.current;
      smoothed += (incoming - smoothed) * 0.16;
      const t = now / 1000;
      spans.forEach((span, index) => {
        let goal: number;
        if (current === "listening") {
          const flutter = 0.62 + 0.38 * Math.sin(t * (5.2 + index * 0.37) + phases[index]);
          goal = 0.14 + smoothed * weights[index] * flutter * 0.86;
        } else if (current === "processing") {
          goal = 0.16 + 0.3 * (0.5 + 0.5 * Math.sin(t * 5.5 - index * 0.6));
        } else {
          goal = 0.14;
        }
        heights[index] += (goal - heights[index]) * 0.24;
        span.style.transform = `scaleY(${heights[index].toFixed(3)})`;
      });
      sink.current?.(current === "listening" ? smoothed : current === "processing" ? 0.25 : 0);
    };

    if (reducedMotion()) {
      paint(0);
      return;
    }
    let frame = requestAnimationFrame(function loop(now) {
      paint(now);
      frame = requestAnimationFrame(loop);
    });
    return () => cancelAnimationFrame(frame);
  }, [bars]);

  // Reduced motion: no animation loop, but still follow the level.
  useEffect(() => {
    if (!reducedMotion() || !container.current) return;
    const spans = Array.from(container.current.children) as HTMLElement[];
    spans.forEach((span, index) => {
      const weight = 0.38 + 0.62 * Math.sin((Math.PI * (index + 0.5)) / spans.length);
      const height = mode === "listening" ? 0.14 + level * weight * 0.86 : mode === "processing" ? 0.3 : 0.14;
      span.style.transform = `scaleY(${height.toFixed(3)})`;
    });
  }, [level, mode]);

  return (
    <div ref={container} className={`overlay-wave is-${mode}`} aria-hidden="true">
      {Array.from({ length: bars }, (_, index) => (
        <span
          key={index}
          style={{ backgroundPosition: `${bars > 1 ? (index / (bars - 1)) * 100 : 50}% 0`, backgroundSize: `${bars * 100}% 100%` }}
        />
      ))}
    </div>
  );
}
