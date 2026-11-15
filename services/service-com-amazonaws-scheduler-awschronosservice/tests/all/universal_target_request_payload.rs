// SPDX-License-Identifier: CC0-1.0

//! What the BODY of a universal target's call is, read where the invoked service sees
//! it: end to end over the real `MultiServiceRouter`, with Lambda and IAM mounted
//! beside EventBridge Scheduler on ONE engine, so the bytes asserted here are the bytes
//! that reached the invoked operation.
//!
//! RULE: a universal target's `Target$Input` — "the text, or well-formed JSON, passed to
//! the target" — IS the named SDK operation's own request as a CLIENT writes it, so the
//! member that operation carries as the request's BODY is written as the TEXT of the
//! bytes it carries and the request body is those bytes, byte for byte. AWS Lambda's
//! `Invoke` binds `Payload` as its `httpPayload`
//! (`smithy_query(query="operation",operation="Invoke",model_path="services/
//! service-com-amazonaws-lambda-awsgirapiservice/smithy-model/
//! com-amazonaws-lambda-awsgirapiservice.json")`), so the invocation is handed exactly
//! what `Payload` spelled — not that text re-encoded as a JSON string, which would reach
//! a handler as a string wherever a document was written.
//!
//! The rows span the class rather than one recorded case: they differ along the JSON
//! value the payload text encodes (a flat object, a nested document carrying arrays,
//! mixed number types, booleans, null, non-ASCII text and embedded quotes, a top-level
//! array, a bare string, a bare number, the empty object), along the exact BYTES of that
//! text (one row carries insignificant whitespace an encoder would drop), along a
//! request that names no payload member at all, and along one whose payload is not JSON
//! — which the invoked operation itself refuses, so nothing is queued for it. That last
//! row is what tells the two readings apart: a text re-encoded as a JSON string would
//! always BE well-formed JSON, so it would always be accepted.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::per_schedule_target_input::{
    emulator_with_lambda, given_function, given_invoking_role, invoked_payloads,
    invoked_payloads_once,
};
use crate::schedule_invocation::{at_expression_body, scheduler, seconds_from_now, POLL_INTERVAL};

/// How long the suite keeps LOOKING after the payloads it expected have arrived, so an
/// invocation this emulator would have queued for a row that expects none — or queued a
/// second time with other bytes — has had its chance to appear. Nothing is asserted
/// about how long anything took: the window is only how long the absence is watched for.
const QUIET_WINDOW: Duration = Duration::from_secs(8);

/// How far ahead of the clock every schedule below states its one occurrence. Each
/// states the SAME instant, so the occurrences fall due together.
const OCCURRENCE_IN: u64 = 6;

/// The universal target every row names: AWS Lambda's `Invoke`, under the SDK service id
/// (`lambda`) and that SDK's own lowerCamelCase name for the operation.
const INVOKE_TARGET: &str = "arn:aws:scheduler:::aws-sdk:lambda:invoke";

/// The IAM action an invocation of a function is authorized by.
const INVOKE_FUNCTION: &str = "lambda:InvokeFunction";

/// The `InvocationType` every row's request names, so each invocation is one Lambda
/// QUEUES — "Event – Invoke the function asynchronously" — which is where the bytes the
/// request carried are read back from.
const EVENT_INVOCATION: &str = "Event";

/// One row: the `Payload` the request writes, and the bytes the invocation must carry.
struct Row {
    case: &'static str,
    /// The text `Payload` is written with, or `None` for a request naming no payload
    /// member at all.
    payload: Option<&'static str>,
    /// The body the invoked operation must be handed, or `None` for a request the
    /// invoked operation itself refuses, which queues nothing.
    queued: Option<&'static str>,
}

/// A nested document: an array of mixed number types, a boolean pair, a null, an empty
/// object, a deeper object, non-ASCII text, an embedded quote and an integer past 32
/// bits.
const NESTED_PAYLOAD: &str = r#"{"marker":"nested","nested":{"list":[1,2.5,-3,true,false,null],"empty":{},"deep":{"k":["v"]}},"unicode":"café ✓ 日本","quote":"she said \"hi\"","big":12345678901234}"#;

/// The same document with the insignificant whitespace a client's own encoder left in
/// it: the body is the bytes as WRITTEN, so the spacing survives.
const SPACED_PAYLOAD: &str = "{ \"marker\" : \"spaced\" ,\n  \"n\" : 1 }";

