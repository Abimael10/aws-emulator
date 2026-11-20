// SPDX-License-Identifier: CC0-1.0

//! The rules a pipe's configuration must satisfy ACROSS the resources it names, end to end
//! over the wire.
//!
//! Two rules no per-member constraint can express, because each spans two members — and
//! both are enforced by the two writes that can configure a pipe (`CreatePipe` and
//! `UpdatePipe`), against the configuration the pipe would END UP with:
//!
//! * the batch size the SOURCE reads is bounded by what the TARGET takes in one delivery:
//!   the source's own `@range` goes to 10,000, while a target whose batch API accepts
//!   fewer entries per call caps it there (Amazon SQS `SendMessageBatch` and EventBridge
//!   `PutEvents` 10, Kinesis `PutRecords` and Amazon Data Firehose `PutRecordBatch` 500,
//!   CloudWatch Logs `PutLogEvents` 10,000), and a target that takes one call per batch
//!   (Lambda, Step Functions, …) caps it not at all;
//! * a filter pattern is an EventBridge event pattern, so text that is not a JSON object is
//!   not a pattern at all and is refused with the parser's own diagnostic for why.
//!
//! Beside them, the rule a single member carries on its own and no `@length` or `@pattern`
//! can express: an `InputTemplate` is the valid JSON text its member is documented to be, so
//! a template that is JSON text and spells no JSON document — with each of its placeholders
//! standing for the value it names — is refused at authoring time rather than discovered by
//! a delivery.
//!
//! The third such rule — a target parameter the TARGET RESOURCE requires that the
//! configuration does not supply, which is how a FIFO queue target comes to need its message
//! group — has its own suite in `tests/all/pipe_target_parameter_refusal.rs`, which reaches
//! [`assert_refused`] below rather than copying it.
//!
//! Each table's rows span what the rule generalises over (the target service, the batch
//! size relative to the cap, which source carries it, the form of the pattern), and every
//! refused call is checked to have persisted nothing. The harness is the one
//! `pipe_lifecycle` and `pipe_update` already own.

use serde_json::{json, Value};

use crate::pipe_lifecycle::{
    create, describe, list, listed_names, minimal_body, router, Reply, PAST_SETTLE,
};
use crate::pipe_refusal_bodies::{field_list, validation_error_fields};
use crate::pipe_update::{create_body, update};

const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-role";
const SOURCE_SQS: &str = "arn:aws:sqs:us-west-2:000000000000:orders";
const SOURCE_KINESIS: &str = "arn:aws:kinesis:us-west-2:000000000000:stream/events";
const TARGET_LAMBDA: &str = "arn:aws:lambda:us-west-2:000000000000:function:handler";
const TARGET_SQS: &str = "arn:aws:sqs:us-west-2:000000000000:results";
const TARGET_EVENT_BUS: &str = "arn:aws:events:us-west-2:000000000000:event-bus/default";
const TARGET_KINESIS: &str = "arn:aws:kinesis:us-west-2:000000000000:stream/results";
const TARGET_FIREHOSE: &str = "arn:aws:firehose:us-west-2:000000000000:deliverystream/records";
const TARGET_LOG_GROUP: &str = "arn:aws:logs:us-west-2:000000000000:log-group:/aws/pipes/demo";
const SOURCE_DYNAMODB: &str =
    "arn:aws:dynamodb:us-west-2:000000000000:table/orders/stream/2026-01-01T00:00:00.000";

/// The `FilterCriteria` a read reports, which is what the pattern rule below is about: the
/// reported `SourceParameters` also carry the batch size the pipe polls its source with,
/// whose own rule and table live in `tests/all/pipe_reported_configuration.rs`.
pub(crate) fn reported_filter_criteria(described: &Reply) -> Value {
    described
        .member("SourceParameters")
        .get("FilterCriteria")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Assert that `reply` is the modelled refusal naming `name` with `message`, in the
/// exception's `ValidationErrorFields` document AND in the modelled `fieldList` — the two
/// renderings a client may read, built from the one list. The document itself is spelled out
/// by the shared helper in `tests/all/pipe_refusal_bodies.rs`, independently of the handler.
///
/// `pub(crate)` so the suite for the third cross-resource rule asserts a refusal the SAME
/// way rather than copying this.
pub(crate) fn assert_refused(case: &str, reply: &Reply, name: &str, message: &str) {
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {reply:?}"
    );
    assert_eq!(
        reply.message(),
        validation_error_fields(&[(name, message)]),
        "case {case:?}: expected the ValidationErrorFields document naming {name}, \
         got {reply:?}"
    );
    assert_eq!(
        field_list(reply),
        vec![(name.to_string(), message.to_string())],
        "case {case:?}: the fieldList must carry the same entry as the message, \
         got {reply:?}"
    );
}

