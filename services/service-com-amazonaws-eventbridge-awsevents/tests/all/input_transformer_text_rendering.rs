// SPDX-License-Identifier: CC0-1.0

//! What an `InputTransformer` SPLICES into its template, end to end over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the
//! real `MultiServiceRouter` with Amazon SQS on the same engine — so every
//! rendering is read where a consumer reads it: off the message on the target
//! queue, and off the dead letter when the rendering is no input a target may be
//! handed.
//!
//! The RULE asserted here: an `InputTemplate` is of two kinds, and which one it is
//! decides what a placeholder contributes.
//!
//! * A STRUCTURED template — one opening a JSON object or array — carries its
//!   placeholders "in quotes or out" (`InputTransformer$InputTemplate`): one
//!   standing where a JSON value does contributes the selected value's JSON TEXT,
//!   and one standing inside the template's own string literal contributes that
//!   value's reading as text, written as the literal carries those characters, so
//!   a quote, a newline, a tab, a backslash or a non-ASCII character in a selected
//!   string reaches the target as the character it is and the body parses back to
//!   the value the event carried.
//! * A PLAIN-TEXT template — a sentence, and the quoted string a sentence is
//!   written as — is text: every placeholder contributes that same reading as text
//!   and NOTHING is escaped for the text around it, so a value carrying a quote or
//!   a control character renders exactly those characters.
//!
//! A value's reading AS TEXT is its own characters for a string, its literal for a
//! scalar, and, for a container, its elements' and members' readings inside its own
//! brackets with JSON's quoting dropped and a member written `name:value` — an
//! array of two ids reads `[o1,o2]` and a list of objects `[{id:o1,v:1},{id:o2}]`.
//!
//! What a target may be HANDED is a JSON document, so a rendering that is not one
//! is never handed over: the invocation fails under the condition `INVALID_JSON`
//! and the event goes to the target's dead-letter queue with the rule and the
//! target named on it — the half `tests/all/transformed_input_is_json.rs` states for
//! the template forms, asserted here for the plain-text template's own unescaped
//! splice.
//!
//! The tables span the class rather than one recorded case: the rows differ along
//! the KIND of place a path names (a whole array, an element, an index past the
//! end, an element of a nested array, a member of the first element, a member of
//! every element, an empty array, a member holding `null`), along WHERE the
//! placeholder stands (as a JSON value, inside a string literal, in a plain-text
//! template), and along WHAT the selected value carries (a container, a number,
//! `null`, and the strings holding a quote, a newline, a tab, a backslash,
//! non-ASCII characters and markup).
//!
//! The harness (the router with EventBridge, Amazon SQS and the other services a
//! target lives in on one engine, and the queue and rule fixtures) is
//! `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    rule_arn, DEFAULT_EVENT_BUS_NAME, INPUT_IS_NOT_JSON, INVALID_JSON, InvocationOutcome,
};

use crate::target_dispatch::{
    drain, emulator, events, given_queue, given_rule, invocations, publish_details, REGION,
};

/// The rule every test below publishes through, and the id of the target under
/// test.
const RULE: &str = "transformer-text";
const TARGET_ID: &str = "t1";

/// The `Detail` every rendering below is built from: the arrays and the nested
/// containers an array path selects out of, the two places that are there and hold
/// nothing (an empty array, `null`), and the strings carrying each character a JSON
/// string may not carry as it stands.
fn detail() -> String {
    json!({
        "items": [10, 20, 30],
        "grid": [[1, 2], [3, 4]],
        "objs": [{ "id": "o1", "v": 1 }, { "id": "o2" }],
        "empty": [],
        "nul": null,
        "quote": "she said \"go\"",
        "line": "first\nsecond",
        "tab": "a\tb",
        "backslash": "C:\\tmp",
        "unicode": "café ☕",
        "markup": "<b>bold</b>",
    })
    .to_string()
}

