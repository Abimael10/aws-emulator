// SPDX-License-Identifier: CC0-1.0

//! What this service answers a body that states no value for a member the input REQUIRES —
//! a member carried as an explicit `null`, or one left out of a structure the body does
//! carry — driven end to end over the wire.
//!
//! THE RULE: a member the input requires and the body states no value for breaks the
//! model's own `@required` rule, so the request is refused as the CONSTRAINT report it is —
//! this service's `ValidationException`, one clause per violated member inside the
//! `<N> validation error(s) detected: ` envelope — together with every other bound the same
//! body breaks, and the refusal applies nothing. A member stated `null` and the same member
//! left out are the same absence and earn the same answer, because a `null` is how a client
//! marshals a member it is not stating.
//!
//! Two corollaries the suites below read back:
//!
//! * the report is the WHOLE body's. A body breaking several bounds at once is refused with
//!   one clause per violated member, counted in one envelope, so a caller sees every defect
//!   of a malformed request rather than one per round trip — including the bounds that stand
//!   BEHIND the absence in the body, which no walk over a built input could have reached.
//! * nothing the SERVICE had to invent to read the rest of the body is reported: a member
//!   the caller stated nothing for earns the absence clause and nothing else.
//!
//! The one surface observed to answer such a `null` differently is the vector-search one,
//! which answers the reader's own diagnosis of the body
//! (`crate::body_form_and_stated_absences`'s rule 3) — the two groups are pinned side by
//! side, so the difference is visible rather than silent.
//!
//! Each suite is a TABLE spanning what the rule generalises over rather than one recorded
//! request: the modelled type of the member with no value (a string, a long, a map, a list,
//! a boolean, a structure), WHERE it stands (a top-level member, a member of a nested
//! structure, a member of a structure inside a list, a member of a structure inside a map),
//! HOW the body states the absence (an explicit `null`, or the member left out), the
//! operation that carries it, and how many other bounds the same body breaks.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table the rows address when they address a real one.
const TABLE: &str = "Reported";

/// The ARN of that table, which the ARN-addressed tagging surface names it by.
fn table_arn() -> String {
    format!(
        "arn:aws:dynamodb:{}:000000000000:table/{TABLE}",
        crate::emulator::REGION
    )
}

/// The clause one member with no value earns, as the framework words it: the path the
/// validating walk reports the member under, and the requirement that it not be null.
fn absent_clause(path: &str) -> String {
    format!("Value null at '{path}' failed to satisfy constraint: Member must not be null")
}

/// The clause a string outside its `@length` minimum earns.
fn too_short_clause(value: &str, path: &str, min: usize) -> String {
    format!(
        "Value '{value}' at '{path}' failed to satisfy constraint: Member must have length \
         greater than or equal to {min}"
    )
}

/// The clause a string outside its `@pattern` earns, with the expression as this service
/// states it.
fn pattern_clause(value: &str, path: &str) -> String {
    format!(
        "Value '{value}' at '{path}' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: [a-zA-Z0-9_.-]+"
    )
}