/// One row of the batch-size table.
struct Batch {
    case: &'static str,
    source: &'static str,
    /// The `SourceParameters` the pipe is created with, carrying the batch size.
    source_parameters: Value,
    target: &'static str,
    /// The batch size those parameters carry, and the cap the target imposes — `None` when
    /// the target imposes none, which is what makes the row accepted.
    batch_size: i64,
    cap: Option<i64>,
    /// The target's own namespace, as the refusal names it.
    target_type: &'static str,
}

/// RULE: a pipe's batch size may not exceed the largest batch its TARGET accepts per
/// delivery; past it the write is refused with `ValidationException` (400) reporting
/// `BatchSize`, the configured size and the target's maximum, and stores nothing.
///
/// The rows differ along what that rule generalises over: the target service and so the
/// cap (10 for a queue and for an event bus, 500 for a stream and for a delivery stream,
/// 10,000 for a log group, none for a function), where the batch size sits relative to it
/// (at it, one past it, far past it), and which source's parameters carry the batch size.
///
/// The caps are the per-call entry limits of the target services' own batch APIs
/// (`state::target_batch_size_cap`): Amazon SQS `SendMessageBatch` and EventBridge
/// `PutEvents` 10 entries, Kinesis `PutRecords` and Amazon Data Firehose `PutRecordBatch`
/// 500 records, CloudWatch Logs `PutLogEvents` 10,000 events.
#[tokio::test]
async fn a_batch_size_past_what_the_target_takes_is_refused() {
    let rows = vec![
        Batch {
            case: "sqs-target-at-the-cap",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 10}}),
            target: TARGET_SQS,
            batch_size: 10,
            cap: None,
            target_type: "sqs",
        },
        Batch {
            case: "sqs-target-one-past-the-cap",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 11}}),
            target: TARGET_SQS,
            batch_size: 11,
            cap: Some(10),
            target_type: "sqs",
        },
        Batch {
            case: "sqs-target-far-past-the-cap",
            source: SOURCE_SQS,
            source_parameters: json!({
                "SqsQueueParameters": {"BatchSize": 50, "MaximumBatchingWindowInSeconds": 5},
            }),
            target: TARGET_SQS,
            batch_size: 50,
            cap: Some(10),
            target_type: "sqs",
        },
        Batch {
            case: "event-bus-target-past-the-cap",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 11}}),
            target: TARGET_EVENT_BUS,
            batch_size: 11,
            cap: Some(10),
            target_type: "events",
        },
        Batch {
            case: "stream-target-within-its-larger-cap",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 500}}),
            target: TARGET_KINESIS,
            batch_size: 500,
            cap: None,
            target_type: "kinesis",
        },
        Batch {
            case: "stream-target-past-its-cap",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 501}}),
            target: TARGET_KINESIS,
            batch_size: 501,
            cap: Some(500),
            target_type: "kinesis",
        },
        Batch {
            // A target that takes one call per batch caps nothing, so the source's own
            // modelled maximum is the only bound.
            case: "function-target-caps-nothing",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 10_000}}),
            target: TARGET_LAMBDA,
            batch_size: 10_000,
            cap: None,
            target_type: "lambda",
        },
        Batch {
            case: "delivery-stream-target-at-the-cap",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 500}}),
            target: TARGET_FIREHOSE,
            batch_size: 500,
            cap: None,
            target_type: "firehose",
        },
        Batch {
            case: "delivery-stream-target-one-past-the-cap",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 501}}),
            target: TARGET_FIREHOSE,
            batch_size: 501,
            cap: Some(500),
            target_type: "firehose",
        },
        Batch {
            // The cap belongs to the TARGET, so the stream the records come off does not
            // change it — and the source member's own maximum is still refused by it.
            case: "stream-source-far-past-the-delivery-stream-targets-cap",
            source: SOURCE_DYNAMODB,
            source_parameters: json!({
                "DynamoDBStreamParameters": {"StartingPosition": "LATEST", "BatchSize": 10_000},
            }),
            target: TARGET_FIREHOSE,
            batch_size: 10_000,
            cap: Some(500),
            target_type: "firehose",
        },
        Batch {
            case: "stream-source-one-past-the-event-bus-targets-cap",
            source: SOURCE_DYNAMODB,
            source_parameters: json!({
                "DynamoDBStreamParameters": {"StartingPosition": "LATEST", "BatchSize": 11},
            }),
            target: TARGET_EVENT_BUS,
            batch_size: 11,
            cap: Some(10),
            target_type: "events",
        },
        Batch {
            case: "stream-source-within-the-delivery-stream-targets-cap",
            source: SOURCE_DYNAMODB,
            source_parameters: json!({
                "DynamoDBStreamParameters": {"StartingPosition": "LATEST", "BatchSize": 500},
            }),
            target: TARGET_FIREHOSE,
            batch_size: 500,
            cap: None,
            target_type: "firehose",
        },
        Batch {
            // The widest cap in the catalog is the source member's own maximum, so a
            // log-group target refuses nothing the member admits.
            case: "log-group-target-at-the-cap",
            source: SOURCE_SQS,
            source_parameters: json!({"SqsQueueParameters": {"BatchSize": 10_000}}),
            target: TARGET_LOG_GROUP,
            batch_size: 10_000,
            cap: None,
            target_type: "logs",
        },
        Batch {
            // The batch size is the same member on every source's parameters, so the rule
            // does not care which source carries it.
            case: "stream-source-past-the-queue-targets-cap",
            source: SOURCE_KINESIS,
            source_parameters: json!({
                "KinesisStreamParameters": {"StartingPosition": "LATEST", "BatchSize": 25},
            }),
            target: TARGET_SQS,
            batch_size: 25,
            cap: Some(10),
            target_type: "sqs",
        },
    ];

    for row in rows {
        let router = router();
        let created = create(
            &router,
            row.case,
            create_body(
                row.source,
                row.target,
                json!({"SourceParameters": row.source_parameters}),
            ),
        )
        .await;

        let Some(cap) = row.cap else {
            assert_eq!(
                created.status,
                200,
                "case {:?}: a batch size within the target's cap must be accepted, \
                 got {created:?}",
                row.case
            );
            let described = describe(&router, row.case).await;
            assert_eq!(
                described.member("SourceParameters"),
                &row.source_parameters,
                "case {:?}: the accepted parameters must read back, got {described:?}",
                row.case
            );
            continue;
        };

        assert_refused(
            row.case,
            &created,
            "BatchSize",
            &format!(
                "The configured batch size {} is greater than the max supported batch \
                 size {cap} for target {}.",
                row.batch_size, row.target_type
            ),
        );
        let described = describe(&router, row.case).await;
        assert_eq!(
            described.status,
            404,
            "case {:?}: a refused CreatePipe must have stored no pipe, got {described:?}",
            row.case
        );
        let listed = list(&router, "").await;
        assert_eq!(
            listed_names(&listed),
            Vec::<String>::new(),
            "case {:?}: a refused CreatePipe must leave the pipe out of ListPipes, \
             got {listed:?}",
            row.case
        );
    }
}

