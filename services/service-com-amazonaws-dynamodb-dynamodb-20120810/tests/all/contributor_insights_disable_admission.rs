// SPDX-License-Identifier: CC0-1.0

//! Which `UpdateContributorInsights(DISABLE)` calls DynamoDB SERVES, and what an accepted
//! one answers — the other half of the lifecycle `crate::contributor_insights` pins (whose
//! helpers this suite drives the operations through).
//!
//! The rules pinned here:
//!
//! 1. a `DISABLE` applies to the key access insights CONFIGURATION of the resource it
//!    addresses, and only from a status the action can be performed FROM: it is served for a
//!    resource an enable has configured once that enable's move to `ENABLED` has fallen due,
//!    and again for a resource an earlier disable has already turned off, the configuration
//!    outliving the disable — and it is refused while the enable is still in flight
//!    (`ENABLING`), as well as for a resource no enable has ever named, which carries no
//!    configuration to turn off. Each refusal is HTTP 400 `ValidationException` carrying
//!    DynamoDB's invalid-state sentence for that action, which names the statuses the action
//!    requires; the refused call writes nothing;
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
use crate::emulator::{
    call, call_in, emulator, error_code, insights_when, message, ok, ok_in, REGION,
};

/// A second region to sign for, which is not [`REGION`]: a table name is unique within ONE
/// region, so the same name in both is two resources with settings of their own.
const OTHER_REGION: &str = "us-west-2";

/// How DynamoDB refuses a `DISABLE` against a resource whose insights are not on: the whole
/// sentence is fixed text, naming neither the table nor the index — it states the statuses
/// the action requires.
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
async fn a_disable_of_insights_no_enable_has_configured_is_refused_and_writes_nothing() {
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

    // The signed region's own table and index are turned ON and their enables waited out,
    // so each refusal below is about the STATUS of the resource the call addresses and not
    // about the surface: the same names in the other region, and the neighbours in this
    // one, were never enabled.
    set_insights(&router, "InsightsConfigured", None, "ENABLE", None).await;
    set_insights(
        &router,
        "InsightsConfigured",
        Some("On.Index"),
        "ENABLE",
        Some("THROTTLED_KEYS"),
    )
    .await;
    for index in [None, Some("On.Index")] {
        insights_when(
            &router,
            "the enable settles",
            "InsightsConfigured",
            index,
            "ENABLED",
        )
        .await;
    }

    // A resource whose insights ARE on is served, and so is the SAME disable repeated against
    // the resource that pass turned off: the configuration an enable wrote outlives the
    // disable, so the action still has a configuration to apply to. These two rows are the
    // resources the refusal table below then contrasts with.
    for (case, index) in [
        ("the configured table", None),
        ("its configured index", Some("On.Index")),
    ] {
        let (status, body) = disable_in(&router, REGION, "InsightsConfigured", index, None).await;
        assert_eq!(
            (status, reported(&body, "ContributorInsightsStatus")),
            (200, json!("DISABLED")),
            "case {case}: a disable of a resource whose insights are on is served, got {status} \
             with {body}"
        );
    }
    for (case, index) in [
        ("the table an earlier disable has turned off", None),
        ("its index, turned off by that same pass", Some("On.Index")),
    ] {
        let (status, repeated) =
            disable_in(&router, REGION, "InsightsConfigured", index, None).await;
        assert_eq!(
            (status, reported(&repeated, "ContributorInsightsStatus")),
            (200, json!("DISABLED")),
            "case {case}: the configuration an enable wrote outlives the disable, so the same \
             disable repeated is served, got {status} with {repeated}"
        );
    }
    let listed_before = listed_in(&router, REGION).await;

    // Rows differ in why the resource addressed carries no key access insights configuration
    // for a disable to apply to — a table no update has ever named, an index of a table whose
    // OWN insights were on, a table of the other region whose same-named table here was on,
    // and that table's index — and in whether the request names a mode, and which.
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
        let before = describe_insights_in(&router, region, table, index).await;
        let (status, body) = disable_in(&router, region, table, index, mode).await;
        assert_eq!(
            (status, error_code(&body), message(&body)),
            (
                400,
                "ValidationException".to_string(),
                NOT_DISABLEABLE.to_string()
            ),
            "case {case}: a disable of insights no enable has configured is refused, got \
             {status} with {body}"
        );

        // The refused call wrote nothing: the resource it addressed reads exactly as it did
        // before it, rules and all, and the listing is untouched.
        let described = describe_insights_in(&router, region, table, index).await;
        assert_eq!(
            (
                reported(&described, "ContributorInsightsStatus"),
                reported(&described, "ContributorInsightsMode"),
                reported(&described, "LastUpdateDateTime"),
                reported(&described, "ContributorInsightsRuleList"),
            ),
            (
                reported(&before, "ContributorInsightsStatus"),
                reported(&before, "ContributorInsightsMode"),
                reported(&before, "LastUpdateDateTime"),
                reported(&before, "ContributorInsightsRuleList"),
            ),
            "case {case}: the refused call left the resource as it was, got {described} after \
             {before}"
        );
        assert_eq!(
            reported(&described, "ContributorInsightsStatus"),
            json!("DISABLED"),
            "case {case}: every refused row addresses a resource no enable has configured, \
             got {described}"
        );
        let still_listed = listed_in(&router, REGION).await;
        assert_eq!(
            still_listed, listed_before,
            "case {case}: the refused call left the listing as it was, got {still_listed:?}"
        );
    }
}

