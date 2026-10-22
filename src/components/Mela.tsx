import { useId, type CSSProperties } from "react";
import type { MelaFlag, MelaMood } from "../lib/mela";
import "../mela.css";

interface MelaProps {
  mood: MelaMood;
  /** "pill" peeks over the dictation pill; "window" sits in the corner of the main window. */
  place: "pill" | "window";
  /** Microphone level 0–1; only read while listening. */
  level?: number;
  /** Target language when translating. */
  flag?: MelaFlag | null;
  /** Ducked out of sight (the pill is closing). */
  away?: boolean;
}

/** Mela the marmot. Purely decorative: hidden from assistive technology and never clickable. */
export function Mela({ mood, place, level = 0, flag = null, away = false }: MelaProps) {
  const css = { "--mela-level": level.toFixed(3) } as CSSProperties;
  return (
    <div
      className={`mela mela-${place}${away ? " is-away" : ""}${flag ? " has-flag" : ""}`}
      data-mood={mood}
      style={css}
      aria-hidden="true"
    >
      <div className="mela-rig">
        <Crystal flag={flag} />
      </div>
    </div>
  );
}

/** Mood class helper: the part shows only in the listed moods. */
const on = (...moods: MelaMood[]) => "mela-pt " + moods.map((mood) => "on-" + mood).join(" ");

/** A flag drawn into a box; three bands, vertical or horizontal, an optional sun. */
function FlagShape({ flag, x, y, w, h, edge = true }: { flag: MelaFlag; x: number; y: number; w: number; h: number; edge?: boolean }) {
  const n = flag.bands.length;
  return (
    <g>
      {flag.bands.map((color, index) =>
        flag.vertical ? (
          <rect key={index} x={x + (w / n) * index} y={y} width={w / n + 0.05} height={h} fill={color} />
        ) : (
          <rect key={index} x={x} y={y + (h / n) * index} width={w} height={h / n + 0.05} fill={color} />
        ),
      )}
      {flag.sun ? <circle cx={x + w / 2} cy={y + h / 2} r={h / 7} fill="#f6b40e" /> : null}
      {edge ? <rect x={x} y={y} width={w} height={h} fill="none" stroke="rgb(0 0 0 / 22%)" strokeWidth={0.5} /> : null}
    </g>
  );
}

const BEAD = "#1c1a28";

/* A clear glass figurine, standing with her paws at her chest. Built for movement:
   paws, eyes, head and flag are persistent parts that travel between poses (CSS
   transitions in mela.css) instead of being swapped. */

const CRYSTAL_BODY =
  "M50 17C32 17 22.5 26.5 22.5 38.5C22.5 46.5 26.5 51.5 31 55C24 62 21 72 22 82C23 92 32 97 50 97C68 97 77 92 78 82C79 72 76 62 69 55C73.5 51.5 77.5 46.5 77.5 38.5C77.5 26.5 68 17 50 17Z";

