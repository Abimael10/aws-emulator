// SPDX-License-Identifier: CC0-1.0

//! WHAT an API DESTINATION's endpoint receives, end to end over the wire: EventBridge, AWS
//! Lambda and Amazon SQS on one engine, with each destination's `InvocationEndpoint` pointing
//! at a Lambda FUNCTION URL whose `python3.x` handler echoes the request it was given.
//!
//! The RULE pinned here: EventBridge makes ONE request per matched target to the
//! destination's `InvocationEndpoint`, with the DESTINATION's `HttpMethod`, on the
//! destination's own path with no query string appended, carrying the material of the
//! connection the destination references (for a `BASIC` connection an
//! `Authorization: Basic base64("<Username>:<Password>")` header, RFC 7617) — and its BODY is
//! the matched event's bare envelope, unless the TARGET asked for something else, in which
//! case the body is exactly what the target asked for (`Input`, `InputPath` or an
//! `InputTransformer` rendering) and nothing else about the request changes.
//!
//! `tests/all/api_destination_status_classification.rs` spans what the endpoint's ANSWER does
//! to the delivery (a 2xx, the 4xx range, a 5xx) and pins the method, the `Authorization` and
//! the envelope's identity for ONE body form. This suite is the other axis: the rows span
//! what the target asks to be sent — the whole envelope, two renderings of a transformer
//! whose placeholders stand as JSON values and inside the template's own strings, a
//! placeholder of the reserved namespace, one selected part of the event, and a constant of
//! the target's own — and each row's destination is created with its own method and its own
//! path, so a request made with another destination's method, on another path, or carrying
//! another row's body fails that row. Every expected body is composed in the test from what
//! was published and what the target asked for, never imported from the implementation.
//!
//! The endpoint every row points at answers a status that is the endpoint REFUSING the
//! request, because that is the only answer under which the request EventBridge made is
//! readable by a client: the refusal's dead letter reports the endpoint's own payload, which
//! for this endpoint is its echo of that request. A delivered (2xx) request is answered and
//! dropped, so nothing a client reads carries it.
//!
//! The invocation is made through the in-process emulator transport, so the endpoint a
//! destination names is one this emulator serves itself — here a function URL over a handler
//! that runs — and no socket is opened to anything else.

use serde_json::{json, Value};

use crate::api_destination_status_classification::{
    attribute, basic_authorization, emulator, events, given_basic_connection,
    given_dead_letter_queue, given_echo_endpoint, member, until_dead_lettered, Emulator,
    DETAIL_TYPE, REGION,
};

/// The status the echo endpoint of every row answers: a 4xx is the endpoint refusing the
/// request itself, so the delivery ends on the attempt the endpoint answers and the dead
/// letter carries the endpoint's payload.
const REFUSED_STATUS: u16 = 400;

/// The condition a refusal by the endpoint is dead-lettered under.
const REFUSED_CODE: &str = "INVALID_PARAMETER";

/// The members an EventBridge event envelope carries, which is what a target that asked for
/// nothing else receives: the AWS event structure's `version`, `id`, `detail-type`, `source`,
/// `account`, `time`, `region`, `resources` and `detail` — named here in sorted order, which
/// is how this suite compares them.
const ENVELOPE_MEMBERS: [&str; 9] = [
    "account",
    "detail",
    "detail-type",
    "id",
    "region",
    "resources",
    "source",
    "time",
    "version",
];

/// The `version` every event EventBridge delivers carries (the AWS event structure documents
/// `"version": "0"` for an event by the service's own schema).
const ENVELOPE_VERSION: &str = "0";

/// The resource ARNs every event this suite publishes names, so the envelope's `resources`
/// member is compared against something rather than against the empty list.
const RESOURCES: [&str; 1] = ["arn:aws:s3:::hooks"];

/// The retry allowance every target of this suite carries.
///
/// Generous, because what a row asserts is the REQUEST and not the retry accounting: the
/// endpoint's refusal ends the delivery on whichever attempt it answers, so an attempt a
/// loaded host leaves unanswered within the client execution timeout costs the row nothing.
/// Which answers are retried at all is
/// `tests/all/api_destination_status_classification.rs`'s.
const GENEROUS_RETRY_ATTEMPTS: i32 = 5;

/// The number the published event's `detail` carries, which two rows below pull out of it.
const MARKER: i64 = 7;

/// The text the published event's `detail` carries beside it.
const NOTE: &str = "mark";

/// What a row writes where the delivered event's id belongs, which is known only once the
/// event has been published ([`filled`]).
const EVENT_ID_STANDS_FOR: &str = "<the delivered event's id>";

/// `expected` with every stand-in above replaced by the id this run gave the event.
fn filled(expected: &Value, event_id: &str) -> Value {
    match expected {
        Value::String(text) if text == EVENT_ID_STANDS_FOR => json!(event_id),
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(name, value)| (name.clone(), filled(value, event_id)))
                .collect(),
        ),
        Value::Array(elements) => Value::Array(
            elements.iter().map(|value| filled(value, event_id)).collect(),
        ),
        other => other.clone(),
    }
}

