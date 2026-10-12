use crate::apply_json;
use serde_json::{json, Value};

const A: &str = "00000000-0000-4000-8000-000000000001";
const B: &str = "00000000-0000-4000-8000-000000000002";

fn operation() -> Value {
    json!({"version":1,"id":{"replica":A,"sequence":"1"},"body":{"kind":"conversation_created","title":"Synthetic 🧪"}})
}

fn call(input: Value) -> Value {
    serde_json::from_str(&apply_json(&input.to_string())).unwrap()
}

fn empty() -> Value {
    json!({"version":1,"revision":"0","records":[],"retired":[]})
}

fn record(op: Value) -> Value {
    let result = call(json!({"kind":"record","operation":op}));
    assert_eq!(result["ok"], true, "{result}");
    result["value"].clone()
}

fn plan(snapshot: Value, records: Vec<Value>) -> Value {
    let revision = snapshot["revision"].clone();
    call(
        json!({"kind":"plan","snapshot":snapshot,"command":{"kind":"import","expected_revision":revision,"records":records}}),
    )
}

fn history(snapshot: &Value, head: Value, before: Value, max_turns: usize) -> Value {
    call(json!({"kind":"history","snapshot":snapshot,"request":{
        "expected_revision":snapshot["revision"],"conversation":operation()["id"],
        "selected_head":head,"before":before,"max_turns":max_turns,"max_text_bytes":2097152}}))
}

fn projection(
    snapshot: &Value,
    conversation: Value,
    head: Value,
    busy: bool,
    read: Value,
) -> Value {
    crate::domain::RESTORE_CALLS.with(|calls| calls.set(0));
    let result = call(json!({"kind":"projection","snapshot":snapshot,"request":{
        "conversation":conversation,"selected_head":head,"local_work_in_flight":busy,"read":read}}));
    assert_eq!(crate::domain::RESTORE_CALLS.with(|calls| calls.get()), 1);
    result
}

#[test]
fn projection_empty_missing_and_deleted_selections_return_the_same_view_without_reads() {
    let creation = record(operation());
    let populated = plan(empty(), vec![creation.clone()])["value"]["snapshot"].clone();
    let deleted = plan(
        populated.clone(),
        vec![record(
            json!({"version":1,"id":{"replica":A,"sequence":"4"},
        "body":{"kind":"conversation_deleted","conversation":creation["operation"]["id"]}}),
        )],
    )["value"]["snapshot"]
        .clone();
    for (snapshot, conversation) in [
        (empty(), Value::Null),
        (empty(), operation()["id"].clone()),
        (populated, Value::Null),
        (deleted, operation()["id"].clone()),
    ] {
        let bundled = projection(
            &snapshot,
            conversation,
            Value::Null,
            false,
            json!({"kind":"context","max_turns":20,"max_text_bytes":262144}),
        );
        assert_eq!(bundled["ok"], true, "{bundled}");
        assert_eq!(
            bundled["value"]["view"],
            call(json!({"kind":"view","snapshot":snapshot}))["value"]
        );
        assert!(bundled["value"]["continuation"].is_null());
        assert!(bundled["value"]["read"].is_null());
    }
}

#[test]
fn projection_matches_legacy_continuation_and_context_for_fast_forward_forks_and_busy_work() {
    let c = operation()["id"].clone();
    let root = record(turn("2", c.clone()));
    let mut child = turn("3", c.clone());
    child["body"]["parent"] = root["operation"]["id"].clone();
    let child = record(child);
    let linear =
        plan(empty(), vec![record(operation()), root.clone(), child])["value"]["snapshot"].clone();
    let forked =
        plan(linear.clone(), vec![record(turn("4", c.clone()))])["value"]["snapshot"].clone();
    let mut sibling = turn("5", c.clone());
    sibling["body"]["parent"] = root["operation"]["id"].clone();
    let selected_fork = plan(linear.clone(), vec![record(sibling)])["value"]["snapshot"].clone();
    let no_turns = plan(empty(), vec![record(operation())])["value"]["snapshot"].clone();
    for (snapshot, head, busy) in [
        (no_turns, Value::Null, false),
        (linear.clone(), Value::Null, false),
        (linear.clone(), root["operation"]["id"].clone(), false),
        (linear, root["operation"]["id"].clone(), true),
        (forked.clone(), Value::Null, false),
        (forked, root["operation"]["id"].clone(), false),
        (selected_fork, root["operation"]["id"].clone(), false),
    ] {
        let bundled = projection(
            &snapshot,
            c.clone(),
            head.clone(),
            busy,
            json!({"kind":"context","max_turns":20,"max_text_bytes":262144}),
        );
        assert_eq!(bundled["ok"], true, "{bundled}");
        let decision = call(json!({"kind":"continuation","snapshot":snapshot,"request":{
            "expected_revision":snapshot["revision"],"conversation":c,"selected_head":head,"local_work_in_flight":busy}}));
        assert_eq!(bundled["value"]["continuation"], decision["value"]);
        assert_eq!(
            bundled["value"]["view"],
            call(json!({"kind":"view","snapshot":snapshot}))["value"]
        );
        if decision["value"]["kind"] == "choose" && head.is_null() {
            assert!(bundled["value"]["read"].is_null());
        } else {
            let effective = if decision["value"]["kind"] == "fast_forward" {
                decision["value"]["head"].clone()
            } else {
                head
            };
            let legacy = call(json!({"kind":"context","snapshot":snapshot,"request":{
                "expected_revision":snapshot["revision"],"conversation":c,"selected_head":effective,
                "max_turns":20,"max_text_bytes":262144}}));
            assert_eq!(bundled["value"]["read"], legacy);
        }
    }
}

