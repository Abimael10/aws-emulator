// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service lists an enumerated type's admissible values in when it refuses a
//! value outside that type.
//!
//! RULE: the set is listed in the order the front door's own TABLE of those values is walked
//! — each value's bucket ascending, the bucket being the low bits of the value's string hash
//! (the 31-polynomial over its UTF-16 code units, at the table's width), and two values of
//! one bucket read from the head each was pushed onto, so the later-declared one comes first
//! — and NOT in the order the model declares the type's values in, nor a sort of them.
//!
//! The order belongs to the TYPE, so every member carrying one enum is listed alike whatever
//! the operation and however deep the member sits. The rows below differ along what the rule
//! generalises over: which enumerated type failed, whether its values collide in a bucket
//! (`NamespaceFilterName` has one collision, the other two none), whether the stated order
//! happens to be the declared one, its reverse or neither, which operation carried it, and
//! how deep in the request the member sits — a list element of a top-level list, a member of
//! that element, and a list element two structures down.
//!
//! Each expected set is checked to be a PERMUTATION of the values the model declares, read
//! from the generated `ops::types` lists, so a listing here cannot drop or invent a value
//! and pass by being wrong twice over.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production; the harness lives in `cloud_map_core`.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_servicediscovery_route53autonaming_v20170314 as cloudmap;

use crate::cloud_map_core::{dns_namespace, emulator, refusal};

/// `NamespaceFilterName`, declared `TYPE`, `NAME`, `HTTP_NAME`, `RESOURCE_OWNER`
/// (`smithy_query(query="shape", shape="NamespaceFilterName")`): `HTTP_NAME` and
/// `RESOURCE_OWNER` share a bucket, so the later-declared `RESOURCE_OWNER` is listed first,
/// and the other two follow in ascending bucket.
const NAMESPACE_FILTER_NAME_SET: &str = "RESOURCE_OWNER, HTTP_NAME, NAME, TYPE";

/// `FilterCondition`, declared `EQ`, `IN`, `BETWEEN`, `BEGINS_WITH`
/// (`smithy_query(query="shape", shape="FilterCondition")`): no two values share a bucket,
/// and the buckets rise in an order that is neither the declaration nor its reverse.
const FILTER_CONDITION_SET: &str = "IN, BETWEEN, EQ, BEGINS_WITH";

/// `RecordType`, declared `SRV`, `A`, `AAAA`, `CNAME`
/// (`smithy_query(query="shape", shape="RecordType")`): no collision here either.
const RECORD_TYPE_SET: &str = "AAAA, A, CNAME, SRV";

/// The clause the boundary states for a value outside an enumerated type, under the count of
/// how many values failed.
fn expected_message(value: &str, path: &str, value_set: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy enum value set: [{value_set}]"
    )
}

/// The set a refusal listed, in the order it listed it.
fn listed_set(message: &str, case: &str) -> Vec<String> {
    message
        .split_once("enum value set: [")
        .and_then(|(_, tail)| tail.strip_suffix(']'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a stated value set, got {message:?}"))
        .split(", ")
        .map(str::to_string)
        .collect()
}

/// One row: the request to make, the value outside its enumerated type, the member path the
/// refusal states it at, the set it must list and the model's own list of that type's values.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
    value: &'static str,
    path: &'static str,
    value_set: &'static str,
    declared: &'static [&'static str],
}

/// Every enumerated type a request of this service can carry a bad value for, each at a
/// member of its own, listed in the order the table of its values is walked.
#[tokio::test]
async fn an_enum_refusal_lists_the_set_in_the_order_its_table_is_walked() {
    let router = emulator();
    let namespace_id = dns_namespace(&router, "enum-order").await;

    let cases: Vec<Row> = vec![
        Row {
            case: "a filter name outside NamespaceFilterName",
            operation: "ListNamespaces",
            body: json!({ "Filters": [{ "Name": "NOT_A_REAL_FILTER_NAME", "Values": ["x"] }] }),
            value: "NOT_A_REAL_FILTER_NAME",
            path: "filters.1.member.name",
            value_set: NAMESPACE_FILTER_NAME_SET,
            declared: cloudmap::ops::types::NAMESPACE_FILTER_NAME_VALUES,
        },
        Row {
            case: "a filter condition outside FilterCondition",
            operation: "ListNamespaces",
            body: json!({
                "Filters": [{ "Name": "NAME", "Values": ["x"], "Condition": "SOUNDS_LIKE" }]
            }),
            value: "SOUNDS_LIKE",
            path: "filters.1.member.condition",
            value_set: FILTER_CONDITION_SET,
            declared: cloudmap::ops::types::FILTER_CONDITION_VALUES,
        },
        Row {
            // Two structures down: the record list of the service's DnsConfig.
            case: "a DNS record type outside RecordType",
            operation: "CreateService",
            body: json!({
                "Name": "enum-order-svc",
                "NamespaceId": namespace_id,
                "DnsConfig": { "DnsRecords": [{ "Type": "TXT", "TTL": 60 }] },
            }),
            value: "TXT",
            path: "dnsConfig.dnsRecords.1.member.type",
            value_set: RECORD_TYPE_SET,
            declared: cloudmap::ops::types::RECORD_TYPE_VALUES,
        },
    ];

    for row in cases {
        let Row {
            case,
            operation,
            body,
            value,
            path,
            value_set,
            declared,
        } = row;
        let (status, code, message) = refusal(&router, operation, body).await;
        let message = message.unwrap_or_default();
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidInput"),
            "case {case:?}: expected 400 InvalidInput, got {status} {code} {message:?}"
        );
        let expected = expected_message(value, path, value_set);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // The set LISTED holds exactly the values the model declares, whatever order it is
        // listed in: a listing that dropped or invented one would satisfy the string above
        // only by being wrong in both places at once.
        let listed: BTreeSet<String> = listed_set(&message, case).into_iter().collect();
        let modelled: BTreeSet<String> = declared.iter().map(|value| (*value).to_string()).collect();
        assert_eq!(
            listed, modelled,
            "case {case:?}: expected the declared values {modelled:?}, got {listed:?}"
        );
    }
}

/// One type reads the same way wherever it is carried: `FilterCondition` is a member of the
/// namespace, service and operation filters alike, and each refusal lists its values in the
/// one order.
#[tokio::test]
async fn one_type_reads_the_same_way_under_every_member_carrying_it() {
    let router = emulator();

    for (case, operation, filter_name) in [
        ("a namespace filter", "ListNamespaces", "NAME"),
        ("a service filter", "ListServices", "NAMESPACE_ID"),
        ("an operation filter", "ListOperations", "NAMESPACE_ID"),
    ] {
        let body = json!({
            "Filters": [{ "Name": filter_name, "Values": ["ns-0"], "Condition": "SOUNDS_LIKE" }]
        });
        let (status, code, message) = refusal(&router, operation, body).await;
        let message = message.unwrap_or_default();
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidInput"),
            "case {case:?}: expected 400 InvalidInput, got {status} {code} {message:?}"
        );
        let expected =
            expected_message("SOUNDS_LIKE", "filters.1.member.condition", FILTER_CONDITION_SET);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}