#[tokio::test]
async fn a_disable_issued_while_the_enable_is_still_in_flight_is_refused() {
    // The rule: a `DISABLE` is performed from a status that admits it, and a resource whose
    // key access insights are still being turned on is not in one — so a disable issued while
    // that enable's move to `ENABLED` is still owed is refused, and writes nothing: the enable
    // in flight still settles.
    //
    // Each attempt re-states the enable, which starts the span again, and only asserts once a
    // read confirms the status still reads `ENABLING`: the span is generous, but a loaded
    // machine could let it elapse between the enable and the disable, which is not a defect.
    const ATTEMPTS: usize = 5;

    let router = emulator();
    create_table(&router, "InsightsInFlight", &["Flight.Index"]).await;
    settle().await;

    for (resource, index) in [("the table", None), ("its index", Some("Flight.Index"))] {
        let mut refused_in_flight = false;
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..ATTEMPTS {
            set_insights(&router, "InsightsInFlight", index, "ENABLE", None).await;
            let in_flight = describe_insights(&router, "InsightsInFlight", index).await;
            let status = reported(&in_flight, "ContributorInsightsStatus");
            if status != json!("ENABLING") {
                seen.push(format!("{status}"));
                continue;
            }
            let (code, body) = disable_in(&router, REGION, "InsightsInFlight", index, None).await;
            assert_eq!(
                (code, error_code(&body), message(&body)),
                (
                    400,
                    "ValidationException".to_string(),
                    NOT_DISABLEABLE.to_string()
                ),
                "case {resource}: a disable issued while the enable is still ENABLING is \
                 refused, got {code} with {body}"
            );
            // The refused call wrote nothing: the enable it did not cancel still settles.
            let settled = insights_when(
                &router,
                resource,
                "InsightsInFlight",
                index,
                "ENABLED",
            )
            .await;
            assert_eq!(
                reported(&settled, "ContributorInsightsMode"),
                json!("ACCESSED_AND_THROTTLED_KEYS"),
                "case {resource}: the refused disable left the enable's configuration, got \
                 {settled}"
            );
            // And from the settled status the same disable is served: what the refusal was
            // about is the status, not the resource or the request.
            let (code, served) = disable_in(&router, REGION, "InsightsInFlight", index, None).await;
            assert_eq!(
                (code, reported(&served, "ContributorInsightsStatus")),
                (200, json!("DISABLED")),
                "case {resource}: a disable from the settled status is served, got {code} with \
                 {served}"
            );
            refused_in_flight = true;
            break;
        }
        assert!(
            refused_in_flight,
            "case {resource}: the status never read ENABLING in {ATTEMPTS} attempts, saw \
             {seen:?}"
        );
    }
}

/// One row of the admission table: the case, the index named beside the table (`None` for
/// the table itself), the mode the enable asks for, and the mode in force after it.
type AdmittedCase = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

