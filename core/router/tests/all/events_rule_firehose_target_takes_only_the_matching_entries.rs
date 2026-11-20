// SPDX-License-Identifier: CC0-1.0

//! What a rule's Amazon Data Firehose delivery-stream target receives out of ONE multi-entry
//! `PutEvents`: the entries that rule's `EventPattern` matched, and nothing else.
//!
//! The rule pinned here: **`PutEvents` judges EVERY entry of the batch against EVERY rule's
//! `EventPattern` on its own, and a rule is handed exactly the entries its pattern matched
//! ("Sends custom events to Amazon EventBridge so that they can be matched to rules") \u2014 so for
//! a rule whose target is a delivery stream, the record EventBridge puts with
//! `firehose:PutRecord` is made once per MATCHED entry and never for an entry the pattern did
//! not match, and the stream's Amazon S3 destination therefore comes to hold the matched
//! entries of that batch and no other.**
//!
//! That makes the negative leg gradeable rather than vacuous: every row of the table below
//! publishes the SAME batch, so a prefix that is missing an entry and a prefix that holds one
//! too many are both failures of the row, and a target that dropped everything would fail the
//! positive expectations the same batch carries. The counts are judged in a second pass, once
//! EVERY row that delivers has delivered, so "one too many" is read with every delivery of the
//! publication already made rather than while one is still in flight.
//!
//! Only a cross-service suite can ask this: the flow spans four services over ONE engine
//! exactly as `core/router/src/lib.rs` mounts them \u2014 EventBridge for the rules and the
//! publication, IAM for the role the put is made under, Firehose for the streams, and Amazon S3
//! for the objects their deliveries write. The sibling suite
//! `events_rule_firehose_target_delivery.rs` pins which ROLE may make that put for a single
//! matching event; this one pins which ENTRIES reach the stream at all, and reuses that suite's
//! service set, Region, role builder and wire encoder rather than restating them.
//!
//! The rows differ along what the rule generalises over: which envelope member the pattern
//! filters on (`source`, `detail-type`, a member nested inside `detail`), how many values the
//! filter lists, whether two members are required together, the FORM of the comparison (exact
//! values, a numeric range), and how many entries of the batch the pattern ends up matching
//! (two, three, one, none). Nothing is asserted about how fast the host ran: every wait looks
//! again for the CONDITION under a generous deadline.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::events_rule_fans_out_to_firehose_and_queue_targets::{
    delivered_object, events, firehose, given_an_active_stream, keys_under, DEADLINE,
    EVENTS_PRINCIPAL, LOOK_AGAIN, PUT_RECORD_ACTION,
};
use crate::events_rule_firehose_target_delivery::{emulator, given_role, send, REGION};

/// The bucket every delivery stream of this suite writes into.
const BUCKET: &str = "events-firehose-pattern-selectivity";

/// The role every STREAM delivers to that bucket under: Firehose has to be able to assume it,
/// and it has to allow the object write the delivery makes.
const DELIVERY_ROLE: &str = "selectivity-firehose-delivery-role";

/// The role EVERY row's target is invoked under, so the one thing that differs between the
/// rows is the pattern each rule carries. The principal EventBridge assumes it as, and the call
/// it has to allow, are the ones the sibling fan-out suite already names.
const TARGET_ROLE: &str = "selectivity-events-target-role";

/// The marker each published entry carries in `detail.marker`, unique per entry: this is how a
/// delivered object is traced back to the entry that produced it.
const ALPHA_CREATED: &str = "marker-alpha-created";
const BETA_CREATED: &str = "marker-beta-created";
const ALPHA_CANCELLED: &str = "marker-alpha-cancelled";
const GAMMA_DISPATCHED: &str = "marker-gamma-dispatched";

/// One entry of the ONE batch every row below is judged against.
struct Entry {
    source: &'static str,
    detail_type: &'static str,
    /// What `detail.marker` carries.
    marker: &'static str,
    /// What `detail.tier` carries, the nested member one row's pattern filters on.
    tier: &'static str,
    /// What `detail.amount` carries, the nested member one row's numeric range compares.
    amount: i64,
}