#[test]
fn projection_history_preserves_cursor_replay_and_reports_read_errors_with_the_fresh_view() {
    let c = operation()["id"].clone();
    let root = record(turn("2", c.clone()));
    let mut child = turn("3", c.clone());
    child["body"]["parent"] = root["operation"]["id"].clone();
    let child = record(child);
    let snapshot =
        plan(empty(), vec![record(operation()), root, child.clone()])["value"]["snapshot"].clone();
    let head = child["operation"]["id"].clone();
    let first = history(&snapshot, head.clone(), Value::Null, 1);
    let cursor = first["value"]["older_before"].clone();
    let mut descendant = turn("4", c.clone());
    descendant["body"]["parent"] = head.clone();
    let descendant = record(descendant);
    let advanced = plan(snapshot.clone(), vec![descendant.clone()])["value"]["snapshot"].clone();
    let advanced_head = descendant["operation"]["id"].clone();
    let advanced_page = projection(
        &advanced,
        c.clone(),
        head.clone(),
        false,
        json!({"kind":"history","before":cursor,"max_turns":1,"max_text_bytes":2097152}),
    );
    assert_eq!(
        advanced_page["value"]["continuation"],
        json!({"kind":"fast_forward","head":advanced_head})
    );
    assert_eq!(
        advanced_page["value"]["read"],
        history(&advanced, advanced_head, cursor.clone(), 1)
    );
    assert_eq!(
        advanced_page["value"]["read"]["error"]["field"],
        "history_cursor_branch"
    );
    let changed = plan(
        snapshot.clone(),
        vec![record(
            json!({"version":1,"id":{"replica":B,"sequence":"77"},
        "body":{"kind":"conversation_created","title":"Synthetic other conversation"}}),
        )],
    )["value"]["snapshot"]
        .clone();
    for (snapshot, before) in [
        (snapshot.clone(), Value::Null),
        (snapshot.clone(), cursor.clone()),
        (snapshot, json!("malformed")),
        (changed, cursor),
    ] {
        let bundled = projection(
            &snapshot,
            c.clone(),
            head.clone(),
            false,
            json!({"kind":"history","before":before,"max_turns":1,"max_text_bytes":2097152}),
        );
        assert_eq!(bundled["ok"], true, "{bundled}");
        assert_eq!(
            bundled["value"]["read"],
            history(&snapshot, head.clone(), before, 1)
        );
        assert_eq!(bundled["value"]["view"]["revision"], snapshot["revision"]);
    }
}

#[test]
fn projection_keeps_snapshot_hash_graph_and_read_quotas() {
    let c = operation()["id"].clone();
    let snapshot = plan(empty(), vec![record(operation())])["value"]["snapshot"].clone();
    for read in [
        json!({"kind":"context","max_turns":21,"max_text_bytes":262144}),
        json!({"kind":"history","before":null,"max_turns":129,"max_text_bytes":2097152}),
    ] {
        let bundled = projection(&snapshot, c.clone(), Value::Null, false, read);
        assert_eq!(bundled["ok"], true, "{bundled}");
        assert_eq!(bundled["value"]["read"]["error"]["kind"], "limit_exceeded");
    }
    let mut forged = snapshot.clone();
    forged["records"][0]["body_key"] = json!("0".repeat(64));
    assert_eq!(
        projection(&forged, c.clone(), Value::Null, false, Value::Null)["error"]["field"],
        "body_key_sha256_mismatch"
    );
    let mut invalid = snapshot;
    invalid["revision"] = json!("01");
    assert_eq!(
        projection(&invalid, c, Value::Null, false, Value::Null)["error"]["kind"],
        "invalid"
    );
    let mut cycle = turn("2", operation()["id"].clone());
    cycle["body"]["parent"] = cycle["id"].clone();
    let graph = json!({"version":1,"revision":"1","records":[record(operation()),record(cycle)],"retired":[]});
    let legacy = call(json!({"kind":"view","snapshot":graph}));
    assert_eq!(legacy["ok"], false);
    assert_eq!(
        projection(
            &graph,
            operation()["id"].clone(),
            Value::Null,
            false,
            Value::Null
        ),
        legacy
    );
    let oversized = format!(
        r#"{{"kind":"projection","snapshot":{{{}}},"request":{{"conversation":null,"selected_head":null,"local_work_in_flight":false,"read":null}}}}"#,
        " ".repeat(4 * 1024 * 1024)
    );
    let result: Value = serde_json::from_str(&apply_json(&oversized)).unwrap();
    assert_eq!(result["error"]["limit"], "native_snapshot_bytes");
}

