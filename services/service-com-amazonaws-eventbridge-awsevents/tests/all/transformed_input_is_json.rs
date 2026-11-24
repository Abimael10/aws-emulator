// SPDX-License-Identifier: CC0-1.0

//! What a target may be HANDED is a JSON document, end to end over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the
//! real `MultiServiceRouter` with Amazon SQS on the same engine — so both halves
//! are read where a client reads them: on the target queue, and on the dead-letter
//! queue the target names.
//!
//! The RULE asserted here: an `InputTransformer` fills every `<key>` placeholder
//! of its `InputTemplate` with what its path selects, TEXTUALLY, and WHERE the
//! placeholder stands decides what the value contributes. A template that opens a
//! JSON value carries its placeholders "in quotes or out": one standing as a value
//! contributes the value's JSON text, one standing inside one of the template's
//! own strings the value's reading as a string — a string's own characters as that
//! string carries them, and any other value's JSON text. So a value with no
//! reading as a string (an object, an array) written inside a string leaves a
//! rendering that is no JSON document, and so does a template that is no JSON
//! document itself.
//!
//! What a target may be HANDED is a JSON document, so an input that is not one is
//! never handed over: the invocation FAILS, the event goes to the target's
//! dead-letter queue under the condition `INVALID_JSON` carrying AWS's own
//! sentence for one, and the target itself receives nothing. A placeholder
//! standing for the whole EVENT carries that event's own JSON document, so a
//! template that is nothing but `<aws.events.event.json>` renders the event and
//! IS delivered.
//!
//! The table spans the class rather than one case: the rows differ along the FORM
//! of the template (a JSON object, a JSON array, a JSON string, a bare
//! placeholder), along WHERE the placeholder stands (as a value, inside a string),
//! and along WHAT the substituted value carries (a plain string, a string holding
//! a quote, one holding a control character, a number, an object, the whole event)
//! — which together decide whether the rendering is a document at all.
//!
//! The harness (the router with EventBridge, Amazon SQS, Step Functions, IAM and
//! CloudWatch on one engine, and the queue and rule fixtures) is
//! `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    rule_arn, DEFAULT_EVENT_BUS_NAME, INPUT_IS_NOT_JSON, INVALID_JSON, InvocationOutcome,
};

use crate::target_dispatch::{
    drain, emulator, given_queue, given_rule, invocations, publish_details, REGION,
};

/// The rule every row publishes through, and the id of its one target.
const RULE: &str = "transformed-input";
const TARGET_ID: &str = "t1";

/// The `Detail` every row transforms: a string, a number and the two strings
/// carrying the characters a JSON string may not carry as they stand — a double
/// quote and a control character — so one event spans every way a substitution can
/// break the template it lands in.
fn detail() -> String {
    json!({
        "orderId": "A-1",
        "amount": 7,
        "quote": "she said \"go\"",
        "line": "first\nsecond",
    })
    .to_string()
}

/// The paths every row's `InputPathsMap` carries, so one map serves the whole
/// table.
fn paths() -> Value {
    json!({
        "order": "$.detail.orderId",
        "amount": "$.detail.amount",
        "quote": "$.detail.quote",
        "line": "$.detail.line",
        "whole": "$.detail",
    })
}