/// The batch: four entries differing in source, detail type and the detail members the
/// patterns below reach into.
fn entries() -> Vec<Entry> {
    vec![
        Entry {
            source: "com.demo.alpha",
            detail_type: "Order Created",
            marker: ALPHA_CREATED,
            tier: "gold",
            amount: 40,
        },
        Entry {
            source: "com.demo.beta",
            detail_type: "Order Created",
            marker: BETA_CREATED,
            tier: "silver",
            amount: 10,
        },
        Entry {
            source: "com.demo.alpha",
            detail_type: "Order Cancelled",
            marker: ALPHA_CANCELLED,
            tier: "gold",
            amount: 250,
        },
        Entry {
            source: "com.demo.gamma",
            detail_type: "Shipment Dispatched",
            marker: GAMMA_DISPATCHED,
            tier: "bronze",
            amount: 5,
        },
    ]
}

/// Every marker the batch publishes, which is the set each row's destination is judged
/// against: the markers the row expects have to be there, and every other one must not be.
fn every_marker() -> Vec<&'static str> {
    entries().into_iter().map(|entry| entry.marker).collect()
}

/// One row: the `EventPattern` a rule carries, and the entries of the batch that pattern
/// matches.
struct Row {
    case: &'static str,
    /// Names this row's rule, its stream and its destination prefix, so rows cannot collide.
    name: &'static str,
    /// The pattern, as the JSON text `PutRule$EventPattern` takes.
    pattern: Value,
    /// The markers of the entries this row's target must receive, in publication order.
    delivers: &'static [&'static str],
}

/// The table. Every row's rule is written against the SAME batch, so what each destination
/// holds is decided by its pattern alone.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a pattern listing one source",
            name: "one-source",
            pattern: json!({ "source": ["com.demo.alpha"] }),
            delivers: &[ALPHA_CREATED, ALPHA_CANCELLED],
        },
        Row {
            case: "a pattern listing two sources",
            name: "two-sources",
            pattern: json!({ "source": ["com.demo.alpha", "com.demo.beta"] }),
            delivers: &[ALPHA_CREATED, BETA_CREATED, ALPHA_CANCELLED],
        },
        Row {
            case: "a pattern filtering on the detail type alone",
            name: "detail-type-only",
            pattern: json!({ "detail-type": ["Order Created"] }),
            delivers: &[ALPHA_CREATED, BETA_CREATED],
        },
        Row {
            case: "a pattern requiring the source and the detail type together",
            name: "source-and-detail-type",
            pattern: json!({
                "source": ["com.demo.alpha"],
                "detail-type": ["Order Cancelled"],
            }),
            delivers: &[ALPHA_CANCELLED],
        },
        Row {
            case: "a pattern filtering on a member nested inside the detail",
            name: "nested-detail-member",
            pattern: json!({ "detail": { "tier": ["bronze"] } }),
            delivers: &[GAMMA_DISPATCHED],
        },
        Row {
            case: "a pattern comparing a nested detail member against a numeric range",
            name: "numeric-detail-member",
            pattern: json!({ "detail": { "amount": [{ "numeric": [">=", 100] }] } }),
            delivers: &[ALPHA_CANCELLED],
        },
        Row {
            case: "a pattern listing a source no entry of the batch carries",
            name: "unmatched-source",
            pattern: json!({ "source": ["com.demo.delta"] }),
            delivers: &[],
        },
    ]
}

