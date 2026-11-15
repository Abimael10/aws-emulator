// SPDX-License-Identifier: CC0-1.0

//! The AUDIT leg of this front door: every control-plane call it serves is a management
//! event of `dynamodb.amazonaws.com`, and the calls on a table's ITEMS are not.
//!
//! RULE: a service that serves a CONTROL-PLANE (management) API call records that call in
//! the account's management-event log — the operation's own modelled name as `eventName`,
//! this service's `eventSource`, the `Management` category, `readOnly` from the operation's
//! own semantics, the Region and the credential it was served for, the members the request
//! carried as `requestParameters`, what a MUTATING call answered as `responseElements` (a
//! reporting call and a failed call have none), the FAULT SHAPE's name as `errorCode` with
//! the message the caller was answered with, and as `requestID` the id the CALLER was
//! answered under — so a client holding a response can find the record of that very call.
//! A call on the CONTENTS of one resource is a DATA event and is recorded in event history
//! as nothing, whatever it answered; neither is a request refused before any handler ran.
//!
//! The tables below span what the rule generalises over rather than the one call the
//! recording the issue was filed about made: the kinds of control-plane call (create, read,
//! list, tag, untag, configure, refused) and every CLASS of data-plane call this service
//! serves (single-item read and write, query, scan, batch, transaction, PartiQL), with both
//! outcomes and both halves of the correlation.
//!
//! The records are read back out of the SHARED log in the engine the service was built from
//! — the one store CloudTrail's `LookupEvents` reads, which is what makes a management call
//! of this service answerable there and what keeps a data call out of it.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::service::management_events::{
    management_event_log, EVENT_SOURCE,
};
use service_common::management_events::ManagementEvent;
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{describe_when_active, emulator_and_engine, REGION};

/// The access key id the requests below are signed with, which is what the recorded caller
/// of each call is.
const ACCESS_KEY_ID: &str = "AKIDTEST";

/// The `eventCategory` CloudTrail stamps on a control-plane call's record.
const MANAGEMENT: &str = "Management";

/// The `eventType` of a record made for an AWS API call.
const AWS_API_CALL: &str = "AwsApiCall";

/// The header AWS reports a response's request id in, lowercased as HTTP compares it.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The table every call below addresses.
const TABLE: &str = "audit-items";

/// What a call of one row is recorded as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    /// A management event: one record in the account's event history, carrying this
    /// `readOnly`.
    Management { read_only: bool },
    /// Nothing in event history at all.
    Nothing,
}

/// One emulator: the router this crate's suites drive, and the shared engine whose
/// management-event log the records are read out of.
struct Audited {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Audited {
    fn new() -> Self {
        let (router, engine) = emulator_and_engine();
        Self { router, engine }
    }

    /// Call one operation and answer `(status, body, request id)` — the three things a
    /// caller observes, since the record names the request by the id the caller was
    /// answered under.
    async fn call(&self, operation: &str, body: &Value) -> (u16, Value, Option<String>) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/dynamodb/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let bytes = response.into_body();
        let parsed: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed, request_id)
    }

    /// Call one operation that has to be served, and answer its body.
    async fn ok(&self, operation: &str, body: &Value) -> Value {
        let (status, answered, _) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "setup: {operation} must answer 200, got {status} with {answered}"
        );
        answered
    }

    /// Every record in the shared log, oldest first, as the CloudTrail event document a
    /// lookup reports.
    fn records(&self) -> Vec<Value> {
        let log = management_event_log(&*self.engine)
            .expect("this engine carries the shared management-event log");
        let held = log.lock();
        held.iter()
            .map(ManagementEvent::to_cloudtrail_event_json)
            .collect()
    }

    /// The records of the calls to `event_name`, oldest first.
    fn records_of(&self, event_name: &str) -> Vec<Value> {
        self.records()
            .into_iter()
            .filter(|event| event.get("eventName") == Some(&json!(event_name)))
            .collect()
    }
}

/// A string member of a record, or `None` when the record does not carry it.
fn at(event: &Value, pointer: &str) -> Option<String> {
    event
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// The `CreateTable` request every case here starts from: one hash key, on demand.
fn create_table_request(name: &str) -> Value {
    json!({
        "TableName": name,
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "BillingMode": "PAY_PER_REQUEST",
    })
}

/// Create the table every row below addresses and wait out its documented
/// `CREATING` -> `ACTIVE` transition, so each call afterwards is really SERVED rather than
/// refused for a table that is not ready.
async fn a_table_of_items(audited: &Audited) -> String {
    audited
        .ok("CreateTable", &create_table_request(TABLE))
        .await;
    let described = describe_when_active(&audited.router, "the table under audit", TABLE).await;
    described["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the description names the table's ARN, got {described}"))
        .to_string()
}

