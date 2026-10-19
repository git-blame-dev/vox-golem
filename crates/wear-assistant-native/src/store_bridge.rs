//! Small trusted JNI adapter. Handles are opaque integers, never native pointers.
use crate::domain;
use jni::objects::{JObject, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use voxgolem_conversation_store::{codec, Error, Store};
use voxgolem_core::conversation_sync::Receipt;
use voxgolem_core::durable_conversation::{
    OperationBody, OperationId, MAX_ENCODED_OPERATION_BYTES,
};

type SharedStore = Arc<Mutex<Store>>;
#[derive(Default)]
struct Handles {
    next: i64,
    stores: BTreeMap<i64, SharedStore>,
}
fn handles() -> &'static Mutex<Handles> {
    static HANDLES: OnceLock<Mutex<Handles>> = OnceLock::new();
    HANDLES.get_or_init(Mutex::default)
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Read,
    Snapshot,
    Revision,
    AllocateId,
    Replica,
    Context,
    Commit,
    Complete,
    Select,
    LocalState,
    ProjectionStillCurrent,
    SaveDraft,
    Draft,
    Publications,
    ReceiptsToPublish,
    Acknowledge,
    Conversations,
    Heads,
}
#[derive(Default)]
struct Field(Option<Box<RawValue>>);
impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<RawValue>::deserialize(deserializer).map(|raw| Self(Some(raw)))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    kind: Kind,
    #[serde(default)]
    request: Field,
    #[serde(default)]
    records: Field,
    #[serde(default)]
    receive: Field,
    #[serde(default)]
    completion: Field,
    #[serde(default)]
    selection: Field,
    #[serde(default)]
    revision: Field,
    #[serde(default)]
    conversation: Field,
    #[serde(default)]
    selected_head: Field,
    #[serde(default)]
    text: Field,
    #[serde(default)]
    interrupted: Field,
    #[serde(default)]
    peer: Field,
    #[serde(default)]
    limit: Field,
    #[serde(default)]
    max_bytes: Field,
    #[serde(default)]
    after: Field,
    #[serde(default)]
    receipts: Field,
    #[serde(default)]
    offset: Field,
}
impl Request {
    fn validate_shape(&self) -> Result<(), Error> {
        let actual = [
            &self.request,
            &self.records,
            &self.receive,
            &self.completion,
            &self.selection,
            &self.revision,
            &self.conversation,
            &self.selected_head,
            &self.text,
            &self.interrupted,
            &self.peer,
            &self.limit,
            &self.max_bytes,
            &self.after,
            &self.receipts,
            &self.offset,
        ]
        .map(|field| field.0.is_some());
        let expected_indices: &[usize] = match self.kind {
            Kind::Read => &[0],
            Kind::Commit => &[1, 2, 3],
            Kind::Complete => &[3],
            Kind::Select => &[4],
            Kind::ProjectionStillCurrent => &[4, 5],
            Kind::Context => &[6, 7],
            Kind::SaveDraft => &[6, 8, 9],
            Kind::Draft => &[6],
            Kind::Publications => &[10, 11, 12, 13],
            Kind::ReceiptsToPublish => &[11, 13],
            Kind::Acknowledge => &[10, 14],
            Kind::Conversations if self.revision.0.is_some() => &[5, 11, 15],
            Kind::Conversations => &[11, 15],
            Kind::Heads if self.revision.0.is_some() || self.selected_head.0.is_some() => {
                &[5, 6, 7, 11, 15]
            }
            Kind::Heads => &[6, 11, 15],
            _ => &[],
        };
        if actual
            .iter()
            .enumerate()
            .any(|(index, present)| *present != expected_indices.contains(&index))
        {
            return Err(codec::invalid("store_request_shape").into());
        }
        Ok(())
    }
}
fn parse<T: serde::de::DeserializeOwned>(field: &Field) -> Result<T, Error> {
    serde_json::from_str(raw(field)?)
        .map_err(|_| Error::Domain(codec::invalid("store_request_json")))
}
fn raw(field: &Field) -> Result<&str, Error> {
    field
        .0
        .as_deref()
        .map(RawValue::get)
        .ok_or_else(|| codec::invalid("store_request_json").into())
}
fn success(value: impl serde::Serialize) -> Result<String, Error> {
    Ok(domain::success(value)?)
}
fn execute(store: &mut Store, raw: &str) -> Result<String, Error> {
    if raw.len() > domain::MAX_GATEWAY_BYTES {
        return Err(codec::limit("native_request_bytes").into());
    }
    let request: Request =
        serde_json::from_str(raw).map_err(|_| codec::invalid("store_request_json"))?;
    request.validate_shape()?;
    match request.kind {
        Kind::Read => {
            let input = self::raw(&request.request)?;
            store.read(|core| domain::read_owned(core, input))
        }
        Kind::Snapshot => success(store.snapshot()?),
        Kind::Revision => success(store.revision()?),
        Kind::AllocateId => success(store.allocate_id()?),
        Kind::Replica => success(store.replica()?),
        Kind::Conversations => {
            let offset = parse(&request.offset)?;
            let limit = parse(&request.limit)?;
            let revision = if request.revision.0.is_some() {
                parse::<Option<String>>(&request.revision)?
            } else {
                None
            };
            match revision {
                Some(revision) => success(store.conversations_at(&revision, offset, limit)?),
                None => success(store.conversations(offset, limit)?),
            }
        }
        Kind::Heads => {
            let conversation = parse(&request.conversation)?;
            let offset = parse(&request.offset)?;
            let limit = parse(&request.limit)?;
            let revision = if request.revision.0.is_some() {
                parse::<Option<String>>(&request.revision)?
            } else {
                None
            };
            let selected_head = if request.selected_head.0.is_some() {
                parse::<Option<OperationId>>(&request.selected_head)?
            } else {
                None
            };
            match revision {
                Some(revision) => success(store.branches_at(
                    &revision,
                    conversation,
                    selected_head,
                    offset,
                    limit,
                )?),
                None => success(store.heads(conversation, offset, limit)?),
            }
        }
        Kind::Context => success(voxgolem_conversation_store::provider_context(
            store.context(
                parse(&request.conversation)?,
                parse(&request.selected_head)?,
                256 * 1024,
            )?,
        )),
        Kind::Commit => {
            let records: Vec<Box<RawValue>> = parse(&request.records)?;
            if records.len() > 128 {
                return Err(codec::limit("batch_records").into());
            }
            let records = records
                .into_iter()
                .map(|raw| codec::parse_record(raw.get()))
                .collect::<Result<Vec<_>, _>>()?;
            let completion: Option<OperationId> = parse(&request.completion)?;
            success(store.import_records(records, parse(&request.receive)?, completion.as_ref())?)
        }
        Kind::Complete => {
            if self::raw(&request.completion)?.len() > MAX_ENCODED_OPERATION_BYTES {
                return Err(codec::limit("encoded_operation_bytes").into());
            }
            success(store.complete_body(parse::<OperationBody>(&request.completion)?)?)
        }
        Kind::Select => {
            store.select(parse(&request.selection)?)?;
            success(true)
        }
        Kind::LocalState => success(store.selection()?),
        Kind::ProjectionStillCurrent => success(store.projection_still_current(
            &parse::<String>(&request.revision)?,
            &parse(&request.selection)?,
        )?),
        Kind::SaveDraft => {
            store.save_draft(
                parse::<Option<OperationId>>(&request.conversation)?.as_ref(),
                &parse::<String>(&request.text)?,
                parse(&request.interrupted)?,
            )?;
            success(true)
        }
        Kind::Draft => {
            success(store.draft(parse::<Option<OperationId>>(&request.conversation)?.as_ref())?)
        }
        Kind::Publications => success(store.publications(
            parse::<Option<String>>(&request.peer)?.as_deref(),
            parse(&request.limit)?,
            parse(&request.max_bytes)?,
            parse::<Option<OperationId>>(&request.after)?.as_ref(),
        )?),
        Kind::ReceiptsToPublish => success(store.receipts_to_publish(
            parse(&request.limit)?,
            parse::<Option<OperationId>>(&request.after)?.as_ref(),
        )?),
        Kind::Acknowledge => {
            store.acknowledge(
                &parse::<String>(&request.peer)?,
                parse::<Vec<Receipt>>(&request.receipts)?,
            )?;
            success(true)
        }
    }
}
fn failure(error: Error) -> String {
    domain::failure(match error {
        Error::Domain(error) => error,
        Error::Database(code) => codec::invalid(&format!("conversation_sqlite_{code:?}")),
        _ => codec::invalid("conversation_storage"),
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_AssistantStoreNative_open(
    mut env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    path: JString<'_>,
) -> jlong {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let path = crate::read_string(&mut env, &path, 4096).map_err(|_| Error::Storage)?;
        let store = Store::open(Path::new(&path))?;
        let mut registry = handles().lock().map_err(|_| Error::Storage)?;
        if registry.stores.len() >= 32 {
            return Err(Error::Domain(codec::limit("store_handles")));
        }
        registry.next = registry.next.checked_add(1).ok_or(Error::Storage)?;
        let handle = registry.next;
        registry.stores.insert(handle, Arc::new(Mutex::new(store)));
        Ok(handle)
    }))
    .unwrap_or(Err(Error::Storage));
    crate::clear_exception(&env);
    match result {
        Ok(handle) => handle,
        Err(_) => {
            let _ = env.throw_new(
                "java/lang/IllegalStateException",
                "conversation_storage_open",
            );
            0
        }
    }
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_AssistantStoreNative_execute(
    mut env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    handle: jlong,
    request: JString<'_>,
) -> jstring {
    let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let input = crate::read_string(&mut env, &request, domain::MAX_GATEWAY_BYTES)
            .map_err(|_| Error::Domain(codec::limit("native_request_bytes")))?;
        let store = handles()
            .lock()
            .map_err(|_| Error::Storage)?
            .stores
            .get(&handle)
            .cloned()
            .ok_or(Error::Closed)?;
        let mut store = store.lock().map_err(|_| Error::Storage)?;
        execute(&mut store, &input)
    }))
    .unwrap_or(Err(Error::Storage))
    .unwrap_or_else(failure);
    crate::clear_exception(&env);
    env.new_string(output)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_AssistantStoreNative_close(
    _env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    handle: jlong,
) {
    let _ = std::panic::catch_unwind(|| {
        let store = handles()
            .lock()
            .ok()
            .and_then(|mut registry| registry.stores.remove(&handle));
        // Last-connection close/checkpoint must not hold the process handle registry.
        drop(store);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_envelopes_preserve_raw_records_and_do_not_require_snapshot_roundtrips() {
        let root = std::path::PathBuf::from("/tmp/opencode")
            .join(format!("synthetic-owned-envelope-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let mut store = Store::open(&root.join("history.db")).unwrap();
        let record = codec::canonical_record(r#"{"version":2,"id":{"replica":"00000000-0000-4000-8000-000000000001","sequence":"1"},"body":{"kind":"conversation_created","title":"Synthetic raw envelope"}}"#).unwrap();
        let committed = execute(&mut store,&serde_json::json!({"kind":"commit","records":[record],"receive":true,"completion":null}).to_string()).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&committed).unwrap()["value"]["revision"],
            "1"
        );
        let view = execute(&mut store, r#"{"kind":"read","request":{"kind":"view"}}"#).unwrap();
        let view: serde_json::Value = serde_json::from_str(&view).unwrap();
        assert_eq!(view["value"]["conversations"].as_array().unwrap().len(), 1);
        assert_eq!(view["value"]["conversation_count"], 1);
        assert!(!view["value"].as_object().unwrap().contains_key("records"));
        assert!(!view["value"]
            .as_object()
            .unwrap()
            .contains_key("quarantine"));
        assert!(execute(
            &mut store,
            r#"{"kind":"read","request":{"kind":"view"},"request":{"kind":"view"}}"#
        )
        .is_err());
        assert!(execute(&mut store, r#"{"kind":"snapshot","records":[]}"#).is_err());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