/// RULE: the same bound is checked against what an UPDATE would leave the pipe with, so it
/// holds for a request that changes only the batch size AND for one that changes only the
/// TARGET under an already-stored batch size — and a refused update changes nothing.
#[tokio::test]
async fn an_update_into_a_batch_size_the_target_cannot_take_is_refused() {
    // (the case and pipe name, the target the pipe is created with, the SourceParameters it
    // is created with, the update body, the cap the refusal must state, the batch size it
    // must state, the target service the refusal must name).
    let rows: Vec<(&str, &str, Value, Value, i64, i64, &str)> = vec![
        (
            "batchupdate",
            TARGET_SQS,
            json!({"SqsQueueParameters": {"BatchSize": 5}}),
            json!({
                "RoleArn": ROLE,
                "SourceParameters": {"SqsQueueParameters": {"BatchSize": 40}},
            }),
            10,
            40,
            "sqs",
        ),
        (
            "targetupdate",
            TARGET_LAMBDA,
            json!({"SqsQueueParameters": {"BatchSize": 40}}),
            json!({"RoleArn": ROLE, "Target": TARGET_SQS}),
            10,
            40,
            "sqs",
        ),
        (
            // The same two shapes of update against the widest cap in the catalog: the
            // rule is the target's limit, whichever target service imposes it.
            "deliverystreambatchupdate",
            TARGET_FIREHOSE,
            json!({"SqsQueueParameters": {"BatchSize": 500}}),
            json!({
                "RoleArn": ROLE,
                "SourceParameters": {"SqsQueueParameters": {"BatchSize": 501}},
            }),
            500,
            501,
            "firehose",
        ),
        (
            "deliverystreamtargetupdate",
            TARGET_LAMBDA,
            json!({"SqsQueueParameters": {"BatchSize": 9_000}}),
            json!({"RoleArn": ROLE, "Target": TARGET_FIREHOSE}),
            500,
            9_000,
            "firehose",
        ),
    ];

    for (case, target, created_parameters, update_body, cap, batch_size, target_type) in rows {
        let router = router();
        let created = create(
            &router,
            case,
            create_body(
                SOURCE_SQS,
                target,
                json!({"SourceParameters": created_parameters}),
            ),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the fixture pipe must be created, got {created:?}"
        );
        tokio::time::sleep(PAST_SETTLE).await;

        let refused = update(&router, case, update_body).await;
        assert_refused(
            case,
            &refused,
            "BatchSize",
            &format!(
                "The configured batch size {batch_size} is greater than the max supported \
                 batch size {cap} for target {target_type}."
            ),
        );

        let described = describe(&router, case).await;
        assert_eq!(
            described.member("SourceParameters"),
            &created_parameters,
            "case {case:?}: a refused update must leave the stored SourceParameters as they \
             were, got {described:?}"
        );
        assert_eq!(
            described.string("Target"),
            target,
            "case {case:?}: a refused update must leave the stored Target as it was, \
             got {described:?}"
        );
    }
}