function Crystal({ flag }: { flag: MelaFlag | null }) {
  const id = useId().replace(/:/g, "");
  const glass = `url(#${id}-glass)`;
  const rim = `url(#${id}-rim)`;
  // Arms grow from the shoulder (stroke-dashoffset) while the paw travels to its spot.
  const limb = (moods: MelaMood[], d: string) => (
    <path className={on(...moods) + " mela-limb"} d={d} pathLength={1} stroke={glass} strokeWidth={7} fill="none" strokeLinecap="round" />
  );
  const paw = (side: "left" | "right", cx: number) => (
    <g className={`mela-paw is-${side}`}>
      <g className="mela-paw-wave">
        <ellipse cx={cx} cy={60} rx={4.6} ry={3.7} fill={`url(#${id}-paw)`} stroke={rim} strokeWidth={0.7} />
      </g>
    </g>
  );
  const bead = (x: number) => (
    <g key={x}>
      <circle cx={x} cy={34} r={2.7} fill={BEAD} />
      <circle cx={x + 1} cy={33} r={0.9} fill="#ffffff" opacity={0.92} />
    </g>
  );
  return (
    <svg viewBox="0 0 100 100" className="mela-art mela-crystal-art">
      <defs>
        <linearGradient id={`${id}-glass`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="var(--mela-glass-top)" />
          <stop offset="1" stopColor="var(--mela-glass-bottom)" />
        </linearGradient>
        <linearGradient id={`${id}-paw`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="var(--mela-glass-paw)" />
          <stop offset="1" stopColor="var(--mela-glass-top)" />
        </linearGradient>
        <linearGradient id={`${id}-rim`} x1="0.2" y1="0" x2="0.8" y2="1">
          <stop offset="0" stopColor="#ffffff" stopOpacity={0.95} />
          <stop offset="0.45" stopColor="#ffffff" stopOpacity={0.25} />
          <stop offset="1" stopColor="#8b5cff" stopOpacity={0.55} />
        </linearGradient>
        <linearGradient id={`${id}-volume`} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" stopColor="#ffffff" stopOpacity={0.22} />
          <stop offset="0.55" stopColor="#ffffff" stopOpacity={0} />
          <stop offset="1" stopColor="var(--mela-glass-shade)" />
        </linearGradient>
        <radialGradient id={`${id}-caustic`} cx="0.62" cy="0.92" r="0.55">
          <stop offset="0" stopColor="#e25cff" stopOpacity={0.42} />
          <stop offset="0.45" stopColor="#3c5bff" stopOpacity={0.22} />
          <stop offset="1" stopColor="#22c7ff" stopOpacity={0} />
        </radialGradient>
        <radialGradient id={`${id}-sheen`} cx="0.3" cy="0.18" r="0.5">
          <stop offset="0" stopColor="#ffffff" stopOpacity={0.8} />
          <stop offset="1" stopColor="#ffffff" stopOpacity={0} />
        </radialGradient>
        <clipPath id={`${id}-clip`}><path d={CRYSTAL_BODY} /></clipPath>
      </defs>
      <ellipse className="mela-crystal-shadow" cx={50} cy={98.4} rx={22} ry={2.4} />
      <ellipse className="mela-crystal-groundhog" cx={80} cy={98} rx={15} ry={2.2} />
      <g className="mela-motion">
        <g className="mela-ear is-left"><g className="mela-ear-twitch">
          <circle cx={27.5} cy={25} r={6} fill={glass} stroke={rim} strokeWidth={0.9} />
          <circle cx={27.8} cy={25.4} r={2.8} className="mela-crystal-inner-ear" />
        </g></g>
        <g className="mela-ear is-right"><g className="mela-ear-twitch">
          <circle cx={72.5} cy={25} r={6} fill={glass} stroke={rim} strokeWidth={0.9} />
          <circle cx={72.2} cy={25.4} r={2.8} className="mela-crystal-inner-ear" />
        </g></g>
        {/* Raised arms sit behind the body, so they come out from the shoulder */}
        {limb(["oops"], "M31 60Q21 57 17 51")}
        {limb(["oops"], "M69 60Q79 57 83 51")}
        {limb(["egg"], "M31 58Q18 46 20 29")}
        {limb(["egg"], "M69 58Q82 46 80 29")}
        {limb(["happy"], "M69 58Q81 52 82 41")}
        {limb(["lang"], "M69 58Q80 52 80 44")}
        <g className="mela-crystal-flag">
          <line x1={80} y1={44} x2={80} y2={7} className="mela-crystal-pole" />
          {flag ? <FlagShape flag={flag} x={80} y={7} w={18} h={12} /> : null}
          <rect x={80} y={7} width={18} height={5.2} fill="#ffffff" opacity={0.24} />
        </g>
        <path d={CRYSTAL_BODY} className="mela-crystal-edge" />
        <path d={CRYSTAL_BODY} fill={glass} />
        <g clipPath={`url(#${id}-clip)`}>
          <rect x={0} y={0} width={100} height={100} fill={`url(#${id}-volume)`} />
          <rect x={0} y={50} width={100} height={50} fill={`url(#${id}-caustic)`} />
          <ellipse cx={50} cy={77} rx={14.5} ry={16} fill="#ffffff" opacity={0.2} />
          <ellipse cx={36} cy={30} rx={13} ry={16} fill={`url(#${id}-sheen)`} />
        </g>
        <path d={CRYSTAL_BODY} fill="none" stroke={rim} strokeWidth={1.1} />
        {/* Specular streaks: the light catching the top of the head and the flank */}
        <path d="M29.5 31C31.5 24.5 37 20.6 44 19.6" className="mela-crystal-glint" />
        <path d="M27.5 66C26.3 71 26.4 77 28 82" className="mela-crystal-glint is-soft" />
        <g className="mela-face">
          <ellipse cx={50} cy={44.5} rx={13} ry={8.4} fill="#ffffff" opacity={0.42} stroke="#ffffff" strokeOpacity={0.7} strokeWidth={0.6} />
          <path d="M46.8 39.6Q50 37.8 53.2 39.6Q52 42.4 50 42.6Q48 42.4 46.8 39.6Z" fill={BEAD} opacity={0.88} />
          <path d="M50 42.6v1.5" stroke={BEAD} strokeWidth={0.9} strokeLinecap="round" opacity={0.7} />
          <rect x={48.1} y={44.2} width={3.8} height={4} rx={0.9} fill="#ffffff" stroke="rgb(28 26 40 / 30%)" strokeWidth={0.45} />
          <line x1={50} y1={44.3} x2={50} y2={48.1} stroke="rgb(28 26 40 / 30%)" strokeWidth={0.45} />
          <g className="mela-eyes">
            <g className={on("idle", "listen", "think", "work", "lang", "oops", "offline", "sleep", "private") + " mela-lids"}>
              <g className="mela-blink">{bead(40.5)}{bead(59.5)}</g>
            </g>
            <g className={on("happy", "egg")}>
              {[40.5, 59.5].map((x) => <path key={x} d={`M${x - 3} 35 q3 -3.9 6 0`} stroke={BEAD} strokeWidth={1.6} fill="none" strokeLinecap="round" />)}
            </g>
          </g>
          <path className={on("oops")} d="M37 28.6l6-1.2M57 27.4l6 1.2" stroke={BEAD} strokeWidth={1.1} strokeLinecap="round" opacity={0.7} />
          <g className="mela-crystal-glasses">
            <circle cx={40.5} cy={34.6} r={5} fill="none" stroke={BEAD} strokeWidth={0.9} opacity={0.75} />
            <circle cx={59.5} cy={34.6} r={5} fill="none" stroke={BEAD} strokeWidth={0.9} opacity={0.75} />
            <path d="M45.5 34.4h9" stroke={BEAD} strokeWidth={0.9} opacity={0.75} />
          </g>
          <g className="mela-crystal-shades">
            <rect x={34} y={30.8} width={13} height={7} rx={3} fill={BEAD} opacity={0.92} />
            <rect x={53} y={30.8} width={13} height={7} rx={3} fill={BEAD} opacity={0.92} />
            <path d="M47 33.2h6" stroke={BEAD} strokeWidth={1.4} />
            <path d="M36 32.6l3-.9M55 32.6l3-.9" stroke="#ffffff" strokeWidth={0.7} opacity={0.6} />
          </g>
        </g>
        {/* Arms that cross the body (covering the eyes) are drawn in front of it */}
        {limb(["private"], "M34 59Q31 46 37 38")}
        {limb(["private"], "M66 59Q69 46 63 38")}
        {paw("left", 44)}
        {paw("right", 56)}
        <ellipse cx={42} cy={96} rx={6.5} ry={2.8} fill={glass} stroke={rim} strokeWidth={0.6} />
        <ellipse cx={58} cy={96} rx={6.5} ry={2.8} fill={glass} stroke={rim} strokeWidth={0.6} />
        {flag ? <g className="mela-badge"><FlagShape flag={flag} x={60} y={84} w={11} h={7} /></g> : null}
      </g>
    </svg>
  );
}
