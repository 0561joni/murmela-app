import { describeError } from "../lib/formatting";
import { useState, type FormEvent } from "react";
import { usePlayful } from "../lib/appearance";
import { PageHeader } from "../components/Feedback";
import type {
  CreateVocabularyTermInput,
  UpdateVocabularyTermInput,
  VocabularyTerm,
} from "../types/domain";

interface VocabularyScreenProps {
  vocabularyTerms: VocabularyTerm[];
  onCreateVocabularyTerm: (input: CreateVocabularyTermInput) => Promise<void>;
  onUpdateVocabularyTerm: (
    termId: string,
    input: UpdateVocabularyTermInput,
  ) => Promise<void>;
  onDeleteVocabularyTerm: (termId: string) => Promise<void>;
}

type EditorDraft = {
  canonical: string;
  aliases: string;
  correctCloseMatches: boolean;
};

const EMPTY_EDITOR: EditorDraft = {
  canonical: "",
  aliases: "",
  correctCloseMatches: true,
};

export function VocabularyScreen({
  vocabularyTerms,
  onCreateVocabularyTerm,
  onUpdateVocabularyTerm,
  onDeleteVocabularyTerm,
}: VocabularyScreenProps) {
  const playful = usePlayful();
  const [query, setQuery] = useState("");
  const [savedMessage, setSavedMessage] = useState("");
  const [isSaving, setIsSaving] = useState(false);
  const [createDraft, setCreateDraft] = useState(EMPTY_EDITOR);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState(EMPTY_EDITOR);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [builtinsOpen, setBuiltinsOpen] = useState(false);

  const customTerms = vocabularyTerms
    .filter(
      (term) =>
        !term.isBuiltin &&
        (
          term.canonical +
          " " +
          term.aliases.map((alias) => alias.alias).join(" ")
        )
          .toLowerCase()
          .includes(query.trim().toLowerCase()),
    )
    .sort((left, right) => left.canonical.localeCompare(right.canonical));
  const builtinTerms = vocabularyTerms
    .filter(
      (term) =>
        term.isBuiltin &&
        (
          term.canonical +
          " " +
          term.aliases.map((alias) => alias.alias).join(" ")
        )
          .toLowerCase()
          .includes(query.trim().toLowerCase()),
    )
    .sort((left, right) => left.canonical.localeCompare(right.canonical));

  async function createTerm(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (createDraft.canonical.trim().length === 0) return;

    setIsSaving(true);
    setSavedMessage("");
    setErrorMessage(null);
    try {
      await onCreateVocabularyTerm(normalizeEditorInput(createDraft));
      setCreateDraft({ ...EMPTY_EDITOR });
      setSavedMessage("Term added");
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to create vocabulary term."),
      );
    } finally {
      setIsSaving(false);
    }
  }

  function beginEdit(term: VocabularyTerm) {
    setErrorMessage(null);
    setEditingId(term.id);
    setEditDraft({
      canonical: term.canonical,
      aliases: term.aliases.map((alias) => alias.alias).join(", "),
      correctCloseMatches: term.matchMode === "exact_and_fuzzy",
    });
  }

  function cancelEdit() {
    setEditingId(null);
    setEditDraft({ ...EMPTY_EDITOR });
  }

  async function saveEdit(event: FormEvent<HTMLFormElement>, termId: string) {
    event.preventDefault();
    if (editDraft.canonical.trim().length === 0) return;

    setIsSaving(true);
    setSavedMessage("");
    setErrorMessage(null);
    try {
      await onUpdateVocabularyTerm(termId, normalizeEditorInput(editDraft));
      cancelEdit();
      setSavedMessage("Changes saved");
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to update vocabulary term."),
      );
    } finally {
      setIsSaving(false);
    }
  }

  async function deleteTerm(termId: string) {
    setIsSaving(true);
    setSavedMessage("");
    setErrorMessage(null);
    try {
      await onDeleteVocabularyTerm(termId);
      setSavedMessage("Term deleted");
      if (editingId === termId) cancelEdit();
    } catch (error) {
      setErrorMessage(
        describeError(error, "Failed to delete vocabulary term."),
      );
    } finally {
      setIsSaving(false);
    }
  }

  return (
    <section className="screen vocabulary-screen">
      <PageHeader
        title="Vocabulary"
        description="Teach Murmela the exact spelling of names, brands and specialist terms."
      />
      <div className="vocabulary-workspace">

        <article className="glass-panel vocabulary-form-card">
          <div className="vocabulary-section-heading">
            <div>
              <h3>Add a term</h3>
              <p className="muted">
                Add the spelling you want to see in future transcripts.
              </p>
            </div>
          </div>

          {savedMessage ? (
            <p className="inline-feedback" role="status">
              {savedMessage}
            </p>
          ) : null}
          {errorMessage ? (
            <p className="error-text" role="alert">
              {errorMessage}
            </p>
          ) : null}

          <form
            className="vocabulary-form"
            aria-label="Add vocabulary term"
            onSubmit={createTerm}
          >
            <VocabularyEditorFields
              draft={createDraft}
              disabled={isSaving}
              idPrefix="create-vocabulary"
              switchLabel="Correct close matches for new term"
              onChange={setCreateDraft}
            />
            <div className="vocabulary-form-actions">
              <button
                type="submit"
                className="primary-button vocabulary-primary-action"
                disabled={isSaving || createDraft.canonical.trim().length === 0}
              >
                {isSaving ? "Saving…" : "Add term"}
              </button>
            </div>
          </form>
        </article>

        <label className="search-field">
          <input
            type="search"
            aria-label="Search vocabulary"
            placeholder="Find a name, term, or spelling…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <section
          className="vocabulary-terms-section"
          aria-labelledby="custom-vocabulary-title"
        >
          <div className="vocabulary-section-heading">
            <div>
              <h3 id="custom-vocabulary-title">Your terms</h3>
              <p className="muted">
                Custom spellings are prioritized during transcription.
              </p>
            </div>
            <span
              className="vocabulary-count"
              aria-label={`${customTerms.length} custom term${customTerms.length === 1 ? "" : "s"}`}
            >
              {customTerms.length}
            </span>
          </div>

          {customTerms.length > 0 ? (
            <div className="vocabulary-term-list">
              {customTerms.map((term) =>
                editingId === term.id ? (
                  <article
                    className="vocabulary-term-row is-editing"
                    key={term.id}
                  >
                    <form
                      className="vocabulary-form vocabulary-edit-form"
                      aria-label={`Edit ${term.canonical}`}
                      onSubmit={(event) => void saveEdit(event, term.id)}
                    >
                      <VocabularyEditorFields
                        draft={editDraft}
                        disabled={isSaving}
                        idPrefix={`edit-vocabulary-${term.id}`}
                        switchLabel={`Correct close matches for ${term.canonical}`}
                        onChange={setEditDraft}
                      />
                      <div className="vocabulary-form-actions">
                        <button
                          type="submit"
                          className="primary-button vocabulary-primary-action"
                          disabled={
                            isSaving || editDraft.canonical.trim().length === 0
                          }
                        >
                          {isSaving ? "Saving…" : "Save changes"}
                        </button>
                        <button
                          type="button"
                          className="secondary-inline-button vocabulary-row-action"
                          disabled={isSaving}
                          onClick={cancelEdit}
                        >
                          Cancel
                        </button>
                      </div>
                    </form>
                  </article>
                ) : (
                  <VocabularyTermRow
                    key={term.id}
                    term={term}
                    disabled={isSaving}
                    onEdit={() => beginEdit(term)}
                    onDelete={() => void deleteTerm(term.id)}
                  />
                ),
              )}
            </div>
          ) : (
            <div className="vocabulary-empty-state">
              <strong>{playful ? "Teach me your weird words" : "No custom terms yet"}</strong>
              <p className="muted">
                {playful
                  ? "Names, brands, jargon: add them above and Murmela spells them right. No judgment, even for “Kubernetes”."
                  : "Add a name or phrase above when Murmela needs help spelling it consistently."}
              </p>
            </div>
          )}
        </section>

        {builtinTerms.length > 0 ? (
          <section className="vocabulary-builtins">
            <button
              type="button"
              className="vocabulary-builtins-toggle"
              aria-expanded={builtinsOpen}
              aria-controls="builtin-vocabulary-list"
              onClick={() => setBuiltinsOpen((open) => !open)}
            >
              <span className="vocabulary-builtins-copy">
                <strong>Included by default</strong>
                <span>Common product names Murmela already recognizes.</span>
              </span>
              <span className="vocabulary-builtins-meta">
                <span className="vocabulary-count">{builtinTerms.length}</span>
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <path d="m4 6 4 4 4-4" />
                </svg>
              </span>
            </button>

            {builtinsOpen ? (
              <div
                id="builtin-vocabulary-list"
                className="vocabulary-term-list vocabulary-builtins-list"
              >
                {builtinTerms.map((term) => (
                  <VocabularyTermRow key={term.id} term={term} />
                ))}
              </div>
            ) : null}
          </section>
        ) : null}
      </div>
    </section>
  );
}

