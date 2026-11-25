// SPDX-License-Identifier: CC0-1.0

//! What the bracketed admissible set of an enum-constraint refusal does NOT depend on.
//!
//! RULE: the set a refusal lists for a value outside an enumerated type is a property of
//! that TYPE alone — its values in the order the front door's own table of them is walked,
//! each value's bucket ascending — so nothing about the REQUEST moves it: not how the
//! refused value was spelled, not which element of a list carried it, not how long that
//! list was, and not which operation or member path reached the type. The clause around it
//! (the count, the echoed value, the member path, the requirement's wording) is the
//! framework's and states what the request actually sent.
//!
//! This suite pins the invariance, where its sibling `enum_value_set_order` pins the order
//! itself for one member per type. The rows differ along what the rule generalises over:
//! the spelling of the refused value (a name no value carries, an admitted value miscased,
//! an admitted value padded, the empty string, text that is itself the set), the POSITION
//! of the failing element in its list and the list's length, and the operation and member
//! path that carried the type — `CreateService`'s `DnsConfig` and `UpdateService`'s nested
//! `Service.DnsConfig`, two members targeting the one enumerated type.
//!
//! The listed set is also held to be a permutation of the values the model declares, and to
//! be neither that declaration order nor a sort of it — the two orders a boundary that
//! listed the values straight off the schema would produce.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production; the harness lives in `cloud_map_core`.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_servicediscovery_route53autonaming_v20170314 as cloudmap;

use crate::cloud_map_core::{dns_namespace, emulator, refusal};

/// `RecordType`, declared `SRV`, `A`, `AAAA`, `CNAME`
/// (`smithy_query(query="shape", shape="RecordType")`), listed in the order the table of
/// those values is walked: each value's bucket ascending, the bucket being the low five bits
/// of its 31-based string hash — `AAAA` 0, `A` 1, `CNAME` 14, `SRV` 23, no two sharing a
/// bucket.
const RECORD_TYPE_SET: &str = "AAAA, A, CNAME, SRV";

/// The counted envelope over one enum clause: the value the request submitted, the member
/// path the clause states it at, and the set the type admits.
fn expected_message(value: &str, path: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy enum value set: [{RECORD_TYPE_SET}]"
    )
}

/// The values a refusal listed, in the order it listed them.
fn listed_set(message: &str, case: &str) -> Vec<String> {
    message
        .split_once("enum value set: [")
        .and_then(|(_, tail)| tail.strip_suffix(']'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a stated value set, got {message:?}"))
        .split(", ")
        .map(str::to_string)
        .collect()
}

/// Refuse `body` on `operation` and hold the whole refusal to the rule: 400 under the one
/// error this model declares for the class, the framework's clause naming the value the
/// request sent at `path`, and the admitted set listed as the type is listed.
async fn assert_refused(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
    value: &str,
    path: &str,
) {
    let (status, code, message) = refusal(router, operation, body).await;
    let message = message.unwrap_or_default();
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidInput"),
        "case {case:?}: expected 400 InvalidInput, got {status} {code} {message:?}"
    );
    let expected = expected_message(value, path);
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );

    // The listing holds exactly the values the model declares — a listing that dropped or
    // invented one could satisfy the sentence above only by being wrong twice over …
    let listed = listed_set(&message, case);
    let held: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
    let declared: BTreeSet<&str> = cloudmap::ops::types::RECORD_TYPE_VALUES
        .iter()
        .copied()
        .collect();
    assert_eq!(
        held, declared,
        "case {case:?}: expected the declared values {declared:?}, got {held:?}"
    );
    // … and it is neither the order the model declares them in nor a sort of them, the two
    // orders a boundary listing the values straight off the schema would state.
    let model_order: Vec<String> = cloudmap::ops::types::RECORD_TYPE_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    assert_ne!(
        listed, model_order,
        "case {case:?}: the set must not be listed in the model's declaration order"
    );
    let mut sorted = model_order.clone();
    sorted.sort_unstable();
    assert_ne!(
        listed, sorted,
        "case {case:?}: the set must not be listed in sorted order"
    );
}

