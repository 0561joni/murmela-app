//! A machine result plus a small user correction layer. Both saved and ephemeral
//! transcripts use this code; the legacy tables are an atomic effective projection.
use crate::asr::TranscriptResult;
use crate::audio_files::SelectedSourceFile;
use crate::diarization::{DiarizationStatus, TranscriptSpeaker};
use crate::speaker_reconciliation::SpeakerAttribution;
use crate::storage::{self, TranscriptDetail, TranscriptSummary};
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewRef {
    Saved { id: String },
    Session { id: String },
}
#[cfg(test)]
impl ReviewRef {
    pub fn id(&self) -> &str {
        match self {
            Self::Saved { id } | Self::Session { id } => id,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corrections {
    retained: HashMap<String, TranscriptSpeaker>,
    names: HashMap<String, String>,
    assignments: HashMap<String, ManualAssignment>,
    merges: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManualAssignment {
    speaker_ids: Vec<String>,
    primary: Option<String>,
    attribution: SpeakerAttribution,
}
impl ManualAssignment {
    fn explicit(ids: Vec<String>) -> Self {
        Self {
            primary: if ids.len() == 1 {
                ids.first().cloned()
            } else {
                None
            },
            attribution: match ids.len() {
                0 => SpeakerAttribution::None,
                1 => SpeakerAttribution::Assigned,
                _ => SpeakerAttribution::Overlap,
            },
            speaker_ids: ids,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewState {
    machine: TranscriptResult,
    corrections: Corrections,
    revision: u64,
}

#[derive(Clone)]
struct Session {
    state: ReviewState,
    source: SelectedSourceFile,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDocument {
    pub reference: ReviewRef,
    pub detail: TranscriptDetail,
    pub revision: u64,
    pub manual_segment_ids: Vec<String>,
    pub unmatched_speaker_ids: Vec<String>,
    pub can_undo: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReviewEdit {
    Rename {
        speaker_id: String,
        name: String,
    },
    AddSpeaker {
        name: String,
    },
    Assign {
        segment_ids: Vec<String>,
        speaker_ids: Vec<String>,
        new_speaker_name: Option<String>,
    },
    Merge {
        speaker_ids: Vec<String>,
        target_id: String,
    },
    Undo,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewError {
    pub code: String,
    pub message: String,
}
impl From<anyhow::Error> for ReviewError {
    fn from(error: anyhow::Error) -> Self {
        let message = error.to_string();
        let (code, text) = message
            .split_once(": ")
            .filter(|(code, _)| code.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
            .map(|(code, text)| (code.to_owned(), text.to_owned()))
            .unwrap_or_else(|| ("REVIEW_ERROR".into(), message));
        Self {
            code,
            message: text,
        }
    }
}

#[derive(Clone)]
pub struct ReviewStore {
    db_path: PathBuf,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    // Serializes read/modify/write, including machine completion and undo. Never
    // held while running an engine, hashing audio, or waiting for a dialog.
    writes: Arc<Mutex<()>>,
    undo: Arc<Mutex<HashMap<ReviewRef, Vec<Corrections>>>>,
}

pub fn ensure_schema(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS transcript_reviews (
        transcript_id TEXT PRIMARY KEY REFERENCES transcripts(id) ON DELETE CASCADE,
        state_json TEXT NOT NULL
    );",
    )?;
    // Text is committed before speakers run; recover that usable text after a
    // process interruption instead of displaying a permanent running state.
    let tx = connection.unchecked_transaction()?;
    let records = {
        let mut statement =
            tx.prepare("SELECT transcript_id,state_json FROM transcript_reviews")?;
        let rows =
            statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, raw) in records {
        let mut state: ReviewState = serde_json::from_str(&raw)?;
        if matches!(
            state.machine.diarization_status,
            DiarizationStatus::Pending | DiarizationStatus::Running
        ) {
            state.machine.diarization_status = DiarizationStatus::Canceled;
            state.machine.diarization_warning = Some(
                "Speaker processing was interrupted. Your transcript and corrections were kept."
                    .into(),
            );
            state.revision += 1;
            tx.execute(
                "UPDATE transcript_reviews SET state_json=?2 WHERE transcript_id=?1",
                params![id, serde_json::to_string(&state)?],
            )?;
        }
    }
    tx.execute("UPDATE transcripts SET diarization_status='canceled',diarization_warning='Speaker processing was interrupted. Your transcript and corrections were kept.' WHERE diarization_status IN ('pending','running')",[])?;
    tx.commit()?;
    Ok(())
}

impl ReviewStore {
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            sessions: Default::default(),
            writes: Default::default(),
            undo: Default::default(),
        }
    }

    pub fn create_session(
        &self,
        id: &str,
        source: SelectedSourceFile,
        result: TranscriptResult,
    ) -> Result<ReviewRef> {
        self.sessions
            .lock()
            .map_err(|_| anyhow!("Review storage unavailable"))?
            .insert(
                id.into(),
                Session {
                    state: initial_state(result),
                    source,
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            );
        Ok(ReviewRef::Session { id: id.into() })
    }

    pub fn get(&self, reference: &ReviewRef) -> Result<ReviewDocument> {
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow!("Review storage unavailable"))?;
        let (state, detail) = self.load(reference)?;
        self.document(reference, &state, detail)
    }

    pub fn rename_title(&self, id: &str, title: &str) -> Result<TranscriptSummary> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 200 || title.chars().any(char::is_control) {
            bail!("Enter a title with 1–200 characters and no line breaks.");
        }
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow!("Review storage unavailable"))?;
        let reference = ReviewRef::Saved { id: id.into() };
        let (mut state, _) = self.load(&reference)?;
        state.revision += 1;
        let mut conn = storage::open_connection_by_path(&self.db_path)?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE transcripts SET title=?2 WHERE id=?1",
            params![id, title],
        )?;
        tx.execute("INSERT INTO transcript_reviews(transcript_id,state_json) VALUES (?1,?2) ON CONFLICT(transcript_id) DO UPDATE SET state_json=excluded.state_json",params![id,serde_json::to_string(&state)?])?;
        tx.commit()?;
        Ok(storage::fetch_transcript_detail(&conn, id)?.summary)
    }

    pub fn source(&self, reference: &ReviewRef) -> Result<SelectedSourceFile> {
        match reference {
            ReviewRef::Session { id } => self
                .sessions
                .lock()
                .map_err(|_| anyhow!("Review storage unavailable"))?
                .get(id)
                .map(|s| s.source.clone())
                .ok_or_else(|| anyhow!("REVIEW_NOT_FOUND: This session is no longer available.")),
            ReviewRef::Saved { id } => {
                let connection = storage::open_connection_by_path(&self.db_path)?;
                connection.query_row("SELECT local_path,original_name,mime_type,size_bytes,duration_ms,sha256 FROM source_files WHERE transcript_id=?1", [id], |row| {
                    Ok(SelectedSourceFile { file_path: row.get(0)?, original_name: row.get(1)?, mime_type: row.get(2)?, size_bytes: row.get(3)?, duration_ms: row.get(4)?, sha256: row.get(5)? })
                }).map_err(|_| anyhow!("SOURCE_FILE_REQUIRED: Original audio is unavailable for this transcript."))
            }
        }
    }

    pub fn relink(&self, reference: &ReviewRef, source: SelectedSourceFile) -> Result<()> {
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow!("Review storage unavailable"))?;
        match reference {
            ReviewRef::Session { id } => {
                let mut sessions = self
                    .sessions
                    .lock()
                    .map_err(|_| anyhow!("Review storage unavailable"))?;
                sessions
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("REVIEW_NOT_FOUND: Session not found."))?
                    .source = source;
            }
            ReviewRef::Saved { id } => {
                let conn = storage::open_connection_by_path(&self.db_path)?;
                conn.execute(
                    "UPDATE source_files SET local_path=?2 WHERE transcript_id=?1",
                    params![id, source.file_path],
                )?;
            }
        }
        Ok(())
    }

    pub fn edit(
        &self,
        reference: &ReviewRef,
        expected_revision: u64,
        edit: ReviewEdit,
    ) -> Result<ReviewDocument> {
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow!("Review storage unavailable"))?;
        let (mut state, detail) = self.load(reference)?;
        if state.revision != expected_revision {
            bail!("REVIEW_CONFLICT: This transcript changed. Review the latest version and try again.");
        }
        let old = state.corrections.clone();
        let is_undo = matches!(edit, ReviewEdit::Undo);
        if is_undo {
            state.corrections = self
                .undo
                .lock()
                .map_err(|_| anyhow!("Undo unavailable"))?
                .get(reference)
                .and_then(|items| items.last())
                .cloned()
                .ok_or_else(|| anyhow!("Nothing to undo."))?;
        } else {
            apply_edit(&mut state, edit)?;
        }
        state.revision += 1;
        let detail = self.persist(reference, &state, detail, None)?;
        {
            let mut undo = self.undo.lock().map_err(|_| anyhow!("Undo unavailable"))?;
            let history = undo.entry(reference.clone()).or_default();
            if is_undo {
                history.pop();
            } else {
                history.push(old);
                if history.len() > 20 {
                    history.remove(0);
                }
            }
        }
        self.document(reference, &state, detail)
    }

    /// Load the latest corrections only after inference completes. A user edit
    /// during inference is therefore never overwritten by an old snapshot.
    pub fn replace_machine(
        &self,
        reference: &ReviewRef,
        result: TranscriptResult,
        reset: bool,
    ) -> Result<ReviewDocument> {
        self.replace_machine_cancellable(reference, result, reset, None)
    }
    pub fn replace_machine_cancellable(
        &self,
        reference: &ReviewRef,
        mut result: TranscriptResult,
        reset: bool,
        cancelled: Option<&AtomicBool>,
    ) -> Result<ReviewDocument> {
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow!("Review storage unavailable"))?;
        let (mut state, detail) = self.load(reference)?;
        if result.segments.len() != state.machine.segments.len()
            || result
                .segments
                .iter()
                .zip(&state.machine.segments)
                .any(|(a, b)| {
                    a.id != b.id
                        || a.text != b.text
                        || a.start_ms != b.start_ms
                        || a.end_ms != b.end_ms
                })
        {
            bail!("REVIEW_CONFLICT: Speaker processing cannot change transcript text or passage boundaries.");
        }
        if !reset {
            remap_speaker_identities(&state.machine, &mut result);
        }
        if reset {
            state.corrections = Corrections::default();
        }
        state.machine = result;
        state.revision += 1;
        let detail = self.persist(reference, &state, detail, cancelled)?;
        if reset {
            self.undo
                .lock()
                .map_err(|_| anyhow!("Undo unavailable"))?
                .remove(reference);
        }
        self.document(reference, &state, detail)
    }

    pub fn machine(&self, reference: &ReviewRef) -> Result<TranscriptResult> {
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow!("Review storage unavailable"))?;
        Ok(self.load(reference)?.0.machine)
    }

