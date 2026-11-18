// SPDX-License-Identifier: CC0-1.0

//! One rule carrying an Amazon Data Firehose delivery-stream target AND an Amazon SQS queue
//! target, where the delivery-stream target reshapes what it receives and the queue target
//! does not.
//!
//! The rule pinned here: **every target of a rule whose `EventPattern` matched is invoked
//! independently, each carrying the payload ITS OWN input override describes — the published
//! envelope for a target carrying no override, the target's constant `Input`, the part of the
//! event its `InputPath` selects, or its `InputTransformer`'s template filled from the paths
//! that transformer maps — so one target's override decides that target's payload alone and
//! changes nothing about what any other target of the same rule receives.**
//!
//! `PutTargets` models the override as three members of one target
//! (`Target{Input, InputPath, InputTransformer}`): "if `Input` is specified ... nothing from
//! the event text itself is passed to the target", "if `InputPath` is specified in the form of
//! JSONPath ... then only the part of the event specified in the path is passed to the
//! target", and an `InputTransformer` is "settings to enable you to provide custom input to a
//! target based on certain event data" whose `InputTemplate` is filled from its
//! `InputPathsMap`, including the placeholders EventBridge reserves for the delivery itself
//! (`aws.events.rule-name`, `aws.events.event.json`). They are members of a TARGET, not of the
//! rule, which is what makes the reshaping per-leg.
//!
//! Only a cross-service suite can pin this: the flow spans the five services
//! `core/router/src/lib.rs` mounts — EventBridge for the rule and the publication, IAM for the
//! role the stream leg is made under, Firehose for the stream, Amazon S3 for the object its
//! delivery writes, and Amazon SQS for the message the other leg receives.
//!
//! The rows differ along what the rule generalises over: the FORM of the delivery-stream
//! target's override (none, a constant, a path selecting an object, a path selecting a scalar,
//! a template filled from the event and the delivery, a template standing for the whole
//! event) and so the KIND of JSON document that reaches the stream (an object, a string).
//!
//! The publication, the per-service wire helpers, the queue builder, the stream wait, the
//! Amazon S3 delivery wait and the envelope assertion are the ones
//! `events_rule_fans_out_to_firehose_and_queue_targets.rs` already holds for this same
//! fan-out, and the Region, the wire encoder and the IAM role builder the ones
//! `events_rule_firehose_target_delivery.rs` holds, so all three suites publish one event, sign
//! for one Region and build a role one way.

use serde_json::{json, Value};

use crate::events_rule_fans_out_to_firehose_and_queue_targets::{
    assert_carries_the_envelope, delivered_messages, delivered_object, emulator, events, firehose,
    given_an_active_stream, given_queue, DETAIL_TYPE, EVENTS_PRINCIPAL, ORDER_ID,
    PUT_RECORD_ACTION, SEND_MESSAGE_ACTION, SOURCE,
};
use crate::events_rule_firehose_target_delivery::{given_role, send, REGION};

/// The bucket every delivery stream of this suite writes into.
const BUCKET: &str = "events-target-input-overrides";

/// The role every STREAM delivers to that bucket under: Firehose has to be able to assume it,
/// and it has to allow the object write the delivery makes.
const DELIVERY_ROLE: &str = "overrides-firehose-delivery-role";

/// The constant one row's delivery-stream target receives instead of the envelope
/// (`Target$Input`: "valid JSON text passed to the target").
const CONSTANT_INPUT: &str = r#"{"fixed":"constant-input"}"#;