/// A `CreateService` request whose DNS records are `records`, with everything else valid, so
/// a row changes the record list and nothing else.
fn create_service(namespace_id: &str, records: Value) -> Value {
    json!({
        "Name": "enum-invariant-svc",
        "NamespaceId": namespace_id,
        "DnsConfig": { "DnsRecords": records },
    })
}

/// An admitted record, for padding a list out to the length a row needs.
fn good_record() -> Value {
    json!({ "Type": "A", "TTL": 60 })
}

/// One record outside the type, spelled as the row spells it.
fn bad_record(spelling: &str) -> Value {
    json!({ "Type": spelling, "TTL": 60 })
}

/// However a caller spells a value outside the type, the set listed is the type's own.
#[tokio::test]
async fn the_listed_set_does_not_move_with_the_spelling_of_the_refused_value() {
    let router = emulator();
    let namespace_id = dns_namespace(&router, "enum-invariant-spelling").await;

    for (case, spelling) in [
        ("an admitted value in lower case", "srv"),
        ("an admitted value in mixed case", "Srv"),
        ("an admitted value padded in front", " SRV"),
        ("an admitted value padded behind", "AAAA "),
        ("a record type no DNS value carries", "TXT"),
        ("the empty string", ""),
        ("text that is itself the admitted set", "AAAA, A, CNAME, SRV"),
    ] {
        assert_refused(
            &router,
            case,
            "CreateService",
            create_service(&namespace_id, json!([bad_record(spelling)])),
            spelling,
            "dnsConfig.dnsRecords.1.member.type",
        )
        .await;
    }
}

/// Whichever element of however long a list carries the bad value, the set listed is the
/// same — only the member path moves, and it names the element the request put it at.
#[tokio::test]
async fn the_listed_set_does_not_move_with_the_element_that_carried_it() {
    let router = emulator();
    let namespace_id = dns_namespace(&router, "enum-invariant-element").await;

    for (case, before, after) in [
        ("the only record of the list", 0, 0),
        ("the last of two records", 1, 0),
        ("the first of two records", 0, 1),
        ("the middle of three records", 1, 1),
    ] {
        let mut records: Vec<Value> = Vec::new();
        for _ in 0..before {
            records.push(good_record());
        }
        records.push(bad_record("srv"));
        for _ in 0..after {
            records.push(good_record());
        }
        // The clause names the element by its position in the list the request sent,
        // counting from one.
        let path = format!("dnsConfig.dnsRecords.{}.member.type", before + 1);
        assert_refused(
            &router,
            case,
            "CreateService",
            create_service(&namespace_id, Value::Array(records)),
            "srv",
            &path,
        )
        .await;
    }
}

/// Both members this model declares against the type list it alike: the create's own
/// `DnsConfig` and the update's `Service.DnsConfig`, one structure deeper
/// (`smithy_query(query="shape", shape="ServiceChange,DnsConfigChange")`).
#[tokio::test]
async fn the_listed_set_does_not_move_with_the_member_that_reached_the_type() {
    let router = emulator();
    let namespace_id = dns_namespace(&router, "enum-invariant-member").await;

    assert_refused(
        &router,
        "the create's own DNS config",
        "CreateService",
        create_service(&namespace_id, json!([bad_record("srv")])),
        "srv",
        "dnsConfig.dnsRecords.1.member.type",
    )
    .await;

    // The update is refused at the boundary, before the service it names is looked up, so
    // the row needs no stored service to reach the clause.
    assert_refused(
        &router,
        "the update's nested DNS config",
        "UpdateService",
        json!({
            "Id": "srv-enum-invariant",
            "Service": { "DnsConfig": { "DnsRecords": [bad_record("srv")] } },
        }),
        "srv",
        "service.dnsConfig.dnsRecords.1.member.type",
    )
    .await;

    assert_refused(
        &router,
        "the update's nested DNS config, at the second record",
        "UpdateService",
        json!({
            "Id": "srv-enum-invariant",
            "Service": {
                "DnsConfig": { "DnsRecords": [good_record(), bad_record("cname")] },
            },
        }),
        "cname",
        "service.dnsConfig.dnsRecords.2.member.type",
    )
    .await;
}
