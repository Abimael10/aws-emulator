// SPDX-License-Identifier: CC0-1.0

//! What an Event API publish does with the per-event ERRORS a channel namespace's
//! `onPublish` handler answers with.
//!
//! RULE: `ChannelNamespace$codeHandlers` is "the event handler functions that run custom
//! business logic to process published events and subscribe requests"
//! (`smithy_query(query="shape", shape="ChannelNamespace")`), so the handler's answer
//! PARTITIONS the batch a publish carried: an event the handler returned carrying an `error`
//! is one the namespace refused — reported under `failed`, with the code a handler-supplied
//! error carries, the sentence the handler supplied for that event and the event's own
//! 0-based position in the request, and delivered to no subscriber — while every other event
//! it was given is reported under `successful` at its own position, whatever the handler did
//! with that event's payload. The publish itself is answered 200 either way, and every event
//! of the batch is reported exactly once.
//!
//! The rows span what the rule generalises over: WHICH positions of the batch the handler
//! errors on (none, the last, every one, both ends of a full batch), how many events one
//! publish carries, the sentence the handler supplies (fixed text, one computed per event
//! from the event and the channel, one that is not a string at all), whether the handler also
//! transforms the events it keeps, and the two answers that are NOT an error (a namespace
//! carrying no handler at all, and a handler that filters an event out instead of erroring on
//! it — that event is published-but-dropped, not failed).

use serde_json::{json, Value};

use crate::event_api_data_plane::{
    connected, create_event_api, create_namespace, delivered, emulator, exchange, publish,
    FILTER_HANDLER,
};

/// The fixed text an event error a DEPLOYED HANDLER supplied is reported under, as against a
/// failure the endpoint itself states: the service's own copy is
/// `service::event_api::publish::HANDLER_ERROR_CODE`.
const CUSTOM_ERROR: &str = "CustomError";

/// The class of handler this suite is about: one that passes an event through when its
/// payload satisfies the namespace's own rule, and answers it with an `error` when it does
/// not. Written as AppSync's own handlers are, over `ctx.events`.
const GATE_HANDLER: &str = "export function onPublish(ctx) { return ctx.events.map((event) => \
     (event.payload && event.payload.ok === true) ? event \
     : ({ ...event, error: 'payload.ok must be true' })) }";

/// The same decision with a sentence computed PER EVENT, from that event and from the channel
/// the publish addressed: the message is the handler's own value, not fixed text.
const PER_EVENT_HANDLER: &str = "export function onPublish(ctx) { return ctx.events.map(\
     (event) => event.payload.allow === true ? event : ({ id: event.id, error: 'channel ' \
     + ctx.info.channel.path + ' refuses ' + event.payload.name })) }";

/// A handler whose `error` is not a string at all.
const DOCUMENT_ERROR_HANDLER: &str = "export function onPublish(ctx) { return ctx.events.map(\
     (event) => event.payload.ok === true ? event : ({ id: event.id, error: \
     { reason: 'refused' } })) }";

/// A handler that TRANSFORMS the events it keeps and errors on the rest, so the two decisions
/// are made in one answer.
const TRANSFORM_AND_GATE_HANDLER: &str = "export function onPublish(ctx) { return \
     ctx.events.map((event) => event.payload.ok === true ? ({ id: event.id, payload: \
     { seen: event.payload.turn } }) : ({ id: event.id, error: 'turn ' + event.payload.turn \
     + ' is not ok' })) }";

/// The event every row publishes LAST, once the row's own batch is answered: it satisfies
/// every handler in the table, so what a subscriber receives after the batch is this event —
/// and an event a handler refused is nowhere in between.
fn sentinel() -> Value {
    json!({"ok": true, "allow": true, "keep": true, "turn": 99, "name": "sentinel"})
}

