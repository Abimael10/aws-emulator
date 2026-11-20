// SPDX-License-Identifier: CC0-1.0

//! STANDARD LOGGING v2 for a viewer that DIALS the address a CloudFront distribution published
//! as its `DomainName` — a socket of its own, no router dispatch and no credential — and what a
//! `DeleteDelivery` then stops.
//!
//! RULE: a vended-log delivery writes one access-log record for every viewer request the
//! SENDING resource's data plane serves, whichever address that request arrived at. The address
//! a service publishes for a client to dial is one this emulator itself answers on
//! (`Distribution$DomainName` is "the domain name that CloudFront assigns when you create a
//! distribution", the one name a viewer is handed), so a request opened against that `host:port`
//! is served by that distribution's edge and is a served request like any other: it becomes ONE
//! record in EVERY live delivery of the distribution's delivery source, carrying exactly that
//! delivery's own `recordFields` with each field's value read off the request and the answer it
//! was given. `DeleteDelivery` "only deletes the connection between the delivery source and
//! delivery destination", so it severs the one delivery it names: records already written to its
//! destination stay where they are, no later dialled request is delivered there, and every
//! sibling delivery of the same source keeps carrying every later one.
//!
//! Only a cross-service suite can state this, and only a suite whose viewer holds a SOCKET: the
//! published address and the edge are CloudFront's, the origin object is Amazon S3's and the
//! records are CloudWatch Logs'. [`super::cloudfront_access_log_deliveries_carry_each_edge_request`]
//! pins the fan-out and the delete for a viewer request DISPATCHED in process with the published
//! name set as its `Host`, which says nothing about whether that name is an address at all, and
//! [`super::cloudfront_dialled_edge_address_over_a_private_s3_origin`] dials the published
//! address but reads no log. The pipeline, the row shape, what a record owes a request, the
//! record reader and the dial are each reused from those two rather than built again here.
//!
//! The rows span what the rule generalises over: the answer each dialled request got (the
//! origin's object under HTTP 200 classified a miss, and a key the bucket does not hold, which
//! the grant does not let the edge list, under HTTP 403 classified an error), crossed with two
//! deliveries on ONE source whose `recordFields` differ in which fields they name, how many and
//! in what order — and then the same address dialled once more after one of those two deliveries
//! has been deleted.
//!
//! Every value asserted is computed from what the row declares or read off the answer the dial
//! received (its status and the identifier the edge stamped on it); the published address, the
//! delivery ids and the request identifiers are the emulator's to mint.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::cloudfront_access_log_deliveries_carry_each_edge_request::{
    expected_value, is_date, is_time, records, records_until, wire, Row, BARRIER_FIELDS,
    FROM_CLOUDFRONT, PRIMARY_FIELDS,
};
use super::cloudfront_dialled_edge_address_over_a_private_s3_origin::{dial, is_dialable};
use super::logs_delivery_source_cloudfront_resource::logs;

/// The bucket this suite's distribution takes its origin from, and the object it holds.
const BUCKET: &str = "cloudfront-dialled-vended-log";
const OBJECT_KEY: &str = "present.txt";
const OBJECT_BYTES: &str = "the object the dialled address serves";

/// The requests of the first phase: two answers of different classes at the one published
/// address.
const ROWS: &[Row] = &[
    Row {
        case: "the origin's object, dialled at the published address",
        path: "/present.txt",
        status: 200,
        result_type: "Miss",
    },
    Row {
        case: "a key the bucket does not hold, which the grant does not let the edge list",
        path: "/absent.txt",
        status: 403,
        result_type: "Error",
    },
];

/// Dial `domain` for `row`'s path and answer the identifier the edge stamped on what came back,
/// having held the answer to the status and the classification the row declares.
async fn dialled(domain: &str, row: &Row) -> String {
    let served = dial(row.case, domain, row.path).await;
    assert_eq!(
        served.status, row.status,
        "case {:?}: expected the published address to answer {}, got {} with {:?}",
        row.case, row.status, served.status, served.body
    );
    let cache = served.header("x-cache");
    let wanted = format!("{}{FROM_CLOUDFRONT}", row.result_type);
    assert_eq!(
        cache, wanted,
        "case {:?}: expected X-Cache {wanted:?} at the published address, got {cache:?}",
        row.case
    );
    let request_id = served.header("x-amz-cf-id");
    assert!(
        !request_id.is_empty(),
        "case {:?}: expected the edge's own X-Amz-Cf-Id on the answer, got {:?}",
        row.case,
        served.raw
    );
    request_id
}

