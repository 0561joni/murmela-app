import {
  forwardRef,
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  needsSpeakerReview,
  speakerLabel,
  timestamp,
} from "../lib/speakerLabels";
import type { TranscriptSegment, TranscriptSpeaker } from "../types/domain";
export interface PassageListHandle {
  reveal(id: string): void;
}
interface Props {
  segments: TranscriptSegment[];
  speakers: Map<string, TranscriptSpeaker>;
  manual: Set<string>;
  selected: Set<string>;
  activeId: string | null;
  onSelect: (id: string) => void;
  onAssign: (id: string) => void;
  onSeek: (ms: number) => void;
  onManualScroll: () => void;
}
export const VirtualPassages = forwardRef<PassageListHandle, Props>(
  function VirtualPassages(props, ref) {
    const viewport = useRef<HTMLDivElement>(null);
    const heights = useRef(new Map<string, number>());
    const [measurement, setMeasurement] = useState(0);
    const [scroll, setScroll] = useState(0);
    const [height, setHeight] = useState(600);
    const [focused, setFocused] = useState<string | null>(null);
    const frame = useRef<number | null>(null);
    const offsets = useMemo(() => {
      const result = new Float64Array(props.segments.length + 1);
      props.segments.forEach((s, i) => {
        result[i + 1] =
          result[i] +
          (heights.current.get(s.id) ??
            80 + Math.ceil(s.text.length / 80) * 24);
      });
      return result;
    }, [props.segments, measurement]);
    const ids = useMemo(
      () => new Map(props.segments.map((s, i) => [s.id, i])),
      [props.segments],
    );
    const shape = useMemo(
      () => props.segments.map((s) => s.id).join("\0"),
      [props.segments],
    );
    useLayoutEffect(() => {
      if (viewport.current) viewport.current.scrollTop = 0;
      setScroll(0);
    }, [shape]);
    const measure = useCallback((id: string, value: number) => {
      if (Math.abs((heights.current.get(id) ?? 0) - value) < 1) return;
      heights.current.set(id, value);
      if (frame.current === null)
        frame.current = requestAnimationFrame(() => {
          frame.current = null;
          setMeasurement((v) => v + 1);
        });
    }, []);
    useEffect(
      () => () => {
        if (frame.current !== null) cancelAnimationFrame(frame.current);
      },
      [],
    );
    useLayoutEffect(() => {
      const el = viewport.current;
      if (!el) return;
      let width = el.clientWidth;
      if (typeof ResizeObserver === "undefined") return;
      const observer = new ResizeObserver(() => {
        setHeight(el.clientHeight || 600);
        if (width !== el.clientWidth) {
          width = el.clientWidth;
          heights.current.clear();
          setMeasurement((v) => v + 1);
        }
      });
      observer.observe(el);
      return () => observer.disconnect();
    }, []);
    useImperativeHandle(
      ref,
      () => ({
        reveal(id) {
          const i = ids.get(id);
          const el = viewport.current;
          if (i === undefined || !el) return;
          el.scrollTop = Math.max(0, offsets[i] - 32);
          setScroll(el.scrollTop);
        },
      }),
      [ids, offsets],
    );
    const lower = (value: number) => {
      let a = 0,
        b = props.segments.length;
      while (a < b) {
        const m = (a + b) >>> 1;
        if (offsets[m + 1] < value) a = m + 1;
        else b = m;
      }
      return a;
    };
    const first = Math.max(0, lower(scroll) - 5);
    const last = Math.min(props.segments.length, lower(scroll + height) + 6);
    const visible = new Set(
      Array.from({ length: Math.max(0, last - first) }, (_, i) => i + first),
    );
    const focusedIndex = focused ? ids.get(focused) : undefined;
    if (focusedIndex !== undefined) visible.add(focusedIndex);
    return (
      <div
        ref={viewport}
        className="review-passages"
        style={{
          height: `min(62vh, ${Math.max(100, offsets[props.segments.length])}px)`,
        }}
        role="region"
        aria-label="Transcript passages"
        tabIndex={0}
        onScroll={(e) => setScroll(e.currentTarget.scrollTop)}
        onWheel={props.onManualScroll}
        onTouchMove={props.onManualScroll}
        onPointerDown={(e) => {
          if (!(e.target as HTMLElement).closest("button,input"))
            props.onManualScroll();
        }}
        onKeyDown={(e) => {
          if (
            [
              "PageDown",
              "PageUp",
              "Home",
              "End",
              "ArrowDown",
              "ArrowUp",
            ].includes(e.key)
          )
            props.onManualScroll();
        }}
      >
        <div
          className="review-passages-canvas"
          style={{ height: offsets[props.segments.length] }}
        >
          {[...visible]
            .sort((a, b) => a - b)
            .map((i) => {
              const segment = props.segments[i];
              return (
                <Passage
                  key={segment.id}
                  segment={segment}
                  top={offsets[i]}
                  index={i}
                  total={props.segments.length}
                  label={
                    props.manual.has(segment.id) &&
                    segment.speakerAttribution === "none"
                      ? "Unknown speaker"
                      : speakerLabel(segment, props.speakers)
                  }
                  color={
                    props.speakers.get(
                      segment.speakerId ?? segment.speakerIds?.[0] ?? "",
                    )?.speakerOrder ?? 0
                  }
                  manual={props.manual.has(segment.id)}
                  needsReview={needsSpeakerReview(segment, props.manual)}
                  selected={props.selected.has(segment.id)}
                  active={props.activeId === segment.id}
                  measure={measure}
                  onSelect={props.onSelect}
                  onAssign={props.onAssign}
                  onSeek={props.onSeek}
                  onFocus={setFocused}
                />
              );
            })}
        </div>
        {!props.segments.length ? (
          <p className="muted review-empty">No passages match this filter.</p>
        ) : null}
      </div>
    );
  },
);
const Passage = memo(function Passage({
  segment,
  top,
  index,
  total,
  label,
  color,
  manual,
  needsReview,
  selected,
  active,
  measure,
  onSelect,
  onAssign,
  onSeek,
  onFocus,
}: {
  segment: TranscriptSegment;
  top: number;
  index: number;
  total: number;
  label: string;
  color: number;
  manual: boolean;
  needsReview: boolean;
  selected: boolean;
  active: boolean;
  measure: (id: string, height: number) => void;
  onSelect: (id: string) => void;
  onAssign: (id: string) => void;
  onSeek: (ms: number) => void;
  onFocus: (id: string | null) => void;
}) {
  const ref = useRef<HTMLElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const update = () => measure(segment.id, el.getBoundingClientRect().height);
    update();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(update);
    observer.observe(el);
    return () => observer.disconnect();
  }, [segment.id, measure]);
  return (
    <article
      ref={ref}
      className={`review-passage${active ? " is-playing" : ""}${selected ? " is-selected" : ""}`}
      style={{ transform: `translateY(${top}px)` }}
      aria-label={`Passage ${index + 1} of ${total}`}
      aria-current={active ? "true" : undefined}
      onFocus={() => onFocus(segment.id)}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node)) onFocus(null);
      }}
    >
      <input
        type="checkbox"
        checked={selected}
        aria-label={`Select passage at ${timestamp(segment.startMs)}`}
        onChange={() => onSelect(segment.id)}
      />
      <div className="review-passage-content">
        <div className="review-passage-meta">
          <button
            className="review-timestamp"
            onClick={() => onSeek(segment.startMs)}
            aria-label={`Play from ${timestamp(segment.startMs)}`}
          >
            {timestamp(segment.startMs)}
          </button>
          <button
            className={`speaker-label speaker-color-${color % 6}`}
            onClick={() => onAssign(segment.id)}
            aria-label={`Change speaker for passage at ${timestamp(segment.startMs)}`}
          >
            {label || "Assign speaker"}
          </button>
          {manual ? (
            <span className="review-manual">Manually assigned</span>
          ) : needsReview && label ? (
            <span className="review-uncertain">Needs review</span>
          ) : null}
        </div>
        <p>{segment.text}</p>
      </div>
    </article>
  );
});
