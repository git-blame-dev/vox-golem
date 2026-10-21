//! Opaque, bounded policy handles. No engine resources or raw pointers cross JNI.
use jni::objects::{JObject, JString};
use jni::sys::{jboolean, jlong, jstring};
use jni::JNIEnv;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use voxgolem_core::reply_speech::{first_paragraph, Decision, ReplySpeech};

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

#[derive(Default)]
struct Registry {
    next: i64,
    owners: BTreeMap<i64, ReplySpeech>,
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
        let mut owner = ReplySpeech::early_clauses();
        owner.begin("reply", enabled);
        self.owners.insert(next, owner);
        next
    }
    fn apply(&mut self, handle: i64, operation: i32, text: Option<&str>) -> Option<Decision> {
        if !(0..=3).contains(&operation) {
            self.owners.remove(&handle);
            return None;
        }
        let owner = self.owners.get_mut(&handle)?;
        let result = match operation {
            0 => owner.text("reply", text.unwrap_or("")),
            1 => owner.corrected("reply"),
            2 => owner.settle("reply", text),
            _ => owner.reset(),
        };
        if operation >= 2 {
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
    if !(0..=3).contains(&operation) {
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
    let speak = d
        .speak
        .map(|s| serde_json::json!({"generation":s.generation,"chunk":s.chunk_id,"text":s.text}));
    let result =
        serde_json::json!({"cancel":d.cancel,"closed":d.stream_closed,"speak":speak}).to_string();
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_is_bounded_and_settlement_releases_real_policy() {
        let mut r = Registry::default();
        let h = r.open(true);
        let first = r
            .apply(h, 0, Some("One two three. "))
            .unwrap()
            .speak
            .unwrap();
        assert_eq!(first.text, "One two three.");
        let final_decision = r.apply(h, 2, Some("Different canonical answer.")).unwrap();
        assert!(final_decision.cancel);
        assert_ne!(first.generation, final_decision.speak.unwrap().generation);
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
    }
}