#[test]
fn projection_rejects_unknown_request_and_read_fields_without_echoing_them() {
    let mut input = json!({"kind":"projection","snapshot":empty(),"request":{
        "conversation":null,"selected_head":null,"local_work_in_flight":false,"read":null}});
    input["request"]["secret"] = json!("Synthetic sensitive marker");
    let result = call(input.clone());
    assert_eq!(result["ok"], false);
    assert!(!result.to_string().contains("Synthetic sensitive marker"));
    input["request"].as_object_mut().unwrap().remove("secret");
    input["request"]["read"] =
        json!({"kind":"context","max_turns":20,"max_text_bytes":262144,"extra":true});
    assert_eq!(call(input)["error"]["field"], "native_request_json");
}

#[test]
fn history_pages_follow_causality_across_replicas_and_decimal_ids_not_provider_budget() {
    let conversation = operation()["id"].clone();
    let mut records = vec![record(operation())];
    let mut expected = vec![];
    let mut parent = Value::Null;
    for index in 0..150 {
        let sequence = if index == 0 { 2 } else { index + 9 };
        let mut op = turn(&sequence.to_string(), conversation.clone());
        op["id"]["replica"] = json!(if index % 2 == 0 { B } else { A });
        op["body"]["parent"] = parent;
        parent = op["id"].clone();
        expected.push(parent.clone());
        records.push(record(op));
    }
    let mut snapshot = empty();
    for batch in records.chunks(128) {
        let imported = plan(snapshot, batch.to_vec());
        assert_eq!(imported["ok"], true, "{imported}");
        snapshot = imported["value"]["snapshot"].clone();
    }
    let mut before = Value::Null;
    let mut actual = vec![];
    loop {
        let page = history(&snapshot, parent.clone(), before, 128);
        assert_eq!(page["ok"], true, "{page}");
        assert_eq!(page["value"]["head"], parent);
        let ids: Vec<_> = page["value"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["operation"]["id"].clone())
            .collect();
        assert!(!ids.is_empty());
        actual.splice(0..0, ids);
        before = page["value"]["older_before"].clone();
        if before.is_null() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let context = call(json!({"kind":"context","snapshot":snapshot,"request":{
        "expected_revision":snapshot["revision"],"conversation":conversation,"selected_head":parent,
        "max_turns":20,"max_text_bytes":262144}}));
    assert_eq!(context["value"]["turns"].as_array().unwrap().len(), 20);
    let oversized_context = call(json!({"kind":"context","snapshot":snapshot,"request":{
        "expected_revision":snapshot["revision"],"conversation":conversation,"selected_head":parent,
        "max_turns":21,"max_text_bytes":262144}}));
    assert_eq!(oversized_context["error"]["limit"], "provider_context");
}

#[test]
fn history_cursor_replay_branch_change_revision_change_and_malformed_are_explicit() {
    let c = operation()["id"].clone();
    let root = record(turn("2", c.clone()));
    let mut child = turn("10", c.clone());
    child["body"]["parent"] = root["operation"]["id"].clone();
    let child = record(child);
    let sibling = record(turn("3", c));
    let snapshot = plan(
        empty(),
        vec![
            record(operation()),
            root.clone(),
            child.clone(),
            sibling.clone(),
        ],
    )["value"]["snapshot"]
        .clone();
    let head = child["operation"]["id"].clone();
    let first = history(&snapshot, head.clone(), Value::Null, 1);
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["value"]["turns"], json!([child]));
    let cursor = first["value"]["older_before"].clone();
    let older = history(&snapshot, head.clone(), cursor.clone(), 1);
    assert_eq!(older["value"]["turns"], json!([root]));
    assert_eq!(older, history(&snapshot, head.clone(), cursor.clone(), 1));
    assert_eq!(
        history(
            &snapshot,
            sibling["operation"]["id"].clone(),
            cursor.clone(),
            1
        )["error"]["field"],
        "history_cursor_branch"
    );
    assert_eq!(
        history(&snapshot, Value::Null, Value::Null, 1)["error"]["kind"],
        "ambiguous_heads"
    );
    assert_eq!(
        history(&snapshot, head.clone(), json!("malformed"), 1)["error"]["field"],
        "history_cursor"
    );
    let changed = plan(snapshot.clone(), vec![record(json!({"version":1,"id":{"replica":B,"sequence":"77"},"body":{"kind":"conversation_created","title":"Synthetic other chat"}}))])["value"]["snapshot"].clone();
    assert_eq!(
        history(&changed, head, cursor, 1)["error"]["field"],
        "history_cursor_stale"
    );
}