    pub fn discard(&self, reference: &ReviewRef) -> Result<()> {
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow!("Review storage unavailable"))?;
        if let ReviewRef::Session { id } = reference {
            self.sessions
                .lock()
                .map_err(|_| anyhow!("Review storage unavailable"))?
                .remove(id);
        }
        self.undo
            .lock()
            .map_err(|_| anyhow!("Undo unavailable"))?
            .remove(reference);
        Ok(())
    }

    fn load(&self, reference: &ReviewRef) -> Result<(ReviewState, TranscriptDetail)> {
        match reference {
            ReviewRef::Session { id } => {
                let sessions = self
                    .sessions
                    .lock()
                    .map_err(|_| anyhow!("Review storage unavailable"))?;
                let session = sessions.get(id).ok_or_else(|| {
                    anyhow!("REVIEW_NOT_FOUND: This session is no longer available.")
                })?;
                let detail = session_detail(id, session);
                Ok((session.state.clone(), detail))
            }
            ReviewRef::Saved { id } => {
                let conn = storage::open_connection_by_path(&self.db_path)?;
                let tx = conn.unchecked_transaction()?;
                let detail = storage::fetch_transcript_detail(&tx, id)?;
                let raw: Option<String> = tx
                    .query_row(
                        "SELECT state_json FROM transcript_reviews WHERE transcript_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let state = if let Some(raw) = raw {
                    serde_json::from_str(&raw)?
                } else {
                    initial_state(result_from_detail(&detail))
                };
                Ok((state, detail))
            }
        }
    }

    fn persist(
        &self,
        reference: &ReviewRef,
        state: &ReviewState,
        mut detail: TranscriptDetail,
        cancelled: Option<&AtomicBool>,
    ) -> Result<TranscriptDetail> {
        let check = || -> Result<()> {
            if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                bail!("JOB_CANCELED: Speaker processing stopped before saving.");
            }
            Ok(())
        };
        let effective = effective_result(state);
        match reference {
            ReviewRef::Saved { id } => {
                let mut conn = storage::open_connection_by_path(&self.db_path)?;
                let tx = conn.transaction()?;
                storage::replace_diarization_in_transaction(&tx, id, &effective, None)?;
                tx.execute("INSERT INTO transcript_reviews(transcript_id,state_json) VALUES (?1,?2) ON CONFLICT(transcript_id) DO UPDATE SET state_json=excluded.state_json", params![id, serde_json::to_string(state)?])?;
                check()?;
                tx.commit()?;
                apply_result_to_detail(&mut detail, &effective);
                Ok(detail)
            }
            ReviewRef::Session { id } => {
                let mut sessions = self
                    .sessions
                    .lock()
                    .map_err(|_| anyhow!("Review storage unavailable"))?;
                let session = sessions
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("REVIEW_NOT_FOUND: Session not found."))?;
                check()?;
                session.state = state.clone();
                apply_result_to_detail(&mut detail, &effective);
                Ok(detail)
            }
        }
    }

    fn document(
        &self,
        reference: &ReviewRef,
        state: &ReviewState,
        mut detail: TranscriptDetail,
    ) -> Result<ReviewDocument> {
        detail.manual_segment_ids = state.corrections.assignments.keys().cloned().collect();
        let machine_ids: HashSet<_> = state
            .machine
            .speakers
            .iter()
            .map(|s| canonical(&state.corrections, &s.speaker_id))
            .collect();
        let assigned_ids: HashSet<_> = state
            .corrections
            .assignments
            .values()
            .flat_map(|a| &a.speaker_ids)
            .map(|id| canonical(&state.corrections, id))
            .collect();
        let unmatched_speaker_ids = detail
            .speakers
            .iter()
            .filter(|s| {
                !machine_ids.contains(&s.speaker_id) && !assigned_ids.contains(&s.speaker_id)
            })
            .map(|s| s.speaker_id.clone())
            .collect();
        Ok(ReviewDocument {
            reference: reference.clone(),
            detail,
            revision: state.revision,
            manual_segment_ids: state.corrections.assignments.keys().cloned().collect(),
            unmatched_speaker_ids,
            can_undo: self
                .undo
                .lock()
                .map_err(|_| anyhow!("Undo unavailable"))?
                .get(reference)
                .is_some_and(|items| !items.is_empty()),
        })
    }
}