/// What the delivery-stream target of one row asks to receive, written as the `PutTargets`
/// members that state it.
enum Override {
    /// No override at all: the target takes the event as published.
    AsPublished,
    /// `Target$Input`: this constant, and nothing of the event.
    Input(&'static str),
    /// `Target$InputPath`: the part of the event this path selects.
    Path(&'static str),
    /// `Target$InputTransformer`: this template, filled from these `InputPathsMap` entries.
    Transformer {
        template: &'static str,
        paths: &'static [(&'static str, &'static str)],
    },
}

/// What the delivery-stream leg's destination then holds.
enum Expect {
    /// The whole published envelope, read by member.
    Envelope,
    /// Exactly this JSON document, read as a value — with `{order}` standing for the detail's
    /// order id and `{rule}` for the row's rule name, the two values a row cannot write
    /// literally because the suite owns one and the row the other.
    Document(&'static str),
}

/// One row: the delivery-stream target's override, and the document its destination holds.
struct Row {
    case: &'static str,
    /// Names this row's stream, destination prefix, queue, rule and target role, so rows
    /// cannot collide.
    name: &'static str,
    over: Override,
    expect: Expect,
}

/// The table: one row per FORM the delivery-stream target's input override takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the stream target carries no override",
            name: "override-none",
            over: Override::AsPublished,
            expect: Expect::Envelope,
        },
        Row {
            case: "the stream target carries a constant Input",
            name: "override-constant",
            over: Override::Input(CONSTANT_INPUT),
            expect: Expect::Document(CONSTANT_INPUT),
        },
        Row {
            case: "the stream target's InputPath selects an object",
            name: "override-path-object",
            over: Override::Path("$.detail"),
            expect: Expect::Document(r#"{"orderId":"{order}"}"#),
        },
        Row {
            case: "the stream target's InputPath selects a scalar",
            name: "override-path-scalar",
            over: Override::Path("$.detail.orderId"),
            expect: Expect::Document(r#""{order}""#),
        },
        Row {
            case: "the stream target's InputTransformer fills a template from the event",
            name: "override-transformer",
            over: Override::Transformer {
                template: r#"{"order":"<order>","rule":"<aws.events.rule-name>"}"#,
                paths: &[("order", "$.detail.orderId")],
            },
            expect: Expect::Document(r#"{"order":"{order}","rule":"{rule}"}"#),
        },
        Row {
            case: "the stream target's InputTransformer stands for the whole event",
            name: "override-transformer-event",
            over: Override::Transformer {
                template: "<aws.events.event.json>",
                paths: &[],
            },
            expect: Expect::Envelope,
        },
    ]
}

impl Row {
    /// This row's delivery-stream target, as `PutTargets` takes it: the stream's ARN, the role
    /// the call is made under, and the members this row's override is written as.
    fn stream_target(&self, account: &str, role_arn: &str) -> Value {
        let mut target = json!({
            "Id": "stream",
            "Arn": format!("arn:aws:firehose:{REGION}:{account}:deliverystream/{}", self.name),
            "RoleArn": role_arn,
        });
        match &self.over {
            Override::AsPublished => {}
            Override::Input(input) => target["Input"] = json!(input),
            Override::Path(path) => target["InputPath"] = json!(path),
            Override::Transformer { template, paths } => {
                let map: serde_json::Map<String, Value> = paths
                    .iter()
                    .map(|(key, path)| ((*key).to_string(), json!(path)))
                    .collect();
                target["InputTransformer"] = json!({
                    "InputPathsMap": map,
                    "InputTemplate": template,
                });
            }
        }
        target
    }

    /// The document this row's destination must hold, as a value — `None` for a row whose
    /// destination holds the published envelope, which is read by member instead.
    fn expected_document(&self) -> Option<Value> {
        match self.expect {
            Expect::Envelope => None,
            Expect::Document(document) => {
                let filled = document
                    .replace("{order}", ORDER_ID)
                    .replace("{rule}", self.name);
                Some(serde_json::from_str(&filled).unwrap_or_else(|error| {
                    panic!(
                        "case {:?}: the row's own expectation {filled:?} must be a JSON \
                         document ({error})",
                        self.case
                    )
                }))
            }
        }
    }
}

/// A rule's delivery-stream target is handed what its OWN input override describes, and the
/// queue target of that same rule is handed the published envelope all the same.
#[tokio::test]
async fn each_target_of_one_rule_receives_what_its_own_input_override_describes() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let rows = rows();

    // The bucket every stream delivers into, and the role every stream delivers under.
    let (status, body) = send(
        &router,
        "s3",
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "setup: the destination bucket must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let delivery_role = given_role(
        &router,
        "setup",
        DELIVERY_ROLE,
        "firehose.amazonaws.com",
        Some("s3:PutObject"),
    )
    .await;

    // Every row's stream, created together so one wait covers the span the creates armed.
    for row in &rows {
        firehose(
            &router,
            row.case,
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": row.name,
                "DeliveryStreamType": "DirectPut",
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": delivery_role,
                    "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                    "Prefix": format!("{}/", row.name),
                    // The shortest buffering the hints admit, so a row waits for the
                    // emulator's own delivery span and nothing longer.
                    "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                },
            }),
        )
        .await;
    }