#[test]
fn history_empty_and_default_unique_head_are_core_selections() {
    let c = record(operation());
    let snapshot = plan(empty(), vec![c.clone()])["value"]["snapshot"].clone();
    assert_eq!(
        history(&snapshot, Value::Null, Value::Null, 128),
        json!({
        "ok":true,"value":{"revision":"1","conversation":c["operation"]["id"],
        "head":null,"turns":[],"older_before":null}})
    );
    let root = record(turn("2", c["operation"]["id"].clone()));
    let snapshot = plan(snapshot, vec![root.clone()])["value"]["snapshot"].clone();
    assert_eq!(
        history(&snapshot, Value::Null, Value::Null, 128)["value"]["turns"],
        json!([root.clone()])
    );
    assert_eq!(
        history(&snapshot, Value::Null, Value::Null, 128)["value"]["head"],
        root["operation"]["id"]
    );
}

#[test]
fn history_bounds_preserve_whole_unicode_and_refusal_bodies() {
    let c = operation()["id"].clone();
    let mut root = turn("2", c.clone());
    root["body"]["prompt"] = json!("🧪");
    root["body"]["answer"] = json!({"kind":"refusal","reason":"Synthetic refusal"});
    let root = record(root);
    let mut child = turn("3", c.clone());
    child["body"]["parent"] = root["operation"]["id"].clone();
    child["body"]["prompt"] = json!("🧪");
    child["body"]["answer"]["text"] = json!("🧪");
    let child = record(child);
    let snapshot = plan(
        empty(),
        vec![record(operation()), root.clone(), child.clone()],
    )["value"]["snapshot"]
        .clone();
    let request = json!({"kind":"history","snapshot":snapshot,"request":{
        "expected_revision":snapshot["revision"],"conversation":c,"selected_head":null,
        "before":null,"max_turns":128,"max_text_bytes":8}});
    let first = call(request.clone());
    assert_eq!(first["value"]["turns"], json!([child]));
    assert!(first["value"]["older_before"].is_string());
    let mut older = request.clone();
    older["request"]["before"] = first["value"]["older_before"].clone();
    older["request"]["max_text_bytes"] = json!(21);
    assert_eq!(call(older)["value"]["turns"], json!([root]));
    for (field, value) in [
        ("max_turns", 0),
        ("max_turns", 129),
        ("max_text_bytes", 0),
        ("max_text_bytes", 2097153),
    ] {
        let mut invalid = request.clone();
        invalid["request"][field] = json!(value);
        assert_eq!(call(invalid)["error"]["limit"], "history_page");
    }
    let mut too_small = request;
    too_small["request"]["max_text_bytes"] = json!(7);
    assert_eq!(call(too_small)["error"]["limit"], "history_turn_text_bytes");
}

#[test]
fn history_cursor_is_bounded_and_anchored_to_conversation_and_causal_before() {
    let c = operation()["id"].clone();
    let root = record(turn("2", c.clone()));
    let mut child = turn("3", c.clone());
    child["body"]["parent"] = root["operation"]["id"].clone();
    let child = record(child);
    let sibling = record(turn("4", c));
    let mut other = operation();
    other["id"]["replica"] = json!(B);
    let other = record(other);
    let other_turn = record(turn("5", other["operation"]["id"].clone()));
    let snapshot = plan(
        empty(),
        vec![
            record(operation()),
            root,
            child.clone(),
            sibling.clone(),
            other.clone(),
            other_turn,
        ],
    )["value"]["snapshot"]
        .clone();
    let head = child["operation"]["id"].clone();
    let first = history(&snapshot, head.clone(), Value::Null, 1);
    let cursor = first["value"]["older_before"].clone();
    let mut request = json!({"kind":"history","snapshot":snapshot,"request":{
        "expected_revision":snapshot["revision"],"conversation":other["operation"]["id"],
        "selected_head":null,"before":cursor,"max_turns":1,"max_text_bytes":2097152}});
    assert_eq!(
        call(request.clone())["error"]["field"],
        "history_cursor_branch"
    );
    request["request"]["conversation"] = operation()["id"].clone();
    request["request"]["selected_head"] = head;
    let mut tampered: Value = serde_json::from_str(cursor.as_str().unwrap()).unwrap();
    tampered["before"] = sibling["operation"]["id"].clone();
    request["request"]["before"] = json!(tampered.to_string());
    assert_eq!(
        call(request.clone())["error"]["field"],
        "history_cursor_before"
    );
    request["request"]["before"] = json!("x".repeat(513));
    assert_eq!(
        call(request.clone())["error"]["limit"],
        "history_cursor_bytes"
    );
    request["request"]["before"] = json!(false);
    assert_eq!(
        call(request.clone())["error"]["field"],
        "native_request_json"
    );
    request["request"]["before"] = Value::Null;
    request["request"]["expected_revision"] = json!("0");
    assert_eq!(call(request)["error"]["kind"], "revision_conflict");
}