/// RULE: a filter pattern is an EventBridge event pattern — a JSON object matched against
/// the event — so a pattern that is not one is refused with `ValidationException` (400)
/// reporting `Pattern` and the parser's own diagnostic for the class of failure, with the
/// offending character taken from the request, and nothing is stored; a pattern that IS an
/// object is accepted and read back verbatim, however nested.
///
/// The rows differ along the form of the pattern (object, nested object, non-JSON text, a
/// JSON scalar, a JSON array, a text that ends inside its object and one that ends inside
/// an array), how many filters the criteria carries and which of them is malformed, and the
/// empty cases the model admits (no pattern, an empty `Filters` array).
#[tokio::test]
async fn a_filter_pattern_that_is_not_an_event_pattern_is_refused() {
    // (the case and pipe name, the FilterCriteria, the reason the refusal must state —
    // `None` when the criteria are accepted).
    let unexpected = |character: char| {
        format!(
            "Unexpected character ('{character}' (code {})): was expecting double-quote to \
             start field name",
            u32::from(character)
        )
    };
    let rows: Vec<(&str, Value, Option<String>)> = vec![
        (
            "objectpattern",
            json!({"Filters": [{"Pattern": "{\"source\":[\"demo\"]}"}]}),
            None,
        ),
        (
            "nestedobjectpattern",
            json!({
                "Filters": [{
                    "Pattern": "{\"detail\":{\"state\":[{\"anything-but\":\"closed\"}]}}",
                }],
            }),
            None,
        ),
        ("nofilters", json!({"Filters": []}), None),
        ("emptypattern", json!({"Filters": [{"Pattern": ""}]}), None),
        (
            "nonjsonpattern",
            json!({"Filters": [{"Pattern": "not-json"}]}),
            Some(unexpected('n')),
        ),
        (
            "unquotedfieldname",
            json!({"Filters": [{"Pattern": "{source:[\"demo\"]}"}]}),
            Some(unexpected('s')),
        ),
        (
            "unterminatedarray",
            json!({"Filters": [{"Pattern": "{\"source\":[\"demo\""}]}),
            Some("Unexpected end-of-input: expected close marker for Array".to_string()),
        ),
        (
            "unterminatedobject",
            json!({"Filters": [{"Pattern": "{\"source\":[\"demo\"]"}]}),
            Some("Unexpected end-of-input: expected close marker for Object".to_string()),
        ),
        (
            "jsonscalarpattern",
            json!({"Filters": [{"Pattern": "\"demo\""}]}),
            Some("Filter is not an object".to_string()),
        ),
        (
            "jsonarraypattern",
            json!({"Filters": [{"Pattern": "[{\"source\":[\"demo\"]}]"}]}),
            Some("Filter is not an object".to_string()),
        ),
        (
            "secondfiltermalformed",
            json!({
                "Filters": [
                    {"Pattern": "{\"source\":[\"demo\"]}"},
                    {"Pattern": "still-not-json"},
                ],
            }),
            Some(unexpected('s')),
        ),
    ];

    for (case, criteria, reason) in rows {
        let router = router();
        let source_parameters = json!({"FilterCriteria": criteria});
        let created = create(
            &router,
            case,
            create_body(
                SOURCE_SQS,
                TARGET_LAMBDA,
                json!({"SourceParameters": source_parameters}),
            ),
        )
        .await;

        let Some(reason) = reason else {
            assert_eq!(
                created.status, 200,
                "case {case:?}: an event pattern must be accepted, got {created:?}"
            );
            let described = describe(&router, case).await;
            assert_eq!(
                reported_filter_criteria(&described),
                criteria,
                "case {case:?}: the accepted criteria must read back verbatim, \
                 got {described:?}"
            );
            continue;
        };

        assert_refused(
            case,
            &created,
            "Pattern",
            &format!("Invalid Event Pattern. Reason: {reason}."),
        );
        let described = describe(&router, case).await;
        assert_eq!(
            described.status, 404,
            "case {case:?}: a refused CreatePipe must have stored no pipe, got {described:?}"
        );
    }
}