/// RULE: every CONTROL-PLANE call this front door serves is recorded in the account's event
/// history, and every call on a table's ITEMS is recorded there as nothing.
///
/// The rows differ along the kind of control-plane call (create, read, list, tag, untag,
/// configure), along the outcome (served, refused), along the CLASS of data-plane call
/// (single-item read and write, query, scan, batch read, batch write, transaction, PartiQL)
/// and along whether a handler ran at all — which is what the rule generalises over.
#[tokio::test]
async fn a_control_plane_call_is_recorded_in_event_history_and_a_data_plane_call_is_not() {
    let audited = Audited::new();
    let table_arn = a_table_of_items(&audited).await;

    // (case, operation, request, the status the caller sees, what event history holds)
    let rows: Vec<(&str, &str, Value, u16, Recorded)> = vec![
        (
            "the create of another table",
            "CreateTable",
            create_table_request("audit-second"),
            200,
            Recorded::Management { read_only: false },
        ),
        (
            "the read of one table",
            "DescribeTable",
            json!({"TableName": TABLE}),
            200,
            Recorded::Management { read_only: true },
        ),
        (
            "the list of the Region's tables",
            "ListTables",
            json!({}),
            200,
            Recorded::Management { read_only: true },
        ),
        (
            "the tag write",
            "TagResource",
            json!({"ResourceArn": table_arn, "Tags": [{"Key": "owner", "Value": "audit"}]}),
            200,
            Recorded::Management { read_only: false },
        ),
        (
            "the read of that tag set",
            "ListTagsOfResource",
            json!({"ResourceArn": table_arn}),
            200,
            Recorded::Management { read_only: true },
        ),
        (
            "the tag removal",
            "UntagResource",
            json!({"ResourceArn": table_arn, "TagKeys": ["owner"]}),
            200,
            Recorded::Management { read_only: false },
        ),
        (
            "a configuration write",
            "UpdateTimeToLive",
            json!({
                "TableName": TABLE,
                "TimeToLiveSpecification": {"Enabled": true, "AttributeName": "expires"},
            }),
            200,
            Recorded::Management { read_only: false },
        ),
        (
            "a control-plane read the store refuses",
            "DescribeTable",
            json!({"TableName": "audit-no-such-table"}),
            400,
            Recorded::Management { read_only: true },
        ),
        (
            "a single-item write",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}}}),
            200,
            Recorded::Nothing,
        ),
        (
            "a single-item read",
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}}}),
            200,
            Recorded::Nothing,
        ),
        (
            "a single-item update",
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": {"pk": {"S": "a"}},
                "UpdateExpression": "SET seen = :one",
                "ExpressionAttributeValues": {":one": {"N": "1"}},
            }),
            200,
            Recorded::Nothing,
        ),
        (
            "a query",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :key",
                "ExpressionAttributeValues": {":key": {"S": "a"}},
            }),
            200,
            Recorded::Nothing,
        ),
        (
            "a scan",
            "Scan",
            json!({"TableName": TABLE}),
            200,
            Recorded::Nothing,
        ),
        (
            "a batch read",
            "BatchGetItem",
            json!({"RequestItems": {TABLE: {"Keys": [{"pk": {"S": "a"}}]}}}),
            200,
            Recorded::Nothing,
        ),
        (
            "a batch write",
            "BatchWriteItem",
            json!({"RequestItems": {TABLE: [{"PutRequest": {"Item": {"pk": {"S": "b"}}}}]}}),
            200,
            Recorded::Nothing,
        ),
        (
            "a transactional write",
            "TransactWriteItems",
            json!({"TransactItems": [{"Put": {"TableName": TABLE, "Item": {"pk": {"S": "c"}}}}]}),
            200,
            Recorded::Nothing,
        ),
        (
            "a transactional read",
            "TransactGetItems",
            json!({"TransactItems": [{"Get": {"TableName": TABLE, "Key": {"pk": {"S": "a"}}}}]}),
            200,
            Recorded::Nothing,
        ),
        (
            "a PartiQL statement",
            "ExecuteStatement",
            json!({"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'a'")}),
            200,
            Recorded::Nothing,
        ),
        (
            "a single-item delete",
            "DeleteItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "b"}}}),
            200,
            Recorded::Nothing,
        ),
        (
            // Refused by the request walk, ahead of every handler: no call was served, so
            // there is nothing for event history to hold.
            "a request refused before any handler ran",
            "ListBackups",
            json!({"Limit": 0}),
            400,
            Recorded::Nothing,
        ),
    ];

    for (case, operation, request, expected_status, expected) in &rows {
        let before = audited.records().len();
        let (status, body, request_id) = audited.call(operation, request).await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with {body}"
        );
        let records = audited.records();
        let Recorded::Management { read_only } = expected else {
            assert_eq!(
                records.len(),
                before,
                "case {case:?}: expected event history to hold nothing new, got {:?}",
                &records[before.min(records.len())..]
            );
            continue;
        };
        assert_eq!(
            records.len(),
            before + 1,
            "case {case:?}: expected exactly one new record, got {:?}",
            &records[before.min(records.len())..]
        );
        let event = records.last().expect("checked just above");
        for (member, expected_value) in [
            ("/eventName", json!(operation)),
            ("/eventSource", json!(EVENT_SOURCE)),
            ("/eventCategory", json!(MANAGEMENT)),
            ("/eventType", json!(AWS_API_CALL)),
            ("/awsRegion", json!(REGION)),
            ("/managementEvent", json!(true)),
            ("/readOnly", json!(*read_only)),
            ("/userIdentity/accessKeyId", json!(ACCESS_KEY_ID)),
        ] {
            assert_eq!(
                event.pointer(member),
                Some(&expected_value),
                "case {case:?}: expected {member} == {expected_value}, got {event}"
            );
        }
        assert_eq!(
            at(event, "/requestID"),
            request_id,
            "case {case:?}: the record names the request the caller was answered for, got \
             {event}"
        );
        let answered = *expected_status == 200;
        assert_eq!(
            event.get("errorCode").is_some(),
            !answered,
            "case {case:?}: an errorCode belongs to a failed call alone, got {event}"
        );
        let elements = event.get("responseElements").unwrap_or(&Value::Null);
        assert_eq!(
            !elements.is_null(),
            answered && !*read_only,
            "case {case:?}: only a served MUTATING call records what it answered, got {event}"
        );
        if !answered {
            assert_eq!(
                at(event, "/errorCode").as_deref(),
                Some("ResourceNotFoundException"),
                "case {case:?}: the record names the fault shape, got {event}"
            );
            assert!(
                at(event, "/errorMessage").is_some_and(|stated| !stated.is_empty()),
                "case {case:?}: the record carries the message the caller saw, got {event}"
            );
        }
    }

    // The data plane left event history alone: no record of ANY of its operations is in it,
    // however many of those calls were served above.
    for operation in [
        "PutItem",
        "GetItem",
        "UpdateItem",
        "DeleteItem",
        "Query",
        "Scan",
        "BatchGetItem",
        "BatchWriteItem",
        "TransactGetItems",
        "TransactWriteItems",
        "ExecuteStatement",
    ] {
        assert!(
            audited.records_of(operation).is_empty(),
            "case {operation:?}: a data event is no part of event history, got {:?}",
            audited.records_of(operation)
        );
    }
    // ... and every record event history DOES hold is a management event of this service.
    for event in audited.records() {
        assert_eq!(
            event.get("eventCategory"),
            Some(&json!(MANAGEMENT)),
            "every record in event history is a management event, got {event}"
        );
        assert_eq!(
            event.get("eventSource"),
            Some(&json!(EVENT_SOURCE)),
            "every record these calls made is this service's, got {event}"
        );
    }
}

