// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ENTITY a `PutLogEvents` request carries, and what the response
//! says when that entity cannot be read.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rule under test: the entity is telemetry metadata about what produced the events, not
//! part of them, so a request whose entity cannot be read still INGESTS its events (HTTP 200,
//! the events readable afterwards) and reports why the entity was rejected in
//! `rejectedEntityInfo.errorType` — "only returned when the entity is rejected. When the
//! entity is rejected, the events may still be accepted." An entity the service can read is
//! reported nowhere at all.
//!
//! The table spans what the rule generalises over: which part of the entity cannot be read
//! (a missing or empty `Type`, a `Type` outside the catalogue, a missing identifying
//! attribute of the type that was named, an attribute of another type beside a complete
//! identity, an unknown key name), which kind of entity it is (one identified by name, one
//! identified by resource type and identifier), and a request that carries no entity at all.

use serde_json::{json, Value};

use crate::harness::{now_ms, Logs};

/// The log group every case below ingests into.
const GROUP: &str = "/entity/ingested";

/// The log stream every case below ingests into.
const STREAM: &str = "stream";

/// The modelled `EntityRejectionErrorType` values a rejection can carry. Source:
/// `smithy_query(query="shape", shape="EntityRejectionErrorType")` — the enum's values are
/// `InvalidEntity`, `InvalidTypeValue`, `InvalidKeyAttributes`, `InvalidAttributes`,
/// `EntitySizeTooLarge`, `UnsupportedLogGroupType` and `MissingRequiredFields`.
const REJECTION_ERROR_TYPES: [&str; 7] = [
    "InvalidEntity",
    "InvalidTypeValue",
    "InvalidKeyAttributes",
    "InvalidAttributes",
    "EntitySizeTooLarge",
    "UnsupportedLogGroupType",
    "MissingRequiredFields",
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

/// One ingestion of `message`, carrying `entity` when one is given.
async fn put_with_entity(logs: &Logs, message: &str, entity: Option<Value>) -> Value {
    let mut body = json!({
        "logGroupName": GROUP,
        "logStreamName": STREAM,
        "logEvents": [{"timestamp": now_ms() - 60 * 1_000, "message": message}],
    });
    if let Some(entity) = entity {
        body["entity"] = entity;
    }
    logs.ok("PutLogEvents", &body).await
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

/// RULE: an entity the service cannot read is rejected with the error type naming the part
/// that could not be read, and the events of that request are ingested all the same.
#[tokio::test]
async fn a_malformed_entity_is_rejected_while_its_events_are_kept() {
    // (case, the entity the request carries, the error type the rejection must carry)
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "a service entity",
            json!({"keyAttributes": {"Type": "Service", "Name": "orders"}}),
            None,
        ),
        (
            "a resource entity",
            json!({"keyAttributes": {
                "Type": "Resource",
                "ResourceType": "AWS::EC2::Instance",
                "Identifier": "i-0123456789abcdef0",
            }}),
            None,
        ),
        (
            "a service entity with attributes beside its identity",
            json!({
                "keyAttributes": {"Type": "Service", "Name": "orders", "Environment": "prod"},
                "attributes": {"AWS.ServiceNameSource": "UserConfiguration"},
            }),
            None,
        ),
        (
            "bad-attribute",
            json!({"keyAttributes": {
                "Type": "Service",
                "Name": "orders",
                "Identifier": "i-0123456789abcdef0",
            }}),
            Some("InvalidAttributes"),
        ),
        (
            "bad-type",
            json!({"keyAttributes": {"Type": "NotAnEntityType", "Name": "orders"}}),
            Some("InvalidTypeValue"),
        ),
        (
            "a type whose case does not match the catalogue",
            json!({"keyAttributes": {"Type": "service", "Name": "orders"}}),
            Some("InvalidTypeValue"),
        ),
        (
            "a key name outside the five the member admits, beside a complete identity",
            json!({"keyAttributes": {"Type": "Service", "Name": "orders", "Owner": "team-a"}}),
            Some("InvalidKeyAttributes"),
        ),
        (
            "no-required-keys",
            json!({"keyAttributes": {"Type": "Service", "Owner": "team-a"}}),
            Some("MissingRequiredFields"),
        ),
        (
            "no Type at all",
            json!({"keyAttributes": {"Name": "orders", "Environment": "prod"}}),
            Some("MissingRequiredFields"),
        ),
        (
            "a service entity that names no Name",
            json!({"keyAttributes": {"Type": "Service", "Environment": "prod"}}),
            Some("MissingRequiredFields"),
        ),
        (
            "a resource entity that names no Identifier",
            json!({"keyAttributes": {"Type": "Resource", "ResourceType": "AWS::EC2::Instance"}}),
            Some("MissingRequiredFields"),
        ),
    ];

    for (case, entity, expected) in rows {
        let logs = ingesting_logs().await;
        let answer = put_with_entity(&logs, case, Some(entity)).await;
        let reported = answer
            .get("rejectedEntityInfo")
            .and_then(|info| info.get("errorType"))
            .and_then(Value::as_str);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected rejectedEntityInfo.errorType {expected:?}, got {answer}"
        );
        if let Some(error_type) = reported {
            assert!(
                REJECTION_ERROR_TYPES.contains(&error_type),
                "case {case:?}: {error_type:?} is not a modelled EntityRejectionErrorType"
            );
        }
        // The events are kept either way: a rejected entity costs the batch nothing.
        assert!(
            answer
                .get("nextSequenceToken")
                .and_then(Value::as_str)
                .is_some(),
            "case {case:?}: an accepted batch answers a nextSequenceToken, got {answer}"
        );
        let stored = stored_messages(&logs).await;
        assert_eq!(
            stored,
            vec![case.to_string()],
            "case {case:?}: expected the event to be stored, got {stored:?}"
        );
    }
}

/// RULE: `rejectedEntityInfo` is "only returned when the entity is rejected", so a request
/// that carries no entity, and one whose entity is read, report none — and a rejection does
/// not stop the next request's entity from being read.
#[tokio::test]
async fn a_request_with_nothing_rejected_reports_no_rejection() {
    let logs = ingesting_logs().await;

    let without = put_with_entity(&logs, "no entity at all", None).await;
    assert_eq!(
        without.get("rejectedEntityInfo"),
        None,
        "case \"no entity at all\": expected no rejectedEntityInfo, got {without}"
    );

    let rejected = put_with_entity(
        &logs,
        "a rejected entity",
        Some(json!({"keyAttributes": {"Type": "Nope", "Name": "orders"}})),
    )
    .await;
    assert!(
        rejected.get("rejectedEntityInfo").is_some(),
        "case \"a rejected entity\": expected a rejection, got {rejected}"
    );

    let after = put_with_entity(
        &logs,
        "a read entity after a rejected one",
        Some(json!({"keyAttributes": {"Type": "Service", "Name": "orders"}})),
    )
    .await;
    assert_eq!(
        after.get("rejectedEntityInfo"),
        None,
        "case \"a read entity after a rejected one\": expected no rejectedEntityInfo, got {after}"
    );

    let stored = stored_messages(&logs).await;
    assert_eq!(
        stored,
        vec![
            "no entity at all".to_string(),
            "a rejected entity".to_string(),
            "a read entity after a rejected one".to_string(),
        ],
        "every batch was ingested, got {stored:?}"
    );
}