/// The whole message a refusal reporting `clauses` carries.
fn envelope(clauses: &[String]) -> String {
    match clauses {
        [clause] => format!("1 validation error detected: {clause}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

/// One row of the report table: the request, and the clauses its refusal must state in the
/// order it must state them.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: fn() -> Value,
    clauses: fn() -> Vec<String>,
}

/// The rows. Every one of them states no value for at least one member the input requires,
/// and each differs from the others along the modelled type of that member, where it stands,
/// how the body states the absence, and what else is wrong with the same body.
const ROWS: &[Row] = &[
    Row {
        case: "a required map stated null, on a read",
        operation: "GetItem",
        request: || json!({"TableName": TABLE, "Key": null}),
        clauses: || vec![absent_clause("key")],
    },
    Row {
        case: "a required map stated null, on a write",
        operation: "PutItem",
        request: || json!({"TableName": TABLE, "Item": null}),
        clauses: || vec![absent_clause("item")],
    },
    Row {
        case: "a required boolean stated null inside a structure",
        operation: "UpdateTimeToLive",
        request: || {
            json!({"TableName": TABLE,
                   "TimeToLiveSpecification": {"Enabled": null, "AttributeName": "ttl"}})
        },
        clauses: || vec![absent_clause("timeToLiveSpecification.enabled")],
    },
    Row {
        case: "a required string stated null inside a structure in a list",
        operation: "TagResource",
        request: || json!({"ResourceArn": table_arn(), "Tags": [{"Key": null, "Value": "v"}]}),
        clauses: || vec![absent_clause("tags.1.member.key")],
    },
    Row {
        case: "the same member left out of that structure entirely",
        operation: "TagResource",
        request: || json!({"ResourceArn": table_arn(), "Tags": [{"Value": "v"}]}),
        clauses: || vec![absent_clause("tags.1.member.key")],
    },
    Row {
        case: "a required long stated null inside a structure in a list",
        operation: "UpdateTable",
        request: || {
            json!({"TableName": TABLE,
                   "GlobalSecondaryIndexUpdates": [
                       {"Update": {"IndexName": "by-label",
                                   "ProvisionedThroughput": {"ReadCapacityUnits": 5,
                                                             "WriteCapacityUnits": null}}}]})
        },
        clauses: || {
            vec![absent_clause(
                "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                 writeCapacityUnits",
            )]
        },
    },
    Row {
        case: "the same member left out of that structure entirely",
        operation: "UpdateTable",
        request: || {
            json!({"TableName": TABLE,
                   "GlobalSecondaryIndexUpdates": [
                       {"Update": {"IndexName": "by-label",
                                   "ProvisionedThroughput": {"ReadCapacityUnits": 5}}}]})
        },
        clauses: || {
            vec![absent_clause(
                "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                 writeCapacityUnits",
            )]
        },
    },
    Row {
        // BOTH members of one structure, which the service states WRITE first — the order it
        // states that structure's clauses in, wherever the structure stands.
        case: "both required longs of one structure stated null",
        operation: "UpdateTable",
        request: || {
            json!({"TableName": TABLE,
                   "GlobalSecondaryIndexUpdates": [
                       {"Update": {"IndexName": "by-label",
                                   "ProvisionedThroughput": {"ReadCapacityUnits": null,
                                                             "WriteCapacityUnits": null}}}]})
        },
        clauses: || {
            vec![
                absent_clause(
                    "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                     writeCapacityUnits",
                ),
                absent_clause(
                    "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                     readCapacityUnits",
                ),
            ]
        },
    },
    Row {
        // The absence BESIDE another bound the same body breaks, in one envelope: the index
        // name breaks its `@pattern` and the capacity member states no value, and the clause
        // about the name comes first because its member is declared first.
        case: "an absence beside a value outside its pattern",
        operation: "UpdateTable",
        request: || {
            json!({"TableName": TABLE,
                   "GlobalSecondaryIndexUpdates": [
                       {"Update": {"IndexName": "bad name!",
                                   "ProvisionedThroughput": {"ReadCapacityUnits": 5,
                                                             "WriteCapacityUnits": null}}}]})
        },
        clauses: || {
            vec![
                pattern_clause(
                    "bad name!",
                    "globalSecondaryIndexUpdates.1.member.update.indexName",
                ),
                absent_clause(
                    "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                     writeCapacityUnits",
                ),
            ]
        },
    },
    Row {
        // Several defects spread over two elements of one list, one of them BEHIND the
        // absence in the body: the report counts every one of them.
        case: "two malformed elements, with an absence in each",
        operation: "UpdateTable",
        request: || {
            json!({"TableName": TABLE,
                   "GlobalSecondaryIndexUpdates": [
                       {"Update": {"IndexName": "bad name!",
                                   "ProvisionedThroughput": {"ReadCapacityUnits": null,
                                                             "WriteCapacityUnits": null}}},
                       {"Update": {"IndexName": "cd",
                                   "ProvisionedThroughput": {"ReadCapacityUnits": 5}}}]})
        },
        clauses: || {
            vec![
                pattern_clause(
                    "bad name!",
                    "globalSecondaryIndexUpdates.1.member.update.indexName",
                ),
                absent_clause(
                    "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                     writeCapacityUnits",
                ),
                absent_clause(
                    "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                     readCapacityUnits",
                ),
                too_short_clause(
                    "cd",
                    "globalSecondaryIndexUpdates.2.member.update.indexName",
                    3,
                ),
                absent_clause(
                    "globalSecondaryIndexUpdates.2.member.update.provisionedThroughput.\
                     writeCapacityUnits",
                ),
            ]
        },
    },
    Row {
        // A member of a structure inside a MAP: a batched write's request map carries one
        // list of requests per table, each element a structure whose `PutRequest` requires
        // the item it writes.
        case: "a required map stated null inside a structure in a map",
        operation: "BatchWriteItem",
        request: || {
            json!({"RequestItems": {TABLE: [{"PutRequest": {"Item": null}}]}})
        },
        clauses: || {
            vec![absent_clause(
                "requestItems.Reported.member.1.member.putRequest.item",
            )]
        },
    },
];

#[tokio::test]
async fn a_member_with_no_value_is_reported_as_the_constraint_violation_it_is() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    for row in ROWS {
        let case = row.case;
        let (status, refused) = call(&router, row.operation, (row.request)()).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({}): expected HTTP 400, got {status} with {refused}",
            row.operation
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?} ({}): a member with no value is a constraint violation, got \
             {refused}",
            row.operation
        );
        let expected = envelope(&(row.clauses)());
        let stated = message(&refused);
        assert_eq!(
            stated, expected,
            "case {case:?} ({}): expected the message {expected:?}, got {stated:?}",
            row.operation
        );
    }
}

