// SPDX-License-Identifier: CC0-1.0

//! What `UpdateContributorInsights` applies when the action it carries asks for the direction
//! the addressed resource's key access insights are ALREADY in, end to end over HTTP.
//!
//! The rule under test: an accepted action that asks for NO change applies nothing — no
//! CloudWatch rule set is rebuilt and no transition is owed — so the status the call answers is
//! the one in FORCE, not a fresh transient one. An action that DOES ask for a change (the other
//! direction, or an enable naming a mode the resource is not configured under) is applied
//! asynchronously, so it answers the transient status of its own direction and the clock owes
//! the settled one a span later. Both halves hold for the table's own setting and for a global
//! secondary index's, which carry their own configuration each
//! (`smithy_query(query="operation",operation="UpdateContributorInsights")`: `TableName`
//! required, `IndexName` optional, `ContributorInsightsAction` required, and
//! `ContributorInsightsStatus` on the answer, one of DISABLED | DISABLING | ENABLED | ENABLING
//! | FAILED).
//!
//! The rows differ along what the rule generalises over: the DIRECTION repeated (enable and
//! disable), the resource addressed (a table and one of its global secondary indexes), the MODE
//! the repeat names (none, the mode in force, and the other mode of the modelled catalog
//! `ACCESSED_AND_THROTTLED_KEYS | THROTTLED_KEYS`, `smithy_query(shape="ContributorInsightsMode")`),
//! and the status in force when the repeat arrives (the settled one and the transient one).
//!
//! Every call goes over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter` the
//! emulator's front doors use, and every wait is a poll for the CONDITION
//! ([`crate::emulator::insights_when`]), never a fixed sleep. The table builder, the update and
//! the read are the crate's own helpers, reused rather than copied.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::contributor_insights::{create_table, describe_insights, set_insights};
use crate::emulator::{emulator, insights_when};

/// The mode an enable that names none is configured under: the wider of the two the modelled
/// `ContributorInsightsMode` catalog declares.
const DEFAULT_MODE: &str = "ACCESSED_AND_THROTTLED_KEYS";

/// The other mode of that catalog, which an enable naming it asks a configured resource to
/// CHANGE to.
const OTHER_MODE: &str = "THROTTLED_KEYS";

/// The index every row addresses beside the table's own setting.
const INDEX: &str = "Repeat.Index";

/// The status and the mode one answer reports, as a pair, so a row asserts both at once.
fn reported(body: &Value) -> (Value, Value) {
    (
        body.get("ContributorInsightsStatus")
            .cloned()
            .unwrap_or(Value::Null),
        body.get("ContributorInsightsMode")
            .cloned()
            .unwrap_or(Value::Null),
    )
}

/// One table carrying [`INDEX`], waited out so neither its own creation nor the index's is
/// still in flight when a row addresses it.
async fn table(router: &MultiServiceRouter, name: &str) {
    create_table(router, name, &[INDEX]).await;
}

/// Every resource of one table a row runs against: the table's own setting and its index's.
fn resources() -> [(&'static str, Option<&'static str>); 2] {
    [("the table's own setting", None), ("its index's", Some(INDEX))]
}