/// The paths every template below fills from, so one map serves every table: one
/// per KIND of place an array path can name, plus the tricky strings.
fn paths() -> Value {
    json!({
        "items": "$.detail.items",
        "second": "$.detail.items[1]",
        "beyond": "$.detail.items[7]",
        "cell": "$.detail.grid[1][0]",
        "grid": "$.detail.grid",
        "firstId": "$.detail.objs[0].id",
        "ids": "$.detail.objs[*].id",
        "objs": "$.detail.objs",
        "empty": "$.detail.empty",
        "nul": "$.detail.nul",
        "quote": "$.detail.quote",
        "line": "$.detail.line",
        "tab": "$.detail.tab",
        "backslash": "$.detail.backslash",
        "unicode": "$.detail.unicode",
        "markup": "$.detail.markup",
    })
}

/// One target carrying `template`, on a queue of its own; publishes one event and
/// answers the bodies that queue then holds together with what the handover was
/// recorded as.
async fn rendered(case: &str, template: &str) -> (Vec<String>, InvocationOutcome) {
    let emulator = emulator();
    let (url, arn) = given_queue(&emulator, "rendered").await;
    given_rule(
        &emulator,
        RULE,
        json!([{
            "Id": TARGET_ID,
            "Arn": arn,
            "InputTransformer": {
                "InputPathsMap": paths(),
                "InputTemplate": template,
            },
        }]),
    )
    .await;

    publish_details(&emulator, &[detail()]).await;

    let bodies: Vec<String> = drain(&emulator, &url)
        .await
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect();
    let handovers = invocations(&emulator);
    let outcome = handovers
        .first()
        .map(|(_, _, outcome)| outcome.clone())
        .unwrap_or_else(|| panic!("case {case:?}: no handover to the target was recorded"));
    (bodies, outcome)
}

/// [`rendered`] for a row whose delivery must be MADE: answers the ONE body the
/// target was handed.
async fn delivered(case: &str, template: &str) -> String {
    let (bodies, outcome) = rendered(case, template).await;
    assert_eq!(
        outcome,
        InvocationOutcome::Invoked,
        "case {case:?}: expected the rendering to be handed over, got {outcome:?}"
    );
    assert_eq!(
        bodies.len(),
        1,
        "case {case:?}: expected exactly one delivery, got {bodies:?}"
    );
    bodies.into_iter().next().unwrap_or_default()
}

/// A structured template renders every kind of place an array path can name, each
/// as the JSON value it selects where the placeholder stands as a value and as its
/// reading as text where it stands inside one of the template's own strings.
#[tokio::test]
async fn a_structured_template_renders_array_paths_as_the_values_they_select() {
    let case = "a JSON object template splicing every array path";
    let template = "{\"whole\":<items>,\"wholeText\":\"<items>\",\"second\":<second>,\
                    \"beyond\":<beyond>,\"cell\":<cell>,\"grid\":<grid>,\
                    \"firstId\":<firstId>,\"ids\":<ids>,\"empty\":<empty>,\
                    \"nul\":<nul>,\"nulText\":\"<nul>\"}";
    let body = delivered(case, template).await;
    let parsed: Value = serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("case {case:?}: the body is not JSON ({error}): {body:?}"));

    // (case, the rendered member, what it must hold)
    let rows = [
        ("a whole array as a value", "whole", json!([10, 20, 30])),
        // Inside the template's own string the same array reads as text, which for
        // an array of numbers is the same characters.
        ("a whole array in a string", "wholeText", json!("[10,20,30]")),
        ("an element", "second", json!(20)),
        // A path naming no place fills the placeholder with the empty string,
        // which is not what a place holding `null` renders as (the rows below).
        ("an index past the end of the array", "beyond", json!("")),
        ("an element of a nested array", "cell", json!(3)),
        ("a nested array", "grid", json!([[1, 2], [3, 4]])),
        ("a member of the first element", "firstId", json!("o1")),
        (
            "a member of every element",
            "ids",
            json!(["o1", "o2"]),
        ),
        ("an empty array", "empty", json!([])),
        ("a member holding null", "nul", json!(null)),
        ("a member holding null in a string", "nulText", json!("null")),
    ];
    for (row, member, expected) in rows {
        assert_eq!(
            parsed[member], expected,
            "case {row:?}: expected {member} to render {expected}, got {:?} in {body:?}",
            parsed[member]
        );
    }
}

