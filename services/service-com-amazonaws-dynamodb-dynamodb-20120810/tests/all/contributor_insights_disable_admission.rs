// SPDX-License-Identifier: CC0-1.0

//! Which `UpdateContributorInsights(DISABLE)` calls DynamoDB SERVES, and what an accepted
//! one answers — the other half of the lifecycle `crate::contributor_insights` pins (whose
//! helpers this suite drives the operations through).
//!
//! The rules pinned here:
//!
//! 1. a `DISABLE` turns off a key access insights CONFIGURATION, so it is served for every
//!    resource that has one — whatever status that resource is in: still being turned on,
//!    on, or already turned off by an earlier disable — and refused for a resource no
//!    enable has ever configured, with HTTP 400 `ValidationException` carrying DynamoDB's
//!    invalid-state sentence for that action; the refused call writes nothing;
//! 2. an accepted `DISABLE` answers the SETTLED status, `DISABLED`, together with the
//!    `ContributorInsightsMode` the resource was configured with — the configuration
//!    outlives the disable, so every later read answers that same mode until an enable
//!    replaces it — and the resource's CloudWatch rules are gone, so it names none.
//!
//! Each rule is asserted over a SET of inputs: the resource addressed (a table, one of its
//! global secondary indexes, a same-named table in another region), the status it is
//! addressed in, the mode it was configured with, and whether the disabling request names a
//! mode of its own. Every call goes over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::contributor_insights::{
    create_table, create_table_in, describe_insights, describe_insights_in, set_insights,
    set_insights_in, settle,
};
use crate::emulator::{call, call_in, emulator, error_code, message, ok, ok_in, REGION};

/// A second region to sign for, which is not [`REGION`]: a table name is unique within ONE
/// region, so the same name in both is two resources with settings of their own.
const OTHER_REGION: &str = "us-west-2";

/// How DynamoDB refuses a `DISABLE` against a resource that has no key access insights
/// configuration: the whole sentence is fixed text, naming neither the table nor the index.
const NOT_DISABLEABLE: &str = "Table or Index is not in a valid state to update Key Access \
                               Insights: ContributorInsights Status must be ENABLED or FAILED \
                               to perform DISABLE operation.";

/// The member a response omits reads as `Null`, whether the body left it out or carried it
/// null.
fn reported(body: &Value, member: &str) -> Value {
    body.get(member).cloned().unwrap_or(Value::Null)
}

/// One `UpdateContributorInsights(DISABLE)` in `region`, with the status and body it
/// answered.
async fn disable_in(
    router: &MultiServiceRouter,
    region: &str,
    table: &str,
    index: Option<&str>,
    mode: Option<&str>,
) -> (u16, Value) {
    let mut request = json!({"TableName": table, "ContributorInsightsAction": "DISABLE"});
    if let Some(index) = index {
        request["IndexName"] = json!(index);
    }
    if let Some(mode) = mode {
        request["ContributorInsightsMode"] = json!(mode);
    }
    call_in(router, region, "UpdateContributorInsights", request).await
}

/// The resources one region's listing reports a summary for, as `table` or `table/index`.
async fn listed_in(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let listed = ok_in(router, region, "ListContributorInsights", json!({})).await;
    listed["ContributorInsightsSummaries"]
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
        .collect()
}

/// One row of the refusal table: the case, the region and table the disable is signed for
/// and addresses, the index it names beside it, and the mode the request carries.
type RefusedCase = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
);

