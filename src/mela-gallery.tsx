/* Dev-only gallery: every Mela mood on light, on dark and in a real pill; `?reel` walks
   through the moods one by one. Served by `vite` at /mela-gallery.html; not part of the build. */
import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Mela } from "./components/Mela";
import { MELA_FLAGS, type MelaMood } from "./lib/mela";
import "./styles.css";

const MOODS: MelaMood[] = ["idle", "listen", "think", "work", "happy", "lang", "oops", "private", "sleep", "offline", "egg"];

function Tile({ mood, dark }: { mood: MelaMood; dark: boolean }) {
  return (
    <div style={{ display: "grid", gap: 6, justifyItems: "center" }}>
      <div data-theme={dark ? "dark" : "light"} style={{ position: "relative", width: 120, height: 110, borderRadius: 14, background: dark ? "#1e1e22" : "#ffffff", boxShadow: "inset 0 0 0 1px rgb(0 0 0 / 8%)", overflow: "hidden", transform: "translateZ(0)" }}>
        <Mela place="window" mood={mood} level={0.7} flag={mood === "lang" || mood === "listen" ? MELA_FLAGS["es-AR"] : null} />
      </div>
      <small style={{ font: "12px -apple-system, sans-serif", color: "#666" }}>{mood}</small>
    </div>
  );
}

function Pill({ mood }: { mood: MelaMood }) {
  return (
    <div style={{ display: "grid", gap: 6, justifyItems: "center" }}>
      <div className="overlay-window" style={{ width: 200, height: 110, display: "flex", alignItems: "flex-end", justifyContent: "center", paddingBottom: 12, borderRadius: 14, background: "linear-gradient(135deg,#5a6a9a,#3a3a5a)" }}>
        <div className="overlay-capsule is-listening has-mela" style={{ width: 92 }}>
          <span className="overlay-glow" aria-hidden="true" />
          <div className="overlay-surface"><div className="overlay-content"><span style={{ color: "#eef0f8", font: "13px -apple-system, sans-serif" }}>Listening</span></div></div>
          <Mela place="pill" mood={mood} level={0.7} flag={mood === "lang" || mood === "listen" ? MELA_FLAGS.de : null} />
        </div>
      </div>
      <small style={{ font: "12px -apple-system, sans-serif", color: "#666" }}>{mood}</small>
    </div>
  );
}

/** `?reel`: one large Mela walking through every mood, to judge the motion. */
const REEL: [MelaMood, number][] = [["idle", 3000], ["listen", 3200], ["think", 2600], ["happy", 2400], ["lang", 2600], ["listen", 1600], ["work", 3200], ["oops", 2400], ["private", 2800], ["idle", 1600], ["sleep", 3600], ["offline", 2600], ["egg", 2800]];
const MOOD_LABEL: Record<MelaMood, string> = { idle: "At rest", listen: "Listening", think: "Transcribing", work: "Working on a file", happy: "Done", lang: "New target language", oops: "Something failed", private: "Not looking at your notes", sleep: "Hibernating", offline: "Wi-Fi off", egg: "Groundhog Day" };

function Reel() {
  const [step, setStep] = useState(0);
  const [level, setLevel] = useState(0);
  useEffect(() => {
    const timer = window.setTimeout(() => setStep((current) => (current + 1) % REEL.length), REEL[step][1]);
    return () => window.clearTimeout(timer);
  }, [step]);
  useEffect(() => {
    let t = 0;
    const timer = window.setInterval(() => {
      t += 0.1;
      setLevel(Math.max(0, Math.sin(t * 2.3) * 0.6 + Math.sin(t * 5.1) * 0.25 + 0.25));
    }, 90);
    return () => window.clearInterval(timer);
  }, []);
  const mood = REEL[step][0];
  const flag = mood === "lang" || (mood === "listen" && step > 4) ? MELA_FLAGS["es-AR"] : null;
  const stage = (dark: boolean) => (
    <div data-theme={dark ? "dark" : "light"} style={{ position: "relative", width: 380, height: 380, borderRadius: 24, background: dark ? "#1e1e22" : "#ffffff", boxShadow: "inset 0 0 0 1px rgb(0 0 0 / 8%)", overflow: "hidden", transform: "translateZ(0)" }}>
      <div className="mela-reel" style={{ position: "absolute", left: 90, bottom: 40, width: 200, height: 220 }}>
        <Mela place="window" mood={mood} level={mood === "listen" ? level : 0} flag={flag} />
      </div>
    </div>
  );
  return (
    <div style={{ padding: 30, display: "grid", gap: 16, justifyItems: "center", background: "#f4f4f7", minHeight: "100vh", font: "15px -apple-system, sans-serif" }}>
      <div style={{ display: "flex", gap: 24 }}>{stage(false)}{stage(true)}</div>
      <div style={{ fontSize: 20, color: "#1d1d1f" }}>{MOOD_LABEL[mood]}</div>
    </div>
  );
}

function Gallery() {
  const row = { display: "flex", flexWrap: "wrap" as const, gap: 12 };
  return (
    <div style={{ padding: 20, display: "grid", gap: 18, background: "#f4f4f7", minHeight: "100vh" }}>
      <div style={row}>{MOODS.map((m) => <Tile key={m} mood={m} dark={false} />)}</div>
      <div style={row}>{MOODS.map((m) => <Tile key={m} mood={m} dark />)}</div>
      <div style={row}>{(["listen", "think", "happy", "lang", "oops", "idle"] as MelaMood[]).map((m) => <Pill key={m} mood={m} />)}</div>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<StrictMode>{new URLSearchParams(location.search).has("reel") ? <Reel /> : <Gallery />}</StrictMode>);
