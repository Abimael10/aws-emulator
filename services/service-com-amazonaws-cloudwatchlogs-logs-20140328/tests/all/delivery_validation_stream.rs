// SPDX-License-Identifier: CC0-1.0

//! The log stream CloudWatch Logs itself creates in the destination log group of a delivery
//! it has wired to one.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rule under test: **a delivery whose destination resource is a log group of this account
//! is validated by writing to that group, and the stream that write goes to is created with
//! the delivery** — one stream of fixed name in the DESTINATION's own log group, carrying that
//! group's log-stream ARN and the instant it was created, so `DescribeLogStreams` on the
//! destination group lists it and a workflow waiting for a vended delivery to be validated
//! settles. A delivery to a destination resource this service does not own holds no log stream
//! at all, and a second delivery to the same destination keeps the stream that is there.
//!
//! The table spans what the rule generalises over — which kind of resource the destination
//! names, which group it is, and how many deliveries are wired to it — rather than the one
//! recorded delivery.

use serde_json::{json, Value};

use crate::harness::{Logs, REGION};

/// The operations under test.
const CREATE_DELIVERY: &str = "CreateDelivery";
const DESCRIBE_STREAMS: &str = "DescribeLogStreams";

/// The name of the stream a validated delivery subscription writes to. Fixed text, stated here
/// independently of the crate under test.
const VALIDATION_STREAM: &str = "log_stream_created_by_aws_to_validate_log_delivery_subscriptions";

/// A delivery source over one log group of this account, as `PutDeliverySource` creates one.
async fn delivery_source(logs: &Logs, name: &str, group: &str) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    let (_, group_arn) = logs.group_arns(group).await;
    logs.ok(
        "PutDeliverySource",
        &json!({"name": name, "resourceArn": group_arn, "logType": "INSIGHTS_QUERY_LOGS"}),
    )
    .await;
}

/// A delivery destination over `resource_arn`, answering the destination's own ARN.
async fn delivery_destination(logs: &Logs, name: &str, resource_arn: &str) -> String {
    let answer = logs
        .ok(
            "PutDeliveryDestination",
            &json!({
                "name": name,
                "deliveryDestinationConfiguration": {"destinationResourceArn": resource_arn},
            }),
        )
        .await;
    answer["deliveryDestination"]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("a put answers the destination's ARN, got {answer}"))
        .to_string()
}

/// The streams of one log group, as `DescribeLogStreams` reports them.
async fn streams(logs: &Logs, case: &str, group: &str) -> Vec<Value> {
    let page = logs
        .ok(DESCRIBE_STREAMS, &json!({"logGroupName": group}))
        .await;
    page.get("logStreams")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a read reports logStreams, got {page}"))
        .clone()
}

/// One row: a delivery to make, and the log group its validation stream belongs in.
struct Row {
    /// What the row varies, named in every assertion.
    case: &'static str,
    /// The delivery source to pair, and the log group it sends from.
    source: &'static str,
    source_group: &'static str,
    /// The delivery destination to pair: the resource it names, and its own name.
    resource_arn: String,
    destination_name: &'static str,
    /// The log group the validation stream belongs in — `None` for a destination resource
    /// this service does not own, which holds no log stream at all.
    expected_group: Option<String>,
}