/// What the target is handed for a row.
enum Delivered {
    /// Exactly this text.
    Text(&'static str),
    /// The published event's own envelope — whose id and time are the
    /// publication's, so what is asserted is that it IS that envelope rather than
    /// one literal.
    Envelope,
    /// Nothing at all: the rendering is no JSON document, so the invocation
    /// fails.
    Nothing,
}

/// One row: the case, the template, and what the target is handed.
struct Row {
    case: &'static str,
    template: &'static str,
    delivered: Delivered,
}

/// A rendering that is not a JSON document is never handed to the target: it is
/// dead-lettered instead, over a table of templates and substituted values.
#[tokio::test]
async fn a_rendering_that_is_not_json_is_dead_lettered_instead_of_delivered() {
    let rows = [
        Row {
            case: "a JSON object template whose placeholders stand as values",
            template: "{\"o\": <order>, \"n\": <amount>}",
            delivered: Delivered::Text("{\"o\": \"A-1\", \"n\": 7}"),
        },
        Row {
            case: "a JSON object template whose placeholder stands in a string",
            template: "{\"o\": \"<order>\"}",
            delivered: Delivered::Text("{\"o\": \"A-1\"}"),
        },
        Row {
            // A selected STRING is written as the string it lands in carries it,
            // so the rendering is still the JSON the template was written as.
            case: "a JSON object template whose string member carries a quote",
            template: "{\"q\": \"<quote>\"}",
            delivered: Delivered::Text("{\"q\": \"she said \\\"go\\\"\"}"),
        },
        Row {
            case: "a JSON object template whose string member carries a newline",
            template: "{\"l\": \"<line>\"}",
            delivered: Delivered::Text("{\"l\": \"first\\nsecond\"}"),
        },
        Row {
            case: "a plain-text template written as the JSON string it is",
            template: "\"order <order> for <amount>\"",
            delivered: Delivered::Text("\"order A-1 for 7\""),
        },
        Row {
            // The placeholder standing for the whole event carries that event's
            // own JSON document, so the target is handed the envelope.
            case: "a template that is nothing but the reserved event placeholder",
            template: "<aws.events.event.json>",
            delivered: Delivered::Envelope,
        },
        Row {
            // An OBJECT has no reading as a string, so what lands inside the
            // template's own string is the document it is — which that string
            // cannot hold.
            case: "a JSON object template whose string member takes an object",
            template: "{\"d\": \"<whole>\"}",
            delivered: Delivered::Nothing,
        },
        Row {
            // The same, one template form along: an array's own string element.
            case: "a JSON array template whose string element takes an object",
            template: "[\"<whole>\"]",
            delivered: Delivered::Nothing,
        },
        Row {
            // A quoted-string template IS one JSON string, so a document
            // substituted into it breaks it in half just the same.
            case: "a quoted-string template taking an object",
            template: "\"the order was <whole>\"",
            delivered: Delivered::Nothing,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let (target_url, target_arn) =
            given_queue(&emulator, &format!("transformed-target-{index}")).await;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, &format!("transformed-dlq-{index}")).await;
        given_rule(
            &emulator,
            RULE,
            json!([{
                "Id": TARGET_ID,
                "Arn": target_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "InputTransformer": {
                    "InputPathsMap": paths(),
                    "InputTemplate": row.template,
                },
            }]),
        )
        .await;

        publish_details(&emulator, &[detail()]).await;

        let handed_over: Vec<String> = drain(&emulator, &target_url)
            .await
            .iter()
            .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
            .collect();
        let dead_lettered = drain(&emulator, &dead_letter_url).await;
        let recorded = invocations(&emulator);
        let outcome = recorded
            .first()
            .map(|(_, _, outcome)| outcome.clone())
            .unwrap_or_else(|| panic!("case {case:?}: nothing was recorded"));

        if !matches!(row.delivered, Delivered::Nothing) {
            match row.delivered {
                Delivered::Text(expected) => assert_eq!(
                    handed_over,
                    vec![expected.to_string()],
                    "case {case:?}: expected {:?} to deliver {expected:?}, got {handed_over:?}",
                    row.template
                ),
                Delivered::Envelope => {
                    assert_eq!(
                        handed_over.len(),
                        1,
                        "case {case:?}: expected the envelope to be delivered, got \
                         {handed_over:?}"
                    );
                    let envelope: Value = serde_json::from_str(&handed_over[0])
                        .unwrap_or_else(|error| {
                            panic!(
                                "case {case:?}: the delivered text is not the event ({error}): \
                                 {:?}",
                                handed_over[0]
                            )
                        });
                    assert_eq!(
                        envelope["detail"]["orderId"],
                        json!("A-1"),
                        "case {case:?}: expected the published event, got {envelope}"
                    );
                }
                Delivered::Nothing => unreachable!("the branch above excludes it"),
            }
            assert!(
                dead_lettered.is_empty(),
                "case {case:?}: a rendering that is JSON is delivered, not dead-lettered, got \
                 {dead_lettered:?}"
            );
            assert_eq!(
                outcome,
                InvocationOutcome::Invoked,
                "case {case:?}: expected the handover to be recorded as invoked, got {outcome:?}"
            );
            continue;
        }

        assert!(
            handed_over.is_empty(),
            "case {case:?}: a rendering that is no JSON document is never handed over, got \
             {handed_over:?}"
        );
        assert_eq!(
            outcome,
            InvocationOutcome::Failed {
                code: INVALID_JSON.to_string(),
                message: INPUT_IS_NOT_JSON.to_string(),
            },
            "case {case:?}: expected the invocation to fail on its own input, got {outcome:?}"
        );
        assert_eq!(
            dead_lettered.len(),
            1,
            "case {case:?}: the event goes to the target's dead-letter queue, got \
             {dead_lettered:?}"
        );
        let attributes = &dead_lettered[0]["MessageAttributes"];
        assert_eq!(
            attributes["ERROR_CODE"]["StringValue"],
            json!(INVALID_JSON),
            "case {case:?}: expected the dead-letter ERROR_CODE {INVALID_JSON}, got {:?}",
            dead_lettered[0]
        );
        assert_eq!(
            attributes["ERROR_MESSAGE"]["StringValue"],
            json!(INPUT_IS_NOT_JSON),
            "case {case:?}: expected the dead letter to say what was wrong with the input, got \
             {:?}",
            dead_lettered[0]
        );
        assert_eq!(
            attributes["RULE_ARN"]["StringValue"],
            json!(rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, RULE)),
            "case {case:?}: expected the dead letter to name the rule, got {:?}",
            dead_lettered[0]
        );
        assert_eq!(
            attributes["TARGET_ARN"]["StringValue"],
            json!(target_arn),
            "case {case:?}: expected the dead letter to name the target, got {:?}",
            dead_lettered[0]
        );
        // The dead letter carries what the delivery was to have sent, so a
        // consumer sees the input that could not be handed over.
        assert!(
            dead_lettered[0]["Body"].is_string(),
            "case {case:?}: expected the dead letter to carry a body, got {:?}",
            dead_lettered[0]
        );
    }
}