#[test]
fn history_rejects_missing_pending_deleted_and_quarantined_paths() {
    let c = record(operation());
    let mut pending = turn("3", c["operation"]["id"].clone());
    pending["body"]["parent"] = json!({"replica":A,"sequence":"2"});
    let pending = record(pending);
    let snapshot = plan(empty(), vec![c.clone(), pending.clone()])["value"]["snapshot"].clone();
    let head = pending["operation"]["id"].clone();
    assert_eq!(
        history(&snapshot, head.clone(), Value::Null, 128)["error"]["kind"],
        "missing"
    );
    assert_eq!(
        history(&snapshot, Value::Null, Value::Null, 128)["error"]["kind"],
        "missing"
    );
    assert_eq!(
        history(
            &snapshot,
            json!({"replica":B,"sequence":"99"}),
            Value::Null,
            128
        )["error"]["kind"],
        "missing"
    );
    let mut wrong_parent = operation();
    wrong_parent["id"]["sequence"] = json!("2");
    let quarantined =
        plan(snapshot.clone(), vec![record(wrong_parent)])["value"]["snapshot"].clone();
    assert_eq!(
        history(&quarantined, head.clone(), Value::Null, 128)["error"]["kind"],
        "quarantined"
    );
    assert_eq!(
        history(&quarantined, Value::Null, Value::Null, 128)["error"]["kind"],
        "quarantined"
    );
    let root = record(turn("5", c["operation"]["id"].clone()));
    for unresolved in [snapshot.clone(), quarantined] {
        let with_ready = plan(unresolved, vec![root.clone()])["value"]["snapshot"].clone();
        assert_eq!(
            history(&with_ready, Value::Null, Value::Null, 128)["value"]["turns"],
            json!([root.clone()])
        );
    }
    let deletion = record(json!({"version":1,"id":{"replica":A,"sequence":"4"},
        "body":{"kind":"conversation_deleted","conversation":c["operation"]["id"]}}));
    let deleted = plan(snapshot, vec![deletion])["value"]["snapshot"].clone();
    assert_eq!(
        history(&deleted, head, Value::Null, 128)["error"]["kind"],
        "deleted"
    );
}

#[test]
fn canonical_body_hash_ignores_key_order_whitespace_and_unicode_escapes() {
    let result = record(operation());
    let reordered = format!(
        r#"{{"operation":{{"body":{{"title":"Synthetic \ud83e\uddea","kind":"conversation_created"}},"id":{{"sequence":"1","replica":"{A}"}},"version":1}},"kind":"record"}}"#
    );
    let alternate: Value = serde_json::from_str(&apply_json(&reordered)).unwrap();
    assert_eq!(result, alternate["value"]);
    assert_eq!(
        result["body_key"],
        "63fb5f4ae23359b8632ff27a178d958d60a82d57be7f6fdf59e3a86276974b5b"
    );
}

#[test]
fn record_plan_restore_inventory_receipt_roundtrip_is_stateless_and_revision_pinned() {
    let original = record(operation());
    let result = plan(empty(), vec![original.clone()]);
    assert_eq!(result["ok"], true);
    assert_eq!(result["value"]["base_revision"], "0");
    let snapshot = result["value"]["snapshot"].clone();
    assert_eq!(snapshot["revision"], "1");
    let view = call(json!({"kind":"view","snapshot":snapshot}));
    assert_eq!(view["value"]["records"], json!([original]));
    let inventory = call(
        json!({"kind":"inventory","snapshot":snapshot,"request":{"cursor":{"revision":"1","after":null},"max_items":128}}),
    );
    assert_eq!(
        inventory["value"]["entries"][0]["body_key"],
        original["body_key"]
    );
    let reconcile = call(json!({"kind":"reconcile","snapshot":empty(),"page":inventory["value"]}));
    assert_eq!(
        reconcile["value"]["fetch_records"][0]["id"],
        original["operation"]["id"]
    );
    let receipts = call(
        json!({"kind":"receipts","snapshot":snapshot,"recipient":B,"operation_ids":[original["operation"]["id"]]}),
    );
    assert_eq!(receipts["value"][0]["body_key"], original["body_key"]);
    assert_eq!(receipts["value"][0]["recipient_replica"], B);
    let duplicate = plan(snapshot.clone(), vec![original.clone()]);
    assert_eq!(duplicate["value"]["snapshot"], snapshot);
    assert_eq!(
        duplicate["value"]["duplicates"],
        json!([original["operation"]["id"]])
    );
    assert_eq!(
        call(json!({"kind":"view","snapshot":empty()}))["value"]["records"],
        json!([])
    );
    let conflict = call(
        json!({"kind":"plan","snapshot":snapshot,"command":{"kind":"import","expected_revision":"0","records":[]}}),
    );
    assert_eq!(conflict["error"]["kind"], "revision_conflict");
}