#[tokio::test]
async fn a_disable_of_a_resource_that_was_never_configured_is_refused_and_writes_nothing() {
    let router = emulator();
    create_table_in(&router, REGION, "InsightsUnconfigured", &["Un.Index"]).await;
    create_table_in(
        &router,
        REGION,
        "InsightsConfigured",
        &["On.Index", "Off.Index"],
    )
    .await;
    create_table_in(
        &router,
        OTHER_REGION,
        "InsightsConfigured",
        &["On.Index", "Off.Index"],
    )
    .await;
    settle().await;

    // The signed region's own table and index are turned ON, so the refusals below are
    // about the resource each call addresses and not about the surface: the same names in
    // the other region, and the neighbours in this one, were never configured.
    set_insights(&router, "InsightsConfigured", None, "ENABLE", None).await;
    set_insights(
        &router,
        "InsightsConfigured",
        Some("On.Index"),
        "ENABLE",
        Some("THROTTLED_KEYS"),
    )
    .await;
    settle().await;
    let configured_before = listed_in(&router, REGION).await;

    // Rows differ in why the resource addressed has no configuration — a table no update
    // has ever named, an index of a table whose OWN insights are on, a table of the other
    // region whose same-named table here is on, that table's index — and in whether the
    // request names a mode, and which.
    let cases: [RefusedCase; 5] = [
        (
            "a table no update has ever named",
            REGION,
            "InsightsUnconfigured",
            None,
            None,
        ),
        (
            "an index of that table, with the wider mode named",
            REGION,
            "InsightsUnconfigured",
            Some("Un.Index"),
            Some("ACCESSED_AND_THROTTLED_KEYS"),
        ),
        (
            "an index whose table's own insights are on",
            REGION,
            "InsightsConfigured",
            Some("Off.Index"),
            None,
        ),
        (
            "the same-named table in the other region",
            OTHER_REGION,
            "InsightsConfigured",
            None,
            Some("THROTTLED_KEYS"),
        ),
        (
            "that other region's index of the same name",
            OTHER_REGION,
            "InsightsConfigured",
            Some("On.Index"),
            None,
        ),
    ];

    for (case, region, table, index, mode) in cases {
        let (status, body) = disable_in(&router, region, table, index, mode).await;
        assert_eq!(
            (status, error_code(&body), message(&body)),
            (
                400,
                "ValidationException".to_string(),
                NOT_DISABLEABLE.to_string()
            ),
            "case {case}: a disable with no configuration to turn off is refused, got {status} \
             with {body}"
        );

        // The refused call wrote nothing: the resource it addressed still reads as one no
        // update has named, and the configured resources are untouched.
        let described = describe_insights_in(&router, region, table, index).await;
        assert_eq!(
            (
                reported(&described, "ContributorInsightsStatus"),
                reported(&described, "ContributorInsightsMode"),
                reported(&described, "LastUpdateDateTime"),
                reported(&described, "ContributorInsightsRuleList"),
            ),
            (json!("DISABLED"), Value::Null, Value::Null, Value::Null),
            "case {case}: the refused call left the resource unconfigured, got {described}"
        );
        let still_listed = listed_in(&router, REGION).await;
        assert_eq!(
            still_listed, configured_before,
            "case {case}: the refused call left the configured resources listed, got \
             {still_listed:?}"
        );
    }

    // The resources that DO have a configuration are served, so the refusals above are
    // about the configuration each call addressed and not about the action.
    for (case, index) in [
        ("the configured table", None),
        ("its configured index", Some("On.Index")),
    ] {
        let (status, body) = disable_in(&router, REGION, "InsightsConfigured", index, None).await;
        assert_eq!(
            (status, reported(&body, "ContributorInsightsStatus")),
            (200, json!("DISABLED")),
            "case {case}: a disable of a configured resource is served, got {status} with {body}"
        );
    }
}

/// One row of the admission table: the case, the index named beside the table (`None` for
/// the table itself), the mode the enable asks for, the mode in force after it, whether the
/// enable's span is waited out, and whether a first disable turns the insights off before
/// the one under test.
type AdmittedCase = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    bool,
    bool,
);

