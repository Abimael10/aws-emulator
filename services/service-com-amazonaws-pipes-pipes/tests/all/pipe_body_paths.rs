// SPDX-License-Identifier: CC0-1.0

//! What a JSON PATH into a polled record selects, end to end over the wire: the paths a
//! pipe's `TargetParameters.InputTemplate` and its target parameters carry.
//!
//! Pipes and SQS are mounted on the SAME engine and every call goes through
//! `MultiServiceRouter::dispatch` — the one path a socket-served request takes — because
//! what a path selected is asserted where a client sees it: in the payload the target
//! queue received.
//!
//! The RULE asserted over the table below: a path a pipe resolves against a polled record
//! reads that record's message body in the format its producer sent it in — a body
//! spelling a JSON document is the document it parses to, so `$.body.<member>` selects
//! that member of it and the render carries the member's own value, while a body spelling
//! no document is the text it carries and a path INTO it selects nothing at all. The
//! reading is the filter's reading: one pipe cannot read a record two ways. What a stage
//! handed the event itself receives is unchanged — the record envelope with the body as
//! sent.
//!
//! The rows differ along what the rule generalises over: the format of the body (object,
//! list, plain text), how deep the path reaches into it, the type of the value it selects
//! (string, number, the whole document), where the path sits in the template (bare,
//! quoted, unquoted inside an object, beside a path that reads the envelope), and whether
//! a filter reads the same body the template renders from.
//!
//! Two rules about the RENDER itself are asserted over tables of their own: what a stage is
//! handed is the template's own text with every placeholder filled, byte for byte, each
//! value written as its place in the template asks; and the reserved `aws.pipes.` variables
//! of a template render the values the service fills — the pipe's own identity, the ARNs it
//! is configured with, the instant the stage received the event, and the event itself.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_data_plane::{
    collect_at_least, collect_during, emulator, given_queue, sqs, wait_for_counted,
    wait_for_visible, QUIET,
};
use crate::pipe_lifecycle::{call, describe, REGION};

/// The role every pipe below names. Nothing in this suite turns on its policies.
const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-body-paths";

/// How long a wait for work the pipe has to do is given before it is a failure. Well
/// above the data plane's poll interval and the span a pipe takes to reach RUNNING, so a
/// row fails only when the pipe did not do the work at all.
const BUDGET: Duration = Duration::from_secs(20);

/// The attributes that together say how many records a queue HOLDS: the ones waiting on it
/// and the ones a consumer has taken and not deleted.
const HELD_ATTRIBUTES: [&str; 2] = [
    "ApproximateNumberOfMessages",
    "ApproximateNumberOfMessagesNotVisible",
];

/// One message a row sends to its source queue, and what the pipe must deliver for it.
struct Sent {
    /// The `MessageBody` as the producer sends it.
    body: &'static str,
    /// The payload the target must receive for it, or `None` for a record the pipe's
    /// filter drops.
    delivered: Option<&'static str>,
}

/// One row of the path table.
struct Row {
    /// The case, which is also the pipe's and the queues' name.
    case: &'static str,
    /// `TargetParameters.InputTemplate`: what the target is handed instead of the record.
    template: &'static str,
    /// The patterns `FilterCriteria.Filters` carries, when the row filters at all.
    patterns: &'static [&'static str],
    /// What the row sends, in send order.
    messages: &'static [Sent],
}