/// The text `prefix` holds once every marker of `wanted` is in it — or a failure naming the row
/// once [`DEADLINE`] has passed.
///
/// The reading itself is the sibling fan-out suite's `delivered_object`, which lists the prefix
/// and concatenates what it holds in key order (a stream's buffer decides how many objects one
/// delivery writes, so what a row is judged on is the whole prefix rather than any one key);
/// what this adds is the predicate the wait is for.
async fn delivered_under(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    wanted: &[&str],
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = delivered_object(router, case, BUCKET, prefix).await;
        if wanted.iter().all(|marker| body.contains(marker)) {
            return body;
        }
        let missing: Vec<&str> = wanted
            .iter()
            .copied()
            .filter(|marker| !body.contains(marker))
            .collect();
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the matched entries {wanted:?} under {prefix:?}, still \
             missing {missing:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A rule's Firehose target is handed the entries of a batch its pattern matched, and no
/// others.
#[tokio::test]
async fn an_events_rules_firehose_target_takes_only_the_entries_its_pattern_matched() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let rows = rows();
    let published_entries = entries();
    let markers = every_marker();

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
    let target_role = given_role(
        &router,
        "setup",
        TARGET_ROLE,
        EVENTS_PRINCIPAL,
        Some(PUT_RECORD_ACTION),
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

    // Every row's rule, its one target naming that row's stream under the one shared role.
    for row in &rows {
        let case = row.case;
        events(
            &router,
            case,
            "PutRule",
            json!({
                "Name": row.name,
                "EventPattern": row.pattern.to_string(),
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
                "Targets": [{
                    "Id": "stream",
                    "Arn": format!(
                        "arn:aws:firehose:{REGION}:{account}:deliverystream/{}",
                        row.name
                    ),
                    "RoleArn": target_role,
                }],
            }),
        )
        .await;
        assert_eq!(
            answered["FailedEntryCount"],
            json!(0),
            "case {case:?}: the target must be accepted, got {answered}"
        );
    }

    // Firehose refuses a record put at a stream that is still being created, so the
    // publication waits for every stream to REPORT that it is active.
    for row in &rows {
        given_an_active_stream(&router, row.case, row.name).await;
    }

    // ONE publication carrying every entry of the batch: what each row's pattern matches is the
    // only thing that differs between the rows.
    let batch: Vec<Value> = published_entries
        .iter()
        .map(|entry| {
            json!({
                "Source": entry.source,
                "DetailType": entry.detail_type,
                "Detail": json!({
                    "marker": entry.marker,
                    "tier": entry.tier,
                    "amount": entry.amount,
                })
                .to_string(),
            })
        })
        .collect();
    let published = events(
        &router,
        "the publication",
        "PutEvents",
        json!({ "Entries": batch }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "the publication must be accepted whole: {published}"
    );

    // The rows that DELIVER first: every one of them is waited for before any count is judged,
    // so each destination is then read with every delivery of this publication already made —
    // an entry one too many is as gradeable as one missing.
    for row in rows.iter().filter(|row| !row.delivers.is_empty()) {
        let prefix = format!("{}/", row.name);
        delivered_under(&router, row.case, &prefix, row.delivers).await;
    }
    for row in rows.iter().filter(|row| !row.delivers.is_empty()) {
        let case = row.case;
        let prefix = format!("{}/", row.name);
        let body = delivered_object(&router, case, BUCKET, &prefix).await;
        for marker in &markers {
            let expected = usize::from(row.delivers.contains(marker));
            let seen = body.matches(marker).count();
            assert_eq!(
                seen, expected,
                "case {case:?}: the entry {marker:?} is delivered once if the pattern \
                 {} matched it and never otherwise; expected {expected} occurrence(s) under \
                 {prefix:?}, got {seen} in {body:?}",
                row.pattern
            );
        }
    }

    // And a rule whose pattern matched no entry of the batch put no record at all, so its
    // destination holds nothing — while every row above delivered from the same publication.
    for row in rows.iter().filter(|row| row.delivers.is_empty()) {
        let case = row.case;
        let prefix = format!("{}/", row.name);
        let keys = keys_under(&router, case, BUCKET, &prefix).await;
        assert!(
            keys.is_empty(),
            "case {case:?}: a pattern matching no entry of the batch reaches no stream, so its \
             destination holds nothing; got {keys:?}"
        );
    }
}