#[tokio::test]
async fn an_accepted_disable_answers_the_settled_status_with_the_mode_in_force() {
    let router = emulator();

    // Rows differ in the status the resource is addressed in — the enable still in flight,
    // the enable settled, and a resource an earlier disable has already turned off — in the
    // resource addressed (a table and one of its indexes), and in the mode it was configured
    // with: the two values `ContributorInsightsMode` declares
    // (`smithy_query(query="shape", shape="ContributorInsightsMode")` gives
    // ACCESSED_AND_THROTTLED_KEYS | THROTTLED_KEYS) and naming none at all, where the wider
    // one is configured.
    let cases: [AdmittedCase; 6] = [
        (
            "the table, the enable in flight, no mode named",
            None,
            None,
            "ACCESSED_AND_THROTTLED_KEYS",
            false,
            false,
        ),
        (
            "the table, the enable settled, the wider mode",
            None,
            Some("ACCESSED_AND_THROTTLED_KEYS"),
            "ACCESSED_AND_THROTTLED_KEYS",
            true,
            false,
        ),
        (
            "the table, already off, throttled keys only",
            None,
            Some("THROTTLED_KEYS"),
            "THROTTLED_KEYS",
            true,
            true,
        ),
        (
            "the index, the enable in flight, throttled keys only",
            Some("Admitted.Index"),
            Some("THROTTLED_KEYS"),
            "THROTTLED_KEYS",
            false,
            false,
        ),
        (
            "the index, the enable settled, no mode named",
            Some("Admitted.Index"),
            None,
            "ACCESSED_AND_THROTTLED_KEYS",
            true,
            false,
        ),
        (
            "the index, already off, the wider mode",
            Some("Admitted.Index"),
            Some("ACCESSED_AND_THROTTLED_KEYS"),
            "ACCESSED_AND_THROTTLED_KEYS",
            true,
            true,
        ),
    ];

    for (position, (case, index, asked, expected_mode, wait, twice)) in
        cases.into_iter().enumerate()
    {
        // Each row gets a table of its own, so the status it is addressed in is the one its
        // own calls left it in.
        let table = format!("InsightsAdmitted{position}");
        create_table(&router, &table, &["Admitted.Index"]).await;
        settle().await;

        set_insights(&router, &table, index, "ENABLE", asked).await;
        if wait {
            settle().await;
        }
        // Whichever half of the enable the resource is read in, the mode is the one the
        // enable configured.
        let before = describe_insights(&router, &table, index).await;
        let status_before = reported(&before, "ContributorInsightsStatus");
        assert!(
            (status_before == json!("ENABLING") || status_before == json!("ENABLED"))
                && reported(&before, "ContributorInsightsMode") == json!(expected_mode),
            "case {case}: the enable is in one of its two statuses with the mode it asked for, \
             got {before}"
        );
        if twice {
            let (status, body) = disable_in(&router, REGION, &table, index, None).await;
            assert_eq!(
                (status, reported(&body, "ContributorInsightsStatus")),
                (200, json!("DISABLED")),
                "case {case}: the first disable turns the insights off, got {status} with {body}"
            );
        }

        let (status, answered) = disable_in(&router, REGION, &table, index, None).await;
        assert_eq!(
            (
                status,
                reported(&answered, "TableName"),
                reported(&answered, "IndexName"),
                reported(&answered, "ContributorInsightsStatus"),
                reported(&answered, "ContributorInsightsMode"),
            ),
            (
                200,
                json!(table),
                index.map_or(Value::Null, |index| json!(index)),
                json!("DISABLED"),
                json!(expected_mode),
            ),
            "case {case}: the disable answers the settled status with the mode in force, got \
             {status} with {answered}"
        );

        // Read back: the resource is off, still reporting the mode it was configured with,
        // and the rules the enable minted are gone.
        let read_back = describe_insights(&router, &table, index).await;
        settle().await;
        let later = describe_insights(&router, &table, index).await;
        assert_eq!(
            (
                reported(&read_back, "ContributorInsightsStatus"),
                reported(&read_back, "ContributorInsightsMode"),
                reported(&read_back, "ContributorInsightsRuleList"),
                reported(&later, "ContributorInsightsStatus"),
                reported(&later, "ContributorInsightsMode"),
            ),
            (
                json!("DISABLED"),
                json!(expected_mode),
                Value::Null,
                json!("DISABLED"),
                json!(expected_mode),
            ),
            "case {case}: the resource stays off with its configuration still reported and no \
             rules, got {read_back} then {later}"
        );

        // It is out of the listing from the disable on, and a later enable replaces the mode
        // the disable left in place.
        let listed = ok(
            &router,
            "ListContributorInsights",
            json!({"TableName": table}),
        )
        .await;
        assert_eq!(
            listed["ContributorInsightsSummaries"]
                .as_array()
                .map_or(0, Vec::len),
            0,
            "case {case}: a resource whose insights are off has no summary, got {listed}"
        );
        let re_enabled = set_insights_in(
            &router,
            REGION,
            &table,
            index,
            "ENABLE",
            Some("THROTTLED_KEYS"),
        )
        .await;
        assert_eq!(
            (
                reported(&re_enabled, "ContributorInsightsStatus"),
                reported(&re_enabled, "ContributorInsightsMode"),
            ),
            (json!("ENABLING"), json!("THROTTLED_KEYS")),
            "case {case}: an enable after the disable configures the mode it names, got \
             {re_enabled}"
        );
    }
}