/// RULE: the pattern rule holds for `UpdatePipe` too — an update carrying a malformed
/// pattern is refused and leaves the stored criteria as they were, while one carrying an
/// event pattern replaces them.
#[tokio::test]
async fn an_update_carrying_a_malformed_filter_pattern_changes_nothing() {
    let router = router();
    let stored = json!({"FilterCriteria": {"Filters": [{"Pattern": "{\"source\":[\"a\"]}"}]}});
    let created = create(
        &router,
        "filterupdate",
        create_body(
            SOURCE_SQS,
            TARGET_LAMBDA,
            json!({"SourceParameters": stored}),
        ),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the fixture pipe must be created, got {created:?}"
    );
    tokio::time::sleep(PAST_SETTLE).await;

    let refused = update(
        &router,
        "filterupdate",
        json!({
            "RoleArn": ROLE,
            "SourceParameters": {"FilterCriteria": {"Filters": [{"Pattern": "nope"}]}},
        }),
    )
    .await;
    assert_refused(
        "malformed-update",
        &refused,
        "Pattern",
        "Invalid Event Pattern. Reason: Unexpected character ('n' (code 110)): was \
         expecting double-quote to start field name.",
    );
    let described = describe(&router, "filterupdate").await;
    assert_eq!(
        reported_filter_criteria(&described),
        stored["FilterCriteria"],
        "a refused update must leave the stored criteria as they were, got {described:?}"
    );

    let replacement = json!({"FilterCriteria": {"Filters": [{"Pattern": "{\"source\":[\"b\"]}"}]}});
    let accepted = update(
        &router,
        "filterupdate",
        json!({"RoleArn": ROLE, "SourceParameters": replacement}),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "an update carrying an event pattern must be accepted, got {accepted:?}"
    );
    tokio::time::sleep(PAST_SETTLE).await;
    let described = describe(&router, "filterupdate").await;
    assert_eq!(
        reported_filter_criteria(&described),
        replacement["FilterCriteria"],
        "the accepted criteria must read back, got {described:?}"
    );
}

/// RULE: a pipe carrying none of these configurations is unaffected — the three rules
/// refuse a configuration, they do not require one.
#[tokio::test]
async fn a_pipe_with_no_source_or_target_parameters_is_accepted() {
    let router = router();
    let created = create(
        &router,
        "barepipe",
        minimal_body(SOURCE_SQS, TARGET_SQS),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "a pipe with no parameters at all must be accepted, got {created:?}"
    );
    let described = describe(&router, "barepipe").await;
    assert_eq!(
        (
            reported_filter_criteria(&described),
            described.member("TargetParameters").clone()
        ),
        (Value::Null, json!({})),
        "the pipe must report no filter criteria and no target parameters, got {described:?}"
    );
}

