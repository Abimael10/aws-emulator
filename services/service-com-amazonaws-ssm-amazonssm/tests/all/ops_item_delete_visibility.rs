// SPDX-License-Identifier: CC0-1.0

//! What every read path of OpsCenter reports once a `DeleteOpsItem` has settled.
//!
//! RULE: `DeleteOpsItem` marks the OpsItem its id names in the Region the call was signed
//! for as owed removal, and once that removal is due nothing holds the record any more —
//! the single-OpsItem read is refused, no query of that Region reports it, and every other
//! OpsItem of that Region is untouched. The call reports success whether or not it reached
//! a record ("this operation is idempotent. The system doesn't throw an exception if you
//! repeatedly call this operation for the same OpsItem", `smithy_query(query="doc",
//! shape="DeleteOpsItem")`), and asking again neither fails nor puts the removal off.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call_in_region`], [`error_code`], [`router_with_engine`])
//! dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the
//! multi-service router — and every OpsItem is opened through `all/ops_items.rs`'s
//! [`minimal`] body. No assertion reads the state struct: what a delete did is read back
//! through the operations a client has.
//!
//! A delete is eventually consistent, so the record it marks is removed at a stored due
//! instant ([`SETTLES_AFTER`]). A case moves the store to that instant
//! ([`settle_deletes_due`]) rather than waiting for it, the way this crate's other suites
//! move a due instant on, so nothing here sleeps or depends on how fast the host runs.
//!
//! The first table's rows differ along what the deleted OpsItem was when the delete reached
//! it — its type, the optional members it carries, the status it had been edited to, how
//! many sibling OpsItems share its source, and the Region it lives in — rather than the
//! single OpsItem a recorded unit happened to delete. `all/ops_item_deletion.rs` holds the
//! same rule against the single-OpsItem read and the related-item listing; this suite holds
//! it against the QUERY path and against the due instant a repeated delete leaves.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::ops_items::OPS_ITEM_DELETE_SETTLES_AFTER;
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::ops_items::minimal;
use crate::parameter_store::{call_in_region, error_code, router_with_engine, REGION};

/// The window a delete of an OpsItem settles in
/// (`crate::state::ops_items::OPS_ITEM_DELETE_SETTLES_AFTER`), which the service stores as a
/// due instant rather than sleeping for.
const SETTLES_AFTER: Duration = OPS_ITEM_DELETE_SETTLES_AFTER;

/// The error an OpsItem id the Region holds nothing under is refused with, which is the
/// exception the model declares for the single-OpsItem read (`smithy_query(query="operation",
/// operation="GetOpsItem")`: `OpsItemNotFoundException`, httpError 400).
const NOT_FOUND_CODE: &str = "OpsItemNotFoundException";

/// The status that refusal carries (the same `httpError`).
const NOT_FOUND_STATUS: u16 = 400;

/// Settle every delete the store owed at `at`, as the clock does once the due instant has
/// passed: the removal is materialised from the stored instant, so a case moves the instant
/// the store is asked about instead of waiting out the window.
fn settle_deletes_due(engine: &Arc<engine::InMemoryEngine>, at: SystemTime) {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .settle_ops_item_deletions_due(at);
}

/// `CreateOpsItem` in `region`, asserting it succeeded and answering the minted id.
async fn open(router: &MultiServiceRouter, case: &str, region: &str, body: &Value) -> String {
    let (status, response) = call_in_region(router, region, "CreateOpsItem", body).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateOpsItem {body} must succeed, got {status} {response}"
    );
    response["OpsItemId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: no OpsItemId in {response}"))
        .to_string()
}