/// RULE: a path a pipe resolves against a polled record reads the record's message body in
/// the format its producer sent it in, so a path into a body spelling a JSON document
/// selects the member it names in that document and the delivery carries that member's own
/// value; a body spelling no document is the text it carries.
#[tokio::test]
async fn a_path_into_a_record_body_selects_what_the_body_spells() {
    let rows = [
        Row {
            case: "a-scalar-member-of-a-json-body",
            template: "<$.body.id>",
            patterns: &[],
            messages: &[Sent {
                body: r#"{"type": "order", "id": "ord-7"}"#,
                delivered: Some("ord-7"),
            }],
        },
        Row {
            case: "a-number-member-of-a-json-body",
            template: "<$.body.count>",
            patterns: &[],
            messages: &[Sent {
                body: r#"{"count": 3}"#,
                delivered: Some("3"),
            }],
        },
        Row {
            case: "a-nested-member-of-a-json-body",
            template: "<$.body.order.id>",
            patterns: &[],
            messages: &[Sent {
                body: r#"{"order": {"id": "deep-1"}}"#,
                delivered: Some("deep-1"),
            }],
        },
        Row {
            case: "an-element-of-a-json-list-body",
            template: "<$.body[1].id>",
            patterns: &[],
            messages: &[Sent {
                body: r#"[{"id": "first"}, {"id": "second"}]"#,
                delivered: Some("second"),
            }],
        },
        Row {
            case: "the-whole-parsed-body",
            template: "<$.body>",
            patterns: &[],
            messages: &[Sent {
                body: r#"{"id": "whole-1"}"#,
                // The document the body spells, which is what a path into the body reads
                // — re-emitted as the JSON it is rather than as the text it travelled as.
                delivered: Some(r#"{"id": "whole-1"}"#),
            }],
        },
        Row {
            case: "a-body-path-quoted-by-the-template",
            template: "\"<$.body.id>\"",
            patterns: &[],
            messages: &[Sent {
                body: r#"{"id": "quoted-1"}"#,
                // A render is the template's own text with the placeholder filled, so the
                // quotes the template opened and closed with are delivered as written.
                delivered: Some("\"quoted-1\""),
            }],
        },
        Row {
            case: "body-paths-inside-an-object-template",
            template: r#"{"order": "<$.body.id>", "n": <$.body.count>, "from": "<$.eventSource>"}"#,
            patterns: &[],
            messages: &[Sent {
                body: r#"{"id": "obj-1", "count": 2}"#,
                delivered: Some(r#"{"order": "obj-1", "n": 2, "from": "aws:sqs"}"#),
            }],
        },
        Row {
            case: "a-plain-text-body-is-the-text-it-carries",
            template: "<$.body>",
            patterns: &[],
            messages: &[Sent {
                body: "a plain body, no document",
                delivered: Some("a plain body, no document"),
            }],
        },
        Row {
            // The two readings together: the filter admits on a member of the parsed body
            // and the template renders another member of that same document.
            case: "a-filter-and-a-template-over-one-body",
            template: "<$.body.id>",
            patterns: &[r#"{"body": {"type": ["match-me"]}}"#],
            messages: &[
                Sent {
                    body: r#"{"type": "no-match", "id": "dropped-1"}"#,
                    delivered: None,
                },
                Sent {
                    body: r#"{"type": "match-me", "id": "admitted-1"}"#,
                    delivered: Some("admitted-1"),
                },
            ],
        },
    ];

    for row in &rows {
        run_row(row).await;
    }
}

/// Keep the table honest: every row is driven by this one body.
async fn run_row(row: &Row) {
    let case = row.case;
    let router = emulator();
    let (source_url, source_arn) = given_queue(&router, &format!("{case}-src")).await;
    let (target_url, target_arn) = given_queue(&router, &format!("{case}-tgt")).await;

    let created = call(
        &router,
        "POST",
        &format!("/v1/pipes/{case}"),
        Some(json!({
            "Source": source_arn,
            "Target": target_arn,
            "RoleArn": ROLE,
            "DesiredState": "RUNNING",
            "SourceParameters": source_parameters(row.patterns),
            "TargetParameters": { "InputTemplate": row.template },
        })),
        REGION,
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreatePipe answered {} {}",
        created.status, created.body
    );

    for message in row.messages {
        send(&router, &source_url, case, message.body).await;
    }

    let wanted: BTreeSet<String> = row
        .messages
        .iter()
        .filter_map(|message| message.delivered)
        .map(canonical)
        .collect();

    // Wait for everything the pipe must deliver, then keep watching: a record it must
    // DROP would arrive late, so the quiet window is what makes its absence an answer
    // rather than a race.
    let mut delivered = collect_at_least(&router, &target_url, wanted.len(), BUDGET).await;
    delivered.extend(collect_during(&router, &target_url, QUIET).await);
    let seen: BTreeSet<String> = delivered.iter().map(|body| canonical(body)).collect();
    assert_eq!(
        seen, wanted,
        "case {case:?}: expected the target to receive exactly {wanted:?}, got {delivered:?}"
    );
    assert_eq!(
        delivered.len(),
        wanted.len(),
        "case {case:?}: expected {} delivery(ies), got {delivered:?}",
        wanted.len()
    );

    // Every record the pipe decided about is consumed: a delivered one is deleted and a
    // dropped one is too, so nothing is left waiting on the source.
    let left = wait_for_visible(&router, &source_url, 0, BUDGET).await;
    assert_eq!(
        left, 0,
        "case {case:?}: expected every polled record consumed, {left} are still visible \
         on the source"
    );
}

/// RULE: the reading is the PATHS' alone — a stage handed the event itself receives the
/// record envelope with the message body as the producer sent it, whatever that text
/// spells — and a path that names no one value of the event stands for the EMPTY STRING:
/// the record is delivered with that placeholder filled by nothing, never with the path
/// itself, and it is consumed like any other.
///
/// The rows differ along what the rule generalises over: whether the pipe renders a
/// template at all, the format of the body (object, list, plain text), why the path
/// resolves to nothing (a member the document does not carry, a path into a body that
/// spells no document, a path under a value that is no document) and where the
/// unresolved placeholder stands (inside a string literal, as a JSON value, in a template
/// that is not JSON text).
#[tokio::test]
async fn the_payload_keeps_the_body_as_sent_and_an_unresolvable_path_renders_empty() {
    // (case, the template the pipe carries or none at all, the body sent, the bytes the
    // target must receive — asserted for a row that renders a template; a row with none
    // is asserted on the envelope it is handed)
    let rows: Vec<(&str, Option<&str>, &str, &str)> = vec![
        ("no-template-json-body", None, r#"{"id": "as-sent-1"}"#, ""),
        ("no-template-json-list-body", None, "[1, 2, 3]", ""),
        ("no-template-text-body", None, "as-sent-text", ""),
        (
            "a-path-into-a-text-body",
            Some(r#"{"id": "<$.body.id>", "from": "<$.eventSource>"}"#),
            "no document here",
            r#"{"id": "", "from": "aws:sqs"}"#,
        ),
        (
            "a-path-to-an-absent-member",
            Some(r#"{"id": "<$.body.missing>", "present": "<$.body.id>"}"#),
            r#"{"id": "present"}"#,
            r#"{"id": "", "present": "present"}"#,
        ),
        (
            // Standing where a JSON value stands, the empty string is the value the
            // render writes, so the template's own JSON is still JSON.
            "an-absent-path-standing-as-a-json-value",
            Some(r#"{"v": <$.body.missing>, "id": "<$.body.id>"}"#),
            r#"{"id": "value-1"}"#,
            r#"{"v": "", "id": "value-1"}"#,
        ),
        (
            "an-absent-path-under-a-member-that-is-no-document",
            Some(r#"{"deep": "<$.body.id.deeper>", "id": "<$.body.id>"}"#),
            r#"{"id": "deep-1"}"#,
            r#"{"deep": "", "id": "deep-1"}"#,
        ),
        (
            // Not JSON text: the characters the template wrote, with nothing where the
            // unresolved path stood.
            "an-absent-path-in-a-text-template",
            Some("missing=<$.body.missing>;id=<$.body.id>"),
            r#"{"id": "text-1"}"#,
            "missing=;id=text-1",
        ),
    ];

    for (case, template, body, expected) in rows {
        let router = emulator();
        let (source_url, source_arn) = given_queue(&router, &format!("{case}-src")).await;
        let (target_url, target_arn) = given_queue(&router, &format!("{case}-tgt")).await;
        let mut request = json!({
            "Source": source_arn,
            "Target": target_arn,
            "RoleArn": ROLE,
            "DesiredState": "RUNNING",
            "SourceParameters": source_parameters(&[]),
        });
        if let Some(template) = template {
            request["TargetParameters"] = json!({ "InputTemplate": template });
        }
        let created = call(
            &router,
            "POST",
            &format!("/v1/pipes/{case}"),
            Some(request),
            REGION,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        send(&router, &source_url, case, body).await;

        let delivered = collect_at_least(&router, &target_url, 1, BUDGET).await;
        let first = delivered
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: nothing was delivered"));
        match template {
            // No template: the envelope itself, carrying the body as the producer sent it
            // — the reading the paths take is theirs alone.
            None => {
                let envelope: Value = serde_json::from_str(first).unwrap_or_else(|error| {
                    panic!("case {case:?}: a delivered body is not JSON ({error}): {first:?}")
                });
                assert_eq!(
                    envelope["body"],
                    json!(body),
                    "case {case:?}: expected the delivered record to carry the body as \
                     sent, got {envelope}"
                );
            }
            // A path that names no one value of the event: the render carries the empty
            // string for it and the rest of the template as written.
            Some(_) => {
                assert_eq!(
                    canonical(first),
                    canonical(expected),
                    "case {case:?}: expected the target to receive {expected:?}, \
                     got {first:?}"
                );
            }
        }
        // Either way the record the pipe decided about is consumed: an unresolved path is
        // a routine condition and not a record left behind. Waited for, because the delete
        // follows the delivery this row already saw.
        let held = wait_for_counted(&router, &source_url, &HELD_ATTRIBUTES, 0, BUDGET).await;
        assert_eq!(
            held, 0,
            "case {case:?}: expected the delivered record consumed, the source holds {held}"
        );
    }
}

/// RULE: the documented whole-event escape hatch renders the event as the JSON document it
/// is, beside an unresolved path that renders the empty string in the same template — so a
/// template that reads a member the event does not carry still delivers the event.
#[tokio::test]
async fn an_unresolved_path_and_the_whole_event_are_delivered_together() {
    let case = "unresolved-path-and-whole-event";
    let router = emulator();
    let (source_url, source_arn) = given_queue(&router, &format!("{case}-src")).await;
    let (target_url, target_arn) = given_queue(&router, &format!("{case}-tgt")).await;
    let template = concat!(
        r#"{"missing": "<$.body.missing>", "marker": "<$.body.marker>", "#,
        r#""event": <aws.pipes.event.json>}"#,
    );
    let created = call(
        &router,
        "POST",
        &format!("/v1/pipes/{case}"),
        Some(json!({
            "Source": source_arn,
            "Target": target_arn,
            "RoleArn": ROLE,
            "DesiredState": "RUNNING",
            "SourceParameters": source_parameters(&[]),
            "TargetParameters": { "InputTemplate": template },
        })),
        REGION,
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreatePipe answered {} {}",
        created.status, created.body
    );

    let body = r#"{"marker": "carried-1"}"#;
    send(&router, &source_url, case, body).await;
    let delivered = collect_at_least(&router, &target_url, 1, BUDGET).await;
    let first = delivered
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: nothing was delivered"));
    let rendered: Value = serde_json::from_str(first).unwrap_or_else(|error| {
        panic!("case {case:?}: the delivered payload is not JSON ({error}): {first:?}")
    });
    assert_eq!(
        rendered["missing"],
        json!(""),
        "case {case:?}: expected the unresolved path to render the empty string, \
         got {rendered}"
    );
    assert_eq!(
        rendered["marker"],
        json!("carried-1"),
        "case {case:?}: expected the resolved path to render its value, got {rendered}"
    );
    assert_eq!(
        rendered["event"]["body"],
        json!({ "marker": "carried-1" }),
        "case {case:?}: expected the whole event rendered as JSON, got {rendered}"
    );
}

/// RULE: what a stage is handed for one event is the template's own text with every
/// placeholder filled, byte for byte, and HOW a value is written follows the form of the
/// template: in JSON text a placeholder standing as a value carries the value's JSON — so a
/// string is quoted by the render itself — one standing inside a string literal carries a
/// string's characters escaped for that literal and any other value's own JSON, and a
/// template that is not JSON text carries every value's characters unescaped.
///
/// The rows differ along what the rule generalises over: where the placeholder stands (as a
/// JSON value, inside a string literal, in a template that is not JSON), the type of the
/// value it carries (string, string holding a quote, object, number) and the spacing the
/// template's author wrote.
#[tokio::test]
async fn an_input_template_delivers_its_own_bytes_with_every_value_written_as_its_place_asks() {
    // The one body every row sends, holding a value of each type a placeholder can carry.
    const BODY: &str = r#"{"text": "hello", "object": {"key": "value"}, "quoted": "she said \"hi\" today", "count": 3}"#;

    // (case, the template, the BYTES the target must receive)
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "an-unquoted-string-is-auto-quoted",
            r#"{"v": <$.body.text>}"#,
            r#"{"v": "hello"}"#,
        ),
        (
            // Inlined inside a string literal, an object keeps its own JSON quotes: the
            // literal the template opened is broken rather than the object escaped.
            "an-object-inlined-in-a-string-literal",
            r#"{"v": "x <$.body.object>"}"#,
            r#"{"v": "x {"key":"value"}"}"#,
        ),
        (
            "a-quoted-string-is-escaped-quote-and-all",
            r#"{"v": "<$.body.quoted>"}"#,
            r#"{"v": "she said \"hi\" today"}"#,
        ),
        (
            "an-unquoted-number",
            r#"{"n": <$.body.count>}"#,
            r#"{"n": 3}"#,
        ),
        (
            // Every form at once, with the author's own spacing.
            "the-three-forms-in-one-template",
            r#"{"unquoted_string": <$.body.text>, "obj_in_string": "x <$.body.object>", "quoted_with_embedded_quote": "<$.body.quoted>"}"#,
            r#"{"unquoted_string": "hello", "obj_in_string": "x {"key":"value"}", "quoted_with_embedded_quote": "she said \"hi\" today"}"#,
        ),
        (
            // Not JSON text: the characters, nothing escaped and nothing quoted.
            "a-template-that-is-not-json-text",
            "order <$.body.text> says <$.body.quoted>",
            "order hello says she said \"hi\" today",
        ),
    ];

    for (case, template, expected) in rows {
        let router = emulator();
        let (source_url, source_arn) = given_queue(&router, &format!("{case}-src")).await;
        let (target_url, target_arn) = given_queue(&router, &format!("{case}-tgt")).await;
        let created = call(
            &router,
            "POST",
            &format!("/v1/pipes/{case}"),
            Some(json!({
                "Source": source_arn,
                "Target": target_arn,
                "RoleArn": ROLE,
                "DesiredState": "RUNNING",
                "SourceParameters": source_parameters(&[]),
                "TargetParameters": { "InputTemplate": template },
            })),
            REGION,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        send(&router, &source_url, case, BODY).await;

        let delivered = collect_at_least(&router, &target_url, 1, BUDGET).await;
        let first = delivered
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: nothing was delivered"));
        assert_eq!(
            first, expected,
            "case {case:?}: expected the target to receive {expected:?}, got {first:?}"
        );
    }
}

/// RULE: the reserved `aws.pipes.*` variables of an `InputTemplate` render the values the
/// service fills — the pipe's own ARN and name, the ARNs of the source, enrichment and
/// target it is configured with, the instant the stage received the event and the event
/// itself — so a delivered payload carries those values and never the placeholder text.
///
/// The pipe's own ARN and name are read back from `DescribePipe`, so the render is compared
/// against what the control plane reports rather than against text this test wrote. The
/// ingestion time is the one value the CLOCK fills, so it is asserted on its form: an
/// instant a consumer can read back.
#[tokio::test]
async fn the_reserved_pipe_variables_render_the_pipes_own_identity() {
    let case = "reserved-pipe-variables";
    let router = emulator();
    let (source_url, source_arn) = given_queue(&router, &format!("{case}-src")).await;
    let (target_url, target_arn) = given_queue(&router, &format!("{case}-tgt")).await;

    // Every reserved variable the service fills, each as one member of a JSON template.
    let template = concat!(
        r#"{"pipeArn": "<aws.pipes.pipe-arn>", "pipeName": "<aws.pipes.pipe-name>", "#,
        r#""sourceArn": "<aws.pipes.source-arn>", "targetArn": "<aws.pipes.target-arn>", "#,
        r#""enrichmentArn": "<aws.pipes.enrichment-arn>", "#,
        r#""ingestedAt": "<aws.pipes.event.ingestion-time>", "#,
        r#""event": <aws.pipes.event.json>}"#,
    );
    let created = call(
        &router,
        "POST",
        &format!("/v1/pipes/{case}"),
        Some(json!({
            "Source": source_arn,
            "Target": target_arn,
            "RoleArn": ROLE,
            "DesiredState": "RUNNING",
            "SourceParameters": source_parameters(&[]),
            "TargetParameters": { "InputTemplate": template },
        })),
        REGION,
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreatePipe answered {} {}",
        created.status, created.body
    );
    let described = describe(&router, case).await;
    let pipe_arn = described.string("Arn");
    let pipe_name = described.string("Name");

    send(&router, &source_url, case, r#"{"marker": "reserved-1"}"#).await;
    let delivered = collect_at_least(&router, &target_url, 1, BUDGET).await;
    let first = delivered
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: nothing was delivered"));
    let rendered: Value = serde_json::from_str(first).unwrap_or_else(|error| {
        panic!("case {case:?}: the delivered payload is not JSON ({error}): {first:?}")
    });

    // (the member, what the variable renders) for every value the pipe's own
    // configuration fills.
    let expected: Vec<(&str, String)> = vec![
        ("pipeArn", pipe_arn.clone()),
        ("pipeName", pipe_name.clone()),
        ("sourceArn", source_arn.clone()),
        ("targetArn", target_arn.clone()),
        // This pipe configures no enrichment, so the variable stands for no resource.
        ("enrichmentArn", String::new()),
    ];
    for (member, value) in expected {
        assert_eq!(
            rendered[member].as_str(),
            Some(value.as_str()),
            "case {case:?}: expected {member} to render {value:?}, got {rendered}"
        );
    }

    let ingested = rendered["ingestedAt"].as_str().unwrap_or_default();
    assert!(
        aws_smithy_types::DateTime::from_str(
            ingested,
            aws_smithy_types::date_time::Format::DateTime
        )
        .is_ok(),
        "case {case:?}: expected an instant a consumer can read, got {ingested:?}"
    );
    // The event as the document it is: the record envelope, body decoded as a reader of
    // the record reads it.
    assert_eq!(
        rendered["event"]["body"],
        json!({ "marker": "reserved-1" }),
        "case {case:?}: expected the event variable to carry the record, got {rendered}"
    );
    assert_eq!(
        rendered["event"]["eventSourceARN"].as_str(),
        Some(source_arn.as_str()),
        "case {case:?}: expected the event variable to carry the record envelope, got \
         {rendered}"
    );
}

/// The `SourceParameters` a pipe is created with: one record per batch, and the patterns
/// `FilterCriteria.Filters` carries when the row filters at all.
fn source_parameters(patterns: &[&str]) -> Value {
    let mut parameters = json!({ "SqsQueueParameters": { "BatchSize": 1 } });
    if !patterns.is_empty() {
        let filters: Vec<Value> = patterns
            .iter()
            .map(|pattern| json!({ "Pattern": pattern }))
            .collect();
        parameters["FilterCriteria"] = json!({ "Filters": filters });
    }
    parameters
}

/// Send one body to the queue at `url`.
async fn send(router: &MultiServiceRouter, url: &str, case: &str, body: &str) {
    let (status, answer) = sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SendMessage of {body:?} answered {status} {answer}"
    );
}

/// One payload in a form two of them can be compared by: the JSON it spells, with every
/// object's members in one order, or the text it is when it spells no JSON. A render is
/// asserted on the VALUE it carries and not on the byte order a serializer chose for it.
///
/// `pub(crate)` so the suite that asserts the same reading of a Kinesis record's payload
/// (`tests/all/pipe_kinesis_stream_source.rs`) compares a render the same way rather than
/// copying it.
pub(crate) fn canonical(payload: &str) -> String {
    match serde_json::from_str::<Value>(payload) {
        Ok(value) => ordered(&value).to_string(),
        Err(_) => payload.to_string(),
    }
}

/// `value` with the members of every object it holds in name order.
fn ordered(value: &Value) -> Value {
    match value {
        Value::Object(members) => {
            let mut names: Vec<&String> = members.keys().collect();
            names.sort();
            let mut rebuilt = serde_json::Map::new();
            for name in names {
                let nested = members.get(name).unwrap_or(&Value::Null);
                rebuilt.insert(name.clone(), ordered(nested));
            }
            Value::Object(rebuilt)
        }
        Value::Array(items) => Value::Array(items.iter().map(ordered).collect()),
        other => other.clone(),
    }
}