#[test]
fn forged_import_and_restore_body_keys_are_rejected_not_rewritten() {
    let mut forged = record(operation());
    forged["body_key"] = json!("0".repeat(64));
    let result = plan(empty(), vec![forged.clone()]);
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["field"], "body_key_sha256_mismatch");
    let snapshot = json!({"version":1,"revision":"1","records":[forged],"retired":[]});
    let result = call(json!({"kind":"view","snapshot":snapshot}));
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["field"], "body_key_sha256_mismatch");
}

#[test]
fn strict_parse_errors_do_not_echo_input_or_accept_duplicate_fields() {
    for input in [
        r#"{"kind":"view","snapshot":{"version":1,"revision":"0","records":[],"retired":[]},"secret":"Synthetic sensitive marker"}"#,
        r#"{"kind":"view","kind":"record","operation":{}}"#,
        r#"{"kind":"record","operation":{"version":1,"version":1}}"#,
        r#"{"kind":"record","operation":{"version":1,"id":{"replica":"bad","sequence":"1"},"body":{"kind":"conversation_created","title":"Synthetic","extra":true}}}"#,
    ] {
        let result: Value = serde_json::from_str(&apply_json(input)).unwrap();
        assert_eq!(result["ok"], false);
        assert!(!result.to_string().contains("Synthetic sensitive marker"));
    }
}

#[test]
fn native_raw_request_operation_and_snapshot_bounds_are_checked_before_parsing() {
    let result: Value =
        serde_json::from_str(&apply_json(&" ".repeat(16 * 1024 * 1024 + 1))).unwrap();
    assert_eq!(result["error"]["limit"], "native_request_bytes");
    let raw_op = format!(
        r#"{{"kind":"record","operation":{{{}}}}}"#,
        " ".repeat(2 * 1024 * 1024)
    );
    let result: Value = serde_json::from_str(&apply_json(&raw_op)).unwrap();
    assert_eq!(result["error"]["limit"], "encoded_operation_bytes");
    let raw_snapshot = format!(
        r#"{{"kind":"view","snapshot":{{{}}}}}"#,
        " ".repeat(4 * 1024 * 1024)
    );
    let result: Value = serde_json::from_str(&apply_json(&raw_snapshot)).unwrap();
    assert_eq!(result["error"]["limit"], "native_snapshot_bytes");
}

fn turn(sequence: &str, conversation: Value) -> Value {
    json!({
        "version":1,"id":{"replica":A,"sequence":sequence},
        "body":{"kind":"turn_completed","conversation":conversation,"parent":null,
            "request":{"replica":A,"sequence":"100"},"prompt":"Synthetic question",
            "answer":{"kind":"text","text":"Synthetic answer"},"terminal_stage":"instant",
            "provenance":{"requested_model":"custom-sol-none","returned_model":null,
                "policy_id":"synthetic-policy","policy_version":"1",
                "instruction_profile_id":"synthetic-profile","instruction_profile_version":"1","context":[]}}
    })
}