fn initial_state(machine: TranscriptResult) -> ReviewState {
    let mut corrections = Corrections::default();
    for speaker in &machine.speakers {
        if speaker.display_name != format!("Speaker {}", speaker.speaker_order + 1) {
            corrections
                .retained
                .insert(speaker.speaker_id.clone(), speaker.clone());
            corrections
                .names
                .insert(speaker.speaker_id.clone(), speaker.display_name.clone());
        }
    }
    ReviewState {
        machine,
        corrections,
        revision: 1,
    }
}

fn canonical(corrections: &Corrections, id: &str) -> String {
    let mut id = id;
    for _ in 0..=corrections.merges.len() {
        match corrections.merges.get(id) {
            Some(next) => id = next,
            None => break,
        }
    }
    id.into()
}

fn effective_result(state: &ReviewState) -> TranscriptResult {
    let mut result = state.machine.clone();
    let c = &state.corrections;
    let mut speakers: HashMap<_, _> = result
        .speakers
        .iter()
        .chain(c.retained.values())
        .map(|s| (s.speaker_id.clone(), s.clone()))
        .collect();
    speakers.retain(|id, _| canonical(c, id) == *id);
    for speaker in speakers.values_mut() {
        if let Some(name) = c.names.get(&speaker.speaker_id) {
            speaker.display_name = name.clone();
        }
    }
    for segment in &mut result.segments {
        if let Some(assignment) = c.assignments.get(&segment.id) {
            segment.speaker_id = assignment.primary.clone();
            segment.speaker_ids = if assignment.speaker_ids.is_empty() {
                None
            } else {
                Some(assignment.speaker_ids.clone())
            };
            segment.speaker_attribution = assignment.attribution;
            segment.speaker_confidence = None;
        }
        segment.speaker_id = segment.speaker_id.as_ref().map(|id| canonical(c, id));
        if let Some(ids) = &mut segment.speaker_ids {
            *ids = ids.iter().map(|id| canonical(c, id)).collect();
            ids.sort();
            ids.dedup();
            if ids.len() == 1 && segment.speaker_attribution == SpeakerAttribution::Overlap {
                segment.speaker_id = ids.first().cloned();
                segment.speaker_attribution = SpeakerAttribution::Assigned;
            }
        }
    }
    for turn in &mut result.diarization_turns {
        turn.speaker_ids = turn.speaker_ids.iter().map(|id| canonical(c, id)).collect();
        turn.speaker_ids.sort();
        turn.speaker_ids.dedup();
        turn.is_overlap = turn.speaker_ids.len() > 1;
    }
    result.speakers = speakers.into_values().collect();
    result.speakers.sort_by(|a, b| {
        a.speaker_order
            .cmp(&b.speaker_order)
            .then(a.speaker_id.cmp(&b.speaker_id))
    });
    // Distinct stable colors/order even when unmatched retained speakers survive a rerun.
    for (index, s) in result.speakers.iter_mut().enumerate() {
        s.speaker_order = index as i32;
    }
    result
}