/// RULE: a management record reproduces the members the request carried as
/// `requestParameters`, so a client reading event history learns WHICH resource a call
/// addressed — the table's ARN for a call that addresses it by ARN, and the table's name for
/// one that addresses it by name.
///
/// The rows differ along how the request names the resource (an ARN member, a name member, a
/// nested specification) and along the kind of call, which is what the rule generalises over.
#[tokio::test]
async fn a_record_carries_the_members_the_request_addressed_its_resource_with() {
    let audited = Audited::new();
    let table_arn = a_table_of_items(&audited).await;

    audited
        .ok(
            "TagResource",
            &json!({
                "ResourceArn": table_arn,
                "Tags": [{"Key": "sim2real-marker", "Value": "token-0"}],
            }),
        )
        .await;
    audited
        .ok(
            "UpdateTimeToLive",
            &json!({
                "TableName": TABLE,
                "TimeToLiveSpecification": {"Enabled": true, "AttributeName": "expires"},
            }),
        )
        .await;

    // (case, operation, the member of the record, the value the request carried)
    for (case, operation, pointer, expected) in [
        (
            "the ARN a tag write addressed",
            "TagResource",
            "/requestParameters/resourceArn",
            table_arn.clone(),
        ),
        (
            "the tag key a tag write carried",
            "TagResource",
            "/requestParameters/tags/0/key",
            "sim2real-marker".to_string(),
        ),
        (
            "the tag value a tag write carried",
            "TagResource",
            "/requestParameters/tags/0/value",
            "token-0".to_string(),
        ),
        (
            "the name a create addressed",
            "CreateTable",
            "/requestParameters/tableName",
            TABLE.to_string(),
        ),
        (
            "the name a configuration write addressed",
            "UpdateTimeToLive",
            "/requestParameters/tableName",
            TABLE.to_string(),
        ),
        (
            "the specification that write carried",
            "UpdateTimeToLive",
            "/requestParameters/timeToLiveSpecification/attributeName",
            "expires".to_string(),
        ),
    ] {
        let records = audited.records_of(operation);
        let event = records
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: {operation} must be recorded"));
        assert_eq!(
            at(event, pointer).as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {pointer} == {expected:?}, got {event}"
        );
    }

    // What a mutating call ANSWERED is in the record too, which is how a reader learns the
    // state the call left the resource in.
    let updated = audited.records_of("UpdateTimeToLive");
    let event = updated.last().expect("the write is recorded");
    assert_eq!(
        at(
            event,
            "/responseElements/timeToLiveSpecification/attributeName"
        )
        .as_deref(),
        Some("expires"),
        "the record carries what the write answered, got {event}"
    );
}