/// A PLAIN-TEXT template splices every value as its reading as text, over a TABLE
/// whose rows differ along the kind of value spliced.
#[tokio::test]
async fn a_plain_text_template_splices_every_value_as_text() {
    // (case, the template, the body the target is handed)
    let rows = [
        (
            "an array of strings",
            "\"ids=<ids>\"",
            "\"ids=[o1,o2]\"",
        ),
        (
            "a list of objects",
            "\"objs=<objs>\"",
            "\"objs=[{id:o1,v:1},{id:o2}]\"",
        ),
        (
            "an array of numbers",
            "\"items=<items>\"",
            "\"items=[10,20,30]\"",
        ),
        (
            "a nested array",
            "\"grid=<grid>\"",
            "\"grid=[[1,2],[3,4]]\"",
        ),
        ("an element", "\"second=<second>\"", "\"second=20\""),
        (
            "an element of a nested array",
            "\"cell=<cell>\"",
            "\"cell=3\"",
        ),
        (
            "an index past the end of the array",
            "\"beyond=<beyond>.\"",
            "\"beyond=.\"",
        ),
        ("an empty array", "\"empty=<empty>\"", "\"empty=[]\""),
        ("a member holding null", "\"nul=<nul>\"", "\"nul=null\""),
        (
            // Nothing is escaped for the text around the splice, so a backslash
            // arrives as the one character the event carried.
            "a string carrying a backslash",
            "\"b=<backslash>\"",
            "\"b=C:\\tmp\"",
        ),
        (
            "a string carrying non-ASCII characters",
            "\"u=<unicode>\"",
            "\"u=café ☕\"",
        ),
        (
            // The `<` of the markup is the VALUE's own character, not a
            // placeholder of the template.
            "a string carrying markup",
            "\"m=<markup>\"",
            "\"m=<b>bold</b>\"",
        ),
        (
            "several placeholders in one sentence",
            "\"<firstId> ids=<ids> objs=<objs> second=<second> empty=<empty>\"",
            "\"o1 ids=[o1,o2] objs=[{id:o1,v:1},{id:o2}] second=20 empty=[]\"",
        ),
    ];

    for (case, template, expected) in rows {
        let body = delivered(case, template).await;
        assert_eq!(
            body, expected,
            "case {case:?}: expected {template:?} to deliver {expected:?}, got {body:?}"
        );
    }
}

/// A STRUCTURED template escapes every string it splices into one of its own
/// string literals, so each tricky string reaches the target as the value the
/// event carried — asserted by reading the body back as JSON and comparing it with
/// the published `detail`.
#[tokio::test]
async fn a_structured_template_escapes_the_strings_it_splices() {
    let case = "a JSON object template splicing every tricky string";
    let template = "{\"quote\":\"<quote>\",\"line\":\"<line>\",\"tab\":\"<tab>\",\
                    \"backslash\":\"<backslash>\",\"unicode\":\"<unicode>\",\
                    \"markup\":\"<markup>\",\"q\":\"q=<quote>\",\"n\":\"n=<line>\"}";
    let body = delivered(case, template).await;
    let parsed: Value = serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("case {case:?}: the body is not JSON ({error}): {body:?}"));
    let published: Value = serde_json::from_str(&detail())
        .unwrap_or_else(|error| panic!("case {case:?}: the published detail is JSON ({error})"));

    for member in ["quote", "line", "tab", "backslash", "unicode", "markup"] {
        assert_eq!(
            parsed[member], published[member],
            "case {member:?}: expected the published string back, got {:?} in {body:?}",
            parsed[member]
        );
    }
    // The same value spliced into a LONGER string of the template: the quoting is
    // the string literal's, so the text around the splice survives too.
    for (member, prefix, source) in [("q", "q=", "quote"), ("n", "n=", "line")] {
        let expected = format!(
            "{prefix}{}",
            published[source].as_str().unwrap_or_default()
        );
        assert_eq!(
            parsed[member],
            json!(expected),
            "case {member:?}: expected {expected:?} back, got {:?} in {body:?}",
            parsed[member]
        );
    }
}