function VocabularyEditorFields({
  draft,
  disabled,
  idPrefix,
  switchLabel,
  onChange,
}: {
  draft: EditorDraft;
  disabled: boolean;
  idPrefix: string;
  switchLabel: string;
  onChange: (draft: EditorDraft) => void;
}) {
  return (
    <div className="vocabulary-editor-fields">
      <div className="field-stack">
        <label htmlFor={`${idPrefix}-canonical`}>Correct spelling</label>
        <input
          id={`${idPrefix}-canonical`}
          placeholder="e.g. CloudOpus"
          autoComplete="off"
          disabled={disabled}
          value={draft.canonical}
          onChange={(event) =>
            onChange({ ...draft, canonical: event.target.value })
          }
        />
      </div>

      <div className="field-stack">
        <label htmlFor={`${idPrefix}-aliases`}>
          Common mishearings or alternatives
        </label>
        <input
          id={`${idPrefix}-aliases`}
          aria-describedby={`${idPrefix}-aliases-help`}
          placeholder="cloud opus, cloud oppus"
          autoComplete="off"
          disabled={disabled}
          value={draft.aliases}
          onChange={(event) =>
            onChange({ ...draft, aliases: event.target.value })
          }
        />
        <small
          id={`${idPrefix}-aliases-help`}
          className="vocabulary-field-help"
        >
          Separate multiple alternatives with commas.
        </small>
      </div>

      <button
        type="button"
        className="vocabulary-match-toggle"
        role="switch"
        aria-checked={draft.correctCloseMatches}
        aria-label={switchLabel}
        disabled={disabled}
        onClick={() =>
          onChange({
            ...draft,
            correctCloseMatches: !draft.correctCloseMatches,
          })
        }
      >
        <span className="vocabulary-match-copy">
          <strong>Correct close matches</strong>
          <span>Helpful for small pronunciation or spelling differences.</span>
        </span>
        <span
          className={`vocabulary-switch-track${draft.correctCloseMatches ? " is-on" : ""}`}
          aria-hidden="true"
        >
          <span className="vocabulary-switch-thumb" />
        </span>
      </button>
    </div>
  );
}