#[test]
fn core_context_continuation_forks_and_tombstones_are_not_reimplemented() {
    let creation = record(operation());
    let conversation = creation["operation"]["id"].clone();
    let first = record(turn("2", conversation.clone()));
    let result = plan(empty(), vec![creation, first.clone()]);
    let snapshot = result["value"]["snapshot"].clone();
    let request = json!({"expected_revision":"1","conversation":conversation,"selected_head":null,"max_turns":20,"max_text_bytes":262144});
    let context = call(json!({"kind":"context","snapshot":snapshot,"request":request}));
    assert_eq!(context["value"]["turns"], json!([first.clone()]));
    let continuation = call(
        json!({"kind":"continuation","snapshot":snapshot,"request":{"expected_revision":"1","conversation":conversation,"selected_head":null,"local_work_in_flight":false}}),
    );
    assert_eq!(continuation["value"]["kind"], "fast_forward");
    let mut second_op = turn("3", conversation.clone());
    second_op["body"]["request"]["sequence"] = json!("101");
    let fork = plan(snapshot.clone(), vec![record(second_op)]);
    let fork_snapshot = fork["value"]["snapshot"].clone();
    let mut fork_request = request;
    fork_request["expected_revision"] = json!("2");
    let context = call(json!({"kind":"context","snapshot":fork_snapshot,"request":fork_request}));
    assert_eq!(context["error"]["kind"], "ambiguous_heads");
    let delete = record(
        json!({"version":1,"id":{"replica":A,"sequence":"4"},"body":{"kind":"conversation_deleted","conversation":conversation}}),
    );
    let deleted = plan(fork_snapshot, vec![delete]);
    assert_eq!(
        deleted["value"]["snapshot"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        deleted["value"]["snapshot"]["retired"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let restore = call(json!({"kind":"view","snapshot":deleted["value"]["snapshot"]}));
    assert_eq!(restore["ok"], true);
    let completed_id = first["operation"]["id"].clone();
    let late = plan(deleted["value"]["snapshot"].clone(), vec![first]);
    assert_eq!(late["value"]["duplicates"].as_array().unwrap().len(), 1);
    assert_eq!(late["value"]["suppressed"], json!([completed_id]));
    assert!(late["value"]["publish"].as_array().unwrap().is_empty());
}

#[test]
fn omitted_optional_fields_are_canonicalized_to_explicit_nulls() {
    let mut op = turn("2", operation()["id"].clone());
    let canonical = record(op.clone());
    op["body"].as_object_mut().unwrap().remove("parent");
    op["body"]["provenance"]
        .as_object_mut()
        .unwrap()
        .remove("returned_model");
    assert_eq!(record(op), canonical);
    assert!(canonical["operation"]["body"]
        .as_object()
        .unwrap()
        .contains_key("parent"));
}

#[test]
fn imported_store_must_fit_native_encoded_quota_without_silent_eviction() {
    let conversation = record(operation());
    let mut snapshot = plan(empty(), vec![conversation.clone()])["value"]["snapshot"].clone();
    let mut records = vec![];
    for n in 2..22 {
        let mut op = turn(&n.to_string(), conversation["operation"]["id"].clone());
        op["body"]["request"]["sequence"] = json!((100 + n).to_string());
        op["body"]["answer"]["text"] = json!("x".repeat(230 * 1024));
        records.push(record(op));
    }
    let result = plan(snapshot.clone(), records);
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["limit"], "native_snapshot_bytes");
    snapshot["records"][0]["body_key"] = json!("f".repeat(64));
    let result = call(
        json!({"kind":"inventory","snapshot":snapshot,"request":{"cursor":{"revision":"1","after":null},"max_items":128}}),
    );
    assert_eq!(result["error"]["field"], "body_key_sha256_mismatch");
}

#[test]
fn core_prompt_and_provenance_limits_apply_to_record_creation() {
    let mut op = turn("2", operation()["id"].clone());
    op["body"]["prompt"] = json!("🧪".repeat(8193));
    assert_eq!(
        call(json!({"kind":"record","operation":op}))["error"]["limit"],
        "prompt"
    );
    let mut op = turn("2", operation()["id"].clone());
    op["body"]["provenance"]["context"] = json!((1..=21)
        .map(|n| json!({"replica":A,"sequence":n.to_string()}))
        .collect::<Vec<_>>());
    assert_eq!(
        call(json!({"kind":"record","operation":op}))["error"]["limit"],
        "provenance_context"
    );
}

#[test]
fn quarantine_survives_gateway_restore_and_does_not_skip_digest_verification() {
    let creation = record(operation());
    let parent = json!({"replica":A,"sequence":"2"});
    let mut pending = turn("3", creation["operation"]["id"].clone());
    pending["body"]["parent"] = parent.clone();
    pending["body"]["provenance"]["context"] = json!([parent.clone()]);
    let first = plan(empty(), vec![creation, record(pending)]);
    assert_eq!(first["ok"], true);
    let mut wrong_parent = operation();
    wrong_parent["id"] = parent;
    let second = plan(
        first["value"]["snapshot"].clone(),
        vec![record(wrong_parent)],
    );
    assert_eq!(second["ok"], true, "{second}");
    let mut snapshot = second["value"]["snapshot"].clone();
    assert_eq!(snapshot["quarantine"].as_array().unwrap().len(), 1);
    let view = call(json!({"kind":"view","snapshot":snapshot}));
    assert_eq!(view["value"]["quarantine"], snapshot["quarantine"]);
    let pending_record = snapshot["records"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|record| record["operation"]["id"]["sequence"] == "3")
        .unwrap();
    pending_record["body_key"] = json!("0".repeat(64));
    assert_eq!(
        call(json!({"kind":"view","snapshot":snapshot}))["error"]["field"],
        "body_key_sha256_mismatch"
    );
    assert_eq!(
        call(json!({"kind":"view","snapshot":empty()}))["value"]["quarantine"],
        json!([])
    );
}

#[test]
fn real_hashed_cross_conversation_quarantine_remains_restorable_after_witness_deletion() {
    let c = record(operation());
    let mut d_op = operation();
    d_op["id"] = json!({"replica":B,"sequence":"1"});
    d_op["body"]["title"] = json!("Synthetic D");
    let d = record(d_op);
    let a2 = json!({"replica":A,"sequence":"2"});
    let a3 = json!({"replica":A,"sequence":"3"});
    let b2 = json!({"replica":B,"sequence":"2"});
    let b3 = json!({"replica":B,"sequence":"3"});
    let mut q_op = turn("3", c["operation"]["id"].clone());
    q_op["body"]["parent"] = a2.clone();
    q_op["body"]["provenance"]["context"] = json!([{"replica":A,"sequence":"88"}, a2]);
    let q = record(q_op);
    let mut t_op = turn("3", d["operation"]["id"].clone());
    t_op["id"] = b3.clone();
    t_op["body"]["parent"] = b2.clone();
    t_op["body"]["provenance"]["context"] = json!([a3, b2]);
    let t = record(t_op);
    let mut snapshot = empty();
    for records in [
        vec![c.clone(), d.clone()],
        vec![t.clone()],
        vec![q.clone()],
        vec![record(turn("2", c["operation"]["id"].clone()))],
    ] {
        let result = plan(snapshot, records);
        assert_eq!(result["ok"], true, "{result}");
        snapshot = result["value"]["snapshot"].clone();
        assert_eq!(call(json!({"kind":"view","snapshot":snapshot}))["ok"], true);
    }
    assert_eq!(snapshot["quarantine"].as_array().unwrap().len(), 2);
    let deletion = record(
        json!({"version":1,"id":{"replica":B,"sequence":"99"},"body":{"kind":"conversation_deleted","conversation":c["operation"]["id"]}}),
    );
    let deleted = plan(snapshot, vec![deletion]);
    assert_eq!(deleted["ok"], true, "{deleted}");
    let snapshot = deleted["value"]["snapshot"].clone();
    let restored = call(json!({"kind":"view","snapshot":snapshot}));
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(
        snapshot["quarantine"],
        json!([{"id":b3,"reason":"invalid_context"}])
    );
    assert!(snapshot["records"].as_array().unwrap().contains(&t));
    let retired_q = snapshot["retired"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == q["operation"]["id"])
        .unwrap();
    assert_eq!(retired_q["body_key"], q["body_key"]);
    assert_eq!(retired_q["conversation"], c["operation"]["id"]);
    let context = call(
        json!({"kind":"context","snapshot":snapshot,"request":{"expected_revision":snapshot["revision"],"conversation":d["operation"]["id"],"selected_head":b3,"max_turns":20,"max_text_bytes":262144}}),
    );
    assert_eq!(context["error"]["kind"], "quarantined");
    let inventory = call(
        json!({"kind":"inventory","snapshot":snapshot,"request":{"cursor":{"revision":snapshot["revision"],"after":null},"max_items":128}}),
    );
    assert!(inventory["value"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["id"] != b3));
    let receipts = call(
        json!({"kind":"receipts","snapshot":snapshot,"recipient":B,"operation_ids":[b3,q["operation"]["id"]]}),
    );
    assert_eq!(receipts["ok"], true);
    assert_eq!(receipts["value"][0]["body_key"], t["body_key"]);
    assert_eq!(receipts["value"][1]["body_key"], q["body_key"]);
    let duplicate = plan(snapshot.clone(), vec![t]);
    assert_eq!(duplicate["ok"], true);
    assert_eq!(duplicate["value"]["quarantined"], json!([b3]));
    assert_eq!(duplicate["value"]["snapshot"], snapshot);
    assert_eq!(duplicate["value"]["publish"], json!([]));
}

#[test]
fn invalid_quarantine_is_a_bounded_failure_and_old_snapshots_default_to_empty() {
    let c = record(operation());
    let t = record(turn("2", c["operation"]["id"].clone()));
    let snapshot = plan(empty(), vec![c, t.clone()])["value"]["snapshot"].clone();
    let mut unjustified = snapshot.clone();
    unjustified["quarantine"] = json!([{"id":t["operation"]["id"],"reason":"invalid_context"}]);
    let result = call(json!({"kind":"view","snapshot":unjustified}));
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["field"], "quarantine_valid_record");
    let mut malformed = snapshot;
    malformed["quarantine"] = json!(false);
    let result = call(json!({"kind":"view","snapshot":malformed}));
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["field"], "native_request_json");
    assert_eq!(
        call(json!({"kind":"view","snapshot":empty()}))["value"]["quarantine"],
        json!([])
    );
}
