//! Bounded, renderer-independent desktop projection. The caller serializes access
//! and emits returned revision receipts only after releasing its observation lock.

use std::collections::VecDeque;

use serde::Serialize;

use super::{PartialTranscriptionEventPayload, PromptExecutionEventPayload, RuntimePhasePayload};

pub(crate) type Revision = u64;
const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const MAX_EVENTS: usize = 64;
const MAX_RING_BYTES: usize = 256 * 1024;
// Canonical raw text is bounded separately. JSON escaping plus the active text
// projection and its terminal turn may each retain a representation of that text.
const MAX_PROJECTION_BYTES: usize = 4 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SOURCES: usize = 32;
const MAX_METADATA_BYTES: usize = 4096;
const MAX_SOURCE_URL_BYTES: usize = 2048;
const MAX_SOURCE_TITLE_BYTES: usize = 512;
const MAX_PROMPT_BYTES: usize = voxgolem_core::conversation::MAX_PROMPT_BYTES;
#[cfg(test)]
const MAX_REQUEST_ID_BYTES: usize = voxgolem_core::conversation::MAX_REQUEST_ID_BYTES;
const SNAPSHOT_ENVELOPE_BYTES: usize = 128;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VoiceState {
    Stopped,
    Starting,
    Active,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct VoiceProjection {
    pub(crate) capture_id: Option<u64>,
    pub(crate) state: VoiceState,
    pub(crate) runtime_phase: RuntimePhasePayload,
    pub(crate) auto_stop_on_silence: bool,
    pub(crate) auto_start_available: bool,
    pub(crate) wake_confidence: Option<f32>,
    pub(crate) partial_transcript: Option<PartialTranscriptionEventPayload>,
    pub(crate) error: Option<String>,
}

impl Default for VoiceProjection {
    fn default() -> Self {
        Self {
            capture_id: None,
            state: VoiceState::Stopped,
            runtime_phase: RuntimePhasePayload::Initializing,
            auto_stop_on_silence: true,
            auto_start_available: true,
            wake_confidence: None,
            partial_transcript: None,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct SpeechProjection {
    pub(crate) playback_id: Option<u64>,
    pub(crate) playing: bool,
    pub(crate) pending: bool,
}

/// Prepared durable conversation/request identity, not numeric cancellation fences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ExecutionId {
    pub(crate) session_id: String,
    pub(crate) generation: String,
}

impl From<&voxgolem_core::ipc::ExecutionId> for ExecutionId {
    fn from(id: &voxgolem_core::ipc::ExecutionId) -> Self {
        Self {
            session_id: id.session_id.as_str().into(),
            generation: id.generation.as_str().into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NativePromptProjection {
    pub(crate) request_id: String,
    pub(crate) execution_id: ExecutionId,
    pub(crate) prompt: String,
    pub(crate) updates: Vec<PromptExecutionEventPayload>,
    pub(crate) result: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DesktopRuntimeSnapshot {
    pub(crate) revision: Revision,
    pub(crate) voice: VoiceProjection,
    pub(crate) prompt: Option<NativePromptProjection>,
    pub(crate) speech: SpeechProjection,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DesktopRuntimeEvent {
    Voice {
        revision: Revision,
        voice: VoiceProjection,
    },
    PromptStarted {
        revision: Revision,
        prompt: NativePromptProjection,
    },
    PromptUpdate {
        revision: Revision,
        request_id: String,
        execution_id: ExecutionId,
        update: PromptExecutionEventPayload,
    },
    PromptFinished {
        revision: Revision,
        request_id: String,
        execution_id: ExecutionId,
        result: serde_json::Value,
    },
    Speech {
        revision: Revision,
        speech: SpeechProjection,
    },
}

impl DesktopRuntimeEvent {
    fn revision(&self) -> Revision {
        match self {
            Self::Voice { revision, .. }
            | Self::PromptStarted { revision, .. }
            | Self::PromptUpdate { revision, .. }
            | Self::PromptFinished { revision, .. }
            | Self::Speech { revision, .. } => *revision,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ReadDesktopRuntimePayload {
    pub(crate) revision: Revision,
    pub(crate) snapshot: Option<DesktopRuntimeSnapshot>,
    pub(crate) events: Vec<DesktopRuntimeEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationError {
    RevisionExhausted,
    UnsafeInteger,
    InvalidProjection,
    ProjectionFull,
    Serialization,
}

impl std::fmt::Display for ObservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RevisionExhausted => "desktop observation revision exhausted",
            Self::UnsafeInteger => "desktop observation integer is not JavaScript-safe",
            Self::InvalidProjection => "invalid desktop observation projection",
            Self::ProjectionFull => "desktop observation projection budget exceeded",
            Self::Serialization => "desktop observation serialization failed",
        })
    }
}

impl std::error::Error for ObservationError {}

fn json_size(value: &impl Serialize) -> Result<usize, ObservationError> {
    // Counting serialization avoids allocating a second serialized text buffer.
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.checked_add(bytes.len()).ok_or_else(|| {
                std::io::Error::other("desktop observation serialization size overflow")
            })?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| ObservationError::Serialization)?;
    Ok(counter.0)
}

fn safe_integer(value: u64) -> Result<(), ObservationError> {
    if value > MAX_SAFE_INTEGER {
        Err(ObservationError::UnsafeInteger)
    } else {
        Ok(())
    }
}

fn validate_update(update: &PromptExecutionEventPayload) -> Result<(), ObservationError> {
    fn bounded(text: &str, limit: usize) -> Result<(), ObservationError> {
        if text.len() > limit {
            Err(ObservationError::ProjectionFull)
        } else {
            Ok(())
        }
    }
    match update {
        PromptExecutionEventPayload::CustomDirectTimings {
            first_text_ms,
            completed_ms,
        } => {
            safe_integer(*first_text_ms)?;
            safe_integer(*completed_ms)?;
        }
        PromptExecutionEventPayload::Text { text }
        | PromptExecutionEventPayload::Reasoning { text } => bounded(text, MAX_TEXT_BYTES)?,
        PromptExecutionEventPayload::Correction {
            text, correction, ..
        } => {
            bounded(text, MAX_TEXT_BYTES)?;
            bounded(correction, MAX_METADATA_BYTES)?;
        }
        PromptExecutionEventPayload::Stage {
            detail: Some(detail),
            ..
        } => bounded(detail, MAX_METADATA_BYTES)?,
        PromptExecutionEventPayload::Status { message }
        | PromptExecutionEventPayload::Error { message } => bounded(message, MAX_METADATA_BYTES)?,
        PromptExecutionEventPayload::Tool {
            tool,
            status,
            detail,
        } => {
            bounded(tool, MAX_METADATA_BYTES)?;
            bounded(status, MAX_METADATA_BYTES)?;
            bounded(detail, MAX_METADATA_BYTES)?;
        }
        PromptExecutionEventPayload::Sources { sources } => {
            if sources.len() > MAX_SOURCES {
                return Err(ObservationError::ProjectionFull);
            }
            for source in sources {
                bounded(&source.url, MAX_SOURCE_URL_BYTES)?;
                bounded(&source.title, MAX_SOURCE_TITLE_BYTES)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn same_slot(a: &PromptExecutionEventPayload, b: &PromptExecutionEventPayload) -> bool {
    match (a, b) {
        (
            PromptExecutionEventPayload::Completed { .. }
            | PromptExecutionEventPayload::Cancelled { .. },
            PromptExecutionEventPayload::Completed { .. }
            | PromptExecutionEventPayload::Cancelled { .. },
        ) => true,
        (
            PromptExecutionEventPayload::Stage { stage: a, .. },
            PromptExecutionEventPayload::Stage { stage: b, .. },
        ) => a == b,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

enum UpdatePlan {
    Append {
        index: usize,
        text: String,
    },
    Replace {
        index: usize,
        update: PromptExecutionEventPayload,
    },
    Push(PromptExecutionEventPayload),
}

struct PreparedUpdate {
    mutation: UpdatePlan,
    slot: usize,
    slot_bytes: usize,
    delta: isize,
}

impl PreparedUpdate {
    fn commit(self, updates: &mut Vec<PromptExecutionEventPayload>, sizes: &mut Vec<usize>) {
        self.mutation.commit(updates);
        if self.slot == sizes.len() {
            sizes.push(self.slot_bytes);
        } else {
            sizes[self.slot] = self.slot_bytes;
        }
    }
}

impl UpdatePlan {
    fn commit(self, updates: &mut Vec<PromptExecutionEventPayload>) {
        match self {
            Self::Append { index, text } => {
                if let PromptExecutionEventPayload::Text { text: current } = &mut updates[index] {
                    current.push_str(&text);
                }
            }
            Self::Replace { index, update } => updates[index] = update,
            Self::Push(update) => updates.push(update),
        }
    }
}

/// Cost is the exact serialized byte change of the updates array. Text append
/// scans only the incoming delta, never the accumulated response.
fn plan_update(
    updates: &[PromptExecutionEventPayload],
    sizes: &[usize],
    incoming: &PromptExecutionEventPayload,
) -> Result<PreparedUpdate, ObservationError> {
    validate_update(incoming)?;
    if json_size(incoming)? > MAX_PROJECTION_BYTES {
        return Err(ObservationError::ProjectionFull);
    }
    let normalized = match incoming {
        PromptExecutionEventPayload::Correction { text, .. } => {
            PromptExecutionEventPayload::Text { text: text.clone() }
        }
        _ => incoming.clone(),
    };
    let index = updates.iter().position(|old| same_slot(old, &normalized));
    if let Some(index) = index {
        if let (
            PromptExecutionEventPayload::Text { text: current },
            PromptExecutionEventPayload::Text { text },
            PromptExecutionEventPayload::Text { .. },
        ) = (&updates[index], &normalized, incoming)
        {
            if current.len().saturating_add(text.len()) > MAX_TEXT_BYTES {
                return Err(ObservationError::ProjectionFull);
            }
            let added = json_size(text)? - 2;
            return Ok(PreparedUpdate {
                mutation: UpdatePlan::Append {
                    index,
                    text: text.clone(),
                },
                slot: index,
                slot_bytes: sizes[index] + added,
                delta: added as isize,
            });
        }
        let replacement = match (&updates[index], normalized) {
            (
                PromptExecutionEventPayload::Sources { sources: previous },
                PromptExecutionEventPayload::Sources { sources: incoming },
            ) => {
                let mut sources = previous.clone();
                for source in incoming {
                    if sources.len() == MAX_SOURCES {
                        break;
                    }
                    if !sources.iter().any(|old| old.url == source.url) {
                        sources.push(source);
                    }
                }
                PromptExecutionEventPayload::Sources { sources }
            }
            (_, update) => update,
        };
        let size = json_size(&replacement)?;
        Ok(PreparedUpdate {
            mutation: UpdatePlan::Replace {
                index,
                update: replacement,
            },
            slot: index,
            slot_bytes: size,
            delta: size as isize - sizes[index] as isize,
        })
    } else {
        let normalized = match normalized {
            PromptExecutionEventPayload::Sources { sources } => {
                let mut unique = Vec::new();
                for source in sources {
                    if unique.len() == MAX_SOURCES {
                        break;
                    }
                    if !unique
                        .iter()
                        .any(|old: &super::SourcePayload| old.url == source.url)
                    {
                        unique.push(source);
                    }
                }
                PromptExecutionEventPayload::Sources { sources: unique }
            }
            update => update,
        };
        let size = json_size(&normalized)?;
        let comma = usize::from(!updates.is_empty());
        Ok(PreparedUpdate {
            mutation: UpdatePlan::Push(normalized),
            slot: updates.len(),
            slot_bytes: size,
            delta: (size + comma) as isize,
        })
    }
}

pub(crate) struct DesktopObservation {
    revision: Revision,
    pending_notice: Option<Revision>,
    voice: VoiceProjection,
    voice_bytes: usize,
    speech: SpeechProjection,
    speech_bytes: usize,
    prompt: Option<NativePromptProjection>,
    prompt_bytes: usize,
    update_sizes: Vec<usize>,
    ring: VecDeque<(DesktopRuntimeEvent, usize)>,
    ring_bytes: usize,
    /// Every cursor strictly below this revision requires a snapshot.
    resync_before: Revision,
}

impl Default for DesktopObservation {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopObservation {
    pub(crate) fn new() -> Self {
        let voice = VoiceProjection::default();
        let speech = SpeechProjection::default();
        Self {
            revision: 0,
            pending_notice: None,
            voice_bytes: json_size(&voice).expect("default voice is serializable"),
            speech_bytes: json_size(&speech).expect("default speech is serializable"),
            voice,
            speech,
            prompt: None,
            prompt_bytes: 4,
            update_sizes: Vec::new(),
            ring: VecDeque::new(),
            ring_bytes: 0,
            resync_before: 0,
        }
    }

    fn next_revision(&self) -> Result<Revision, ObservationError> {
        if self.revision >= MAX_SAFE_INTEGER {
            Err(ObservationError::RevisionExhausted)
        } else {
            Ok(self.revision + 1)
        }
    }

    fn check_budget(
        &self,
        voice: usize,
        speech: usize,
        prompt: usize,
        reserve_result: bool,
    ) -> Result<(), ObservationError> {
        // Includes snapshot envelope and maximum revision width with room to spare.
        // An active projection cannot consume its terminal result's capacity.
        if voice
            .saturating_add(speech)
            .saturating_add(prompt)
            .saturating_add(SNAPSHOT_ENVELOPE_BYTES)
            .saturating_add(if reserve_result {
                MAX_RESULT_BYTES - 4
            } else {
                0
            })
            > MAX_PROJECTION_BYTES
        {
            Err(ObservationError::ProjectionFull)
        } else {
            Ok(())
        }
    }

    fn record(&mut self, event: DesktopRuntimeEvent, bytes: usize) -> Option<Revision> {
        self.revision = event.revision();
        if bytes > MAX_RING_BYTES {
            self.ring.clear();
            self.ring_bytes = 0;
            self.resync_before = self.revision;
        } else {
            while self.ring.len() >= MAX_EVENTS || self.ring_bytes + bytes > MAX_RING_BYTES {
                if let Some((event, removed)) = self.ring.pop_front() {
                    self.resync_before = event.revision();
                    self.ring_bytes -= removed;
                } else {
                    break;
                }
            }
            self.ring_bytes += bytes;
            self.ring.push_back((event, bytes));
        }
        self.notification()
    }

    /// Reserve a single invalidation receipt. Sending is the parent's job, outside
    /// its lock. A delayed/stale receipt remains safe because reads are cursored.
    pub(crate) fn notification(&mut self) -> Option<Revision> {
        if self.pending_notice.is_some() {
            None
        } else {
            self.pending_notice = Some(self.revision);
            Some(self.revision)
        }
    }

    pub(crate) fn publication_failed(&mut self, receipt: Revision) {
        if self.pending_notice == Some(receipt) {
            self.pending_notice = None;
        }
    }

    pub(crate) fn voice(
        &mut self,
        voice: VoiceProjection,
    ) -> Result<Option<Revision>, ObservationError> {
        let revision = self.next_revision()?;
        if let Some(id) = voice.capture_id {
            safe_integer(id)?;
        }
        if voice
            .wake_confidence
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(ObservationError::InvalidProjection);
        }
        if let Some(partial) = &voice.partial_transcript {
            safe_integer(partial.session_id)?;
            safe_integer(partial.revision)?;
            if partial.text.len() > MAX_TEXT_BYTES {
                return Err(ObservationError::ProjectionFull);
            }
        }
        if voice
            .error
            .as_ref()
            .is_some_and(|error| error.len() > MAX_METADATA_BYTES)
        {
            return Err(ObservationError::ProjectionFull);
        }
        let size = json_size(&voice)?;
        self.check_budget(
            size,
            self.speech_bytes,
            self.prompt_bytes,
            self.prompt
                .as_ref()
                .is_some_and(|prompt| prompt.result.is_none()),
        )?;
        let event = DesktopRuntimeEvent::Voice {
            revision,
            voice: voice.clone(),
        };
        let bytes = json_size(&event)?;
        self.voice = voice;
        self.voice_bytes = size;
        Ok(self.record(event, bytes))
    }

    pub(crate) fn speech(
        &mut self,
        speech: SpeechProjection,
    ) -> Result<Option<Revision>, ObservationError> {
        let revision = self.next_revision()?;
        if let Some(id) = speech.playback_id {
            safe_integer(id)?;
        }
        let size = json_size(&speech)?;
        self.check_budget(
            self.voice_bytes,
            size,
            self.prompt_bytes,
            self.prompt
                .as_ref()
                .is_some_and(|prompt| prompt.result.is_none()),
        )?;
        let event = DesktopRuntimeEvent::Speech {
            revision,
            speech: speech.clone(),
        };
        let bytes = json_size(&event)?;
        self.speech = speech;
        self.speech_bytes = size;
        Ok(self.record(event, bytes))
    }

    pub(crate) fn start_prompt(
        &mut self,
        mut prompt: NativePromptProjection,
    ) -> Result<Option<Revision>, ObservationError> {
        let revision = self.next_revision()?;
        if voxgolem_core::conversation::validate_request_id(&prompt.request_id).is_err()
            || prompt.prompt.trim().is_empty()
            || prompt.prompt.len() > MAX_PROMPT_BYTES
            || [
                &prompt.execution_id.session_id,
                &prompt.execution_id.generation,
            ]
            .iter()
            .any(|value| {
                !value.contains(':')
                    || serde_json::from_value::<voxgolem_core::ipc::WireId>(serde_json::json!(
                        value
                    ))
                    .is_err()
            })
        {
            return Err(ObservationError::InvalidProjection);
        }
        if let Some(result) = &prompt.result {
            validate_result(result)?;
            validate_result_identity(&prompt, result)?;
        }
        if json_size(&prompt)? > MAX_PROJECTION_BYTES {
            return Err(ObservationError::ProjectionFull);
        }
        let incoming = std::mem::take(&mut prompt.updates);
        let mut sizes = Vec::new();
        for update in incoming {
            plan_update(&prompt.updates, &sizes, &update)?.commit(&mut prompt.updates, &mut sizes);
        }
        let size = json_size(&prompt)?;
        self.check_budget(
            self.voice_bytes,
            self.speech_bytes,
            size,
            prompt.result.is_none(),
        )?;
        let event = DesktopRuntimeEvent::PromptStarted {
            revision,
            prompt: prompt.clone(),
        };
        let bytes = json_size(&event)?;
        self.prompt = Some(prompt);
        self.prompt_bytes = size;
        self.update_sizes = sizes;
        Ok(self.record(event, bytes))
    }

    pub(crate) fn update_prompt_for_execution(
        &mut self,
        request_id: &str,
        execution_id: &ExecutionId,
        update: &PromptExecutionEventPayload,
    ) -> Result<Option<Revision>, ObservationError> {
        let Some(prompt) = self.prompt.as_ref().filter(|prompt| {
            prompt.request_id == request_id
                && prompt.execution_id == *execution_id
                && prompt.result.is_none()
        }) else {
            return Ok(None);
        };
        let revision = self.next_revision()?;
        let plan = plan_update(&prompt.updates, &self.update_sizes, update)?;
        let size = self
            .prompt_bytes
            .checked_add_signed(plan.delta)
            .ok_or(ObservationError::ProjectionFull)?;
        self.check_budget(self.voice_bytes, self.speech_bytes, size, true)?;
        // Source deltas must expose the same capped, stable set as a resync;
        // otherwise a continuously attached renderer could accumulate links that
        // the bounded authoritative projection deliberately did not retain.
        let event_update = match &plan.mutation {
            UpdatePlan::Push(PromptExecutionEventPayload::Sources { sources })
            | UpdatePlan::Replace {
                update: PromptExecutionEventPayload::Sources { sources },
                ..
            } => PromptExecutionEventPayload::Sources {
                sources: sources.clone(),
            },
            _ => update.clone(),
        };
        let event = DesktopRuntimeEvent::PromptUpdate {
            revision,
            request_id: request_id.to_owned(),
            execution_id: execution_id.clone(),
            update: event_update,
        };
        let bytes = json_size(&event)?;
        // The ring also has a per-entry budget. Large but valid projections force
        // snapshot resync instead of cancelling native execution.
        let prompt = self.prompt.as_mut().expect("validated prompt");
        plan.commit(&mut prompt.updates, &mut self.update_sizes);
        self.prompt_bytes = size;
        Ok(self.record(event, bytes))
    }

    pub(crate) fn finish_prompt_for_execution(
        &mut self,
        request_id: &str,
        execution_id: &ExecutionId,
        result: serde_json::Value,
    ) -> Result<Option<Revision>, ObservationError> {
        let Some(prompt) = self.prompt.as_ref().filter(|prompt| {
            prompt.request_id == request_id
                && prompt.execution_id == *execution_id
                && prompt.result.is_none()
        }) else {
            return Ok(None);
        };
        let revision = self.next_revision()?;
        let result_size = validate_result(&result)?;
        validate_result_identity(prompt, &result)?;
        let size = self.prompt_bytes - 4 + result_size;
        self.check_budget(self.voice_bytes, self.speech_bytes, size, false)?;
        let event = DesktopRuntimeEvent::PromptFinished {
            revision,
            request_id: prompt.request_id.clone(),
            execution_id: execution_id.clone(),
            result: result.clone(),
        };
        let bytes = json_size(&event)?;
        self.prompt.as_mut().expect("validated prompt").result = Some(result);
        self.prompt_bytes = size;
        Ok(self.record(event, bytes))
    }

    #[cfg(test)]
    fn update_prompt(
        &mut self,
        request_id: &str,
        update: &PromptExecutionEventPayload,
    ) -> Result<Option<Revision>, ObservationError> {
        let Some(id) = self.prompt.as_ref().map(|p| p.execution_id.clone()) else {
            return Ok(None);
        };
        self.update_prompt_for_execution(request_id, &id, update)
    }

    #[cfg(test)]
    fn finish_prompt(
        &mut self,
        request_id: &str,
        result: serde_json::Value,
    ) -> Result<Option<Revision>, ObservationError> {
        let Some(id) = self.prompt.as_ref().map(|p| p.execution_id.clone()) else {
            return Ok(None);
        };
        self.finish_prompt_for_execution(request_id, &id, result)
    }

    /// Reset/retirement has no invented wire event kind. It advances the cursor
    /// and requires a snapshot, removing any replay of the retired projection.
    pub(crate) fn clear_prompt(&mut self) -> Result<Option<Revision>, ObservationError> {
        let revision = self.next_revision()?;
        self.prompt = None;
        self.prompt_bytes = 4;
        self.update_sizes.clear();
        self.ring.clear();
        self.ring_bytes = 0;
        self.revision = revision;
        self.resync_before = revision;
        Ok(self.notification())
    }

    /// One main renderer consumer only. Read and acknowledgment are one atomic
    /// operation under the caller's lock; subsequent mutations reserve a fresh
    /// notice even if the previous notice has not physically been emitted yet.
    pub(crate) fn read(&mut self, since: Option<Revision>) -> ReadDesktopRuntimePayload {
        let use_snapshot =
            since.is_none_or(|since| since < self.resync_before || since > self.revision);
        let snapshot = use_snapshot.then(|| DesktopRuntimeSnapshot {
            revision: self.revision,
            voice: self.voice.clone(),
            prompt: self.prompt.clone(),
            speech: self.speech.clone(),
        });
        let events = if use_snapshot {
            Vec::new()
        } else {
            self.ring
                .iter()
                .filter(|(event, _)| event.revision() > since.unwrap_or_default())
                .map(|(event, _)| event.clone())
                .collect()
        };
        self.pending_notice = None;
        ReadDesktopRuntimePayload {
            revision: self.revision,
            snapshot,
            events,
        }
    }
}

fn validate_result(result: &serde_json::Value) -> Result<usize, ObservationError> {
    fn validate_numbers(value: &serde_json::Value, depth: usize) -> Result<(), ObservationError> {
        if depth > 64 {
            return Err(ObservationError::InvalidProjection);
        }
        match value {
            serde_json::Value::Number(number) => {
                if let Some(value) = number.as_u64() {
                    safe_integer(value)?;
                } else if let Some(value) = number.as_i64() {
                    safe_integer(value.unsigned_abs())?;
                } else if number
                    .as_f64()
                    .is_none_or(|value| !value.is_finite() || value.abs() > MAX_SAFE_INTEGER as f64)
                {
                    return Err(ObservationError::UnsafeInteger);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    validate_numbers(value, depth + 1)?;
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values() {
                    validate_numbers(value, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    validate_numbers(result, 0)?;
    let fields = result
        .as_object()
        .ok_or(ObservationError::InvalidProjection)?;
    let request = fields
        .get("request_id")
        .and_then(|v| v.as_str())
        .ok_or(ObservationError::InvalidProjection)?;
    if voxgolem_core::conversation::validate_request_id(request).is_err()
        || serde_json::from_value::<voxgolem_core::ipc::RuntimePhase>(
            fields
                .get("runtime_phase")
                .cloned()
                .ok_or(ObservationError::InvalidProjection)?,
        )
        .is_err()
    {
        return Err(ObservationError::InvalidProjection);
    }
    let valid = match fields.get("kind").and_then(|v| v.as_str()) {
        Some("completed" | "refused") => {
            fields.len() == 5
                && fields.get("turn").is_some_and(|v| v.is_object())
                && fields.get("save_disposition").and_then(|v| v.as_str()) == Some("saved")
        }
        Some("unsaved") => {
            fields.len() == 5
                && fields.get("unsaved_result").is_some_and(|v| v.is_object())
                && fields.get("save_disposition").and_then(|v| v.as_str()) == Some("unsaved")
        }
        Some("cancelled") => fields.len() == 3,
        Some("error") => {
            fields.len() == 4
                && fields
                    .get("error_message")
                    .and_then(|v| v.as_str())
                    .is_some_and(|message| {
                        !message.trim().is_empty() && message.len() <= MAX_METADATA_BYTES
                    })
        }
        _ => false,
    };
    if !valid {
        return Err(ObservationError::InvalidProjection);
    }
    let size = json_size(result)?;
    if size > MAX_RESULT_BYTES {
        return Err(ObservationError::ProjectionFull);
    }
    Ok(size)
}

fn validate_result_identity(
    prompt: &NativePromptProjection,
    result: &serde_json::Value,
) -> Result<(), ObservationError> {
    if result["request_id"].as_str() != Some(prompt.request_id.as_str()) {
        return Err(ObservationError::InvalidProjection);
    }
    let body = match result["kind"].as_str() {
        Some("completed" | "refused") => Some(&result["turn"]),
        Some("unsaved") => Some(&result["unsaved_result"]),
        _ => None,
    };
    if body.is_some_and(|body| {
        body["request_id"].as_str() != Some(prompt.request_id.as_str())
            || body["execution_id"]["session_id"].as_str()
                != Some(prompt.execution_id.session_id.as_str())
            || body["execution_id"]["generation"].as_str()
                != Some(prompt.execution_id.generation.as_str())
    }) {
        return Err(ObservationError::InvalidProjection);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared_identity() -> voxgolem_core::ipc::ExecutionId {
        serde_json::from_value(serde_json::json!({"session_id":"00000000-0000-4000-8000-000000000001:17", "generation":"00000000-0000-4000-8000-000000000001:2"})).unwrap()
    }

    #[test]
    fn observer_requires_both_alias_and_durable_identity_and_current_error_shape() {
        let mut owner = DesktopObservation::new();
        let current = prompt("synthetic-reused-alias");
        let mut wrong = current.execution_id.clone();
        wrong.generation = "00000000-0000-4000-8000-000000000001:99".into();
        owner.start_prompt(current.clone()).unwrap();
        assert_eq!(
            owner.update_prompt_for_execution(
                &current.request_id,
                &wrong,
                &text("Synthetic stale delta")
            ),
            Ok(None)
        );
        assert_eq!(
            owner.finish_prompt_for_execution(
                &current.request_id,
                &wrong,
                canonical_result(
                    &current.request_id,
                    "Synthetic question",
                    "Synthetic stale answer"
                )
            ),
            Ok(None)
        );
        assert_eq!(owner.revision, 1);
        let mut mismatched = canonical_result(
            &current.request_id,
            "Synthetic question",
            "Synthetic answer",
        );
        mismatched["turn"]["execution_id"]["generation"] = wrong.generation.into();
        assert_eq!(
            owner.finish_prompt_for_execution(
                &current.request_id,
                &current.execution_id,
                mismatched
            ),
            Err(ObservationError::InvalidProjection)
        );
        assert_eq!(owner.revision, 1);
        let error = serde_json::json!({"kind":"error","request_id":current.request_id,"runtime_phase":"sleeping","error_message":"Synthetic operation failed"});
        for extra in ["turn", "save_disposition", "unsaved_result"] {
            let mut invalid = error.clone();
            invalid[extra] = serde_json::Value::Null;
            assert_eq!(
                validate_result(&invalid),
                Err(ObservationError::InvalidProjection)
            );
        }
        for message in ["".to_owned(), "x".repeat(MAX_METADATA_BYTES + 1)] {
            let mut invalid = error.clone();
            invalid["error_message"] = message.into();
            assert_eq!(
                validate_result(&invalid),
                Err(ObservationError::InvalidProjection)
            );
        }
        assert_eq!(
            validate_result(
                &serde_json::json!({"outcome":"error","request_id":"synthetic-reused-alias","runtime_phase":"sleeping","error_message":"Synthetic legacy shape"})
            ),
            Err(ObservationError::InvalidProjection)
        );
        owner
            .finish_prompt_for_execution(&current.request_id, &current.execution_id, error)
            .unwrap();
        let read = serde_json::to_value(owner.read(Some(1))).unwrap();
        assert_eq!(
            read["events"][0]["execution_id"],
            serde_json::to_value(prepared_identity()).unwrap()
        );
        assert_eq!(read["events"][0]["result"]["kind"], "error");
    }

    fn prompt(id: &str) -> NativePromptProjection {
        NativePromptProjection {
            request_id: id.into(),
            execution_id: ExecutionId::from(&prepared_identity()),
            prompt: "Synthetic question".into(),
            updates: Vec::new(),
            result: None,
        }
    }

    fn text(text: &str) -> PromptExecutionEventPayload {
        PromptExecutionEventPayload::Text { text: text.into() }
    }

    fn assert_exact_prompt_cost(owner: &DesktopObservation) {
        assert_eq!(owner.prompt_bytes, json_size(&owner.prompt).unwrap());
        let prompt = owner.prompt.as_ref().unwrap();
        assert_eq!(owner.update_sizes.len(), prompt.updates.len());
        for (update, size) in prompt.updates.iter().zip(&owner.update_sizes) {
            assert_eq!(json_size(update).unwrap(), *size);
        }
    }

    fn canonical_result(request_id: &str, user: &str, answer: &str) -> serde_json::Value {
        use voxgolem_core::ipc;
        assert!(user.len() <= MAX_PROMPT_BYTES);
        assert!(user.len() + answer.len() <= voxgolem_core::conversation::MAX_COMMITTED_TEXT_BYTES);
        let turn = ipc::Turn {
            sources: Vec::new(),
            request_id: request_id.into(),
            execution_id: prepared_identity(),
            user: ipc::Message {
                id: 1_u64.into(),
                role: ipc::MessageRole::User,
                content: ipc::MessageContentKind::Text(user.into()),
            },
            answer: ipc::Message {
                id: 2_u64.into(),
                role: ipc::MessageRole::Assistant,
                content: ipc::MessageContentKind::Text(answer.into()),
            },
            final_stage: ipc::Stage::Instant,
        };
        let mut value = serde_json::to_value(ipc::PromptOutcome::completed(turn).unwrap()).unwrap();
        value["request_id"] = request_id.into();
        value["runtime_phase"] = "sleeping".into();
        value
    }

    fn active_budget_usage(owner: &DesktopObservation) -> usize {
        owner.voice_bytes
            + owner.speech_bytes
            + owner.prompt_bytes
            + SNAPSHOT_ENVELOPE_BYTES
            + MAX_RESULT_BYTES
            - 4
    }

    fn full_active_projection() -> DesktopObservation {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        owner
            .update_prompt("synthetic-1", &text(&"\u{0001}".repeat(MAX_TEXT_BYTES)))
            .unwrap();
        let empty_reasoning = PromptExecutionEventPayload::Reasoning {
            text: String::new(),
        };
        let plan = plan_update(
            &owner.prompt.as_ref().unwrap().updates,
            &owner.update_sizes,
            &empty_reasoning,
        )
        .unwrap();
        let available = MAX_PROJECTION_BYTES
            - active_budget_usage(&owner)
            - usize::try_from(plan.delta).unwrap();
        // JSON encodes U+0001 in six bytes and ASCII in one. Fill the exact
        // remaining serialized budget while respecting the decoder's raw limit.
        let reasoning = "\u{0001}".repeat(available / 6) + &"x".repeat(available % 6);
        assert!(reasoning.len() <= MAX_TEXT_BYTES);
        assert_eq!(json_size(&reasoning).unwrap(), available + 2);
        owner
            .update_prompt(
                "synthetic-1",
                &PromptExecutionEventPayload::Reasoning { text: reasoning },
            )
            .unwrap();
        assert_eq!(active_budget_usage(&owner), MAX_PROJECTION_BYTES);
        assert_exact_prompt_cost(&owner);
        owner
    }

    #[test]
    fn one_notification_until_read_then_new_notice() {
        let mut owner = DesktopObservation::new();
        assert_eq!(owner.speech(SpeechProjection::default()).unwrap(), Some(1));
        assert_eq!(owner.speech(SpeechProjection::default()).unwrap(), None);
        assert_eq!(owner.read(Some(0)).revision, 2);
        assert_eq!(owner.speech(SpeechProjection::default()).unwrap(), Some(3));
        owner.publication_failed(1);
        assert_eq!(
            owner.notification(),
            None,
            "old failure cannot clear notice 3"
        );
        owner.publication_failed(3);
        assert_eq!(owner.notification(), Some(3));
    }

    #[test]
    fn evicted_and_future_cursors_resynchronize() {
        let mut owner = DesktopObservation::new();
        for _ in 0..70 {
            owner.speech(SpeechProjection::default()).unwrap();
        }
        let read = owner.read(Some(0));
        assert!(read.snapshot.is_some());
        assert!(read.events.is_empty());
        assert!(owner.read(Some(71)).snapshot.is_some());
        assert!(owner.read(Some(70)).snapshot.is_none());
        assert!(owner.ring.len() <= MAX_EVENTS);
        assert!(owner.ring_bytes <= MAX_RING_BYTES);
    }

    #[test]
    fn text_accumulates_and_correction_replaces_without_replay() {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        for delta in ["First", "\n", "quoted \"", "é", "\u{0001}"] {
            owner.update_prompt("synthetic-1", &text(delta)).unwrap();
            assert_exact_prompt_cost(&owner);
        }
        let snapshot = owner.read(None).snapshot.unwrap();
        assert_eq!(snapshot.prompt.unwrap().updates.len(), 1);
        owner
            .update_prompt(
                "synthetic-1",
                &PromptExecutionEventPayload::Correction {
                    stage: super::super::StagePayload::Review,
                    text: "Replacement".into(),
                    correction: "Synthetic correction".into(),
                },
            )
            .unwrap();
        owner.update_prompt("synthetic-1", &text(" tail")).unwrap();
        assert_exact_prompt_cost(&owner);
        let snapshot = serde_json::to_value(owner.read(None).snapshot.unwrap()).unwrap();
        assert_eq!(
            snapshot["prompt"]["updates"],
            serde_json::json!([{ "kind": "text", "text": "Replacement tail" }])
        );
    }

    #[test]
    fn stages_and_metadata_replace_while_sources_merge_stably() {
        use super::super::{SourcePayload, StagePayload, StageStatusPayload};
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        for stage in [
            StagePayload::Instant,
            StagePayload::Deep,
            StagePayload::Review,
        ] {
            for status in [StageStatusPayload::Queued, StageStatusPayload::Completed] {
                owner
                    .update_prompt(
                        "synthetic-1",
                        &PromptExecutionEventPayload::Stage {
                            stage,
                            status,
                            detail: None,
                        },
                    )
                    .unwrap();
            }
        }
        for i in 0..40 {
            owner
                .update_prompt(
                    "synthetic-1",
                    &PromptExecutionEventPayload::Sources {
                        sources: vec![SourcePayload {
                            url: format!("https://source-{i}.invalid"),
                            title: format!("Synthetic {i}"),
                        }],
                    },
                )
                .unwrap();
            owner
                .update_prompt(
                    "synthetic-1",
                    &PromptExecutionEventPayload::Status {
                        message: format!("Synthetic status {i}"),
                    },
                )
                .unwrap();
            assert_exact_prompt_cost(&owner);
        }
        let updates = &owner.prompt.as_ref().unwrap().updates;
        assert_eq!(updates.len(), 5);
        let PromptExecutionEventPayload::Sources { sources } = &updates[3] else {
            panic!("missing sources")
        };
        assert_eq!(sources.len(), 32);
        assert_eq!(sources[0].url, "https://source-0.invalid");
        assert_eq!(sources[31].url, "https://source-31.invalid");
        let read = owner.read(Some(owner.revision - 1));
        // The last event is status; inspect the preceding sources delta too.
        assert_eq!(read.events.len(), 1);
        let read = owner.read(Some(owner.revision - 2));
        let DesktopRuntimeEvent::PromptUpdate {
            update: PromptExecutionEventPayload::Sources { sources },
            ..
        } = &read.events[0]
        else {
            panic!("missing live sources")
        };
        assert_eq!(sources.len(), MAX_SOURCES);
        assert_eq!(sources.last().unwrap().url, "https://source-31.invalid");
    }

    #[test]
    fn stale_or_terminal_updates_do_not_increment_revision() {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        owner
            .update_prompt("old-request", &text("ignored"))
            .unwrap();
        owner
            .finish_prompt("old-request", serde_json::json!({}))
            .unwrap();
        assert_eq!(owner.revision, 1);
        owner
            .finish_prompt(
                "synthetic-1",
                canonical_result("synthetic-1", "Synthetic question", "Synthetic answer"),
            )
            .unwrap();
        assert_exact_prompt_cost(&owner);
        owner
            .update_prompt("synthetic-1", &text("ignored after terminal"))
            .unwrap();
        owner
            .finish_prompt("synthetic-1", serde_json::json!({"kind":"cancelled","request_id":"synthetic-1","runtime_phase":"sleeping"}))
            .unwrap();
        assert_eq!(owner.revision, 2);
        assert_eq!(owner.prompt.unwrap().result.unwrap()["kind"], "completed");
    }

    #[test]
    fn ring_byte_eviction_preserves_current_projection() {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        for _ in 0..5 {
            owner
                .update_prompt(
                    "synthetic-1",
                    &PromptExecutionEventPayload::Reasoning {
                        text: "x".repeat(80 * 1024),
                    },
                )
                .unwrap();
        }
        assert!(owner.ring.len() < 6);
        assert!(owner.ring_bytes <= MAX_RING_BYTES);
        assert!(owner.read(Some(1)).snapshot.is_some());
        assert_exact_prompt_cost(&owner);
        assert!(owner.read(Some(5)).events.len() == 1);
    }

    #[test]
    fn oversize_event_forces_snapshot_not_execution_cancellation() {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        owner
            .update_prompt("synthetic-1", &text(&"\n".repeat(150 * 1024)))
            .unwrap();
        assert!(owner.ring.is_empty());
        let read = owner.read(Some(1));
        assert_eq!(read.revision, 2);
        assert!(read.snapshot.unwrap().prompt.unwrap().result.is_none());
        assert_exact_prompt_cost(&owner);
    }

    #[test]
    fn rejection_is_transactional_and_text_is_bounded() {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        owner
            .update_prompt("synthetic-1", &text(&"x".repeat(MAX_TEXT_BYTES)))
            .unwrap();
        assert_eq!(
            owner.update_prompt("synthetic-1", &text("x")),
            Err(ObservationError::ProjectionFull)
        );
        assert_eq!(owner.revision, 2);
        assert_exact_prompt_cost(&owner);
        assert_eq!(
            owner.finish_prompt(
                "synthetic-1",
                serde_json::json!({"bad":MAX_SAFE_INTEGER + 1})
            ),
            Err(ObservationError::UnsafeInteger)
        );
        assert_eq!(owner.revision, 2);
        assert!(owner.prompt.as_ref().unwrap().result.is_none());
    }

    #[test]
    fn decoder_raw_bounds_are_rejected_before_revision_or_projection_mutation() {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        for update in [
            PromptExecutionEventPayload::Reasoning {
                text: "x".repeat(MAX_TEXT_BYTES + 1),
            },
            PromptExecutionEventPayload::Reasoning {
                text: "é".repeat(MAX_TEXT_BYTES / 2 + 1),
            },
            PromptExecutionEventPayload::Status {
                message: "x".repeat(MAX_METADATA_BYTES + 1),
            },
            PromptExecutionEventPayload::Error {
                message: "x".repeat(MAX_METADATA_BYTES + 1),
            },
            PromptExecutionEventPayload::Stage {
                stage: super::super::StagePayload::Instant,
                status: super::super::StageStatusPayload::Running,
                detail: Some("x".repeat(MAX_METADATA_BYTES + 1)),
            },
            PromptExecutionEventPayload::Tool {
                tool: "x".repeat(MAX_METADATA_BYTES + 1),
                status: "running".into(),
                detail: String::new(),
            },
            PromptExecutionEventPayload::Tool {
                tool: "Synthetic tool".into(),
                status: "running".into(),
                detail: "x".repeat(MAX_METADATA_BYTES + 1),
            },
            PromptExecutionEventPayload::Correction {
                stage: super::super::StagePayload::Review,
                text: "Synthetic replacement".into(),
                correction: "x".repeat(MAX_METADATA_BYTES + 1),
            },
            PromptExecutionEventPayload::Sources {
                sources: vec![super::super::SourcePayload {
                    url: "x".repeat(MAX_SOURCE_URL_BYTES + 1),
                    title: "Synthetic source".into(),
                }],
            },
            PromptExecutionEventPayload::Sources {
                sources: vec![super::super::SourcePayload {
                    url: "https://source.invalid/synthetic".into(),
                    title: "x".repeat(MAX_SOURCE_TITLE_BYTES + 1),
                }],
            },
            PromptExecutionEventPayload::Sources {
                sources: (0..=MAX_SOURCES)
                    .map(|index| super::super::SourcePayload {
                        url: format!("https://source.invalid/synthetic-{index}"),
                        title: "Synthetic source".into(),
                    })
                    .collect(),
            },
        ] {
            assert_eq!(
                owner.update_prompt("synthetic-1", &update),
                Err(ObservationError::ProjectionFull)
            );
            assert_eq!(owner.revision, 1);
            assert!(owner.prompt.as_ref().unwrap().updates.is_empty());
        }
        assert_eq!(
            owner.voice(VoiceProjection {
                error: Some("x".repeat(MAX_METADATA_BYTES + 1)),
                ..VoiceProjection::default()
            }),
            Err(ObservationError::ProjectionFull)
        );
        assert_eq!(
            owner.voice(VoiceProjection {
                partial_transcript: Some(PartialTranscriptionEventPayload {
                    session_id: 1,
                    revision: 1,
                    text: "x".repeat(MAX_TEXT_BYTES + 1)
                }),
                ..VoiceProjection::default()
            }),
            Err(ObservationError::ProjectionFull)
        );
        assert_eq!(owner.revision, 1);
        assert!(owner.voice.error.is_none());
        assert!(owner.voice.partial_transcript.is_none());
    }

    #[test]
    fn aggregate_projection_budget_is_enforced_before_mutation() {
        let mut owner = full_active_projection();
        let before = owner.revision;
        let voice = VoiceProjection {
            error: Some("Synthetic capture error".into()),
            ..VoiceProjection::default()
        };
        assert_eq!(owner.voice(voice), Err(ObservationError::ProjectionFull));
        assert_eq!(owner.revision, before);
        assert!(owner.voice.error.is_none());
        assert_exact_prompt_cost(&owner);
    }

    #[test]
    fn active_projection_reserves_capacity_for_bounded_terminal_result() {
        let mut owner = full_active_projection();
        let before = owner.revision;
        assert_eq!(
            owner.update_prompt(
                "synthetic-1",
                &PromptExecutionEventPayload::Status {
                    message: "Synthetic status".into()
                }
            ),
            Err(ObservationError::ProjectionFull)
        );
        assert_eq!(owner.revision, before);
        let user = owner.prompt.as_ref().unwrap().prompt.clone();
        let answer =
            "\u{0001}".repeat(voxgolem_core::conversation::MAX_COMMITTED_TEXT_BYTES - user.len());
        let result = canonical_result("synthetic-1", &user, &answer);
        let result_bytes = json_size(&result).unwrap();
        assert!(result_bytes > MAX_TEXT_BYTES);
        assert!(result_bytes <= MAX_RESULT_BYTES);
        owner.finish_prompt("synthetic-1", result).unwrap();
        assert_exact_prompt_cost(&owner);
        assert!(json_size(&owner.read(None).snapshot.unwrap()).unwrap() <= MAX_PROJECTION_BYTES);
    }

    #[test]
    fn quote_and_control_heavy_canonical_outcomes_fit_actual_encoded_budgets() {
        for fill in ['"', '\u{0001}'] {
            let mut owner = DesktopObservation::new();
            owner
                .voice(VoiceProjection {
                    partial_transcript: Some(PartialTranscriptionEventPayload {
                        session_id: 1,
                        revision: 1,
                        text: "x".repeat(MAX_TEXT_BYTES),
                    }),
                    auto_stop_on_silence: false,
                    auto_start_available: false,
                    ..VoiceProjection::default()
                })
                .unwrap();
            let mut initial = prompt(&"s".repeat(MAX_REQUEST_ID_BYTES));
            initial.prompt = fill.to_string().repeat(MAX_PROMPT_BYTES);
            let request_id = initial.request_id.clone();
            let user = initial.prompt.clone();
            let answer = fill
                .to_string()
                .repeat(voxgolem_core::conversation::MAX_COMMITTED_TEXT_BYTES - user.len());
            owner.start_prompt(initial).unwrap();
            owner.update_prompt(&request_id, &text(&answer)).unwrap();
            let result = canonical_result(&request_id, &user, &answer);
            let encoded = serde_json::to_vec(&result).unwrap();
            assert_eq!(encoded.len(), json_size(&result).unwrap());
            assert!(encoded.len() > voxgolem_core::conversation::MAX_COMMITTED_TEXT_BYTES);
            assert!(encoded.len() <= MAX_RESULT_BYTES);
            owner.finish_prompt(&request_id, result.clone()).unwrap();
            assert_exact_prompt_cost(&owner);
            let snapshot = owner.read(None).snapshot.unwrap();
            assert!(json_size(&snapshot).unwrap() <= MAX_PROJECTION_BYTES);
            assert!(!snapshot.voice.auto_stop_on_silence);
            assert!(!snapshot.voice.auto_start_available);
            assert_eq!(
                snapshot.voice.partial_transcript.unwrap().text.len(),
                MAX_TEXT_BYTES
            );
            assert_eq!(snapshot.prompt.unwrap().result.unwrap(), result);
            assert!(
                owner.read(Some(0)).snapshot.is_some(),
                "oversized ring entries require resync, not cancellation"
            );
        }
    }

    #[test]
    fn raw_prompt_identity_and_voice_error_boundaries_match_decoder() {
        let mut owner = DesktopObservation::new();
        let mut initial = prompt("synthetic-1");
        initial.prompt = "x".repeat(MAX_PROMPT_BYTES + 1);
        assert_eq!(
            owner.start_prompt(initial),
            Err(ObservationError::InvalidProjection)
        );
        assert_eq!(
            owner.start_prompt(prompt(&"s".repeat(MAX_REQUEST_ID_BYTES + 1))),
            Err(ObservationError::InvalidProjection)
        );
        assert_eq!(owner.revision, 0);
        owner
            .voice(VoiceProjection {
                error: Some("\u{0001}".repeat(MAX_METADATA_BYTES)),
                ..VoiceProjection::default()
            })
            .unwrap();
        assert_eq!(
            owner.voice.error.as_ref().unwrap().len(),
            MAX_METADATA_BYTES
        );
        let defaults = VoiceProjection::default();
        assert!(defaults.auto_stop_on_silence && defaults.auto_start_available);
        for bad in [" ", "\n\t"] {
            let mut initial = prompt("synthetic-1");
            initial.prompt = bad.into();
            assert_eq!(
                owner.start_prompt(initial),
                Err(ObservationError::InvalidProjection)
            );
            assert_eq!(
                owner.start_prompt(prompt(bad)),
                Err(ObservationError::InvalidProjection)
            );
        }
        assert_eq!(owner.revision, 1);
    }

    #[test]
    fn initial_updates_are_reduced_and_invalid_start_preserves_previous_prompt() {
        let mut owner = DesktopObservation::new();
        let mut initial = prompt("synthetic-1");
        initial.updates = vec![
            text("a"),
            text("b"),
            PromptExecutionEventPayload::Status {
                message: "first".into(),
            },
            PromptExecutionEventPayload::Status {
                message: "latest".into(),
            },
        ];
        owner.start_prompt(initial).unwrap();
        assert_exact_prompt_cost(&owner);
        assert_eq!(owner.prompt.as_ref().unwrap().updates.len(), 2);
        let mut invalid = prompt("synthetic-2");
        invalid.execution_id.generation = "invalid".into();
        assert_eq!(
            owner.start_prompt(invalid),
            Err(ObservationError::InvalidProjection)
        );
        assert_eq!(owner.revision, 1);
        assert_eq!(owner.prompt.as_ref().unwrap().request_id, "synthetic-1");
        let mut result = serde_json::json!({});
        for _ in 0..66 {
            result = serde_json::json!([result]);
        }
        assert_eq!(
            owner.finish_prompt("synthetic-1", result),
            Err(ObservationError::InvalidProjection)
        );
        assert_eq!(owner.revision, 1);
    }

    #[test]
    fn voice_wire_values_are_validated_before_publication() {
        let mut owner = DesktopObservation::new();
        for confidence in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            assert_eq!(
                owner.voice(VoiceProjection {
                    wake_confidence: Some(confidence),
                    ..VoiceProjection::default()
                }),
                Err(ObservationError::InvalidProjection)
            );
        }
        let partial = PartialTranscriptionEventPayload {
            session_id: MAX_SAFE_INTEGER + 1,
            revision: 1,
            text: "Synthetic partial".into(),
        };
        assert_eq!(
            owner.voice(VoiceProjection {
                partial_transcript: Some(partial),
                ..VoiceProjection::default()
            }),
            Err(ObservationError::UnsafeInteger)
        );
        assert_eq!(owner.revision, 0);
    }

    #[test]
    fn overflow_fails_closed_and_unsafe_wire_numbers_are_rejected() {
        let mut owner = DesktopObservation::new();
        let speech = SpeechProjection {
            playback_id: Some(MAX_SAFE_INTEGER + 1),
            ..SpeechProjection::default()
        };
        assert_eq!(owner.speech(speech), Err(ObservationError::UnsafeInteger));
        assert_eq!(owner.revision, 0);
        owner.revision = MAX_SAFE_INTEGER;
        assert_eq!(
            owner.speech(SpeechProjection::default()),
            Err(ObservationError::RevisionExhausted)
        );
        assert_eq!(
            owner.clear_prompt(),
            Err(ObservationError::RevisionExhausted)
        );
        assert_eq!(owner.revision, MAX_SAFE_INTEGER);
    }

    #[test]
    fn read_ack_and_delayed_emit_do_not_lose_new_mutation() {
        let mut owner = DesktopObservation::new();
        let old_receipt = owner.speech(SpeechProjection::default()).unwrap().unwrap();
        assert_eq!(owner.read(None).revision, old_receipt);
        let new_receipt = owner.voice(VoiceProjection::default()).unwrap().unwrap();
        owner.publication_failed(old_receipt);
        assert_eq!(owner.notification(), None);
        let read = owner.read(Some(old_receipt));
        assert_eq!(read.revision, new_receipt);
        assert_eq!(read.events.len(), 1);
        assert!(matches!(&read.events[0], DesktopRuntimeEvent::Voice { .. }));
    }

    #[test]
    fn clear_prompt_fences_old_deltas_with_snapshot() {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        owner
            .update_prompt("synthetic-1", &text("old projection"))
            .unwrap();
        owner.clear_prompt().unwrap();
        let read = owner.read(Some(2));
        assert!(read.events.is_empty());
        assert!(read.snapshot.unwrap().prompt.is_none());
        assert_eq!(owner.update_prompt("synthetic-1", &text("late")), Ok(None));
        assert_eq!(owner.revision, 3);
    }

    #[test]
    fn wire_uses_flat_tagged_events_and_prepared_durable_identity() {
        let mut owner = DesktopObservation::new();
        owner.start_prompt(prompt("synthetic-1")).unwrap();
        owner.update_prompt("synthetic-1", &text("delta")).unwrap();
        let read = serde_json::to_value(owner.read(Some(0))).unwrap();
        assert_eq!(read["snapshot"], serde_json::Value::Null);
        assert_eq!(read["events"][0]["kind"], "prompt_started");
        assert_eq!(
            read["events"][0]["prompt"]["execution_id"]["session_id"],
            "00000000-0000-4000-8000-000000000001:17"
        );
        assert_eq!(
            read["events"][1],
            serde_json::json!({"revision":2,"kind":"prompt_update","request_id":"synthetic-1","execution_id": prepared_identity(),"update":{"kind":"text","text":"delta"}})
        );
    }
}
