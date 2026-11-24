// SPDX-License-Identifier: CC0-1.0

//! That `UpdateContributorInsights` is asynchronous in BOTH directions: an accepted call
//! answers the TRANSITIONAL status of the action it was given and the clock owes the move to
//! that action's settled status.
//!
//! The rule pinned here, over every accepted action rather than over one of them: the call
//! answers the transitional member of `ContributorInsightsStatus` for its own direction —
//! `ENABLING` for an `ENABLE`, `DISABLING` for a `DISABLE`
//! (`smithy_query(query="shape", shape="ContributorInsightsStatus")` gives DISABLED |
//! DISABLING | ENABLED | ENABLING | FAILED) — never the settled one, and
//! `DescribeContributorInsights` reports that settled status (`ENABLED` / `DISABLED`) only
//! once the clock has made the move due. A client polling the read is therefore told when the
//! resource's CloudWatch insight rules are the ones the action asked for, and the timestamp
//! the read carries ("Timestamp of the last time the status was changed",
//! `DescribeContributorInsightsOutput$LastUpdateDateTime`) moves on with the status.
//!
//! The rule is asserted over a SET of inputs rather than one: both directions, each resource
//! one table carries (the table itself and a global secondary index), both values
//! `ContributorInsightsMode` declares and naming none at all, and a disable that names a mode
//! of its own as well as one that names none. The sequence each resource is walked through
//! alternates the directions, so every row's action is performed from the status its
//! predecessor settled on.
//!
//! Every call goes over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::ops::types::CONTRIBUTOR_INSIGHTS_STATUS_VALUES;
use smithy_server_runtime::MultiServiceRouter;

use crate::contributor_insights::{create_table, describe_insights};
use crate::emulator::{call, describe_when_active, emulator, insights_when, ok, REGION};

/// The member a response omits reads as `Null`, whether the body left it out or carried it
/// null.
fn reported(body: &Value, member: &str) -> Value {
    body.get(member).cloned().unwrap_or(Value::Null)
}

/// The epoch seconds a report's `LastUpdateDateTime` carries, or `None` where the report
/// omits the member.
fn last_update(report: &Value) -> Option<f64> {
    report.get("LastUpdateDateTime").and_then(Value::as_f64)
}

/// One `UpdateContributorInsights`, with the status and body it answered.
async fn update(
    router: &MultiServiceRouter,
    table: &str,
    index: Option<&str>,
    action: &str,
    mode: Option<&str>,
) -> (u16, Value) {
    let mut request = json!({"TableName": table, "ContributorInsightsAction": action});
    if let Some(index) = index {
        request["IndexName"] = json!(index);
    }
    if let Some(mode) = mode {
        request["ContributorInsightsMode"] = json!(mode);
    }
    call(router, "UpdateContributorInsights", request).await
}

/// Whether the region's listing reports a summary for one resource, as `table`/`index`.
async fn listed(router: &MultiServiceRouter, table: &str, index: Option<&str>) -> bool {
    let answered = ok(
        router,
        "ListContributorInsights",
        json!({"TableName": table}),
    )
    .await;
    answered["ContributorInsightsSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|summary| {
            summary["TableName"] == json!(table)
                && summary["IndexName"] == index.map_or(Value::Null, |index| json!(index))
        })
}

/// One row of the sequence: the action asked for, the mode the request names, the status the
/// call must answer, the status the clock must settle it on, and the mode in force after it.
struct Step {
    case: &'static str,
    action: &'static str,
    mode: Option<&'static str>,
    transitional: &'static str,
    settled: &'static str,
    mode_after: &'static str,
}