/// One row: the namespace's handler, the batch published to it, and how the answer must
/// partition that batch.
struct Row {
    case: &'static str,
    /// The channel namespace the row's events are published to, which carries its handler.
    namespace: &'static str,
    /// The namespace's `codeHandlers`, or none.
    code_handlers: Option<&'static str>,
    /// The events the publish carries, in order.
    events: Vec<Value>,
    /// The 0-based positions the answer must report under `successful`.
    successful: Vec<u64>,
    /// The positions it must report under `failed`, each with the sentence the handler
    /// supplied for that event.
    failed: Vec<(u64, &'static str)>,
    /// The payloads a subscriber on the published channel must receive, in order.
    published: Vec<Value>,
    /// The payload the trailing [`sentinel`] event must arrive as under this row's handler.
    sentinel_published: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: one event the handler keeps and one it errors on",
            namespace: "default",
            code_handlers: Some(GATE_HANDLER),
            events: vec![json!({"ok": true, "turn": 1}), json!({"ok": false, "turn": 2})],
            successful: vec![0],
            failed: vec![(1, "payload.ok must be true")],
            published: vec![json!({"ok": true, "turn": 1})],
            sentinel_published: sentinel(),
        },
        Row {
            case: "the same handler over a batch it keeps entirely",
            namespace: "gate-all-kept",
            code_handlers: Some(GATE_HANDLER),
            events: vec![json!({"ok": true, "turn": 1}), json!({"ok": true, "turn": 2})],
            successful: vec![0, 1],
            failed: Vec::new(),
            published: vec![json!({"ok": true, "turn": 1}), json!({"ok": true, "turn": 2})],
            sentinel_published: sentinel(),
        },
        Row {
            case: "the same handler erroring on every event of the batch",
            namespace: "gate-none-kept",
            code_handlers: Some(GATE_HANDLER),
            events: vec![json!({"ok": false, "turn": 1}), json!({"turn": 2})],
            successful: Vec::new(),
            failed: vec![
                (0, "payload.ok must be true"),
                (1, "payload.ok must be true"),
            ],
            published: Vec::new(),
            sentinel_published: sentinel(),
        },
        Row {
            case: "a full batch whose first and last events are the ones it errors on",
            namespace: "gate-both-ends",
            code_handlers: Some(GATE_HANDLER),
            events: vec![
                json!({"ok": false, "turn": 1}),
                json!({"ok": true, "turn": 2}),
                json!({"ok": true, "turn": 3}),
                json!({"ok": true, "turn": 4}),
                json!({"ok": false, "turn": 5}),
            ],
            successful: vec![1, 2, 3],
            failed: vec![
                (0, "payload.ok must be true"),
                (4, "payload.ok must be true"),
            ],
            published: vec![
                json!({"ok": true, "turn": 2}),
                json!({"ok": true, "turn": 3}),
                json!({"ok": true, "turn": 4}),
            ],
            sentinel_published: sentinel(),
        },
        Row {
            case: "a sentence the handler computes per event, from the event and the channel",
            namespace: "per-event",
            code_handlers: Some(PER_EVENT_HANDLER),
            events: vec![
                json!({"allow": false, "name": "ana"}),
                json!({"allow": true, "name": "bo"}),
                json!({"allow": false, "name": "cy"}),
            ],
            successful: vec![1],
            failed: vec![
                (0, "channel /per-event/room refuses ana"),
                (2, "channel /per-event/room refuses cy"),
            ],
            published: vec![json!({"allow": true, "name": "bo"})],
            sentinel_published: sentinel(),
        },
        Row {
            case: "an error the handler did not write as a string",
            namespace: "document-error",
            code_handlers: Some(DOCUMENT_ERROR_HANDLER),
            events: vec![json!({"ok": false, "turn": 1})],
            successful: Vec::new(),
            failed: vec![(0, "{\"reason\":\"refused\"}")],
            published: Vec::new(),
            sentinel_published: sentinel(),
        },
        Row {
            case: "a handler that transforms what it keeps and errors on the rest",
            namespace: "transformed",
            code_handlers: Some(TRANSFORM_AND_GATE_HANDLER),
            events: vec![json!({"ok": true, "turn": 7}), json!({"ok": false, "turn": 8})],
            successful: vec![0],
            failed: vec![(1, "turn 8 is not ok")],
            published: vec![json!({"seen": 7})],
            sentinel_published: json!({"seen": 99}),
        },
        Row {
            case: "a namespace with no handler reports every event successful",
            namespace: "open",
            code_handlers: None,
            events: vec![json!({"ok": false, "turn": 1}), json!({"ok": true, "turn": 2})],
            successful: vec![0, 1],
            failed: Vec::new(),
            published: vec![json!({"ok": false, "turn": 1}), json!({"ok": true, "turn": 2})],
            sentinel_published: sentinel(),
        },
        Row {
            case: "a handler that filters an event out rather than erroring on it",
            namespace: "filtered",
            code_handlers: Some(FILTER_HANDLER),
            events: vec![
                json!({"keep": true, "turn": 1}),
                json!({"keep": false, "turn": 2}),
            ],
            successful: vec![0, 1],
            failed: Vec::new(),
            published: vec![json!({"keep": true, "turn": 1})],
            sentinel_published: sentinel(),
        },
    ]
}