/// Two targets of ONE rule are decided apart: the one whose template renders a
/// JSON document receives the event while the one whose rendering is not a
/// document is dead-lettered, so a single mis-written transformer takes nothing
/// else down with it.
#[tokio::test]
async fn one_targets_unrenderable_input_does_not_stop_another_target_of_the_rule() {
    let case = "a rule with one renderable JSON template and one that breaks";
    let emulator = emulator();
    let (good_url, good_arn) = given_queue(&emulator, "sibling-good").await;
    let (bad_url, bad_arn) = given_queue(&emulator, "sibling-bad").await;
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, "sibling-dlq").await;
    given_rule(
        &emulator,
        RULE,
        json!([
            {
                "Id": "good",
                "Arn": good_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "InputTransformer": {
                    "InputPathsMap": paths(),
                    "InputTemplate": "{\"o\": \"<order>\"}",
                },
            },
            {
                "Id": "bad",
                "Arn": bad_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "InputTransformer": {
                    "InputPathsMap": paths(),
                    "InputTemplate": "{\"d\": \"<whole>\"}",
                },
            },
        ]),
    )
    .await;

    publish_details(&emulator, &[detail()]).await;

    let delivered: Vec<String> = drain(&emulator, &good_url)
        .await
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        delivered,
        vec!["{\"o\": \"A-1\"}".to_string()],
        "case {case:?}: the target whose rendering is JSON receives it, got {delivered:?}"
    );
    let refused = drain(&emulator, &bad_url).await;
    assert!(
        refused.is_empty(),
        "case {case:?}: the target whose rendering is no JSON document receives nothing, got \
         {refused:?}"
    );
    let dead_lettered = drain(&emulator, &dead_letter_url).await;
    assert_eq!(
        dead_lettered.len(),
        1,
        "case {case:?}: exactly the refused target's event is dead-lettered, got \
         {dead_lettered:?}"
    );
    assert_eq!(
        dead_lettered[0]["MessageAttributes"]["TARGET_ARN"]["StringValue"],
        json!(bad_arn),
        "case {case:?}: expected the dead letter to name the refused target, got {:?}",
        dead_lettered[0]
    );
}