/// The sequence every resource is walked through: the two actions alternating, so each one is
/// performed from the status its predecessor settled on, differing in the mode the enable
/// configures (the two values the model declares and naming none) and in whether the disable
/// names a mode of its own — which it does not apply, the configuration outliving it.
const STEPS: &[Step] = &[
    Step {
        case: "the first enable, no mode named",
        action: "ENABLE",
        mode: None,
        transitional: "ENABLING",
        settled: "ENABLED",
        mode_after: "ACCESSED_AND_THROTTLED_KEYS",
    },
    Step {
        case: "the disable of it",
        action: "DISABLE",
        mode: None,
        transitional: "DISABLING",
        settled: "DISABLED",
        mode_after: "ACCESSED_AND_THROTTLED_KEYS",
    },
    Step {
        case: "an enable narrowed to throttled keys",
        action: "ENABLE",
        mode: Some("THROTTLED_KEYS"),
        transitional: "ENABLING",
        settled: "ENABLED",
        mode_after: "THROTTLED_KEYS",
    },
    Step {
        case: "a disable that names the wider mode of its own",
        action: "DISABLE",
        mode: Some("ACCESSED_AND_THROTTLED_KEYS"),
        transitional: "DISABLING",
        settled: "DISABLED",
        mode_after: "THROTTLED_KEYS",
    },
    Step {
        case: "an enable widened to all access again",
        action: "ENABLE",
        mode: Some("ACCESSED_AND_THROTTLED_KEYS"),
        transitional: "ENABLING",
        settled: "ENABLED",
        mode_after: "ACCESSED_AND_THROTTLED_KEYS",
    },
    Step {
        case: "the disable of that one",
        action: "DISABLE",
        mode: None,
        transitional: "DISABLING",
        settled: "DISABLED",
        mode_after: "ACCESSED_AND_THROTTLED_KEYS",
    },
];

/// The index every table in this suite carries, so a row can address an index as well as a
/// table.
const INDEX: &str = "Transitional.Index";

#[tokio::test]
async fn every_accepted_action_on_a_table_answers_its_transitional_status() {
    walk_the_sequence("the table", "InsightsTransitionalTable", None).await;
}

#[tokio::test]
async fn every_accepted_action_on_an_index_answers_its_transitional_status() {
    walk_the_sequence("its index", "InsightsTransitionalIndex", Some(INDEX)).await;
}

/// Walk one resource through [`STEPS`], asserting the rule on every step: the accepted call
/// answers the transitional status of its own direction and the clock settles it on that
/// direction's other status.
///
/// One resource per test, each on its own table, so the two walk their sequences
/// independently: what a row reads is what its own calls left.
async fn walk_the_sequence(resource: &str, table: &str, index: Option<&str>) {
    let router = emulator();
    create_table(&router, table, &[INDEX]).await;
    // The table's own create is asynchronous too, so it is waited out the way a client's
    // waiter does before the actions below are asked for.
    describe_when_active(&router, resource, table).await;

    for step in STEPS {
        let case = format!("{resource}, {}", step.case);
        // Both statuses are members the model declares, which is what makes the
        // transitional one a status a client may branch on.
        assert!(
            CONTRIBUTOR_INSIGHTS_STATUS_VALUES.contains(&step.transitional)
                && CONTRIBUTOR_INSIGHTS_STATUS_VALUES.contains(&step.settled)
                && step.transitional != step.settled,
            "case {case:?}: the statuses of a direction are two distinct declared members, \
             got {:?} and {:?} against {CONTRIBUTOR_INSIGHTS_STATUS_VALUES:?}",
            step.transitional,
            step.settled
        );

        let (code, answered) = update(&router, table, index, step.action, step.mode).await;
        assert_eq!(
            (
                code,
                reported(&answered, "ContributorInsightsStatus"),
                reported(&answered, "ContributorInsightsMode"),
            ),
            (200, json!(step.transitional), json!(step.mode_after)),
            "case {case:?}: the accepted call answers its transitional status with the mode \
             then configured, got {code} with {answered}"
        );

        // A read straight after reports a status of the SAME change — the transitional one
        // while the move is still owed, the settled one once the clock has made it due —
        // and never the status of the action's opposite.
        let in_flight = describe_insights(&router, table, index).await;
        let reading = reported(&in_flight, "ContributorInsightsStatus");
        assert!(
            reading == json!(step.transitional) || reading == json!(step.settled),
            "case {case:?}: a read reports the transitional status or the settled one, got \
             {in_flight}"
        );

        // The listing follows the ACCEPTED action, not the clock: a resource an enable is
        // being applied to has a configuration to report from the call on, and one a
        // disable is being applied to has none from the call on.
        let enabling = step.action == "ENABLE";
        let summarised = listed(&router, table, index).await;
        assert_eq!(
            summarised, enabling,
            "case {case:?}: the listing reports a summary for the resource exactly while its \
             insights are being turned on or are on, got {summarised}"
        );

        // The clock settles the change on the other status of the same direction, with the
        // configuration the call left in force, and the timestamp of that later status is
        // no earlier than the one the call's own status carried.
        let settled = insights_when(&router, &case, table, index, step.settled).await;
        assert_eq!(
            (
                reported(&settled, "ContributorInsightsStatus"),
                reported(&settled, "ContributorInsightsMode"),
            ),
            (json!(step.settled), json!(step.mode_after)),
            "case {case:?}: the clock settles the change on the other status of its own \
             direction, got {settled}"
        );
        let reached_at = last_update(&settled).unwrap_or_default();
        let requested_at = last_update(&in_flight).unwrap_or_default();
        assert!(
            reached_at >= requested_at && requested_at > 0.0,
            "case {case:?}: the settled status was reached no earlier than the transitional \
             one, got {reached_at} after {requested_at} in {settled}"
        );
    }
}