const ROWS: &[Row] = &[
    Row {
        case: "a flat object, the shape a handler reads fields off",
        payload: Some(r#"{"marker":"flat","tenant":"b"}"#),
        queued: Some(r#"{"marker":"flat","tenant":"b"}"#),
    },
    Row {
        case: "a nested document of every JSON kind",
        payload: Some(NESTED_PAYLOAD),
        queued: Some(NESTED_PAYLOAD),
    },
    Row {
        case: "a payload whose text carries insignificant whitespace",
        payload: Some(SPACED_PAYLOAD),
        queued: Some(SPACED_PAYLOAD),
    },
    Row {
        case: "a top-level array",
        payload: Some(r#"[1,"two",{"three":3}]"#),
        queued: Some(r#"[1,"two",{"three":3}]"#),
    },
    Row {
        case: "a bare JSON string",
        payload: Some(r#""a payload that is one string""#),
        queued: Some(r#""a payload that is one string""#),
    },
    Row {
        case: "a bare JSON number",
        payload: Some("1234567890123"),
        queued: Some("1234567890123"),
    },
    Row {
        case: "the empty JSON object",
        payload: Some("{}"),
        queued: Some("{}"),
    },
    Row {
        // "The JSON that you want to provide to your Lambda function as input" is not
        // required: a request naming no payload invokes the function with no event, so
        // the body is empty rather than a value this emulator invented.
        case: "a request naming no payload member",
        payload: None,
        queued: Some(""),
    },
    Row {
        // Lambda refuses a payload that is not JSON with its modelled
        // `InvalidRequestContentException`, so the invocation is never queued. It is
        // only reachable because the bytes travel as written: the same text re-encoded
        // as a JSON string would be well-formed JSON and would be accepted.
        case: "a payload that is not JSON at all",
        payload: Some("a line of plain text, not JSON"),
        queued: None,
    },
];

/// Every row: the occurrence calls `lambda:invoke` with the request the `Input` wrote,
/// and the invocation Lambda holds carries the `Payload` member's own bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_universal_target_hands_the_payload_members_bytes_to_the_invoked_operation() {
    let suite = "the payload of a universal target's call";
    let (emulator, engine) = emulator_with_lambda();
    let function = "universal-target-handler";
    let function_arn = given_function(&emulator, suite, function).await;
    let role = given_invoking_role(
        &emulator,
        suite,
        "universal-invoke-role",
        &[(INVOKE_FUNCTION, &function_arn)],
    )
    .await;

    let occurrence = seconds_from_now(OCCURRENCE_IN);
    for (index, row) in ROWS.iter().enumerate() {
        let mut request = json!({
            "FunctionName": function_arn,
            "InvocationType": EVENT_INVOCATION,
        });
        if let Some(payload) = row.payload {
            request["Payload"] = json!(payload);
        }
        let name = format!("payload-{index}");
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/{name}"),
            Some(json!({
                "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
                "ScheduleExpressionTimezone": "UTC",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "State": "ENABLED",
                "Target": {
                    "Arn": INVOKE_TARGET,
                    "RoleArn": role,
                    "Input": request.to_string(),
                },
            })),
        )
        .await;
        assert_eq!(
            status,
            200,
            "case {:?}: CreateSchedule answered {status} {created}",
            row.case
        );
    }

    let expected: BTreeSet<String> = ROWS
        .iter()
        .filter_map(|row| row.queued.map(ToString::to_string))
        .collect();
    let held = invoked_payloads_once(&engine, function, expected.len()).await;
    for row in ROWS {
        match row.queued {
            Some(body) => assert!(
                held.contains(body),
                "case {:?}: expected the invocation to carry the payload's own bytes \
                 {body:?}, got {held:?}",
                row.case
            ),
            None => assert!(
                !held.iter().any(|carried| carried.contains("plain text")),
                "case {:?}: expected no invocation to have been queued for a payload the \
                 invoked operation refuses, got {held:?}",
                row.case
            ),
        }
    }
    assert_eq!(
        held, expected,
        "case {suite:?}: expected exactly the bytes each row's Payload wrote, got {held:?}"
    );

    // Nothing further arrives: no row's payload reaches the function re-encoded beside
    // the bytes it was written with, and the refused one never reaches it at all.
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        let again = invoked_payloads(&engine, function);
        assert_eq!(
            again, expected,
            "case {suite:?}: expected the function to hold exactly each row's own payload \
             bytes, got {again:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