#[tokio::test]
async fn a_publish_reports_the_events_its_namespaces_handler_answered_with_an_error() {
    let router = emulator();
    let api = create_event_api(&router, "Events").await;

    for row in rows() {
        let case = row.case;
        create_namespace(&router, &api.api_id, row.namespace, row.code_handlers).await;
        let channel = format!("/{}/room", row.namespace);
        let mut socket = connected(case, &api).await;
        let subscribed = exchange(
            case,
            &mut socket,
            &json!({"type": "subscribe", "id": "sub-1", "channel": channel}),
        )
        .await;
        assert_eq!(
            subscribed.as_ref().map(|frame| frame["type"].clone()),
            Some(json!("subscribe_success")),
            "case {case:?}: subscribe must be answered subscribe_success, got {subscribed:?}"
        );

        let (status, answer) =
            publish(case, &api, &channel, &row.events, Some(&api.api_key)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a publish whose events the handler judged is still answered 200, \
             got {status} {answer}"
        );
        let successful = entries(case, &answer, "successful");
        let failed = entries(case, &answer, "failed");
        assert_eq!(
            indexes(&successful),
            row.successful,
            "case {case:?}: expected successful at {:?}, got {answer}",
            row.successful
        );
        let expected_failed: Vec<u64> = row.failed.iter().map(|(index, _)| *index).collect();
        assert_eq!(
            indexes(&failed),
            expected_failed,
            "case {case:?}: expected failed at {expected_failed:?}, got {answer}"
        );
        for (entry, (index, message)) in failed.iter().zip(&row.failed) {
            assert_eq!(
                entry["code"],
                json!(CUSTOM_ERROR),
                "case {case:?}: the event at {index} failed on its handler's own error, so it \
                 carries the code {CUSTOM_ERROR:?}, got {entry}"
            );
            assert_eq!(
                entry["message"].as_str(),
                Some(*message),
                "case {case:?}: the event at {index} carries the sentence its handler \
                 supplied, got {entry}"
            );
        }
        // Every event of the batch is reported exactly once, and each entry names the event
        // it is of by the identifier this publish minted for it.
        assert_eq!(
            successful.len() + failed.len(),
            row.events.len(),
            "case {case:?}: expected every one of {} events reported once, got {answer}",
            row.events.len()
        );
        let mut identifiers: Vec<&str> = successful
            .iter()
            .chain(failed.iter())
            .map(|entry| {
                entry["identifier"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: every entry names its event, got {answer}")
                })
            })
            .collect();
        identifiers.sort_unstable();
        let minted = identifiers.len();
        identifiers.dedup();
        assert_eq!(
            identifiers.len(),
            minted,
            "case {case:?}: no two entries name the same event, got {answer}"
        );

        // Exactly the events the handler returned without an error are delivered: the
        // sentinel published after the batch is the next frame this subscriber reads, so an
        // event the namespace refused reached it nowhere in between.
        let (sentinel_status, sentinel_answer) = publish(
            case,
            &api,
            &channel,
            std::slice::from_ref(&sentinel()),
            Some(&api.api_key),
        )
        .await;
        assert_eq!(
            sentinel_status, 200,
            "case {case:?}: the sentinel publish must be answered 200, got {sentinel_answer}"
        );
        assert_eq!(
            sentinel_answer["failed"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: every handler of this table keeps the sentinel event, got \
             {sentinel_answer}"
        );
        let expected: Vec<Value> = row
            .published
            .iter()
            .cloned()
            .chain(std::iter::once(row.sentinel_published.clone()))
            .collect();
        let received = delivered(case, &mut socket, expected.len()).await;
        assert_eq!(
            received, expected,
            "case {case:?}: expected {expected:?} delivered, got {received:?}"
        );
    }
}