/// RULE: a record's `requestID` is the id the CALLER was answered under, so two calls to one
/// operation are told apart by the call each record answered — and a client holding one
/// response finds the record of that very call and of no other.
///
/// The rows are repeated calls of one operation differing only in the value they carry, which
/// is the case a join by anything but the request id cannot resolve.
#[tokio::test]
async fn each_record_names_the_request_its_own_caller_was_answered_for() {
    let audited = Audited::new();
    let table_arn = a_table_of_items(&audited).await;

    let mut answered: Vec<String> = Vec::new();
    for marker in ["token-0", "token-1", "token-2"] {
        let (status, body, request_id) = audited
            .call(
                "TagResource",
                &json!({
                    "ResourceArn": table_arn,
                    "Tags": [{"Key": "sim2real-marker", "Value": marker}],
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {marker:?}: the tag write must be served, got {status} with {body}"
        );
        let stated = request_id.unwrap_or_else(|| {
            panic!("case {marker:?}: every response names the request it answers")
        });
        let records = audited.records_of("TagResource");
        let event = records
            .last()
            .unwrap_or_else(|| panic!("case {marker:?}: the call must be recorded"));
        assert_eq!(
            at(event, "/requestID").as_deref(),
            Some(stated.as_str()),
            "case {marker:?}: the record must name the request the caller was answered for \
             ({stated}), got {event}"
        );
        assert_eq!(
            at(event, "/requestParameters/tags/0/value").as_deref(),
            Some(marker),
            "case {marker:?}: the record carries the value this call sent, got {event}"
        );
        answered.push(stated);
    }

    for (at_call, stated) in answered.iter().enumerate() {
        assert_eq!(
            answered.iter().filter(|other| *other == stated).count(),
            1,
            "call {at_call}: expected a request id of its own, got {answered:?}"
        );
        // The id is spelled in this service's own form — an unbroken, fixed-width run of
        // uppercase letters and digits — which is what the record is joined to the caller's
        // response by (`tests/all/response_request_id.rs` owns that form).
        assert!(
            stated
                .chars()
                .all(|written| written.is_ascii_uppercase() || written.is_ascii_digit()),
            "call {at_call}: expected this service's own id form, got {stated:?}"
        );
    }
}