#[tokio::test]
async fn the_disable_of_one_resource_leaves_every_other_resource_where_it_was() {
    // The rule holds per RESOURCE: the transitional status a disable leaves is the addressed
    // resource's own, so a table's disable settles the table and reaches neither its indexes
    // nor another table. Rows are the resources of one region, each read after every step.
    let router = emulator();
    const TABLE: &str = "InsightsScopedTransition";
    const OTHER: &str = "InsightsUntouchedTransition";
    const FIRST: &str = "Scoped.One";
    const SECOND: &str = "Scoped.Two";
    create_table(&router, TABLE, &[FIRST, SECOND]).await;
    create_table(&router, OTHER, &[FIRST]).await;
    describe_when_active(&router, "the table is made", TABLE).await;
    describe_when_active(&router, "the other table is made", OTHER).await;

    // Every resource is turned on and waited out, so the disable below is performed from the
    // one status it may be performed from on each of them.
    let resources: [(&str, Option<&str>); 4] = [
        (TABLE, None),
        (TABLE, Some(FIRST)),
        (TABLE, Some(SECOND)),
        (OTHER, None),
    ];
    for (table, index) in resources {
        let (code, _) = update(&router, table, index, "ENABLE", None).await;
        assert_eq!(code, 200, "case {table}/{index:?}: the enable is accepted");
    }
    for (table, index) in resources {
        insights_when(&router, "the enables settle", table, index, "ENABLED").await;
    }

    // One resource is disabled, and every other one still reports the settled enable while
    // that disable is in flight and after it has settled.
    let (code, answered) = update(&router, TABLE, Some(FIRST), "DISABLE", None).await;
    assert_eq!(
        (code, reported(&answered, "ContributorInsightsStatus")),
        (200, json!("DISABLING")),
        "case the index disabled: its own call answers the transitional status, got {code} with \
         {answered}"
    );
    for (table, index) in resources {
        if (table, index) == (TABLE, Some(FIRST)) {
            continue;
        }
        let described = describe_insights(&router, table, index).await;
        assert_eq!(
            reported(&described, "ContributorInsightsStatus"),
            json!("ENABLED"),
            "case {table}/{index:?}: another resource's disable leaves it where it was, got \
             {described}"
        );
    }
    let settled = insights_when(
        &router,
        "the index disabled",
        TABLE,
        Some(FIRST),
        "DISABLED",
    )
    .await;
    assert_eq!(
        reported(&settled, "ContributorInsightsStatus"),
        json!("DISABLED"),
        "case the index disabled: the clock settles it off, got {settled}"
    );
    for (table, index) in resources {
        if (table, index) == (TABLE, Some(FIRST)) {
            continue;
        }
        let described = describe_insights(&router, table, index).await;
        assert_eq!(
            (
                reported(&described, "ContributorInsightsStatus"),
                described["TableName"].clone(),
            ),
            (json!("ENABLED"), json!(table)),
            "case {table}/{index:?}: it is still on once that disable has settled, got {described}"
        );
    }
    // And the region's own listing reports exactly the resources still on, in the one
    // ascending order it pages in.
    let listing = ok(&router, "ListContributorInsights", json!({})).await;
    let reported_resources: Vec<String> = listing["ContributorInsightsSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| {
            format!(
                "{}{}",
                summary["TableName"].as_str().unwrap_or_default(),
                summary["IndexName"]
                    .as_str()
                    .map(|index| format!("/{index}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        reported_resources,
        vec![
            TABLE.to_string(),
            format!("{TABLE}/{SECOND}"),
            OTHER.to_string(),
        ],
        "case the listing of {REGION}: the disabled resource is the only one out of it, got \
         {listing}"
    );
}