#[tokio::test]
async fn a_null_is_answered_exactly_as_the_member_left_out() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // (case, operation, the member the body states as absent, the rest of the body)
    //
    // The rows span the modelled type of the member and the operation; each is sent twice,
    // once with the member carried as `null` and once with it left out, and the two answers
    // must be identical — which is what makes a `null` an absence rather than a value.
    let rows: Vec<(&str, &str, &str, Value)> = vec![
        ("a required map, on a read", "GetItem", "Key", json!({"TableName": TABLE})),
        ("a required map, on a write", "PutItem", "Item", json!({"TableName": TABLE})),
        (
            "a required structure, on a time-to-live write",
            "UpdateTimeToLive",
            "TimeToLiveSpecification",
            json!({"TableName": TABLE}),
        ),
        (
            "a required list, on a tag write",
            "TagResource",
            "Tags",
            json!({"ResourceArn": table_arn()}),
        ),
        (
            "a required string, on a tag read",
            "ListTagsOfResource",
            "ResourceArn",
            json!({}),
        ),
        (
            "a required map, on a batched write",
            "BatchWriteItem",
            "RequestItems",
            json!({}),
        ),
    ];

    for (case, operation, member, rest) in rows {
        let mut stating = rest.clone();
        stating[member] = Value::Null;
        let (stated_status, stated) = call(&router, operation, stating.clone()).await;
        let (plain_status, plain) = call(&router, operation, rest.clone()).await;
        assert_eq!(
            (stated_status, &stated),
            (plain_status, &plain),
            "case {case:?} ({operation}): {stating} must be answered exactly as {rest}, got \
             {stated_status} with {stated} against {plain_status} with {plain}"
        );
        assert_eq!(
            stated_status, 400,
            "case {case:?} ({operation}): a member the input requires has no value, so the \
             request is refused, got {stated_status} with {stated}"
        );
    }
}

#[tokio::test]
async fn a_reported_request_applied_nothing() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // One refused request per thing it could have changed: an item written, a tag set, the
    // table's time to live, and a secondary index added to the table.
    for row in ROWS {
        let (status, refused) = call(&router, row.operation, (row.request)()).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the request to be refused, got {status} with {refused}",
            row.case
        );
    }

    let described = ok(&router, "DescribeTable", json!({"TableName": TABLE})).await;
    assert_eq!(
        described["Table"]["TableStatus"],
        json!("ACTIVE"),
        "a refused request left the table settled as it was, got {described}"
    );
    assert!(
        described["Table"].get("GlobalSecondaryIndexes").is_none(),
        "a refused update added no index, got {described}"
    );
    let tags = ok(
        &router,
        "ListTagsOfResource",
        json!({"ResourceArn": table_arn()}),
    )
    .await;
    assert_eq!(
        tags["Tags"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "a refused tag write stored no tag, got {tags}"
    );
    let ttl = ok(&router, "DescribeTimeToLive", json!({"TableName": TABLE})).await;
    assert_eq!(
        ttl["TimeToLiveDescription"]["TimeToLiveStatus"],
        json!("DISABLED"),
        "a refused time-to-live write changed nothing, got {ttl}"
    );
    let scanned = ok(&router, "Scan", json!({"TableName": TABLE})).await;
    assert_eq!(
        scanned["Count"],
        json!(0),
        "a refused write stored no item, got {scanned}"
    );
}