/// A delivery wired to a log group of this account creates the validation stream in THAT
/// group, with the group's own log-stream ARN and a creation time; a delivery to a resource of
/// another service creates none; and a second delivery to the same destination keeps the
/// stream it already has.
#[tokio::test]
async fn a_delivery_to_a_log_group_creates_the_validation_stream_in_it() {
    let logs = Logs::new();

    // Two destinations that ARE log groups of this account, and two that are resources of
    // other services.
    let mut destination_group_arns: Vec<(String, String)> = Vec::new();
    for group in [
        "/aws/vendedlogs/destination-one",
        "/aws/vendedlogs/destination/two",
    ] {
        logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
            .await;
        let (_, group_arn) = logs.group_arns(group).await;
        destination_group_arns.push((group.to_string(), group_arn));
    }

    let rows: Vec<Row> = vec![
        Row {
            case: "a log group destination",
            source: "vpc-flow-logs",
            source_group: "/vended/source-one",
            resource_arn: destination_group_arns[0].1.clone(),
            destination_name: "to-logs-one",
            expected_group: Some(destination_group_arns[0].0.clone()),
        },
        Row {
            case: "another log group destination",
            source: "waf_logs",
            source_group: "/vended/source-two",
            resource_arn: destination_group_arns[1].1.clone(),
            destination_name: "to-logs-two",
            expected_group: Some(destination_group_arns[1].0.clone()),
        },
        Row {
            case: "an S3 bucket destination",
            source: "cloudfront-logs",
            source_group: "/vended/source-three",
            resource_arn: "arn:aws:s3:::vended-logs-bucket".to_string(),
            destination_name: "to-s3",
            expected_group: None,
        },
        Row {
            case: "a Firehose delivery stream destination",
            source: "bedrock-logs",
            source_group: "/vended/source-four",
            resource_arn: "arn:aws:firehose:eu-west-2:123456789012:deliverystream/vended-logs"
                .to_string(),
            destination_name: "to-firehose",
            expected_group: None,
        },
    ];

    for row in &rows {
        let Row {
            case,
            source,
            source_group,
            resource_arn,
            destination_name,
            expected_group,
        } = row;
        delivery_source(&logs, source, source_group).await;
        let destination = delivery_destination(&logs, destination_name, resource_arn).await;
        logs.ok(
            CREATE_DELIVERY,
            &json!({"deliverySourceName": source, "deliveryDestinationArn": destination}),
        )
        .await;

        match expected_group {
            Some(group) => {
                let reported = streams(&logs, case, group).await;
                let stream = reported
                    .iter()
                    .find(|stream| stream["logStreamName"].as_str() == Some(VALIDATION_STREAM))
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the destination group holds the validation stream, \
                             got {reported:?}"
                        )
                    });
                let (_, group_arn) = logs.group_arns(group).await;
                assert_eq!(
                    stream["arn"].as_str(),
                    Some(format!("{group_arn}:log-stream:{VALIDATION_STREAM}").as_str()),
                    "case {case:?}: the stream carries its own log-stream ARN, got {stream}"
                );
                assert!(
                    stream["creationTime"].as_i64().is_some_and(|at| at > 0),
                    "case {case:?}: the stream carries a creation time, got {stream}"
                );
            }
            // A delivery to a resource of another service creates no log stream anywhere: the
            // only log group of this delivery is its SOURCE's, which stays empty.
            None => {
                let reported = streams(&logs, case, source_group).await;
                assert_eq!(
                    reported.len(),
                    0,
                    "case {case:?}: no log stream is created for such a destination, got \
                     {reported:?}"
                );
            }
        }
    }

    // One stream per destination group, and nothing of the other group's.
    for (group, _) in &destination_group_arns {
        let reported = streams(&logs, "one stream per destination group", group).await;
        let names: Vec<&str> = reported
            .iter()
            .filter_map(|stream| stream["logStreamName"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![VALIDATION_STREAM],
            "case \"one stream per destination group\": {group} holds exactly the validation \
             stream, got {reported:?}"
        );
    }

    // A SECOND delivery wired to the same destination keeps the stream that is there, with the
    // creation time it got: the stream belongs to the destination, and nothing re-creates it.
    let (destination_group, _) = destination_group_arns[0].clone();
    let before = streams(&logs, "a second delivery to the same destination", &destination_group)
        .await;
    delivery_source(&logs, "second-source", "/vended/source-five").await;
    let destination = delivery_destination(
        &logs,
        "to-logs-one-again",
        &destination_group_arns[0].1.clone(),
    )
    .await;
    logs.ok(
        CREATE_DELIVERY,
        &json!({"deliverySourceName": "second-source", "deliveryDestinationArn": destination}),
    )
    .await;
    let after = streams(&logs, "a second delivery to the same destination", &destination_group)
        .await;
    assert_eq!(
        after, before,
        "case \"a second delivery to the same destination\": the validation stream is created \
         once, got {after:?}"
    );

    // The same create again is idempotent and still leaves one stream.
    logs.ok(
        CREATE_DELIVERY,
        &json!({"deliverySourceName": "second-source", "deliveryDestinationArn": destination}),
    )
    .await;
    let repeated = streams(&logs, "the same create again", &destination_group).await;
    assert_eq!(
        repeated, before,
        "case \"the same create again\": the validation stream is still the one that was \
         created, got {repeated:?}"
    );
}

/// A create this service REFUSES wires nothing, so it leaves the destination log group exactly
/// as it found it: no validation stream for a delivery that was never made.
///
/// The rows span the refusal classes a create can earn after the destination has been resolved:
/// a `recordFields` selection the log source does not support, and a delivery source the account
/// does not hold.
#[tokio::test]
async fn a_refused_create_leaves_the_destination_group_untouched() {
    let logs = Logs::new();
    let group = "/aws/vendedlogs/refused-destination";
    logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    let (_, group_arn) = logs.group_arns(group).await;
    let destination = delivery_destination(&logs, "to-logs-refused", &group_arn).await;

    // Security Hub findings publish ONE record field and mark it mandatory, so a selection of
    // them is a caller leaving nothing out and the create is refused
    // (`ConfigurationTemplate$allowedFields` of the `SECURITY_FINDING_LOGS` template).
    logs.ok(
        "PutDeliverySource",
        &json!({
            "name": "securityhub-findings",
            "resourceArn": format!("arn:aws:securityhub:{REGION}:123456789012:hub/*"),
            "logType": "SECURITY_FINDING_LOGS",
        }),
    )
    .await;

    // (case, the create to refuse, the error code it earns)
    let refused: Vec<(&str, Value, &str)> = vec![
        (
            "a record-field selection the log source does not support",
            json!({
                "deliverySourceName": "securityhub-findings",
                "deliveryDestinationArn": destination,
                "recordFields": ["event"],
            }),
            "ValidationException",
        ),
        (
            "a delivery source the account does not hold",
            json!({
                "deliverySourceName": "no-such-source",
                "deliveryDestinationArn": destination,
            }),
            "ResourceNotFoundException",
        ),
    ];
    for (case, request, expected) in &refused {
        let code = logs.err(CREATE_DELIVERY, request).await;
        assert_eq!(
            code, *expected,
            "case {case:?}: expected {expected}, got {code}"
        );
        let reported = streams(&logs, case, group).await;
        assert_eq!(
            reported.len(),
            0,
            "case {case:?}: a refused create wires nothing, so the destination group holds no \
             stream, got {reported:?}"
        );
    }

    // The same pair, accepted: the stream the refusals left out appears, so the rows above are
    // empty because the create was refused and not because nothing ever creates it.
    logs.ok(
        CREATE_DELIVERY,
        &json!({
            "deliverySourceName": "securityhub-findings",
            "deliveryDestinationArn": destination,
        }),
    )
    .await;
    let names: Vec<String> = streams(&logs, "the create that is served", group)
        .await
        .iter()
        .filter_map(|stream| stream["logStreamName"].as_str().map(ToString::to_string))
        .collect();
    assert_eq!(
        names,
        vec![VALIDATION_STREAM.to_string()],
        "case \"the create that is served\": the validation stream appears, got {names:?}"
    );
}