#[tokio::test]
async fn an_accepted_disable_answers_the_settled_status_with_the_mode_in_force() {
    let router = emulator();

    // Rows differ in the resource addressed (a table and one of its indexes) and in the
    // mode it was configured with: the two values `ContributorInsightsMode` declares
    // (`smithy_query(query="shape", shape="ContributorInsightsMode")` gives
    // ACCESSED_AND_THROTTLED_KEYS | THROTTLED_KEYS) and naming none at all, where the wider
    // one is configured. The status is the one a disable is performed from in every row:
    // the enable settled, which is what the action requires.
    let cases: [AdmittedCase; 6] = [
        (
            "the table, no mode named",
            None,
            None,
            "ACCESSED_AND_THROTTLED_KEYS",
        ),
        (
            "the table, the wider mode",
            None,
            Some("ACCESSED_AND_THROTTLED_KEYS"),
            "ACCESSED_AND_THROTTLED_KEYS",
        ),
        (
            "the table, throttled keys only",
            None,
            Some("THROTTLED_KEYS"),
            "THROTTLED_KEYS",
        ),
        (
            "the index, throttled keys only",
            Some("Admitted.Index"),
            Some("THROTTLED_KEYS"),
            "THROTTLED_KEYS",
        ),
        (
            "the index, no mode named",
            Some("Admitted.Index"),
            None,
            "ACCESSED_AND_THROTTLED_KEYS",
        ),
        (
            "the index, the wider mode",
            Some("Admitted.Index"),
            Some("ACCESSED_AND_THROTTLED_KEYS"),
            "ACCESSED_AND_THROTTLED_KEYS",
        ),
    ];

    for (position, (case, index, asked, expected_mode)) in cases.into_iter().enumerate() {
        // Each row gets a table of its own, so the status it is addressed in is the one its
        // own calls left it in.
        let table = format!("InsightsAdmitted{position}");
        create_table(&router, &table, &["Admitted.Index"]).await;
        settle().await;

        set_insights(&router, &table, index, "ENABLE", asked).await;
        // The disable is performed from the settled status, so the enable is waited out: the
        // mode reported is the one the enable configured.
        let before = insights_when(&router, case, &table, index, "ENABLED").await;
        assert_eq!(
            reported(&before, "ContributorInsightsMode"),
            json!(expected_mode),
            "case {case}: the settled enable reports the mode it asked for, got {before}"
        );

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

/// One row of the action sequence in
/// [`the_action_a_request_names_is_the_one_applied`]: the case, the action asked for, the
/// mode it names, whether the enable in flight is waited out first, the status the call
/// answers (or `refused`) and the mode in force after it.
type ActionStep = (
    &'static str,
    &'static str,
    Option<&'static str>,
    bool,
    &'static str,
    &'static str,
);

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

    // Rows are the actions in the order they arrive, each with whether the enable in flight
    // is waited out first, the status the call answers (or `refused`) and the mode in force
    // after it.
    let steps: [ActionStep; 6] = [
        (
            "the disable before any enable",
            "DISABLE",
            None,
            false,
            "refused",
            "<none>",
        ),
        (
            "the enable",
            "ENABLE",
            Some("THROTTLED_KEYS"),
            false,
            "ENABLING",
            "THROTTLED_KEYS",
        ),
        (
            "the disable once that enable has settled",
            "DISABLE",
            None,
            true,
            "DISABLED",
            "THROTTLED_KEYS",
        ),
        (
            "the disable once more",
            "DISABLE",
            None,
            false,
            "DISABLED",
            "THROTTLED_KEYS",
        ),
        (
            "the enable again",
            "ENABLE",
            Some("THROTTLED_KEYS"),
            false,
            "ENABLING",
            "THROTTLED_KEYS",
        ),
        (
            "the disable once that second enable has settled",
            "DISABLE",
            None,
            true,
            "DISABLED",
            "THROTTLED_KEYS",
        ),
    ];

    for (case, action, mode, settled_first, expected_status, expected_mode) in steps {
        if settled_first {
            insights_when(&router, case, "InsightsActions", None, "ENABLED").await;
        }
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