/// One row: the method its destination is created with, what its target asks to be sent (the
/// members it carries beside its `Arn`), and the document the endpoint then has to receive —
/// `None` for the row that asks for nothing, which receives the bare envelope.
struct Row {
    case: &'static str,
    method: &'static str,
    ask: Value,
    expected: Option<Value>,
}

/// The rows: what a target can ask to be sent, and what that makes the endpoint receive.
///
/// One row per FORM the dispatch chooses between — the target's own constant, the part its
/// `InputPath` selects, its `InputTransformer`'s rendering, and the bare envelope when it asks
/// for none of them — and no more: the rendering GRAMMAR itself (placeholders in quotes or
/// out, the reserved `aws.events.*` namespace, a template that is not JSON) is spanned by
/// `tests/all/input_transformation.rs` and `tests/all/transformed_input_is_json.rs`, so this
/// table stays as short as the forms it has to tell apart — one endpoint invocation per row.
///
/// The `method` column is driven from the enum the model admits (`smithy_query` query="shape"
/// shape="ApiDestinationHttpMethod": POST, GET, HEAD, OPTIONS, PUT, PATCH, DELETE) and spans
/// the three of them that carry a request body, so no row depends on one method. The others
/// are left to a suite that can source what a method documenting no body is sent: `HEAD` also
/// answers no body to echo a request back through, and `OPTIONS` may be answered by an
/// endpoint itself.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // Nothing asked for: "the whole event is passed to the target".
            case: "a target that asks for nothing receives the bare envelope",
            method: "PUT",
            ask: json!({}),
            expected: None,
        },
        Row {
            // A transformer whose placeholders stand as JSON VALUES: each carries the
            // selected value's own JSON text, so a number stays a number.
            case: "a transformer rendering two of the event's values",
            method: "POST",
            ask: json!({
                "InputTransformer": {
                    "InputPathsMap": { "m": "$.detail.m", "id": "$.id" },
                    "InputTemplate": "{\"order\": <m>, \"event\": <id>}",
                },
            }),
            // The id is the one value no row can write down: [`filled`] fills it in once the
            // event has been published.
            expected: Some(json!({ "order": MARKER, "event": EVENT_ID_STANDS_FOR })),
        },
        Row {
            // "InputPath: the value of the JSONPath that is used for extracting part of the
            // matched event when passing it to the target".
            case: "a target that asks for one part of the event",
            method: "PATCH",
            ask: json!({ "InputPath": "$.detail" }),
            expected: Some(json!({ "m": MARKER, "note": NOTE })),
        },
        Row {
            // "Input: valid JSON text passed to the target. In this case, nothing from the
            // event itself is passed to the target".
            case: "a target that asks for a constant of its own",
            method: "PUT",
            ask: json!({ "Input": "{\"fixed\": true}" }),
            expected: Some(json!({ "fixed": true })),
        },
    ]
}

/// What one row was wired up as: the destination's path, the source only its rule matches,
/// and the queue its target dead-letters to.
struct Wiring {
    path: String,
    source: String,
    queue: String,
}

/// The request the endpoint received, read back out of the dead letter the refusal wrote: the
/// message states the status the endpoint answered and the payload it answered with, and this
/// endpoint's payload is its echo of the request.
async fn echoed_request(emulator: &Emulator, case: &str, queue: &str) -> Value {
    let dead_letters = until_dead_lettered(emulator, case, queue).await;
    assert_eq!(
        dead_letters.len(),
        1,
        "case {case:?}: a request the endpoint refused is dead-lettered once, got \
         {dead_letters:?}"
    );
    let message = &dead_letters[0];
    assert_eq!(
        attribute(message, "ERROR_CODE").as_deref(),
        Some(REFUSED_CODE),
        "case {case:?}: an answer of HTTP {REFUSED_STATUS} is the endpoint refusing the \
         request, got {message}"
    );
    let reported = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
    let opening = format!("ApiDestination returned HTTP status {REFUSED_STATUS} with payload: ");
    let payload = reported.strip_prefix(&opening).unwrap_or_else(|| {
        panic!("case {case:?}: expected the message to open {opening:?}, got {reported:?}")
    });
    serde_json::from_str(payload).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the endpoint's echo as the payload, got {error}: {payload:?}"
        )
    })
}

