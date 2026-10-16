import { describeError, formatShortDate } from "../lib/formatting";
import { useEffect, useState } from "react";
import { listTranscripts } from "../lib/api";
import { PageHeader } from "../components/Feedback";
import { AppIcon, IconButton } from "../components/IconButton";
import { usePlayful } from "../lib/appearance";
import {
  deleteTextTranslation,
  languageName,
  listTextTranslations,
  type TextTranslationSummary,
} from "../lib/textTranslation";
import type { TranscriptSummary } from "../types/domain";

interface HistoryScreenProps {
  transcripts: TranscriptSummary[];
  /** Opens a saved transcript on the Review screen. */
  onReview: (id: string) => void;
  onDeleteAll: () => Promise<void>;
  /** Changes whenever the Translate screen saves; reloads the Translations list. */
  translationsVersion?: number;
  onOpenTranslation?: (id: string) => void;
}

export function HistoryScreen({
  transcripts,
  onReview,
  onDeleteAll,
  translationsVersion = 0,
  onOpenTranslation,
}: HistoryScreenProps) {
  const playful = usePlayful();
  const [query, setQuery] = useState("");
  const [sourceFilter, setSourceFilter] = useState<
    "all" | "quick_dictate" | "file_upload" | "meeting" | "translations"
  >("all");
  const [translations, setTranslations] = useState<TextTranslationSummary[]>([]);
  const [translationsError, setTranslationsError] = useState("");
  const showTranslations = sourceFilter === "translations";
  useEffect(() => {
    if (!showTranslations) return;
    let disposed = false;
    const timeout = window.setTimeout(() => {
      void listTextTranslations(query)
        .then((items) => {
          if (!disposed) {
            setTranslations(items);
            setTranslationsError("");
          }
        })
        .catch((error) => {
          if (!disposed) setTranslationsError(describeError(error, "Translations are unavailable."));
        });
    }, query.trim() ? 200 : 0);
    return () => {
      disposed = true;
      window.clearTimeout(timeout);
    };
  }, [showTranslations, query, translationsVersion, transcripts]);
  const [searchResults, setSearchResults] = useState<
    TranscriptSummary[] | null
  >(null);
  const [searchError, setSearchError] = useState("");
  const [searching, setSearching] = useState(false);
  const [confirmDeleteAll, setConfirmDeleteAll] = useState(false);
  const [isDeletingAll, setIsDeletingAll] = useState(false);

  useEffect(() => {
    let disposed = false;
    if (!query.trim()) {
      setSearchResults(null);
      setSearching(false);
      setSearchError("");
      return;
    }
    setSearching(true);
    const timeout = window.setTimeout(() => {
      void listTranscripts(query)
        .then((results) => {
          if (!disposed) {
            setSearchResults(results);
            setSearchError("");
          }
        })
        .catch((error) => {
          if (!disposed)
            setSearchError(
              describeError(error, "Search unavailable."),
            );
        })
        .finally(() => {
          if (!disposed) setSearching(false);
        });
    }, 200);
    return () => {
      disposed = true;
      window.clearTimeout(timeout);
    };
  }, [query, transcripts]);

  const visibleTranscripts = (searchResults ?? transcripts).filter(
    (item) =>
      (sourceFilter === "all" || item.sourceType === sourceFilter) &&
      (searchResults !== null || !query.trim() ||
        (item.title + " " + item.plainText)
          .toLowerCase()
          .includes(query.trim().toLowerCase())),
  );
  async function confirmDelete() {
    setIsDeletingAll(true);
    try {
      await onDeleteAll();
      setConfirmDeleteAll(false);
      setSearchResults(null);
    } catch (error) {
      setSearchError(
        describeError(error, "Could not delete transcripts."),
      );
    } finally {
      setIsDeletingAll(false);
    }
  }

  return (
    <section className="screen library-screen">
      <div className="history-screen-layout">
        <article className="glass-panel history-toolbar-panel">
          <PageHeader
            title="Library"
            description="Find a thought. Pick up where you left off."
          >
            <span className="status-pill">{transcripts.length} saved</span>
          </PageHeader>
          <div className="history-toolbar-actions">
            <label className="search-field">
              <AppIcon name="book" />
              <input
                type="search"
                aria-label="Search transcripts"
                placeholder="Search your transcripts…"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
              />
            </label>
            <div className="segmented-control" aria-label="Filter transcripts">
              {(
                [
                  ["all", "All"],
                  ["quick_dictate", "Dictation"],
                  ["file_upload", "Files"],
                  ["meeting", "Meetings"],
                  ["translations", "Translations"],
                ] as const
              ).map(([value, label]) => (
                <button
                  key={value}
                  aria-pressed={sourceFilter === value}
                  onClick={() => {
                    setSourceFilter(value);
                  }}
                >
                  {label}
                </button>
              ))}
            </div>
            {transcripts.length > 0 ? (
              <IconButton
                icon="trashMultiple"
                label="Delete all transcripts"
                tone="danger"
                state={isDeletingAll ? "busy" : "default"}
                disabled={isDeletingAll}
                onClick={() => setConfirmDeleteAll(true)}
              />
            ) : null}
          </div>
          {confirmDeleteAll ? (
            <div className="glass-subtle confirm-panel history-confirm-panel">
              <p className="transcript-title">Delete everything in the Library?</p>
              <p className="muted">
                This removes all saved transcripts, translations and speaker
                names from this computer. It can't be undone.
              </p>
              <div className="toolbar action-segment">
                <button
                  className="danger-button"
                  disabled={isDeletingAll}
                  onClick={() => void confirmDelete()}
                >
                  {isDeletingAll ? "Deleting…" : "Yes, delete everything"}
                </button>
                <button
                  disabled={isDeletingAll}
                  onClick={() => setConfirmDeleteAll(false)}
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : null}
        </article>

        {searchError || (showTranslations && translationsError) ? (
          <p className="error-text" role="alert">
            {showTranslations ? translationsError : searchError}
          </p>
        ) : null}
        <div className="library-layout">
          <aside
            className="library-list surface"
            aria-label="Saved transcripts"
          >
            <div className="library-list-header" role="status">
              {showTranslations
                ? translations.length + (translations.length === 1 ? " translation" : " translations")
                : searching
                  ? "Searching…"
                  : visibleTranscripts.length + " transcripts"}
            </div>
            {showTranslations
              ? translations.map((item) => (
                  <div className="library-row-wrap" key={item.id}>
                    <button
                      className="library-row"
                      aria-label={"Open translation " + item.title}
                      onClick={() => onOpenTranslation?.(item.id)}
                    >
                      <span className="library-row-meta">
                        <AppIcon name="languages" />
                        {formatShortDate(item.updatedAt)} ·{" "}
                        {item.sourceLanguage ? languageName(item.sourceLanguage) + " → " : "→ "}
                        {languageName(item.targetLanguage)}
                      </span>
                      <strong>{item.title}</strong>
                      <span className="clamped-text muted">{item.snippet}</span>
                    </button>
                    <IconButton
                      icon="trash"
                      label={"Delete translation " + item.title}
                      size="compact"
                      tone="danger"
                      onClick={async () => {
                        await deleteTextTranslation(item.id);
                        setTranslations((current) => current.filter((entry) => entry.id !== item.id));
                      }}
                    />
                  </div>
                ))
              : null}
            {showTranslations && translations.length === 0 ? (
              <div className="empty-state">
                <AppIcon name="languages" />
                <h3>{query ? "No matching translations" : "No translations yet"}</h3>
                <p className="muted">
                  {query
                    ? "Try a different search."
                    : "Texts you translate on the Translate screen are kept here while history is on."}
                </p>
              </div>
            ) : null}
            {showTranslations ? null : visibleTranscripts.map((item) => (
              <button
                className="library-row"
                key={item.id}
                aria-label={"Open transcript " + item.title}
                onClick={() => onReview(item.id)}
              >
                <span className="library-row-meta">
                  <AppIcon
                    name={
                      item.sourceType === "file_upload"
                        ? "folder"
                        : item.sourceType === "meeting"
                          ? "headset"
                          : "microphone"
                    }
                  />
                  {formatShortDate(item.createdAt)}
                </span>
                <strong>{item.title}</strong>
                <span className="clamped-text muted">{item.plainText}</span>
              </button>
            ))}
            {!showTranslations && visibleTranscripts.length === 0 ? (
              <div className="empty-state">
                <AppIcon name="book" />
                <h3>
                  {query || sourceFilter !== "all"
                    ? "No matching transcripts"
                    : playful
                      ? "Nothing here yet"
                      : "Your words belong here"}
                </h3>
                <p className="muted">
                  {query || sourceFilter !== "all"
                    ? "Try a different search or filter."
                    : playful
                      ? "Say something brilliant — or anything, really. Saved dictations and files land here."
                      : "Saved dictations and files will appear here."}
                </p>
              </div>
            ) : null}
          </aside>
          <div className="library-reader">
            <article className="glass-panel history-card empty-state">
              <AppIcon name="book" />
              <div>
                <h2>A little space to reflect.</h2>
                <p className="muted">
                  {showTranslations
                    ? "Choose a translation to open it on the Translate screen."
                    : "Choose a transcript to read, copy, or export."}
                </p>
              </div>
            </article>
          </div>
        </div>
      </div>
    </section>
  );
}