function VocabularyTermRow({
  term,
  disabled = false,
  onEdit,
  onDelete,
}: {
  term: VocabularyTerm;
  disabled?: boolean;
  onEdit?: () => void;
  onDelete?: () => void;
}) {
  return (
    <article className="vocabulary-term-row">
      <div className="vocabulary-term-main">
        <div className="vocabulary-term-title-row">
          <strong>{term.canonical}</strong>
          <span className="vocabulary-match-status">
            {term.matchMode === "exact_and_fuzzy"
              ? "Close matches"
              : "Exact only"}
          </span>
        </div>
        <div
          className="vocabulary-alias-list"
          aria-label={`Alternatives for ${term.canonical}`}
        >
          {term.aliases.length > 0 ? (
            term.aliases.map((alias) => (
              <span className="vocabulary-alias-chip" key={alias.id}>
                {alias.alias}
              </span>
            ))
          ) : (
            <span className="vocabulary-no-aliases">No alternatives</span>
          )}
        </div>
      </div>

      {onEdit && onDelete ? (
        <div className="vocabulary-row-actions">
          <button
            type="button"
            className="secondary-inline-button vocabulary-row-action"
            aria-label={`Edit ${term.canonical}`}
            disabled={disabled}
            onClick={onEdit}
          >
            Edit
          </button>
          <button
            type="button"
            className="vocabulary-delete-action vocabulary-row-action"
            aria-label={`Delete ${term.canonical}`}
            disabled={disabled}
            onClick={onDelete}
          >
            Delete
          </button>
        </div>
      ) : null}
    </article>
  );
}

function normalizeEditorInput(input: EditorDraft): CreateVocabularyTermInput {
  return {
    canonical: input.canonical.trim(),
    aliases: input.aliases
      .split(",")
      .map((alias) => alias.trim())
      .filter((alias) => alias.length > 0),
    matchMode: input.correctCloseMatches ? "exact_and_fuzzy" : "exact_only",
  };
}
