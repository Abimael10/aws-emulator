// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ADDITIONAL ATTRIBUTES a `PutLogEvents` entity carries beside its
//! identity, and what the response says when one of them cannot be read.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rule under test: `Entity$attributes` are "additional attributes of the entity that are
//! not used to specify the identity of the entity", so a producer's own name there carries
//! whatever text it likes — but a name the attribute catalogue publishes a set of values for
//! states one of those values, and a value outside that set cannot be read as that attribute,
//! which the call reports as `rejectedEntityInfo.errorType` `InvalidAttributes` while still
//! ingesting the batch and reporting no `rejectedLogEventsInfo`.
//!
//! The table spans what the rule generalises over: which catalogued name the value is stated
//! under, whether the value is published for it, a value published for the OTHER catalogued
//! name, a name the catalogue does not carry at all, and how many attributes the map holds.

use serde_json::{json, Value};

use crate::harness::{now_ms, Logs};

/// The log group every case below ingests into.
const GROUP: &str = "/entity/attributes";

/// The log stream every case below ingests into.
const STREAM: &str = "stream";

/// The `attributes` names whose values are drawn from a published set, each with that set.
/// Source: the CloudWatch User Guide table of entity attributes that `Entity$attributes` links
/// to (`smithy_query(query="doc", shape="Entity", member="attributes")` — "For details about
/// how to use the attributes, see How to add related information to telemetry"). Every other
/// name in that table — the Kubernetes, EC2 and host coordinates — carries free text.
const PUBLISHED_ATTRIBUTE_VALUES: [(&str, &[&str]); 2] = [
    (
        "PlatformType",
        &[
            "AWS::EC2",
            "AWS::ECS",
            "AWS::EKS",
            "AWS::Lambda",
            "K8s",
            "Generic",
        ],
    ),
    (
        "AWS.ServiceNameSource",
        &[
            "Instrumentation",
            "UserConfiguration",
            "ClientIamRole",
            "K8sWorkload",
            "RdsDbInstance",
            "CloudFormationStack",
        ],
    ),
];

/// A group and a stream to ingest into.
async fn ingesting_logs() -> Logs {
    let logs = Logs::new();
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;
    logs
}

/// One two-event ingestion carrying an entity whose identity is complete and whose additional
/// attributes are `attributes`.
async fn put_with_attributes(logs: &Logs, case: &str, attributes: Value) -> Value {
    let at = now_ms() - 60 * 1_000;
    logs.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": STREAM,
            "logEvents": [
                {"timestamp": at, "message": format!("{case}/first")},
                {"timestamp": at + 1, "message": format!("{case}/second")},
            ],
            "entity": {
                "keyAttributes": {"Type": "Service", "Name": "orders", "Environment": "prod"},
                "attributes": attributes,
            },
        }),
    )
    .await
}

/// The messages the group's stream holds, in the order a read reports them.
async fn stored_messages(logs: &Logs) -> Vec<String> {
    let page = logs
        .ok(
            "GetLogEvents",
            &json!({"logGroupName": GROUP, "logStreamName": STREAM, "startFromHead": true}),
        )
        .await;
    page["events"]
        .as_array()
        .unwrap_or_else(|| panic!("GetLogEvents reports an events list, got {page}"))
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect()
}

/// What one case asserts: the attributes the entity carries and whether they can be read.
fn rows() -> Vec<(String, Value, bool)> {
    let mut rows: Vec<(String, Value, bool)> = Vec::new();
    for (name, published) in PUBLISHED_ATTRIBUTE_VALUES {
        for value in published {
            rows.push((
                format!("{name}={value}, a published value"),
                json!({name: value}),
                true,
            ));
        }
        // A value published for the OTHER catalogued name is no value of this one, and
        // neither is text nothing publishes or a published value in another case.
        let elsewhere = PUBLISHED_ATTRIBUTE_VALUES
            .iter()
            .filter(|(other, _)| *other != name)
            .flat_map(|(_, values)| values.iter().copied());
        for value in elsewhere.chain(["not-a-published-value", "aws::ec2", " "]) {
            if published.contains(&value) {
                continue;
            }
            rows.push((
                format!("{name}={value:?}, not a published value"),
                json!({name: value}),
                false,
            ));
        }
        // The same text under a name the catalogue does not carry is the producer's own.
        rows.push((
            format!("a producer's own name carrying what {name} would refuse"),
            json!({"Producer.OwnAttribute": "not-a-published-value"}),
            true,
        ));
    }
    // How many attributes the map holds, and a catalogued name beside free-text ones.
    rows.push((
        "no attributes at all".to_string(),
        json!({}),
        true,
    ));
    rows.push((
        "free-text coordinates beside a published platform".to_string(),
        json!({
            "PlatformType": "AWS::EKS",
            "EKS.Cluster": "orders-cluster",
            "K8s.Namespace": "orders",
            "K8s.Workload": "orders-api",
        }),
        true,
    ));
    rows.push((
        "free-text coordinates beside an unpublished platform".to_string(),
        json!({
            "PlatformType": "ManagedMainframe",
            "EKS.Cluster": "orders-cluster",
            "K8s.Namespace": "orders",
            "K8s.Workload": "orders-api",
        }),
        false,
    ));
    rows.push((
        "two catalogued names, one of them unpublished".to_string(),
        json!({
            "PlatformType": "AWS::EC2",
            "AWS.ServiceNameSource": "GuessedByHand",
        }),
        false,
    ));
    rows
}

/// RULE: an additional attribute whose name the catalogue publishes values for is read only
/// when it states one of them; otherwise the entity is rejected `InvalidAttributes` — and the
/// batch is ingested either way, with no `rejectedLogEventsInfo`.
#[tokio::test]
async fn an_additional_attribute_is_held_to_the_values_published_for_it() {
    for (case, attributes, readable) in rows() {
        let logs = ingesting_logs().await;
        let answer = put_with_attributes(&logs, &case, attributes).await;
        let reported = answer
            .get("rejectedEntityInfo")
            .and_then(|info| info.get("errorType"))
            .and_then(Value::as_str);
        let expected = if readable {
            None
        } else {
            Some("InvalidAttributes")
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected rejectedEntityInfo.errorType {expected:?}, got {answer}"
        );
        // "When the entity is rejected, the events may still be accepted": an entity
        // rejection never rejects the batch.
        assert_eq!(
            answer.get("rejectedLogEventsInfo"),
            None,
            "case {case:?}: expected no rejectedLogEventsInfo, got {answer}"
        );
        assert!(
            answer
                .get("nextSequenceToken")
                .and_then(Value::as_str)
                .is_some(),
            "case {case:?}: an accepted batch answers a nextSequenceToken, got {answer}"
        );
        // Every event of the batch is readable afterwards, in order.
        let stored = stored_messages(&logs).await;
        let want = vec![format!("{case}/first"), format!("{case}/second")];
        assert_eq!(
            stored, want,
            "case {case:?}: expected the batch stored in order, got {stored:?}"
        );
    }
}
