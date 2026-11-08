//! Opaque, bounded policy handles. No engine resources or raw pointers cross JNI.
use jni::objects::{JObject, JString};
use jni::sys::{jboolean, jlong, jstring};
use jni::JNIEnv;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use voxgolem_core::reply_speech::{first_paragraph, ReplySpeech, SpeechRequest};
use voxgolem_core::speech_delivery::SpeechDelivery;

const MAX_INPUT_UTF16: i32 = 1_048_576;
const MAX_INPUT_UTF8: usize = 3 * MAX_INPUT_UTF16 as usize;

// Inspect Java length before copying across JNI; never retain an unbounded input.
fn bounded_text(env: &mut JNIEnv, text: &JString) -> Result<Option<String>, ()> {
    if text.is_null() {
        return Ok(None);
    }
    let length = env
        .call_method(text, "length", "()I", &[])
        .map_err(|_| ())?
        .i()
        .map_err(|_| ())?;
    if !(0..=MAX_INPUT_UTF16).contains(&length) {
        return Err(());
    }
    let value: String = env.get_string(text).map_err(|_| ())?.into();
    if value.len() > MAX_INPUT_UTF8 {
        return Err(());
    }
    Ok(Some(value))
}

struct Owner {
    selection: ReplySpeech,
    delivery: SpeechDelivery,
    settled: bool,
}
struct Batch {
    cancel: bool,
    closed: bool,
    idle: bool,
    released: bool,
    accepted: bool,
    ready: Vec<SpeechRequest>,
}
#[derive(Default)]
struct Registry {
    next: i64,
    owners: BTreeMap<i64, Owner>,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
impl Registry {
    fn open(&mut self, enabled: bool) -> i64 {
        if self.owners.len() >= 64 {
            return 0;
        }
        let Some(next) = self.next.checked_add(1) else {
            return 0;
        };
        self.next = next;
        let mut selection = ReplySpeech::early_clauses();
        selection.begin("reply", enabled);
        self.owners.insert(
            next,
            Owner {
                selection,
                delivery: SpeechDelivery::new(2),
                settled: false,
            },
        );
        next
    }
    // 0 append, 1 correction, 2 canonical settlement, 3 explicit close/reset,
    // 4 playback acknowledgement ("generation:chunk"). Settlement retains the
    // bounded owner until EVERY admitted playback callback has acknowledged.
    fn apply(&mut self, handle: i64, operation: i32, text: Option<&str>) -> Option<Batch> {
        if !(0..=4).contains(&operation) || operation == 3 {
            self.owners.remove(&handle);
            return None;
        }
        let ack = if operation == 4 {
            let parsed =
                text.and_then(|value| value.split_once(':'))
                    .and_then(|(generation, chunk)| {
                        Some((generation.parse::<u64>().ok()?, chunk.parse::<u64>().ok()?))
                    });
            if parsed.is_none() {
                self.owners.remove(&handle);
                return None;
            }
            parsed
        } else {
            None
        };
        let owner = self.owners.get_mut(&handle)?;
        let mut accepted = true;
        let decision = match operation {
            0 => owner.selection.text("reply", text.unwrap_or("")),
            1 => owner.selection.corrected("reply"),
            2 => {
                owner.settled = true;
                owner.selection.settle("reply", text)
            }
            _ => {
                let (generation, chunk) = ack?;
                accepted = owner.delivery.complete(generation, chunk);
                Default::default()
            }
        };
        let cancel = decision.cancel;
        if owner.delivery.apply(decision).is_err() {
            self.owners.remove(&handle);
            return None;
        }
        let mut ready = Vec::with_capacity(2);
        while let Some(request) = owner.delivery.take_ready() {
            ready.push(request);
        }
        let facts = owner.delivery.facts();
        let idle = owner.delivery.is_idle();
        let released = owner.settled && idle;
        let result = Batch {
            cancel,
            closed: facts.stream_closed,
            idle,
            released,
            accepted,
            ready,
        };
        if released {
            self.owners.remove(&handle);
        }
        Some(result)
    }
}

#[allow(unsafe_code)] // JNI symbol export only; no unsafe operations.
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_speech_AssistantSpeechNative_open(
    _env: JNIEnv,
    _object: JObject,
    enabled: jboolean,
) -> jlong {
    registry()
        .lock()
        .map(|mut r| r.open(enabled != 0))
        .unwrap_or(0)
}

#[allow(unsafe_code)] // JNI symbol export only; no unsafe operations.
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_speech_AssistantSpeechNative_apply(
    mut env: JNIEnv,
    _object: JObject,
    handle: jlong,
    operation: i32,
    text: JString,
) -> jstring {
    if !(0..=4).contains(&operation) {
        if let Ok(mut r) = registry().lock() {
            r.owners.remove(&handle);
        }
        return std::ptr::null_mut();
    }
    let text = match bounded_text(&mut env, &text) {
        Ok(text) => text,
        Err(()) => {
            if let Ok(mut r) = registry().lock() {
                r.owners.remove(&handle);
            }
            return std::ptr::null_mut();
        }
    };
    let decision = registry()
        .lock()
        .ok()
        .and_then(|mut r| r.apply(handle, operation, text.as_deref()));
    let Some(d) = decision else {
        return std::ptr::null_mut();
    };
    let ready: Vec<_> = d
        .ready
        .into_iter()
        .map(|s| serde_json::json!({"generation":s.generation,"chunk":s.chunk_id,"text":s.text}))
        .collect();
    let result = serde_json::json!({"cancel":d.cancel,"closed":d.closed,"idle":d.idle,
        "released":d.released,"accepted":d.accepted,"ready":ready})
    .to_string();
    env.new_string(result)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[allow(unsafe_code)] // JNI symbol export only; no unsafe operations.
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_speech_AssistantSpeechNative_firstParagraph(
    mut env: JNIEnv,
    _object: JObject,
    text: JString,
) -> jstring {
    let Ok(Some(text)) = bounded_text(&mut env, &text) else {
        return std::ptr::null_mut();
    };
    let selected = first_paragraph(&text).unwrap_or_default();
    env.new_string(selected)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// One coarse catalog call per selection/cache miss, never per PCM frame.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_speech_SpeechSelectionNative_select(
    mut env: JNIEnv,
    _object: JObject,
    input: JString,
) -> i32 {
    #[derive(serde::Deserialize)]
    struct Query {
        selection: voxgolem_core::speech_selection::VoiceSelection,
        voices: Vec<voxgolem_core::speech_selection::VoiceDescriptor>,
    }
    let Ok(Some(text)) = bounded_text(&mut env, &input) else {
        return -1;
    };
    let Ok(query) = serde_json::from_str::<Query>(&text) else {
        return -1;
    };
    if query.voices.len() > 1024 {
        return -1;
    }
    query
        .selection
        .select(&query.voices)
        .map_or(-1, |index| index as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_close_retains_handle_until_exact_playback_acks_and_recovers_capacity() {
        let mut r = Registry::default();
        let h = r.open(true);
        let first = r
            .apply(h, 0, Some("One two three. "))
            .unwrap()
            .ready
            .remove(0);
        let end = r
            .apply(h, 2, Some("One two three. Four five six."))
            .unwrap();
        assert!(!end.released && !end.idle);
        assert_eq!(r.owners.len(), 1);
        let second = &end.ready[0];
        let ack = format!("{}:{}", second.generation, second.chunk_id);
        assert!(r.apply(h, 4, Some(&ack)).unwrap().accepted);
        assert!(!r.apply(h, 4, Some(&ack)).unwrap().accepted);
        assert!(r.apply(h, 4, Some("999:999")).unwrap().ready.is_empty());
        let ack = format!("{}:{}", first.generation, first.chunk_id);
        assert!(r.apply(h, 4, Some(&ack)).unwrap().released);
        assert!(r.owners.is_empty());
        assert!(r.apply(h, 0, Some("stale")).is_none());
        for _ in 0..64 {
            assert_ne!(r.open(true), 0);
        }
        assert_eq!(r.open(true), 0);
        let handles: Vec<_> = r.owners.keys().copied().collect();
        for h in handles {
            r.apply(h, 3, None);
        }
        assert!(r.owners.is_empty());
        assert_ne!(r.open(true), 0);
    }
    #[test]
    fn correction_fences_old_ack_and_disabled_admission_does_not_replay() {
        let mut r = Registry::default();
        let h = r.open(true);
        let old = r
            .apply(h, 0, Some("One two three. "))
            .unwrap()
            .ready
            .remove(0);
        let end = r.apply(h, 2, Some("Different canonical answer.")).unwrap();
        assert!(end.cancel);
        assert_eq!(end.ready.len(), 1);
        assert!(
            !r.apply(h, 4, Some(&format!("{}:{}", old.generation, old.chunk_id)))
                .unwrap()
                .accepted
        );
        r.apply(h, 3, None);
        let h = r.open(false);
        assert!(r
            .apply(h, 0, Some("One two three. "))
            .unwrap()
            .ready
            .is_empty());
        assert!(r.apply(h, 2, Some("One two three.")).unwrap().released);
    }
}