fn checked_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        bail!("Speaker names must contain 1–80 characters without line breaks.");
    }
    Ok(name.into())
}

fn apply_edit(state: &mut ReviewState, edit: ReviewEdit) -> Result<()> {
    let effective = effective_result(state);
    let retain = |c: &mut Corrections, id: &str| -> Result<()> {
        let s = effective
            .speakers
            .iter()
            .find(|s| s.speaker_id == id)
            .ok_or_else(|| anyhow!("Speaker not found in this transcript."))?;
        c.retained.insert(id.into(), s.clone());
        Ok(())
    };
    match edit {
        ReviewEdit::Rename { speaker_id, name } => {
            retain(&mut state.corrections, &speaker_id)?;
            state
                .corrections
                .names
                .insert(speaker_id, checked_name(&name)?);
        }
        ReviewEdit::AddSpeaker { name } => {
            add_speaker(state, &name)?;
        }
        ReviewEdit::Assign {
            segment_ids,
            mut speaker_ids,
            new_speaker_name,
        } => {
            if segment_ids.is_empty() {
                bail!("Select at least one passage.");
            }
            let known: HashSet<_> = state
                .machine
                .segments
                .iter()
                .map(|s| s.id.as_str())
                .collect();
            if segment_ids.iter().any(|id| !known.contains(id.as_str())) {
                bail!("Passage not found in this transcript.");
            }
            for id in &speaker_ids {
                retain(&mut state.corrections, id)?;
            }
            if let Some(name) = new_speaker_name {
                speaker_ids.push(add_speaker(state, &name)?);
            }
            speaker_ids.sort();
            speaker_ids.dedup();
            for id in segment_ids {
                state
                    .corrections
                    .assignments
                    .insert(id, ManualAssignment::explicit(speaker_ids.clone()));
            }
        }
        ReviewEdit::Merge {
            speaker_ids,
            target_id,
        } => {
            retain(&mut state.corrections, &target_id)?;
            for id in &speaker_ids {
                retain(&mut state.corrections, id)?;
            }
            let sources: HashSet<_> = speaker_ids
                .iter()
                .filter(|id| **id != target_id)
                .cloned()
                .collect();
            if sources.is_empty() {
                bail!("Choose a different speaker to merge.");
            }
            // Lock affected passages, so a future machine result cannot undo the merge.
            for segment in &effective.segments {
                let mut ids = segment.speaker_ids.clone().unwrap_or_default();
                if let Some(id) = &segment.speaker_id {
                    ids.push(id.clone());
                }
                if ids.iter().any(|id| sources.contains(id)) {
                    ids = ids
                        .into_iter()
                        .map(|id| {
                            if sources.contains(&id) {
                                target_id.clone()
                            } else {
                                id
                            }
                        })
                        .collect();
                    ids.sort();
                    ids.dedup();
                    let assignment = if ids.len() <= 1 {
                        ManualAssignment::explicit(ids)
                    } else {
                        ManualAssignment {
                            speaker_ids: ids,
                            attribution: segment.speaker_attribution,
                            primary: segment.speaker_id.as_ref().map(|id| {
                                if sources.contains(id) {
                                    target_id.clone()
                                } else {
                                    id.clone()
                                }
                            }),
                        }
                    };
                    state
                        .corrections
                        .assignments
                        .insert(segment.id.clone(), assignment);
                }
            }
            for id in sources {
                state.corrections.merges.insert(id, target_id.clone());
            }
        }
        ReviewEdit::Undo => unreachable!(),
    }
    Ok(())
}

fn add_speaker(state: &mut ReviewState, name: &str) -> Result<String> {
    let name = checked_name(name)?;
    let id = uuid::Uuid::new_v4().to_string();
    let order = effective_result(state).speakers.len() as i32;
    state.corrections.retained.insert(
        id.clone(),
        TranscriptSpeaker {
            speaker_id: id.clone(),
            display_name: name.clone(),
            speaker_order: order,
        },
    );
    state.corrections.names.insert(id.clone(), name);
    Ok(id)
}

