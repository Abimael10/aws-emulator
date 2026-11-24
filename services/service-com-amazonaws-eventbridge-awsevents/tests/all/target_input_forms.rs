// SPDX-License-Identifier: CC0-1.0

//! WHICH input a target is handed, and what `PutTargets` refuses about it — end to
//! end over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target:
//! AWSEvents.<Operation>`) through the real `MultiServiceRouter` with Amazon SQS on
//! the same engine, so a refusal is read as a client reads it and the target that
//! survived one is read by publishing through it.
//!
//! The RULES asserted here:
//!
//! * "`Input`, `InputPath`, and `InputTransformer` are mutually exclusive and
//!   optional parameters of a target" (`PutTargets`): each answers the one question
//!   of what the target receives, so a target stating two of them is refused with
//!   HTTP 400 `ValidationException` naming the conflict, and a target stating one
//!   is stored.
//! * "`Input`: valid JSON text passed to the target" (`Target$Input`): what travels
//!   is JSON text, so text that is no JSON document is refused — a target may only
//!   be handed a document.
//! * An `InputTemplate` is the JSON text a target is handed with placeholders in it
//!   (`InputTransformer$InputTemplate`), so it states a JSON document: an object, an
//!   array, a quoted string, or a placeholder standing for one. A bare, unquoted
//!   sentence states none and is refused.
//! * A refused request is refused WHOLE and before anything is stored, so the target
//!   the rule already carried is left exactly as it was and keeps delivering what it
//!   was configured to deliver.
//!
//! The tables span each class rather than the one recorded case: the rows differ
//! along WHICH input members a target states (every non-empty combination of the
//! three), along the FORM of the `Input` text (an object, an array, a quoted string,
//! a number, `null`, a sentence, a truncated document, nothing at all) and along the
//! FORM of the template (an object, an array, a quoted string, a bare placeholder,
//! a sentence, a document never closed).
//!
//! The harness (the router with EventBridge, Amazon SQS, Step Functions, IAM and
//! CloudWatch on one engine, and the queue and rule fixtures) is
//! `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};

use crate::target_dispatch::{
    drain, emulator, events, given_queue, given_rule, publish, Emulator,
};

/// The rule every row below writes targets on, and the id of the target each one
/// upserts.
const RULE: &str = "input-forms";
const TARGET_ID: &str = "t1";

/// The id of the target the rule already carries, which a refused request must
/// leave alone.
const KEPT_TARGET_ID: &str = "kept";