#[tokio::test]
async fn a_publish_over_the_realtime_socket_partitions_its_batch_the_same_way() {
    // The two endpoints take ONE publish path, so a publish made over the socket reports the
    // handler's per-event errors exactly as the HTTP endpoint does.
    let router = emulator();
    let api = create_event_api(&router, "Events").await;
    let case = "a publish made over the subscriber's own socket";
    create_namespace(&router, &api.api_id, "socket", Some(GATE_HANDLER)).await;
    let mut socket = connected(case, &api).await;
    let channel = "/socket/room";
    let subscribed = exchange(
        case,
        &mut socket,
        &json!({"type": "subscribe", "id": "sub-1", "channel": channel}),
    )
    .await;
    assert_eq!(
        subscribed.as_ref().map(|frame| frame["type"].clone()),
        Some(json!("subscribe_success")),
        "case {case:?}: subscribe must be answered subscribe_success, got {subscribed:?}"
    );

    let events: Vec<Value> = [json!({"ok": false, "turn": 1}), json!({"ok": true, "turn": 2})]
        .iter()
        .map(|event| json!(event.to_string()))
        .collect();
    let frame = exchange(
        case,
        &mut socket,
        &json!({"type": "publish", "id": "pub-1", "channel": channel, "events": events}),
    )
    .await
    .unwrap_or_else(|| panic!("case {case:?}: a publish must be answered"));
    assert_eq!(
        frame["type"],
        json!("publish_success"),
        "case {case:?}: expected publish_success, got {frame}"
    );
    assert_eq!(
        indexes(&entries(case, &frame, "successful")),
        vec![1],
        "case {case:?}: the event the handler kept is the one reported successful, got {frame}"
    );
    let failed = entries(case, &frame, "failed");
    assert_eq!(
        indexes(&failed),
        vec![0],
        "case {case:?}: the event the handler errored on is reported failed, got {frame}"
    );
    assert_eq!(
        (
            failed[0]["code"].clone(),
            failed[0]["message"].as_str().unwrap_or_default().to_string(),
        ),
        (json!(CUSTOM_ERROR), "payload.ok must be true".to_string()),
        "case {case:?}: the failed entry carries the handler's own error, got {frame}"
    );
    assert_eq!(
        delivered(case, &mut socket, 1).await,
        vec![json!({"ok": true, "turn": 2})],
        "case {case:?}: only the event the handler kept reaches the subscriber"
    );
}

/// The entries of one of the answer's two lists.
fn entries(case: &str, answer: &Value, list: &str) -> Vec<Value> {
    answer[list]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries a {list:?} list, got {answer}"))
}

/// The positions the entries of one list name, in the order the list states them.
fn indexes(entries: &[Value]) -> Vec<u64> {
    entries
        .iter()
        .map(|entry| entry["index"].as_u64().unwrap_or(u64::MAX))
        .collect()
}