/// A PLAIN-TEXT template escapes nothing, so a value carrying a character a JSON
/// string may not hold as it stands renders a body that is no JSON document: the
/// target receives nothing and the event is dead-lettered under `INVALID_JSON`,
/// over a TABLE whose rows differ along that character.
#[tokio::test]
async fn an_unescaped_splice_that_is_no_json_document_is_dead_lettered() {
    // (case, the template, the text the rendering splices in raw)
    let rows = [
        ("a value carrying a double quote", "\"q=<quote>\"", "q=she said \"go\""),
        ("a value carrying a newline", "\"n=<line>\"", "n=first\nsecond"),
        ("a value carrying a tab", "\"t=<tab>\"", "t=a\tb"),
    ];

    for (index, (case, template, spliced)) in rows.iter().enumerate() {
        let emulator = emulator();
        let (target_url, target_arn) =
            given_queue(&emulator, &format!("unescaped-target-{index}")).await;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, &format!("unescaped-dlq-{index}")).await;
        given_rule(
            &emulator,
            RULE,
            json!([{
                "Id": TARGET_ID,
                "Arn": target_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "RetryPolicy": { "MaximumRetryAttempts": 0 },
                "InputTransformer": {
                    "InputPathsMap": paths(),
                    "InputTemplate": template,
                },
            }]),
        )
        .await;

        publish_details(&emulator, &[detail()]).await;

        let handed_over = drain(&emulator, &target_url).await;
        assert!(
            handed_over.is_empty(),
            "case {case:?}: a rendering that is no JSON document is never handed over, got \
             {handed_over:?}"
        );
        let outcome = invocations(&emulator)
            .first()
            .map(|(_, _, outcome)| outcome.clone())
            .unwrap_or_else(|| panic!("case {case:?}: no handover was recorded"));
        assert_eq!(
            outcome,
            InvocationOutcome::Failed {
                code: INVALID_JSON.to_string(),
                message: INPUT_IS_NOT_JSON.to_string(),
            },
            "case {case:?}: expected the invocation to fail on its own input, got {outcome:?}"
        );

        let dead_lettered = drain(&emulator, &dead_letter_url).await;
        assert_eq!(
            dead_lettered.len(),
            1,
            "case {case:?}: expected one dead letter on the target's queue, got {dead_lettered:?}"
        );
        let dead_letter = &dead_lettered[0];
        // The dead letter carries the body the delivery was to have sent, which is
        // the template rendered with the value spliced in RAW.
        let body = dead_letter["Body"].as_str().unwrap_or_default();
        assert_eq!(
            body,
            format!("\"{spliced}\""),
            "case {case:?}: expected the unescaped rendering on the dead letter, got {body:?}"
        );
        // (the attribute, what it must carry)
        let attributes = [
            ("ERROR_CODE", INVALID_JSON.to_string()),
            ("ERROR_MESSAGE", INPUT_IS_NOT_JSON.to_string()),
            ("RULE_ARN", rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, RULE)),
            ("TARGET_ARN", target_arn.clone()),
        ];
        for (name, expected) in attributes {
            let actual = dead_letter["MessageAttributes"][name]["StringValue"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the dead letter's {name} to be {expected:?}, got \
                 {actual:?} in {dead_letter:?}"
            );
        }

        // Re-putting the SAME target id with a structured template restores the
        // delivery: the template is what decided the refusal, nothing about the
        // target or its queue.
        let (status, answer) = events(
            &emulator,
            "PutTargets",
            &json!({
                "Rule": RULE,
                "Targets": [{
                    "Id": TARGET_ID,
                    "Arn": target_arn,
                    "DeadLetterConfig": { "Arn": dead_letter_arn },
                    "InputTransformer": {
                        "InputPathsMap": paths(),
                        "InputTemplate": "{\"q\":\"<quote>\"}",
                    },
                }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {answer}"
        );

        publish_details(&emulator, &[detail()]).await;

        let restored: Vec<String> = drain(&emulator, &target_url)
            .await
            .iter()
            .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            restored,
            vec!["{\"q\":\"she said \\\"go\\\"\"}".to_string()],
            "case {case:?}: expected the re-put target to deliver its escaped rendering, got \
             {restored:?}"
        );
    }
}