/// Conservative matching uses non-overlapping single-speaker evidence only.
/// Native models without diarization turns use their timestamped passages.
fn evidence(result: &TranscriptResult) -> Vec<(i64, i64, String)> {
    let mut spans = if !result.diarization_turns.is_empty() {
        result
            .diarization_turns
            .iter()
            .filter(|t| t.speaker_ids.len() == 1 && !t.is_overlap)
            .map(|t| (t.start_ms, t.end_ms, t.speaker_ids[0].clone()))
            .collect::<Vec<_>>()
    } else {
        result
            .segments
            .iter()
            .filter_map(|s| {
                if s.speaker_attribution == SpeakerAttribution::Assigned {
                    s.speaker_id
                        .as_ref()
                        .map(|id| (s.start_ms, s.end_ms, id.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    spans.retain(|(a, b, _)| b > a);
    // Union duplicate/overlapping evidence for each identity before summing
    // speech duration, so native overlapping passage boundaries cannot inflate a match.
    spans.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));
    let mut merged: Vec<(i64, i64, String)> = Vec::new();
    for (start, end, id) in spans {
        if let Some(last) = merged.last_mut() {
            if last.2 == id && start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end, id));
    }
    merged.sort();
    merged
}

fn remap_speaker_identities(old: &TranscriptResult, new: &mut TranscriptResult) {
    let a = evidence(old);
    let b = evidence(new);
    let mut old_totals = HashMap::<String, i64>::new();
    let mut new_totals = HashMap::<String, i64>::new();
    for (s, e, id) in &a {
        *old_totals.entry(id.clone()).or_default() += e - s;
    }
    for (s, e, id) in &b {
        *new_totals.entry(id.clone()).or_default() += e - s;
    }
    let mut prefix_end = Vec::with_capacity(a.len());
    let mut maximum_end = i64::MIN;
    for (_, end, _) in &a {
        maximum_end = maximum_end.max(*end);
        prefix_end.push(maximum_end);
    }
    let mut coverage = HashMap::<(String, String), i64>::new();
    for (start, end, id) in &b {
        for (s, e, old_id) in &a[prefix_end.partition_point(|end| end <= start)..] {
            if s >= end {
                break;
            }
            let overlap = end.min(e) - start.max(s);
            if overlap > 0 {
                *coverage.entry((old_id.clone(), id.clone())).or_default() += overlap;
            }
        }
    }
    let best = |id: &str, old_side: bool| -> Option<String> {
        let mut candidates: Vec<_> = coverage
            .iter()
            .filter(|((a, b), _)| if old_side { a == id } else { b == id })
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(a.1));
        if candidates.len() > 1 && candidates[0].1 == candidates[1].1 {
            return None;
        }
        candidates
            .first()
            .map(|((a, b), _)| if old_side { b.clone() } else { a.clone() })
    };
    let mut mapping = HashMap::new();
    for speaker in &new.speakers {
        let id = &speaker.speaker_id;
        let matching = best(id, false)
            .filter(|old_id| best(old_id, true).as_ref() == Some(id))
            .filter(|old_id| {
                let common = *coverage.get(&(old_id.clone(), id.clone())).unwrap_or(&0) as f64;
                common >= 0.8 * old_totals.get(old_id).copied().unwrap_or(0) as f64
                    && common >= 0.8 * new_totals.get(id).copied().unwrap_or(0) as f64
            });
        mapping.insert(
            id.clone(),
            matching.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        );
    }
    for s in &mut new.speakers {
        s.speaker_id = mapping[&s.speaker_id].clone();
    }
    for s in &mut new.segments {
        s.speaker_id = s
            .speaker_id
            .as_ref()
            .and_then(|id| mapping.get(id).cloned());
        if let Some(ids) = &mut s.speaker_ids {
            *ids = ids
                .iter()
                .filter_map(|id| mapping.get(id).cloned())
                .collect();
        }
    }
    for t in &mut new.diarization_turns {
        t.speaker_ids = t
            .speaker_ids
            .iter()
            .filter_map(|id| mapping.get(id).cloned())
            .collect();
    }
}

pub fn result_from_detail(detail: &TranscriptDetail) -> TranscriptResult {
    TranscriptResult {
        job_id: detail.summary.id.clone(),
        model_name: detail.summary.model_name.clone().unwrap_or_default(),
        full_text: detail.full_text.clone(),
        plain_text: detail.summary.plain_text.clone(),
        timestamped_text: detail.timestamped_text.clone(),
        detected_languages: detail.summary.detected_languages.clone(),
        segments: detail.segments.clone(),
        quality_status: detail.summary.quality_status,
        recovered_region_count: detail.summary.recovered_region_count,
        warnings: detail.transcription_warnings.clone(),
        diarization_status: detail.summary.diarization_status,
        diarization_source: detail.diarization_source,
        diarization_model_id: detail.diarization_model_id.clone(),
        diarization_warning: detail.diarization_warning.clone(),
        diarization_policy_version: detail.diarization_policy_version,
        diarization_clustering_threshold: detail.diarization_clustering_threshold,
        diarization_speaker_count_hint: detail.diarization_speaker_count_hint,
        speakers: detail.speakers.clone(),
        diarization_turns: detail.diarization_turns.clone(),
    }
}

fn apply_result_to_detail(detail: &mut TranscriptDetail, r: &TranscriptResult) {
    detail.segments = r.segments.clone();
    detail.speakers = r.speakers.clone();
    detail.diarization_turns = r.diarization_turns.clone();
    detail.summary.diarization_status = r.diarization_status;
    detail.summary.speaker_count = active_speaker_count(r);
    detail.diarization_model_id = r.diarization_model_id.clone();
    detail.diarization_warning = r.diarization_warning.clone();
    detail.diarization_source = r.diarization_source;
    detail.diarization_policy_version = r.diarization_policy_version;
    detail.diarization_clustering_threshold = r.diarization_clustering_threshold;
    detail.diarization_speaker_count_hint = r.diarization_speaker_count_hint;
}

fn session_detail(id: &str, session: &Session) -> TranscriptDetail {
    let r = effective_result(&session.state);
    let mut detail = TranscriptDetail {
        translation: None,
        manual_segment_ids: session
            .state
            .corrections
            .assignments
            .keys()
            .cloned()
            .collect(),
        summary: TranscriptSummary {
            id: id.into(),
            created_at: session.created_at.clone(),
            source_type: if crate::meeting::is_recording_path(Path::new(&session.source.file_path)) {
                storage::SourceType::Meeting
            } else {
                storage::SourceType::FileUpload
            },
            title: crate::output_format::file_display_title(&session.source.original_name),
            plain_text: r.plain_text.clone(),
            status: storage::TranscriptStatus::Completed,
            detected_languages: r.detected_languages.clone(),
            duration_ms: session.source.duration_ms,
            model_name: Some(r.model_name.clone()),
            quality_status: r.quality_status,
            recovered_region_count: r.recovered_region_count,
            diarization_status: DiarizationStatus::NotRequested,
            speaker_count: None,
        },
        full_text: r.full_text.clone(),
        timestamped_text: r.timestamped_text.clone(),
        transcription_warnings: r.warnings.clone(),
        diarization_model_id: None,
        diarization_source: Default::default(),
        diarization_warning: None,
        diarization_policy_version: None,
        diarization_clustering_threshold: None,
        diarization_speaker_count_hint: None,
        segments: vec![],
        speakers: vec![],
        diarization_turns: vec![],
    };
    apply_result_to_detail(&mut detail, &r);
    detail
}

pub fn active_speaker_count(result: &TranscriptResult) -> Option<i32> {
    let ids: HashSet<_> = result
        .segments
        .iter()
        .flat_map(|s| s.speaker_ids.iter().flatten().chain(s.speaker_id.iter()))
        .chain(result.diarization_turns.iter().flat_map(|t| &t.speaker_ids))
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids.len() as i32)
    }
}

