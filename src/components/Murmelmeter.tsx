import { usePlayful } from "../lib/appearance";
import {
  formatCount,
  formatMinutesSaved,
  minutesSaved,
  wordComparison,
} from "../lib/murmelmeter";
import type { DictationStats } from "../types/domain";

/** How much you have murmured: words, time saved and streak. Hidden in Serious mode. */
export function Murmelmeter({ stats }: { stats: DictationStats | null }) {
  const playful = usePlayful();
  if (!playful || !stats) return null;
  const saved = minutesSaved(stats.totalWords, stats.totalDurationMs);
  return (
    <section className="murmelmeter" aria-labelledby="murmelmeter-heading">
      <div className="murmelmeter-header">
        <h2 id="murmelmeter-heading" className="section-title">Murmelmeter</h2>
        <p className="murmelmeter-quip">{wordComparison(stats.totalWords)}</p>
      </div>
      {stats.totalWords > 0 ? (
        <dl className="murmelmeter-stats">
          <div>
            <dt>Today</dt>
            <dd>{formatCount(stats.todayWords)} <span>words</span></dd>
          </div>
          <div>
            <dt>All time</dt>
            <dd>{formatCount(stats.totalWords)} <span>words</span></dd>
          </div>
          <div title="Compared with typing at 40 words per minute">
            <dt>Time saved</dt>
            <dd>{formatMinutesSaved(saved)}</dd>
          </div>
          <div>
            <dt>Streak</dt>
            <dd>
              {stats.streakDays} <span>{stats.streakDays === 1 ? "day" : "days"}</span>
            </dd>
          </div>
        </dl>
      ) : null}
    </section>
  );
}
