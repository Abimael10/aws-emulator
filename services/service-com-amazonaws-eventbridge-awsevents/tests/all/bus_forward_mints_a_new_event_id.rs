// SPDX-License-Identifier: CC0-1.0

//! The `id` a FORWARDED event carries, end to end over the wire (`awsJson1_1`,
//! `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter` with Amazon SQS, IAM and CloudWatch mounted beside
//! EventBridge — so each copy is read where a consumer reads it: off the queue the
//! receiving bus's own rule delivers to.
//!
//! The RULE asserted here: reaching a target whose `Arn` names an event bus is
//! PUBLISHING the event onto that bus, and that publication is a fresh INGESTION
//! there — so the destination bus mints the `id` the copy it matches and delivers
//! carries, while every other member of the envelope is carried over unchanged,
//! the `region` the event was published in included. The id is therefore the
//! handle that tells the forwarded delivery from the original: it is never the
//! `EventId` the publishing `PutEvents` answered, and two copies of one event are
//! never handed the same one.
//!
//! The table spans the class rather than one recorded case: the rows differ along
//! how many buses the event is forwarded onto (one, two), and so along whether the
//! ids of several copies of one event have to differ from each other as well as
//! from the publication's. The Region half of the same rule — a bus of ANOTHER
//! Region, reached through that Region's own endpoint — is
//! `tests/all/cross_region_bus_target.rs`'s table.
//!
//! The harness (the router with EventBridge, Amazon SQS, IAM and CloudWatch on one
//! engine) is `tests/all/target_dispatch.rs`'s, and the bus and rule helpers are
//! `tests/all/bus_forward_authorization.rs`'s.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::SERVICE_PRINCIPAL;

use crate::bus_forward_authorization::{bus_arn, given_bus, given_rule};
use crate::target_dispatch::{
    drain, emulator, events, given_queue, given_role, Emulator, DETAIL_TYPE, SOURCE,
};

/// The bus the event is published onto, and the role the forwards are made under
/// (its own inline policy allows the publication, so no row turns on
/// authorization).
const SOURCE_BUS: &str = "minting-source";
const FORWARDING_ROLE: &str = "minting-forward-role";
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The resource and the detail the published event carries, so a copy is read as
/// the event that was published rather than as one built from it.
const RESOURCE: &str = "arn:aws:s3:::minting-bucket";

fn detail(case: &str) -> Value {
    json!({ "nonce": case, "orderId": "A-1" })
}

/// Publish one event onto `SOURCE_BUS` and answer the `EventId` the publication
/// was given.
async fn publish(emulator: &Emulator, case: &str) -> String {
    let (status, body) = events(
        emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "EventBusName": SOURCE_BUS,
                "Resources": [RESOURCE],
                "Detail": detail(case).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutEvents answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutEvents reported a failed entry: {body}"
    );
    body["Entries"][0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: PutEvents minted no EventId: {body}"))
        .to_string()
}

/// The ONE envelope the queue at `url` holds.
async fn envelope(emulator: &Emulator, case: &str, url: &str) -> Value {
    let messages = drain(emulator, url).await;
    assert_eq!(
        messages.len(),
        1,
        "case {case:?}: expected one delivery on {url}, got {messages:?}"
    );
    let text = messages[0]["Body"].as_str().unwrap_or_default();
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case:?}: the body is not JSON ({error}): {text:?}"))
}

/// A forward mints the copy's own `id` and carries every other member of the
/// envelope over, over a TABLE whose rows differ along how many buses one event is
/// forwarded onto.
#[tokio::test]
async fn a_forwarded_copy_carries_a_new_event_id_and_the_published_envelope() {
    // (case, the destination buses the source bus forwards onto)
    let rows: [(&str, &[&str]); 2] = [
        ("one destination bus", &["minting-one"]),
        ("two destination buses", &["minting-two-a", "minting-two-b"]),
    ];

    for (case, destinations) in rows {
        let emulator = emulator();
        let role_arn = given_role(
            &emulator,
            FORWARDING_ROLE,
            SERVICE_PRINCIPAL,
            Some(PUT_EVENTS_ACTION),
        )
        .await;
        given_bus(&emulator, case, SOURCE_BUS).await;
        // The publishing bus delivers the event to a sink of its own, which is
        // what the copies are compared against: that delivery IS the publication,
        // so it carries the id `PutEvents` answered.
        let (origin_url, origin_arn) = given_queue(&emulator, "minting-origin-sink").await;
        let mut targets = vec![json!({ "Id": "sink", "Arn": origin_arn })];
        let mut sinks: Vec<String> = Vec::new();
        for (index, bus) in destinations.iter().enumerate() {
            given_bus(&emulator, case, bus).await;
            let (url, arn) = given_queue(&emulator, &format!("{bus}-sink")).await;
            sinks.push(url);
            given_rule(
                &emulator,
                case,
                bus,
                &format!("{bus}-rule"),
                json!([{ "Id": "sink", "Arn": arn }]),
            )
            .await;
            targets.push(json!({
                "Id": format!("bus-{index}"),
                "Arn": bus_arn(bus),
                "RoleArn": &role_arn,
            }));
        }
        given_rule(
            &emulator,
            case,
            SOURCE_BUS,
            "forwards",
            Value::Array(targets),
        )
        .await;

        let published = publish(&emulator, case).await;

        let origin = envelope(&emulator, case, &origin_url).await;
        assert_eq!(
            origin["id"],
            json!(published),
            "case {case:?}: the publication's own delivery carries the EventId PutEvents \
             answered, got {origin}"
        );

        let mut minted: BTreeSet<String> = BTreeSet::new();
        for (bus, url) in destinations.iter().zip(&sinks) {
            let copy = envelope(&emulator, case, url).await;
            let delivered_id = copy["id"].as_str().unwrap_or_default().to_string();
            assert!(
                !delivered_id.is_empty() && delivered_id != published,
                "case {case:?}: expected the copy on {bus} to carry an id of its own, got \
                 {delivered_id:?} for a publication of {published:?}"
            );
            assert!(
                minted.insert(delivered_id.clone()),
                "case {case:?}: expected the copy on {bus} to carry an id no other copy of this \
                 event carries, got {delivered_id:?} again"
            );
            // Everything BUT the id is the published event's, so the copy is the
            // event that was published rather than one built from it.
            for member in [
                "version",
                "source",
                "detail-type",
                "detail",
                "resources",
                "account",
                "region",
                "time",
            ] {
                assert_eq!(
                    copy[member], origin[member],
                    "case {case:?}: expected the copy on {bus} to carry the published {member} \
                     {}, got {}",
                    origin[member], copy[member]
                );
            }
        }
    }
}