/// The members that carry an `InputTemplate`, as the model names them: the enrichment's and
/// the target's. Source: `PipeEnrichmentParameters$InputTemplate` and
/// `PipeTargetParameters$InputTemplate`.
const ENRICHMENT_PARAMETERS_MEMBER: &str = "EnrichmentParameters";
const TARGET_PARAMETERS_MEMBER: &str = "TargetParameters";

/// The enrichment a row that carries an enrichment template names.
const ENRICHMENT_LAMBDA: &str = "arn:aws:lambda:us-west-2:000000000000:function:enrich";

/// What a write that carries a template spelling no JSON is refused with: the member that
/// carried it, and the one sentence this service words that class of refusal as.
const MALFORMED_TEMPLATE_MESSAGE: &str = "Invalid JSON format for Input Template.";

/// One row of the input-template table.
struct Template {
    /// The case, which is also the pipe's name.
    case: &'static str,
    /// The member that carries the template.
    member: &'static str,
    /// The `InputTemplate` the write carries.
    template: &'static str,
    /// Whether the template's own JSON is broken, so the write must be refused.
    refused: bool,
}

/// The `CreatePipe` body of one row: the template on the member that carries it, and the
/// enrichment resource a row whose template belongs to the enrichment names.
fn template_body(row: &Template) -> Value {
    let mut extra = serde_json::Map::new();
    extra.insert(
        row.member.to_string(),
        json!({ "InputTemplate": row.template }),
    );
    if row.member == ENRICHMENT_PARAMETERS_MEMBER {
        extra.insert("Enrichment".to_string(), json!(ENRICHMENT_LAMBDA));
    }
    create_body(SOURCE_SQS, TARGET_LAMBDA, Value::Object(extra))
}

/// RULE: an `InputTemplate` is the valid JSON text its member is documented to be, so a
/// write carrying one that is JSON text and spells no JSON document — even with each of its
/// placeholders standing for the value it names — is refused with `ValidationException`
/// (400) naming that member, and stores nothing; one that spells its JSON is accepted and
/// read back verbatim.
///
/// The rows differ along what the rule generalises over: which member carries the template
/// (the target's, the enrichment's), the form of the template (JSON object, JSON array,
/// text that is no JSON at all, the empty string the model admits), where its placeholders
/// stand (inside a string literal, as a JSON value, a reserved variable) and HOW the JSON is
/// broken (a placeholder that never closes, an unclosed object or array, a member with no
/// value, a trailing comma, an unquoted key).
#[tokio::test]
async fn an_input_template_that_spells_no_json_is_refused() {
    let rows = vec![
        Template {
            case: "targettemplateobject",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"{"ok": "<$.body.marker>"}"#,
            refused: false,
        },
        Template {
            case: "targettemplateunquotedpath",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"{"ok": <$.body.marker>}"#,
            refused: false,
        },
        Template {
            case: "targettemplatereservedvariable",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"{"event": <aws.pipes.event.json>}"#,
            refused: false,
        },
        Template {
            case: "targettemplatearray",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"[<$.body.count>, "<$.body.marker>"]"#,
            refused: false,
        },
        Template {
            // A template that opens no JSON is the text its author wrote, and carries no
            // JSON to read.
            case: "targettemplatetext",
            member: TARGET_PARAMETERS_MEMBER,
            template: "order <$.body.marker> received",
            refused: false,
        },
        Template {
            // "To remove an input template, specify an empty string."
            // (`PipeTargetParameters$InputTemplate`).
            case: "targettemplateempty",
            member: TARGET_PARAMETERS_MEMBER,
            template: "",
            refused: false,
        },
        Template {
            case: "targettemplateunterminatedplaceholder",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"{"broken": <$.body.marker"#,
            refused: true,
        },
        Template {
            case: "targettemplateunclosedobject",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"{"ok": "<$.body.marker>""#,
            refused: true,
        },
        Template {
            case: "targettemplateunclosedarray",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"[<$.body.marker>"#,
            refused: true,
        },
        Template {
            case: "targettemplatenovalue",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"{"ok": }"#,
            refused: true,
        },
        Template {
            case: "targettemplatetrailingcomma",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"{"ok": "<$.body.marker>",}"#,
            refused: true,
        },
        Template {
            case: "targettemplateunquotedkey",
            member: TARGET_PARAMETERS_MEMBER,
            template: r#"{ok: "<$.body.marker>"}"#,
            refused: true,
        },
        Template {
            // The same member of the enrichment's parameters carries the same kind of
            // template, so the rule holds there too and is reported alike.
            case: "enrichmenttemplateobject",
            member: ENRICHMENT_PARAMETERS_MEMBER,
            template: r#"{"ok": "<$.body.marker>"}"#,
            refused: false,
        },
        Template {
            case: "enrichmenttemplateunclosedobject",
            member: ENRICHMENT_PARAMETERS_MEMBER,
            template: r#"{"broken": <$.body.marker"#,
            refused: true,
        },
    ];

    for row in &rows {
        let router = router();
        let created = create(&router, row.case, template_body(row)).await;

        if !row.refused {
            assert_eq!(
                created.status, 200,
                "case {:?}: a template that spells its own JSON must be accepted, \
                 got {created:?}",
                row.case
            );
            let described = describe(&router, row.case).await;
            if !row.template.is_empty() {
                assert_eq!(
                    described.member(row.member)["InputTemplate"],
                    json!(row.template),
                    "case {:?}: the accepted template must read back verbatim, \
                     got {described:?}",
                    row.case
                );
            }
            continue;
        }

        assert_refused(
            row.case,
            &created,
            "InputTemplate",
            MALFORMED_TEMPLATE_MESSAGE,
        );
        let described = describe(&router, row.case).await;
        assert_eq!(
            described.status, 404,
            "case {:?}: a refused CreatePipe must have stored no pipe, got {described:?}",
            row.case
        );
        let listed = list(&router, "").await;
        assert_eq!(
            listed_names(&listed),
            Vec::<String>::new(),
            "case {:?}: a refused CreatePipe must leave the pipe out of ListPipes, \
             got {listed:?}",
            row.case
        );
    }
}