    // Every row's rule, carrying BOTH targets: this row's stream, reshaping its input the way
    // this row states, and this row's queue, reshaping nothing.
    let mut queue_urls: Vec<String> = Vec::new();
    for row in &rows {
        let case = row.case;
        let role_arn = given_role(
            &router,
            case,
            row.name,
            EVENTS_PRINCIPAL,
            Some(PUT_RECORD_ACTION),
        )
        .await;
        let (queue_url, queue_arn) =
            given_queue(&router, case, row.name, Some(SEND_MESSAGE_ACTION)).await;
        queue_urls.push(queue_url);
        events(
            &router,
            case,
            "PutRule",
            json!({
                "Name": row.name,
                "EventPattern": json!({
                    "source": [SOURCE],
                    "detail-type": [DETAIL_TYPE],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let answered = events(
            &router,
            case,
            "PutTargets",
            json!({
                "Rule": row.name,
                "Targets": [
                    row.stream_target(account, &role_arn),
                    { "Id": "queue", "Arn": queue_arn },
                ],
            }),
        )
        .await;
        assert_eq!(
            answered["FailedEntryCount"],
            json!(0),
            "case {case:?}: both targets must be accepted, got {answered}"
        );
    }

    // Firehose refuses a record put at a stream that is still being created, so the
    // publication waits for every stream to REPORT that it is active.
    for row in &rows {
        given_an_active_stream(&router, row.case, row.name).await;
    }

    // ONE publication, which every row's rule matches: the override each row's stream target
    // carries is the only thing that differs between them.
    let published = events(
        &router,
        "the publication",
        "PutEvents",
        json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "orderId": ORDER_ID }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "the publication must be accepted whole: {published}"
    );

    for row in &rows {
        let case = row.case;
        let delivered = delivered_object(&router, case, BUCKET, &format!("{}/", row.name)).await;
        match row.expected_document() {
            // A row whose target asked for no reshaping, and one whose template stands for the
            // whole event, both receive the envelope — whose id, account and time are the
            // emulator's to mint, so it is read by the members the publication submitted.
            None => assert_carries_the_envelope(case, "stream", &delivered),
            Some(expected) => {
                let actual: Value = serde_json::from_str(&delivered).unwrap_or_else(|error| {
                    panic!(
                        "case {case:?}: the stream leg receives one JSON document, got \
                         {delivered:?} ({error})"
                    )
                });
                assert_eq!(
                    actual, expected,
                    "case {case:?}: the stream leg receives what its own override describes; \
                     expected {expected}, got {actual}"
                );
            }
        }
    }

    // And the queue leg of every one of those rules received the envelope as published: the
    // reshaping the OTHER leg of its rule asked for decided nothing for it.
    for (row, url) in rows.iter().zip(&queue_urls) {
        let bodies = delivered_messages(&router, row.case, url).await;
        assert_eq!(
            bodies.len(),
            1,
            "case {:?}: the queue leg receives the event once, got {bodies:?}",
            row.case
        );
        assert_carries_the_envelope(row.case, "queue", &bodies[0]);
    }
}