/// RULE: what an api-destination target's endpoint receives as the request BODY is the
/// matched event's bare envelope, unless the target asked for something else — then it is
/// exactly what the target asked for — while the method and the path are the destination's,
/// no query string is appended and the connection's authorization is carried either way.
#[tokio::test]
async fn an_api_destination_request_carries_the_body_the_target_asked_for() {
    let emulator = emulator();
    let endpoint = given_echo_endpoint(&emulator).await;
    let connection_arn = given_basic_connection(&emulator).await;
    let expected_authorization = basic_authorization();

    // One destination, rule, source, target and dead-letter queue per row, driven by a
    // publication of its own: a row's request is then never read off another's, and no row
    // shares a publication's delivery budget with the rest of the table.
    let rows = rows();
    let mut wiring: Vec<Wiring> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let path = format!("/row-{index}/{REFUSED_STATUS}");
        let rule = format!("request-shape-rule-{index}");
        let source = format!("com.demo.api-destination.request-shape.{index}");
        let (queue, queue_arn) =
            given_dead_letter_queue(&emulator, case, &format!("shape-dlq-{index}")).await;
        let created = events(
            &emulator,
            case,
            "CreateApiDestination",
            &json!({
                "Name": format!("request-shape-{index}"),
                "ConnectionArn": connection_arn,
                "InvocationEndpoint": format!("{endpoint}{path}"),
                "HttpMethod": row.method,
            }),
        )
        .await;
        let destination_arn = member(case, &created, "ApiDestinationArn");
        events(
            &emulator,
            case,
            "PutRule",
            &json!({
                "Name": rule,
                "EventPattern": json!({ "source": [source] }).to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let mut target = json!({
            "Id": "hook",
            "Arn": destination_arn,
            "DeadLetterConfig": { "Arn": queue_arn },
            "RetryPolicy": {
                "MaximumRetryAttempts": GENEROUS_RETRY_ATTEMPTS,
                "MaximumEventAgeInSeconds": 900,
            },
        });
        for (name, value) in row.ask.as_object().into_iter().flatten() {
            target[name] = value.clone();
        }
        let wired = events(
            &emulator,
            case,
            "PutTargets",
            &json!({ "Rule": rule, "Targets": [target] }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the target to be accepted, got {wired}"
        );
        wiring.push(Wiring {
            path,
            source,
            queue,
        });
    }

    let detail = json!({ "m": MARKER, "note": NOTE });
    for (row, wired) in rows.iter().zip(&wiring) {
        let case = row.case;
        let published = events(
            &emulator,
            case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": wired.source,
                    "DetailType": DETAIL_TYPE,
                    "Resources": RESOURCES,
                    "Detail": detail.to_string(),
                }],
            }),
        )
        .await;
        let event_id = published["Entries"][0]["EventId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an EventId, got {published}"))
            .to_string();

        let echoed = echoed_request(&emulator, case, &wired.queue).await;
        // Everything that does NOT depend on what the target asked to be sent: the
        // destination's method, the destination's path with no query string appended, and the
        // connection's authorization.
        assert_eq!(
            echoed["method"],
            json!(row.method),
            "case {case:?}: expected the destination's HttpMethod {:?}, got {echoed}",
            row.method
        );
        assert_eq!(
            echoed["path"],
            json!(wired.path),
            "case {case:?}: expected the request on the destination's own path {:?}, got \
             {echoed}",
            wired.path
        );
        assert_eq!(
            echoed["query"],
            json!(""),
            "case {case:?}: nothing configured a query string, so none is appended, got \
             {echoed}"
        );
        assert_eq!(
            echoed["authorization"],
            json!(expected_authorization),
            "case {case:?}: expected the connection's basic authorization, got {echoed}"
        );

        let text = echoed["body"].as_str().unwrap_or_default();
        let sent: Value = serde_json::from_str(text).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a JSON body, got {error}: {text:?}")
        });
        match &row.expected {
            // What the target asked for, and nothing of the envelope around it.
            Some(expected) => {
                let expected = filled(expected, &event_id);
                assert_eq!(
                    sent, expected,
                    "case {case:?}: expected the body the target asked for, got {sent}"
                );
            }
            // The bare envelope: exactly the members an event carries, holding what was
            // published.
            None => {
                let mut members: Vec<String> = sent
                    .as_object()
                    .map(|object| object.keys().cloned().collect())
                    .unwrap_or_default();
                members.sort();
                assert_eq!(
                    members, ENVELOPE_MEMBERS,
                    "case {case:?}: expected exactly the envelope's members, got {sent}"
                );
                assert_eq!(
                    sent["id"],
                    json!(event_id),
                    "case {case:?}: expected the delivered event's id, got {sent}"
                );
                assert_eq!(
                    sent["source"],
                    json!(wired.source),
                    "case {case:?}: expected the published source, got {sent}"
                );
                assert_eq!(
                    sent["detail-type"],
                    json!(DETAIL_TYPE),
                    "case {case:?}: expected the published detail type, got {sent}"
                );
                assert_eq!(
                    sent["detail"], detail,
                    "case {case:?}: expected the published detail, got {sent}"
                );
                assert_eq!(
                    sent["resources"],
                    json!(RESOURCES),
                    "case {case:?}: expected the published resources, got {sent}"
                );
                assert_eq!(
                    sent["region"],
                    json!(REGION),
                    "case {case:?}: expected the Region the event was published in, got {sent}"
                );
                assert_eq!(
                    sent["account"],
                    json!(engine::emulator_account_id()),
                    "case {case:?}: expected the account the event was published in, got {sent}"
                );
                assert_eq!(
                    sent["version"],
                    json!(ENVELOPE_VERSION),
                    "case {case:?}: expected the envelope's own version, got {sent}"
                );
                assert!(
                    sent["time"].as_str().is_some_and(|time| !time.is_empty()),
                    "case {case:?}: expected the instant the event was ingested, got {sent}"
                );
            }
        }
    }
}