#[cfg(test)]
pub(crate) fn fixture_result() -> TranscriptResult {
    serde_json::from_value(serde_json::json!({
        "jobId":"fixture", "modelName":"Test", "fullText":"First. Second.", "plainText":"First. Second.", "timestampedText":"First. Second.",
        "detectedLanguages":["en"],"qualityStatus":"clean","recoveredRegionCount":0,"warnings":[],
        "diarizationStatus":"completed","diarizationSource":"post_process","diarizationModelId":"speaker-model", "diarizationWarning":null,
        "diarizationPolicyVersion":2,"diarizationClusteringThreshold":1.1,"diarizationSpeakerCountHint":null,
        "segments":[
            {"id":"s1","startMs":0,"endMs":10000,"text":"First.","languageCode":"en","segmentOrder":0,"confidence":null,"speakerId":"a","speakerIds":["a"],"speakerAttribution":"assigned","speakerConfidence":null},
            {"id":"s2","startMs":10000,"endMs":20000,"text":"Second.","languageCode":"en","segmentOrder":1,"confidence":null,"speakerId":"b","speakerIds":["b"],"speakerAttribution":"assigned","speakerConfidence":null}
        ],
        "speakers":[{"speakerId":"a","displayName":"Speaker 1","speakerOrder":0},{"speakerId":"b","displayName":"Speaker 2","speakerOrder":1}],
        "diarizationTurns":[
            {"id":"t1","startMs":0,"endMs":10000,"speakerIds":["a"],"confidence":null,"isOverlap":false,"isUncertain":false,"turnOrder":0},
            {"id":"t2","startMs":10000,"endMs":20000,"speakerIds":["b"],"confidence":null,"isOverlap":false,"isUncertain":false,"turnOrder":1}
        ]
    })).unwrap()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_while_saving_rolls_back_projection_machine_and_reset() {
        let path = std::env::temp_dir().join(format!(
            "review-cancel-save-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = storage::open_connection_by_path(&path).unwrap();
        storage::ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap();
        let summary =
            storage::save_file_transcription(&path, &source(), &fixture_result()).unwrap();
        let reference = ReviewRef::Saved { id: summary.id };
        let store = ReviewStore::new(path.clone());
        let before = store
            .edit(
                &reference,
                1,
                ReviewEdit::Rename {
                    speaker_id: "a".into(),
                    name: "Maya".into(),
                },
            )
            .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let flag = canceled.clone();
        let worker_store = store.clone();
        let worker_ref = reference.clone();
        let worker = std::thread::spawn(move || {
            worker_store.replace_machine_cancellable(
                &worker_ref,
                fixture_result(),
                true,
                Some(&flag),
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while store.writes.try_lock().is_ok() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        canceled.store(true, Ordering::SeqCst);
        conn.execute_batch("COMMIT").unwrap();
        assert!(worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .starts_with("JOB_CANCELED:"));
        let after = store.get(&reference).unwrap();
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(&after).unwrap()
        );
        drop(conn);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "10,000-passage saved-edit benchmark; run with --ignored --nocapture"]
    fn benchmark_saved_review_edits() {
        let path =
            std::env::temp_dir().join(format!("review-benchmark-{}.sqlite", uuid::Uuid::new_v4()));
        let conn = storage::open_connection_by_path(&path).unwrap();
        storage::ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap();
        let mut result = fixture_result();
        let template = result.segments[0].clone();
        result.segments = (0..10000)
            .map(|i| {
                let mut s = template.clone();
                s.id = format!("passage-{i}");
                s.start_ms = i * 6000;
                s.end_ms = s.start_ms + 6000;
                s.segment_order = i as i32;
                s
            })
            .collect();
        result.plain_text = result
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let summary = storage::save_file_transcription(&path, &source(), &result).unwrap();
        let reference = ReviewRef::Saved { id: summary.id };
        let store = ReviewStore::new(path.clone());
        let start = std::time::Instant::now();
        let doc = store.get(&reference).unwrap();
        println!(
            "saved_review passages=10000 load_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.
        );
        let start = std::time::Instant::now();
        let renamed = store
            .edit(
                &reference,
                doc.revision,
                ReviewEdit::Rename {
                    speaker_id: "a".into(),
                    name: "Maya".into(),
                },
            )
            .unwrap();
        println!(
            "saved_review passages=10000 rename_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.
        );
        let start = std::time::Instant::now();
        let corrected = store
            .edit(
                &reference,
                renamed.revision,
                ReviewEdit::Assign {
                    segment_ids: vec!["passage-9999".into()],
                    speaker_ids: vec!["b".into()],
                    new_speaker_name: None,
                },
            )
            .unwrap();
        println!(
            "saved_review passages=10000 assign_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.
        );
        assert_eq!(
            corrected.detail.segments[9999].speaker_id.as_deref(),
            Some("b")
        );
        assert_eq!(corrected.detail.summary.plain_text, result.plain_text);
        drop(conn);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
    fn source() -> SelectedSourceFile {
        SelectedSourceFile {
            file_path: "/missing.wav".into(),
            original_name: "Meeting".into(),
            mime_type: "audio/wav".into(),
            size_bytes: 100,
            duration_ms: Some(20000),
            sha256: Some("hash".into()),
        }
    }
    fn session() -> (ReviewStore, ReviewRef) {
        let store = ReviewStore::new(PathBuf::from("/unused"));
        let reference = store
            .create_session("job", source(), fixture_result())
            .unwrap();
        (store, reference)
    }
    fn assert_consistent(d: &ReviewDocument) {
        let ids: HashSet<_> = d
            .detail
            .speakers
            .iter()
            .map(|s| s.speaker_id.as_str())
            .collect();
        for s in &d.detail.segments {
            for id in s.speaker_ids.iter().flatten().chain(s.speaker_id.iter()) {
                assert!(ids.contains(id.as_str()), "dangling {id}");
            }
        }
        assert_eq!(d.detail.summary.plain_text, "First. Second.");
        assert_eq!(d.detail.segments[0].start_ms, 0);
        assert_eq!(d.detail.segments[1].end_ms, 20000);
    }
    #[test]
    fn assignments_add_overlap_unknown_and_undo_keep_text_and_boundaries() {
        let (store, r) = session();
        let d = store
            .edit(
                &r,
                1,
                ReviewEdit::Assign {
                    segment_ids: vec!["s1".into()],
                    speaker_ids: vec![],
                    new_speaker_name: Some("Maya".into()),
                },
            )
            .unwrap();
        assert_eq!(d.manual_segment_ids, vec!["s1"]);
        let added = d.detail.segments[0].speaker_id.clone().unwrap();
        assert!(d
            .detail
            .speakers
            .iter()
            .any(|s| s.speaker_id == added && s.display_name == "Maya"));
        assert_consistent(&d);
        let d = store
            .edit(
                &r,
                2,
                ReviewEdit::Assign {
                    segment_ids: vec!["s2".into()],
                    speaker_ids: vec!["a".into(), added],
                    new_speaker_name: None,
                },
            )
            .unwrap();
        assert_eq!(
            d.detail.segments[1].speaker_attribution,
            SpeakerAttribution::Overlap
        );
        assert_consistent(&d);
        let d = store
            .edit(
                &r,
                3,
                ReviewEdit::Assign {
                    segment_ids: vec!["s2".into()],
                    speaker_ids: vec![],
                    new_speaker_name: None,
                },
            )
            .unwrap();
        assert_eq!(
            d.detail.segments[1].speaker_attribution,
            SpeakerAttribution::None
        );
        assert!(d.detail.segments[1].speaker_ids.is_none());
        let d = store.edit(&r, 4, ReviewEdit::Undo).unwrap();
        assert_eq!(
            d.detail.segments[1].speaker_attribution,
            SpeakerAttribution::Overlap
        );
        assert_consistent(&d);
    }
    #[test]
    fn rerun_uses_latest_edits_and_matches_names_by_time_not_number() {
        let (store, r) = session();
        let mut result = store.machine(&r).unwrap();
        let d = store
            .edit(
                &r,
                1,
                ReviewEdit::Rename {
                    speaker_id: "a".into(),
                    name: "Maya".into(),
                },
            )
            .unwrap();
        store
            .edit(
                &r,
                d.revision,
                ReviewEdit::Assign {
                    segment_ids: vec!["s2".into()],
                    speaker_ids: vec!["a".into()],
                    new_speaker_name: None,
                },
            )
            .unwrap();
        for s in &mut result.speakers {
            s.speaker_id = format!("new-{}", s.speaker_id);
            s.speaker_order = 1 - s.speaker_order;
        }
        for s in &mut result.segments {
            s.speaker_id = s.speaker_id.as_ref().map(|id| format!("new-{id}"));
            s.speaker_ids = s
                .speaker_ids
                .as_ref()
                .map(|ids| ids.iter().map(|id| format!("new-{id}")).collect());
        }
        for t in &mut result.diarization_turns {
            t.speaker_ids = t.speaker_ids.iter().map(|id| format!("new-{id}")).collect();
        }
        let d = store.replace_machine(&r, result, false).unwrap();
        assert_eq!(d.detail.segments[0].speaker_id.as_deref(), Some("a"));
        assert_eq!(d.detail.segments[1].speaker_id.as_deref(), Some("a"));
        assert_eq!(
            d.detail
                .speakers
                .iter()
                .find(|s| s.speaker_id == "a")
                .unwrap()
                .display_name,
            "Maya"
        );
        assert_consistent(&d);
    }
    #[test]
    fn ambiguous_split_retains_named_speaker_without_transferring_name() {
        let (store, r) = session();
        store
            .edit(
                &r,
                1,
                ReviewEdit::Rename {
                    speaker_id: "a".into(),
                    name: "Maya".into(),
                },
            )
            .unwrap();
        let mut result = fixture_result();
        result.diarization_turns[0].end_ms = 5000;
        result.diarization_turns[1].start_ms = 5000;
        let d = store.replace_machine(&r, result, false).unwrap();
        assert!(d.unmatched_speaker_ids.contains(&"a".to_string()));
        assert_ne!(d.detail.segments[0].speaker_id.as_deref(), Some("a"));
        assert_consistent(&d);
    }
    #[test]
    fn merge_persists_through_rerun_and_undo_restores_distinct_speakers() {
        let (store, r) = session();
        let d = store
            .edit(
                &r,
                1,
                ReviewEdit::Merge {
                    speaker_ids: vec!["a".into()],
                    target_id: "b".into(),
                },
            )
            .unwrap();
        assert_eq!(d.detail.speakers.len(), 1);
        assert_eq!(d.detail.segments[0].speaker_id.as_deref(), Some("b"));
        let d = store.replace_machine(&r, fixture_result(), false).unwrap();
        assert_eq!(d.detail.speakers.len(), 1);
        assert_consistent(&d);
        let d = store.edit(&r, d.revision, ReviewEdit::Undo).unwrap();
        assert_eq!(d.detail.speakers.len(), 2);
        assert_consistent(&d);
    }
    #[test]
    fn merging_candidates_does_not_invent_overlapping_speech() {
        let mut state = initial_state(fixture_result());
        state.machine.segments[0].speaker_ids = Some(vec!["a".into(), "b".into()]);
        state.machine.segments[0].speaker_attribution = SpeakerAttribution::Likely;
        let c = add_speaker(&mut state, "Third").unwrap();
        apply_edit(
            &mut state,
            ReviewEdit::Merge {
                speaker_ids: vec!["a".into()],
                target_id: c,
            },
        )
        .unwrap();
        assert_eq!(
            effective_result(&state).segments[0].speaker_attribution,
            SpeakerAttribution::Likely
        );
    }
    #[test]
    fn rejects_stale_revisions_unknown_ids_and_changed_text_atomically() {
        let (store, r) = session();
        assert!(store
            .edit(
                &r,
                0,
                ReviewEdit::Rename {
                    speaker_id: "a".into(),
                    name: "Maya".into()
                }
            )
            .is_err());
        assert!(store
            .edit(
                &r,
                1,
                ReviewEdit::Assign {
                    segment_ids: vec!["other-transcript".into()],
                    speaker_ids: vec!["a".into()],
                    new_speaker_name: None
                }
            )
            .is_err());
        assert!(store
            .edit(
                &r,
                1,
                ReviewEdit::Merge {
                    speaker_ids: vec!["a".into()],
                    target_id: "outsider".into()
                }
            )
            .is_err());
        let mut result = fixture_result();
        result.segments[0].text = "Changed".into();
        assert!(store.replace_machine(&r, result, false).is_err());
        let d = store.get(&r).unwrap();
        assert_eq!(d.revision, 1);
        assert!(!d.can_undo);
        assert_consistent(&d);
    }
    #[test]
    fn reset_only_changes_corrections_when_replacement_is_committed() {
        let (store, r) = session();
        store
            .edit(
                &r,
                1,
                ReviewEdit::Rename {
                    speaker_id: "a".into(),
                    name: "Maya".into(),
                },
            )
            .unwrap();
        let mut invalid = fixture_result();
        invalid.segments.clear();
        assert!(store.replace_machine(&r, invalid, true).is_err());
        assert_eq!(
            store.get(&r).unwrap().detail.speakers[0].display_name,
            "Maya"
        );
        let d = store.replace_machine(&r, fixture_result(), true).unwrap();
        assert_eq!(d.detail.speakers[0].display_name, "Speaker 1");
        assert_consistent(&d);
    }
    #[test]
    fn saved_projection_and_machine_layer_survive_reopening_and_recovery() {
        let path =
            std::env::temp_dir().join(format!("review-test-{}.sqlite", uuid::Uuid::new_v4()));
        let conn = storage::open_connection_by_path(&path).unwrap();
        storage::ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);
        let mut result = fixture_result();
        result.diarization_status = DiarizationStatus::Running;
        let summary = storage::save_file_transcription(&path, &source(), &result).unwrap();
        let r = ReviewRef::Saved { id: summary.id };
        let store = ReviewStore::new(path.clone());
        store
            .edit(
                &r,
                1,
                ReviewEdit::Rename {
                    speaker_id: "a".into(),
                    name: "Maya".into(),
                },
            )
            .unwrap();
        drop(store);
        let conn = storage::open_connection_by_path(&path).unwrap();
        ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap();
        let store = ReviewStore::new(path.clone());
        let d = store.get(&r).unwrap();
        assert_eq!(d.detail.speakers[0].display_name, "Maya");
        assert!(matches!(
            d.detail.summary.diarization_status,
            DiarizationStatus::Canceled
        ));
        let machine = store.machine(&r).unwrap();
        assert_eq!(machine.speakers[0].display_name, "Speaker 1");
        assert_consistent(&d);
        conn.execute("DELETE FROM transcripts WHERE id=?1", [r.id()])
            .unwrap();
        assert!(store.get(&r).is_err());
        drop(conn);
        drop(store);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn native_matching_uses_passages_when_no_turns_exist() {
        let mut a = fixture_result();
        a.diarization_turns.clear();
        let mut b = a.clone();
        for s in &mut b.speakers {
            s.speaker_id = format!("new-{}", s.speaker_id);
        }
        for s in &mut b.segments {
            s.speaker_id = s.speaker_id.as_ref().map(|id| format!("new-{id}"));
            s.speaker_ids = None;
        }
        remap_speaker_identities(&a, &mut b);
        assert_eq!(b.segments[0].speaker_id.as_deref(), Some("a"));
    }
}