/// Hold `held` — every record one delivery's destination carries — to being exactly one record
/// per dialled request, each joined to its request by the identifier the answer carried and
/// carrying exactly `fields`.
fn assert_records(case: &str, fields: &[String], held: &[Value], served: &[(&Row, String)]) {
    assert_eq!(
        held.len(),
        served.len(),
        "case {case:?}: expected one record per dialled request ({}), got {held:?}",
        served.len()
    );
    let wanted: BTreeSet<&str> = fields.iter().map(String::as_str).collect();
    for (row, request_id) in served {
        let record = held
            .iter()
            .find(|record| record["x-edge-request-id"].as_str() == Some(request_id.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?} / {:?}: no record joins the answer's X-Amz-Cf-Id \
                     {request_id:?}, got {held:?}",
                    row.case
                )
            });
        let named: BTreeSet<&str> = record
            .as_object()
            .map(|members| members.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            named, wanted,
            "case {case:?} / {:?}: expected exactly the delivery's own record fields, got \
             {record}",
            row.case
        );
        for field in fields {
            let got = record[field.as_str()].as_str().unwrap_or_default();
            if let Some(expected) = expected_value(row, request_id, field) {
                assert_eq!(
                    got, expected,
                    "case {case:?} / {:?}: expected {field:?} to be {expected:?}, got {got:?} in \
                     {record}",
                    row.case
                );
                continue;
            }
            // A field with no value of its own to compare is held to the FORM that field is
            // written in, read through the same two predicates the sibling suite uses.
            let well_formed = match field.as_str() {
                "date" => is_date(got),
                "time" => is_time(got),
                _ => !got.is_empty(),
            };
            assert!(
                well_formed,
                "case {case:?} / {:?}: {field:?} is not written as that field is written, got \
                 {got:?} in {record}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn a_viewer_dialling_the_published_address_is_vended_to_every_live_delivery() {
    let pipeline = wire(
        BUCKET,
        &[(OBJECT_KEY, OBJECT_BYTES)],
        &[
            ("the primary delivery", PRIMARY_FIELDS),
            ("the barrier delivery", BARRIER_FIELDS),
        ],
    )
    .await;

    // There is nothing for a viewer to dial unless the published name is an address a client on
    // the machine this emulator runs on can open a connection to.
    assert!(
        is_dialable(&pipeline.domain),
        "expected a host:port a viewer can dial as the published DomainName, got {:?}",
        pipeline.domain
    );

    // Phase one: every row dialled at that one address, and the identifier the edge stamped on
    // each answer, which is what joins an answer to its record.
    let mut served: Vec<(&Row, String)> = Vec::new();
    for row in ROWS {
        served.push((row, dialled(&pipeline.domain, row).await));
    }
    let identifiers: Vec<&str> = served
        .iter()
        .map(|(_, request_id)| request_id.as_str())
        .collect();
    let distinct: BTreeSet<&str> = identifiers.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        ROWS.len(),
        "case \"the dialled phase\": expected {} distinct X-Amz-Cf-Id values, got {identifiers:?}",
        ROWS.len()
    );

    // Both deliveries are live, so BOTH destinations carry every dialled request — each in its
    // own layout.
    for delivery in &pipeline.deliveries {
        let held = records_until(&pipeline.setup, &delivery.log_group, ROWS.len()).await;
        assert_records(delivery.case, &delivery.fields, &held, &served);
    }

    // Phase two: the primary delivery is severed, and the same address is dialled once more.
    // Each is looked up by the case `wire` was called with, so neither is re-bound if that
    // call's layout order ever changes.
    let delivery_named = |case: &str| {
        pipeline
            .deliveries
            .iter()
            .find(|delivery| delivery.case == case)
            .unwrap_or_else(|| panic!("the wired pipeline holds a delivery for {case:?}"))
    };
    let primary = delivery_named("the primary delivery");
    let barrier = delivery_named("the barrier delivery");
    let (status, answered) = logs(
        &pipeline.setup,
        "DeleteDelivery",
        &json!({"id": &primary.id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the delete itself\": expected DeleteDelivery to be accepted, got {status} \
         {answered}"
    );

    let later = &ROWS[0];
    let later_id = dialled(&pipeline.domain, later).await;

    // The sibling delivery on the same source keeps carrying what the published address serves.
    let carried = records_until(&pipeline.setup, &barrier.log_group, ROWS.len() + 1).await;
    assert_eq!(
        carried.len(),
        ROWS.len() + 1,
        "case {:?}: expected the later dialled request to keep landing, got {carried:?}",
        barrier.case
    );
    assert!(
        carried
            .iter()
            .any(|record| record["x-edge-request-id"].as_str() == Some(later_id.as_str())),
        "case {:?}: expected a record for the later dialled request {later_id:?}, got {carried:?}",
        barrier.case
    );

    // And the severed delivery's destination keeps the records it already had and takes none of
    // the later request — read at a point where the sibling has already taken it, so this is not
    // a look taken too early.
    let left = records(&pipeline.setup, &primary.log_group).await;
    assert_records(primary.case, &primary.fields, &left, &served);
    assert!(
        left.iter()
            .all(|record| record["x-edge-request-id"].as_str() != Some(later_id.as_str())),
        "case {:?}: a severed delivery takes no record of a later request, got {left:?}",
        primary.case
    );
}