#[tokio::test]
async fn the_action_a_request_names_is_the_one_applied() {
    // The two values `ContributorInsightsAction` declares
    // (`smithy_query(query="shape", shape="ContributorInsightsAction")` gives
    // DISABLE | ENABLE) over one resource, in both orders they can arrive in: what the call
    // answers and what a read then reports follow from the action named, and a value outside
    // the set is refused before any of it.
    let router = emulator();
    create_table(&router, "InsightsActions", &[]).await;
    settle().await;

    let (status, body) = call(
        &router,
        "UpdateContributorInsights",
        json!({"TableName": "InsightsActions", "ContributorInsightsAction": "SUSPEND"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body)),
        (400, "ValidationException".to_string()),
        "case an action outside the set: it is refused, got {status} with {body}"
    );
    let untouched = describe_insights(&router, "InsightsActions", None).await;
    assert_eq!(
        (
            reported(&untouched, "ContributorInsightsStatus"),
            reported(&untouched, "ContributorInsightsMode"),
        ),
        (json!("DISABLED"), Value::Null),
        "case an action outside the set: nothing was written, got {untouched}"
    );

    // Rows are the actions in the order they arrive, each with the status the call answers
    // and the mode in force after it.
    let steps: [(&str, &str, Option<&str>, &str, &str); 4] = [
        (
            "the disable before any enable",
            "DISABLE",
            None,
            "refused",
            "<none>",
        ),
        (
            "the enable",
            "ENABLE",
            Some("THROTTLED_KEYS"),
            "ENABLING",
            "THROTTLED_KEYS",
        ),
        (
            "the disable after it",
            "DISABLE",
            None,
            "DISABLED",
            "THROTTLED_KEYS",
        ),
        (
            "the disable once more",
            "DISABLE",
            None,
            "DISABLED",
            "THROTTLED_KEYS",
        ),
    ];

    for (case, action, mode, expected_status, expected_mode) in steps {
        let mut request = json!({
            "TableName": "InsightsActions",
            "ContributorInsightsAction": action,
        });
        if let Some(mode) = mode {
            request["ContributorInsightsMode"] = json!(mode);
        }
        let (status, body) = call(&router, "UpdateContributorInsights", request).await;
        let answered = if status == 200 {
            reported(&body, "ContributorInsightsStatus")
                .as_str()
                .unwrap_or_default()
                .to_string()
        } else {
            "refused".to_string()
        };
        let mode_after = reported(&body, "ContributorInsightsMode")
            .as_str()
            .unwrap_or("<none>")
            .to_string();
        assert_eq!(
            (answered.as_str(), mode_after.as_str()),
            (expected_status, expected_mode),
            "case {case}: the action named is the one applied, got {status} with {body}"
        );
    }
}