/// RULE: a repeated action that asks for the direction already in force answers the status in
/// force, and leaves what a later reader sees exactly as it was.
#[tokio::test]
async fn an_action_asking_for_the_direction_already_in_force_applies_nothing() {
    let router = emulator();
    table(&router, "InsightsRepeated").await;

    for (case, index) in resources() {
        // Turn the insights on and wait the enable out: the resource is now ENABLED under the
        // mode an enable that names none is configured with.
        let enabled = set_insights(&router, "InsightsRepeated", index, "ENABLE", None).await;
        assert_eq!(
            reported(&enabled),
            (json!("ENABLING"), json!(DEFAULT_MODE)),
            "case {case}: an enable of a resource that was off asks for a change, so it answers \
             the transient status of its direction, got {enabled}"
        );
        let settled = insights_when(
            &router,
            case,
            "InsightsRepeated",
            index,
            "ENABLED",
        )
        .await;
        let last_update = settled.get("LastUpdateDateTime").cloned();

        // The SAME enable again, named with no mode and with the mode in force: each asks for
        // the configuration the resource already has, so neither applies anything and both
        // answer ENABLED.
        for (mode_case, mode) in [("no mode named", None), ("the mode in force", Some(DEFAULT_MODE))] {
            let repeated =
                set_insights(&router, "InsightsRepeated", index, "ENABLE", mode).await;
            assert_eq!(
                reported(&repeated),
                (json!("ENABLED"), json!(DEFAULT_MODE)),
                "case {case} with {mode_case}: a repeated enable asks for no change, so it \
                 answers the status in force, got {repeated}"
            );
            let read = describe_insights(&router, "InsightsRepeated", index).await;
            assert_eq!(
                (
                    read.get("ContributorInsightsStatus").cloned(),
                    read.get("LastUpdateDateTime").cloned()
                ),
                (Some(json!("ENABLED")), last_update.clone()),
                "case {case} with {mode_case}: the repeat owes no move, so a later reader sees \
                 the resource exactly as it was, got {read}"
            );
        }

        // Turn them off and wait the disable out, then repeat the disable the same way: the
        // configuration an enable wrote outlives a disable, so the action is served, and it
        // asks for the direction the resource is already in.
        let disabled = set_insights(&router, "InsightsRepeated", index, "DISABLE", None).await;
        assert_eq!(
            reported(&disabled),
            (json!("DISABLING"), json!(DEFAULT_MODE)),
            "case {case}: a disable of a resource that was on asks for a change, got {disabled}"
        );
        let settled = insights_when(
            &router,
            case,
            "InsightsRepeated",
            index,
            "DISABLED",
        )
        .await;
        let last_update = settled.get("LastUpdateDateTime").cloned();

        for (mode_case, mode) in [
            ("no mode named", None),
            ("the mode in force", Some(DEFAULT_MODE)),
            ("the other mode of the catalog", Some(OTHER_MODE)),
        ] {
            let repeated =
                set_insights(&router, "InsightsRepeated", index, "DISABLE", mode).await;
            assert_eq!(
                reported(&repeated),
                (json!("DISABLED"), json!(DEFAULT_MODE)),
                "case {case} with {mode_case}: a repeated disable asks for no change, so it \
                 answers the status in force and the configuration still in force, got {repeated}"
            );
            let read = describe_insights(&router, "InsightsRepeated", index).await;
            assert_eq!(
                (
                    read.get("ContributorInsightsStatus").cloned(),
                    read.get("LastUpdateDateTime").cloned()
                ),
                (Some(json!("DISABLED")), last_update.clone()),
                "case {case} with {mode_case}: the repeat owes no move, so a later reader sees \
                 the resource exactly as it was, got {read}"
            );
        }
    }
}

/// RULE: an action that DOES ask for a change starts the span again — the other direction, and
/// an enable naming a mode the resource is not configured under.
#[tokio::test]
async fn an_action_that_asks_for_a_change_is_applied_asynchronously() {
    let router = emulator();
    table(&router, "InsightsChanged").await;

    for (case, index) in resources() {
        set_insights(&router, "InsightsChanged", index, "ENABLE", None).await;
        insights_when(&router, case, "InsightsChanged", index, "ENABLED").await;

        // The other mode of the catalog is a different CONFIGURATION, so the enable that names
        // it applies a change: the transient status again, and the new mode from then on.
        let changed = set_insights(
            &router,
            "InsightsChanged",
            index,
            "ENABLE",
            Some(OTHER_MODE),
        )
        .await;
        assert_eq!(
            reported(&changed),
            (json!("ENABLING"), json!(OTHER_MODE)),
            "case {case}: an enable naming the other mode asks for a change, so it answers the \
             transient status of its direction, got {changed}"
        );
        let settled = insights_when(&router, case, "InsightsChanged", index, "ENABLED").await;
        assert_eq!(
            settled.get("ContributorInsightsMode").cloned(),
            Some(json!(OTHER_MODE)),
            "case {case}: the configuration the accepted enable asked for is the one in force, \
             got {settled}"
        );

        // And the other DIRECTION is a change whatever mode is in force.
        let turned_off = set_insights(&router, "InsightsChanged", index, "DISABLE", None).await;
        assert_eq!(
            reported(&turned_off),
            (json!("DISABLING"), json!(OTHER_MODE)),
            "case {case}: a disable of a resource whose insights are on asks for a change, got \
             {turned_off}"
        );
        insights_when(&router, case, "InsightsChanged", index, "DISABLED").await;
    }
}