/// The wire error code (`__type`), without its namespace; the empty string on a
/// success.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The `message` member of a failure body.
fn message(body: &Value) -> &str {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// A rule carrying one working target on the queue `arn`, which every row's
/// upsert is judged against.
async fn given_rule_with_a_kept_target(emulator: &Emulator, arn: &str) {
    given_rule(
        emulator,
        RULE,
        json!([{ "Id": KEPT_TARGET_ID, "Arn": arn }]),
    )
    .await;
}

/// Upsert one target called `id` carrying `members` beside its ARN, and answer
/// what `PutTargets` said.
async fn put_target(
    emulator: &Emulator,
    id: &str,
    arn: &str,
    members: &Value,
) -> (u16, Value) {
    let mut target = json!({ "Id": id, "Arn": arn });
    for (name, value) in members.as_object().cloned().unwrap_or_default() {
        target[name] = value;
    }
    events(
        emulator,
        "PutTargets",
        &json!({ "Rule": RULE, "Targets": [target] }),
    )
    .await
}

/// The ids of the targets the rule carries, in the order it pages through them.
async fn stored_ids(emulator: &Emulator) -> Vec<String> {
    let (status, listed) = events(emulator, "ListTargetsByRule", &json!({ "Rule": RULE })).await;
    assert_eq!(
        status, 200,
        "ListTargetsByRule answered {status} {listed}"
    );
    listed["Targets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|target| target["Id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `Input` a row that states that member carries.
const AN_INPUT: &str = "{\"from\": \"input\"}";

/// The `InputPath` a row that states that member carries.
const A_PATH: &str = "$.detail";

/// The `InputTransformer` a row that states that member carries.
fn a_transformer() -> Value {
    json!({
        "InputPathsMap": { "order": "$.detail.orderId" },
        "InputTemplate": "{\"o\": <order>}",
    })
}

/// One row of the exclusivity table: which of the three members the target states.
struct Forms {
    case: &'static str,
    input: bool,
    input_path: bool,
    transformer: bool,
}

/// The target members a row states, as `PutTargets` takes them.
fn stated(row: &Forms) -> Value {
    let mut members = json!({});
    if row.input {
        members["Input"] = json!(AN_INPUT);
    }
    if row.input_path {
        members["InputPath"] = json!(A_PATH);
    }
    if row.transformer {
        members["InputTransformer"] = a_transformer();
    }
    members
}

/// RULE: a target states AT MOST ONE of `Input`, `InputPath` and
/// `InputTransformer`; one that states two or three is refused with 400
/// `ValidationException` naming the conflict, and the rule's targets are left as
/// they were.
#[tokio::test]
async fn a_target_may_state_only_one_of_the_three_input_members() {
    // Every non-empty combination of the three members: the three singletons are
    // accepted, every pair and the triple are refused.
    let rows = [
        Forms {
            case: "Input alone",
            input: true,
            input_path: false,
            transformer: false,
        },
        Forms {
            case: "InputPath alone",
            input: false,
            input_path: true,
            transformer: false,
        },
        Forms {
            case: "InputTransformer alone",
            input: false,
            input_path: false,
            transformer: true,
        },
        Forms {
            case: "Input together with InputPath",
            input: true,
            input_path: true,
            transformer: false,
        },
        Forms {
            case: "Input together with InputTransformer",
            input: true,
            input_path: false,
            transformer: true,
        },
        Forms {
            case: "InputPath together with InputTransformer",
            input: false,
            input_path: true,
            transformer: true,
        },
        Forms {
            case: "all three at once",
            input: true,
            input_path: true,
            transformer: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // A target id of this row's own, so a refusal that names one is shown to
        // name the id the REQUEST gave rather than any fixed text.
        let target_id = format!("forms-{index}");
        let emulator = emulator();
        let (_, arn) = given_queue(&emulator, "input-forms-target").await;
        given_rule_with_a_kept_target(&emulator, &arn).await;

        let (status, body) = put_target(&emulator, &target_id, &arn, &stated(row)).await;
        let exclusive =
            usize::from(row.input) + usize::from(row.input_path) + usize::from(row.transformer) < 2;
        if exclusive {
            assert_eq!(
                status, 200,
                "case {case:?}: expected a target stating one input member to be stored, got \
                 {status} {body}"
            );
            let ids = stored_ids(&emulator).await;
            assert_eq!(
                ids,
                vec![KEPT_TARGET_ID.to_string(), target_id.clone()],
                "case {case:?}: expected the stored target beside the kept one, got {ids:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected a target stating two input members to be refused, got \
             {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            message(&body),
            format!(
                "Only one of Input, InputPath, or InputTransformer must be provided for target \
                 {target_id}."
            ),
            "case {case:?}: expected the conflict naming the target, got {body}"
        );
        let ids = stored_ids(&emulator).await;
        assert_eq!(
            ids,
            vec![KEPT_TARGET_ID.to_string()],
            "case {case:?}: a refused request leaves the rule's targets alone, got {ids:?}"
        );
    }
}

/// RULE: `Input` is valid JSON TEXT, so a target whose `Input` is not a JSON
/// document is refused and the rule's targets are left as they were.
#[tokio::test]
async fn an_input_that_is_no_json_document_is_refused() {
    // (case, the Input text, whether it is JSON text a target may be handed)
    let rows = [
        ("an object", "{\"from\": \"input\"}", true),
        ("an array", "[1, 2]", true),
        ("a quoted string", "\"plain text\"", true),
        ("a number", "7", true),
        ("the null literal", "null", true),
        ("a document with surrounding whitespace", " {\"a\": 1} ", true),
        // Each of these is text no target may be handed, so it is refused where
        // the target is configured.
        ("a bare sentence", "nothing from the event", false),
        ("an unquoted word", "text", false),
        ("an object that is never closed", "{\"a\": 1", false),
        ("a document with text after it", "{\"a\": 1} and more", false),
        ("nothing at all", "", false),
    ];

    for (index, (case, input, valid)) in rows.into_iter().enumerate() {
        // A target id of this row's own, so the refusal is shown to name the id
        // the REQUEST gave.
        let target_id = format!("input-{index}");
        let emulator = emulator();
        let (_, arn) = given_queue(&emulator, "input-target").await;
        given_rule_with_a_kept_target(&emulator, &arn).await;

        let (status, body) =
            put_target(&emulator, &target_id, &arn, &json!({ "Input": input })).await;
        if valid {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the Input {input:?} to be stored, got {status} {body}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected the Input {input:?} to be refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            message(&body),
            format!("Input for target {target_id} is not valid JSON."),
            "case {case:?}: expected the Input refusal naming the target, got {body}"
        );
        let ids = stored_ids(&emulator).await;
        assert_eq!(
            ids,
            vec![KEPT_TARGET_ID.to_string()],
            "case {case:?}: a refused request leaves the rule's targets alone, got {ids:?}"
        );
    }
}

/// RULE: an `InputTemplate` states a JSON document, so a template that states
/// none is refused and the rule's targets are left as they were.
#[tokio::test]
async fn an_input_template_that_states_no_json_document_is_refused() {
    // (case, the InputTemplate, whether it states a JSON document)
    let rows = [
        ("an object with a bare placeholder", "{\"o\": <order>}", true),
        (
            "an object with a quoted placeholder",
            "{\"o\": \"<order>\"}",
            true,
        ),
        (
            "a placeholder inside a longer string",
            "{\"o\": \"order <order> taken\"}",
            true,
        ),
        ("an array", "[<order>]", true),
        ("a quoted string", "\"order <order> taken\"", true),
        ("an object with no placeholder", "{\"o\": \"fixed\"}", true),
        (
            "nothing but the reserved event placeholder",
            "<aws.events.event.json>",
            true,
        ),
        // A placeholder standing alone states the value it will be filled with;
        // whether THAT value renders a document is the delivery's business, where
        // an input that is none is dead-lettered
        // (`tests/all/transformed_input_is_json.rs`).
        ("nothing but a declared placeholder", "<order>", true),
        // A bare sentence states no document: rendered, it could only produce
        // text no target may be handed.
        ("a bare sentence", "order <order> taken", false),
        ("a sentence carrying no placeholder", "a fixed sentence", false),
        ("an object that is never closed", "{\"o\": <order>", false),
        ("a string that is never closed", "\"<order>", false),
    ];

    for (index, (case, template, states)) in rows.into_iter().enumerate() {
        // A target id of this row's own, so the refusal is shown to name the id
        // the REQUEST gave.
        let target_id = format!("template-{index}");
        let emulator = emulator();
        let (_, arn) = given_queue(&emulator, "template-target").await;
        given_rule_with_a_kept_target(&emulator, &arn).await;

        let transformer = json!({
            "InputTransformer": {
                "InputPathsMap": { "order": "$.detail.orderId" },
                "InputTemplate": template,
            },
        });
        let (status, body) = put_target(&emulator, &target_id, &arn, &transformer).await;
        if states {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the template {template:?} to be stored, got {status} \
                 {body}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected the template {template:?} to be refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            message(&body),
            format!("InputTemplate for target {target_id} is not valid JSON."),
            "case {case:?}: expected the InputTemplate refusal naming the target, got {body}"
        );
        let ids = stored_ids(&emulator).await;
        assert_eq!(
            ids,
            vec![KEPT_TARGET_ID.to_string()],
            "case {case:?}: a refused request leaves the rule's targets alone, got {ids:?}"
        );
    }
}

/// RULE: an upsert a target's own configuration is refused for changes nothing, so
/// the target already stored under that id keeps delivering what it was configured
/// to deliver.
///
/// The rows differ along WHAT the upsert gets wrong — an `Input` that is no JSON
/// document, a template that states none, two input members at once — so the
/// preservation is asserted for the whole class of refusal rather than one of them.
#[tokio::test]
async fn a_refused_upsert_leaves_the_live_target_delivering() {
    // (case, the members the upsert states on the live target's own id)
    let rows = [
        (
            "an Input that is no JSON document",
            json!({ "Input": "not json at all" }),
        ),
        (
            "an InputTemplate that states no document",
            json!({
                "InputTransformer": {
                    "InputPathsMap": { "order": "$.detail.orderId" },
                    "InputTemplate": "order <order> taken",
                },
            }),
        ),
        (
            "two input members at once",
            json!({ "Input": AN_INPUT, "InputPath": A_PATH }),
        ),
    ];

    for (case, upsert) in rows {
        let emulator = emulator();
        let (url, arn) = given_queue(&emulator, "live-target").await;
        // The live target: it takes the event as published, so what it delivers
        // is the envelope itself.
        given_rule(&emulator, RULE, json!([{ "Id": TARGET_ID, "Arn": arn }])).await;
        let first = publish(&emulator).await;
        let delivered = drain(&emulator, &url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: expected the live target to be delivered to, got {delivered:?}"
        );

        let (status, body) = put_target(&emulator, TARGET_ID, &arn, &upsert).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the upsert to be refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );

        // The target is still the one that was working: it is still the only one
        // the rule carries, it still states none of the input members, and the
        // next publication still reaches it with the whole envelope.
        let (status, listed) =
            events(&emulator, "ListTargetsByRule", &json!({ "Rule": RULE })).await;
        assert_eq!(status, 200, "case {case:?}: ListTargetsByRule {listed}");
        let targets = listed["Targets"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            targets.len(),
            1,
            "case {case:?}: expected the rule to carry its one target, got {listed}"
        );
        for member in ["Input", "InputPath", "InputTransformer"] {
            assert!(
                targets[0].get(member).is_none(),
                "case {case:?}: the refused {member} was stored on the live target: {listed}"
            );
        }

        let second = publish(&emulator).await;
        assert_ne!(
            first, second,
            "case {case:?}: the two publications are different events"
        );
        let delivered = drain(&emulator, &url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: expected the live target to keep delivering, got {delivered:?}"
        );
        let body = delivered[0]["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the delivered body is not the envelope ({error}): {body:?}")
        });
        assert_eq!(
            envelope["id"],
            json!(second),
            "case {case:?}: expected the second publication's own envelope, got {envelope}"
        );
    }
}