/// RULE: the same reading of a template holds for `UpdatePipe` — an update carrying one
/// that spells no JSON is refused and leaves the stored template as it was, while one
/// carrying valid JSON text replaces it.
#[tokio::test]
async fn an_update_carrying_a_template_that_spells_no_json_changes_nothing() {
    let stored = r#"{"ok": "<$.body.marker>"}"#;
    // (the case and pipe name, the TargetParameters the update carries, the template the
    // pipe must report afterwards — `None` when the update must be refused)
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "templateupdatebroken",
            json!({"InputTemplate": r#"{"broken": <$.body.marker"#}),
            None,
        ),
        (
            "templateupdateunquotedkey",
            json!({"InputTemplate": r#"{ok: "<$.body.marker>"}"#}),
            None,
        ),
        (
            "templateupdatereplaced",
            json!({"InputTemplate": r#"{"n": <$.body.count>}"#}),
            Some(r#"{"n": <$.body.count>}"#),
        ),
    ];

    for (case, target_parameters, accepted) in rows {
        let router = router();
        let created = create(
            &router,
            case,
            create_body(
                SOURCE_SQS,
                TARGET_LAMBDA,
                json!({"TargetParameters": {"InputTemplate": stored}}),
            ),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the fixture pipe must be created, got {created:?}"
        );
        tokio::time::sleep(PAST_SETTLE).await;

        let answered = update(
            &router,
            case,
            json!({"RoleArn": ROLE, "TargetParameters": target_parameters}),
        )
        .await;
        match accepted {
            None => assert_refused(case, &answered, "InputTemplate", MALFORMED_TEMPLATE_MESSAGE),
            Some(_) => assert_eq!(
                answered.status, 200,
                "case {case:?}: a template that spells its own JSON must be accepted, \
                 got {answered:?}"
            ),
        }
        tokio::time::sleep(PAST_SETTLE).await;
        let described = describe(&router, case).await;
        assert_eq!(
            described.member("TargetParameters")["InputTemplate"],
            json!(accepted.unwrap_or(stored)),
            "case {case:?}: expected the pipe to report the template in force, \
             got {described:?}"
        );
    }
}