/// `DeleteOpsItem` of `ops_item_id` at `region`, asserting it succeeded and reported no
/// members of its own: the modelled `DeleteOpsItemResponse` carries none, so what a delete
/// did is read back through the other operations rather than out of its own response.
async fn delete_ok(router: &MultiServiceRouter, case: &str, region: &str, ops_item_id: &str) {
    let (status, response) = call_in_region(
        router,
        region,
        "DeleteOpsItem",
        &json!({ "OpsItemId": ops_item_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: DeleteOpsItem {ops_item_id} must succeed, got {status} {response}"
    );
    assert_eq!(
        response.as_object().map(serde_json::Map::len),
        Some(0),
        "case {case}: DeleteOpsItem reports no members, got {response}"
    );
}

/// Assert the single-OpsItem read of `ops_item_id` at `region` takes the not-found path.
async fn assert_read_refused(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    ops_item_id: &str,
) {
    let (status, response) = call_in_region(
        router,
        region,
        "GetOpsItem",
        &json!({ "OpsItemId": ops_item_id }),
    )
    .await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case {case}: expected status {NOT_FOUND_STATUS} reading {ops_item_id}, got {status} \
         {response}"
    );
    assert_eq!(
        error_code(&response),
        NOT_FOUND_CODE,
        "case {case}: expected {NOT_FOUND_CODE} reading {ops_item_id}, got {response}"
    );
}

/// The OpsItem ids `DescribeOpsItems` reports at `region` for `filters`, asserting the query
/// succeeded.
async fn queried_ids(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    filters: &[Value],
) -> Vec<String> {
    let (status, response) = call_in_region(
        router,
        region,
        "DescribeOpsItems",
        &json!({ "OpsItemFilters": filters }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: DescribeOpsItems {filters:?} must succeed, got {status} {response}"
    );
    response["OpsItemSummaries"]
        .as_array()
        .map(|summaries| {
            summaries
                .iter()
                .map(|summary| {
                    summary["OpsItemId"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case}: no OpsItemId in {summary}"))
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One filter of a query (`DescribeOpsItems`'s `OpsItemFilters`: a `Key`, an `Operator` and
/// its `Values`).
fn filter(key: &str, operator: &str, values: &[&str]) -> Value {
    json!({ "Key": key, "Operator": operator, "Values": values })
}

/// One row of the table: the OpsItem the delete reaches, and what shares its source.
struct Case<'a> {
    case: &'a str,
    /// The Region the OpsItems are opened in, deleted through and queried at.
    region: &'a str,
    /// The source every OpsItem of this row carries, which is what the query narrows by —
    /// one per row, so the rows cannot read each other's OpsItems.
    source: &'a str,
    /// The members the create names beyond the required three.
    extra: Value,
    /// The edits made before the delete, in order.
    edits: Vec<Value>,
    /// How many OpsItems of the same source are opened beside the one that is deleted.
    siblings: usize,
}

/// The create body for one row, carrying its source and its extra members.
fn body_for(case: &Case<'_>, title: &str) -> Value {
    let mut body = minimal(title);
    body["Source"] = json!(case.source);
    if let Some(extra) = case.extra.as_object() {
        for (member, value) in extra {
            body[member.as_str()] = value.clone();
        }
    }
    body
}

/// Once the delete has settled, no query of the Region reports the OpsItem, the read of it
/// is refused, and the OpsItems that shared its source are reported exactly as before.
#[tokio::test]
async fn a_settled_delete_is_gone_from_every_query_of_its_region() {
    let (router, engine) = router_with_engine();
    let cases = vec![
        Case {
            case: "only the required members, nothing else of its source",
            region: REGION,
            source: "delete-visibility-bare",
            extra: json!({}),
            edits: Vec::new(),
            siblings: 0,
        },
        Case {
            case: "an insight carrying every optional member",
            region: REGION,
            source: "delete-visibility-insight",
            extra: json!({
                "OpsItemType": "/aws/insight",
                "Priority": 1,
                "Severity": "2",
                "Category": "Performance",
                "OperationalData": {
                    "team": { "Value": "platform", "Type": "SearchableString" }
                },
            }),
            edits: Vec::new(),
            siblings: 1,
        },
        Case {
            case: "a change request among two siblings",
            region: REGION,
            source: "delete-visibility-change",
            extra: json!({ "OpsItemType": "/aws/changerequest", "Category": "Recovery" }),
            edits: Vec::new(),
            siblings: 2,
        },
        Case {
            case: "edited to a close before the delete",
            region: REGION,
            source: "delete-visibility-resolved",
            extra: json!({ "Priority": 4 }),
            edits: vec![
                json!({ "Status": "InProgress" }),
                json!({ "Status": "Resolved", "Severity": "4" }),
            ],
            siblings: 3,
        },
        Case {
            case: "in another Region, among siblings",
            region: "eu-central-1",
            source: "delete-visibility-regional",
            extra: json!({ "Severity": "1" }),
            edits: vec![json!({ "Status": "InProgress" })],
            siblings: 2,
        },
    ];

    for case in &cases {
        let Case {
            case: name,
            region,
            source,
            edits,
            siblings,
            ..
        } = case;

        let kept: Vec<String> = {
            let mut kept = Vec::new();
            for index in 0..*siblings {
                let title = format!("A sibling {} of {name}", index + 1);
                kept.push(open(&router, name, region, &body_for(case, &title)).await);
            }
            kept
        };
        let doomed = open(
            &router,
            name,
            region,
            &body_for(case, &format!("The OpsItem deleted for {name}")),
        )
        .await;
        for (step, patch) in edits.iter().enumerate() {
            let mut edit = patch.clone();
            edit["OpsItemId"] = json!(doomed);
            let (status, response) = call_in_region(&router, region, "UpdateOpsItem", &edit).await;
            assert_eq!(
                status, 200,
                "case {name}: edit {} must succeed, got {status} {response}",
                step + 1
            );
        }

        // Every OpsItem of this source is queryable before the delete, the doomed one
        // included.
        let before = queried_ids(
            &router,
            name,
            region,
            &[filter("Source", "Equal", &[source])],
        )
        .await;
        assert!(
            before.contains(&doomed),
            "case {name}: expected the query to report {doomed} before its delete, got {before:?}"
        );
        assert_eq!(
            before.len(),
            siblings + 1,
            "case {name}: expected {} OpsItems of {source} before the delete, got {before:?}",
            siblings + 1
        );

        delete_ok(&router, name, region, &doomed).await;
        settle_deletes_due(&engine, SystemTime::now() + SETTLES_AFTER);

        assert_read_refused(&router, name, region, &doomed).await;

        // The id resolves to nothing in a query that asks for it by id, in one that asks
        // for its source, and in the unfiltered query of its Region.
        let by_id = queried_ids(
            &router,
            name,
            region,
            &[filter("OpsItemId", "Equal", &[doomed.as_str()])],
        )
        .await;
        assert!(
            by_id.is_empty(),
            "case {name}: expected no summary for the deleted {doomed}, got {by_id:?}"
        );
        let by_source = queried_ids(
            &router,
            name,
            region,
            &[filter("Source", "Equal", &[source])],
        )
        .await;
        // Held as sets: which OpsItems a query reports is what this suite is about, and the
        // order it reports them in is `all/ops_item_queries.rs`'s rule.
        let mut reported = by_source.clone();
        reported.sort();
        let mut survivors = kept.clone();
        survivors.sort();
        assert_eq!(
            reported, survivors,
            "case {name}: expected only the OpsItems that were not deleted, got {by_source:?}"
        );
        let unfiltered = queried_ids(&router, name, region, &[]).await;
        assert!(
            !unfiltered.contains(&doomed),
            "case {name}: expected the unfiltered query not to report {doomed}, got \
             {unfiltered:?}"
        );
        for held in &kept {
            assert!(
                unfiltered.contains(held),
                "case {name}: expected the unfiltered query to still report {held}, got \
                 {unfiltered:?}"
            );
        }
    }
}

/// Asking for the same delete again does not put the removal off: the due instant the FIRST
/// call set is the one the removal is owed at, so the record is gone at it however many
/// times the delete was repeated — and a delete asked for once it has settled is served the
/// same way.
#[tokio::test]
async fn asking_for_a_delete_again_does_not_put_the_removal_off() {
    let (router, engine) = router_with_engine();
    for repeats in [1_usize, 2, 5] {
        let case = format!("{repeats} delete(s)");
        let doomed = open(
            &router,
            &case,
            REGION,
            &minimal(&format!("An OpsItem deleted {repeats} time(s)")),
        )
        .await;

        delete_ok(&router, &case, REGION, &doomed).await;
        // The first call's due instant is at the latest this: it ran before this reading of
        // the clock, and the window is measured from when it ran.
        let first_due_at_the_latest = SystemTime::now() + SETTLES_AFTER;
        for again in 1..repeats {
            delete_ok(&router, &format!("{case}: repeat {again}"), REGION, &doomed).await;
        }

        settle_deletes_due(&engine, first_due_at_the_latest);
        assert_read_refused(&router, &case, REGION, &doomed).await;

        // "If the first call is successful, all additional calls return the same successful
        // response as the first call": a delete asked for after the removal settled is
        // served, and brings nothing back.
        delete_ok(&router, &format!("{case}: after settling"), REGION, &doomed).await;
        assert_read_refused(&router, &format!("{case}: after settling"), REGION, &doomed).await;
    }
}
