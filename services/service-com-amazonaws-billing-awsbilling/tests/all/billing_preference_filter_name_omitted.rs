// SPDX-License-Identifier: CC0-1.0

//! The `PREFERENCE_KEY` filter of `GetBillingPreferences` when the request leaves
//! the filter's optional `name` out, over the awsJson1_0 wire.
//!
//! Every assertion here is about ONE rule: `BillingFeatureFilter$name` is not
//! `@required` and `BillingFeatureFilterName` declares exactly one value
//! (`smithy_query(query="shape", shape="BillingFeatureFilter")`), so a filter
//! that omits the member still names that value — the values it carries narrow
//! what the read reports exactly as they do for a filter that spells the name
//! out, and a filter carrying no values narrows nothing either way. The table
//! spans what the rule generalises over: the feature (stored keys, the credit
//! feature whose keys name a credit, a history feature whose entries repeat a
//! key), the key shape (plain, account-list, `credit/{creditId}/{option}`), how
//! many values the filter carries (one, several, the modelled maximum of ten)
//! and whether a value names a stored entry. The same rule is pinned across a
//! paged walk, and a filter whose `name` IS present and unsupported is still
//! refused.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::billing_views::{call, error_code, standalone_emulator_with_engine, ACCOUNT_ID};

/// A ceiling on the pages one walk below may take, so a walk that fails to make
/// progress fails the test instead of running forever.
const MAX_PAGES: usize = 32;

/// The account-list key naming the account every request here authenticates as,
/// which is a member of its own consolidated billing family.
fn account_key() -> String {
    format!("account/{ACCOUNT_ID}")
}

/// `UpdateBillingPreferences` for one feature and one or more key/value pairs.
async fn update(router: &MultiServiceRouter, feature: &str, pairs: &[(&str, &str)]) {
    let keys: Vec<Value> = pairs
        .iter()
        .map(|(key, value)| json!({ "key": key, "value": value }))
        .collect();
    let (status, body) = call(
        router,
        "UpdateBillingPreferences",
        &json!({ "feature": feature, "billingPreferencesPerKey": keys }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: expected the update of {feature:?} to succeed, got {status} {body:?}"
    );
}

/// The `(key, value)` pairs a `GetBillingPreferences` body reports, in the order
/// it reported them.
fn reported(body: &Value) -> Vec<(String, String)> {
    body.get("billingPreferences")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry
                            .get("key")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        entry
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `GetBillingPreferences` body for one feature, carrying one filter: the
/// values, with the filter's `name` spelled out or left out.
fn request(feature: &str, values: &[String], with_name: bool) -> Value {
    let mut filter = json!({ "value": values });
    if with_name {
        filter["name"] = json!("PREFERENCE_KEY");
    }
    json!({ "features": [feature], "filters": [filter] })
}

/// The entries a successful read reports for `body`.
async fn read(router: &MultiServiceRouter, body: &Value, case: &str) -> Vec<(String, String)> {
    let (status, reply) = call(router, "GetBillingPreferences", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the read to succeed, got {status} {reply:?}"
    );
    reported(&reply)
}

/// The world every row reads: the keys this suite stores under each feature, in
/// the order it writes them — which is also the order the history feature
/// records them in. The last write moves a key that was already set, so the
/// history holds that key twice.
///
/// `CREDIT_SHARING`, `CREDIT_LEVEL_SHARING`, `CREDIT_PREFERENCE_OPTIONS` and
/// `CREDIT_SHARING_HISTORY` need no access activation
/// (`state::ACCESS_ACTIVATION_FEATURES`), so the default state reaches them and
/// this suite is about the filter rather than the access gate (pinned in
/// `tests/all/billing_feature_access.rs`).
async fn seeded_emulator() -> MultiServiceRouter {
    let (router, engine) = standalone_emulator_with_engine();
    // A `credit/{creditId}/{option}` key names a credit the family holds, so
    // that credit goes in the store exactly as the credit suites put it there.
    crate::billing_feature_access::seed_credit(&engine);
    let account = account_key();
    update(
        &router,
        "CREDIT_SHARING",
        &[
            (account.as_str(), "DISABLED"),
            ("credit-sharing", "ENABLED"),
            ("key/0", "ENABLED"),
            ("key/1", "ENABLED"),
            ("key/2", "DISABLED"),
        ],
    )
    .await;
    update(&router, "CREDIT_SHARING", &[(account.as_str(), "ENABLED")]).await;
    update(
        &router,
        "CREDIT_LEVEL_SHARING",
        &[("credit-level-sharing", "ENABLED"), ("key/0", "DISABLED")],
    )
    .await;
    update(
        &router,
        "CREDIT_PREFERENCE_OPTIONS",
        &[("credit/1/status", "ENABLED"), ("credit/1/sharing", "DISABLED")],
    )
    .await;
    router
}

/// One row of the filter table: the feature read, the values the one filter
/// carries, and the entries the read must report whether or not the filter
/// spells its `name` out.
struct Row {
    case: &'static str,
    feature: &'static str,
    values: Vec<String>,
    expected: Vec<(String, String)>,
}

/// `(key, value)` as the table spells it.
fn entry(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

fn rows() -> Vec<Row> {
    let account = account_key();
    // Ten values — the modelled `BillingFeatureFilterValues` maximum — of which
    // three name a stored key.
    let mut ten_values = vec![
        "key/0".to_string(),
        "key/1".to_string(),
        "key/2".to_string(),
    ];
    ten_values.extend((0..7).map(|index| format!("gone/{index}")));
    vec![
        Row {
            case: "one stored key",
            feature: "CREDIT_SHARING",
            values: vec!["key/1".to_string()],
            expected: vec![entry("key/1", "ENABLED")],
        },
        Row {
            case: "a plain key and an account-list key",
            feature: "CREDIT_SHARING",
            values: vec!["key/2".to_string(), account.clone()],
            expected: vec![entry(&account, "ENABLED"), entry("key/2", "DISABLED")],
        },
        Row {
            case: "one value naming no stored key",
            feature: "CREDIT_SHARING",
            values: vec!["key/9".to_string()],
            expected: vec![],
        },
        Row {
            case: "ten values, three of them stored",
            feature: "CREDIT_SHARING",
            values: ten_values,
            expected: vec![
                entry("key/0", "ENABLED"),
                entry("key/1", "ENABLED"),
                entry("key/2", "DISABLED"),
            ],
        },
        Row {
            case: "another feature's key of the same name",
            feature: "CREDIT_LEVEL_SHARING",
            values: vec!["key/0".to_string()],
            expected: vec![entry("key/0", "DISABLED")],
        },
        Row {
            case: "a key naming a credit",
            feature: "CREDIT_PREFERENCE_OPTIONS",
            values: vec!["credit/1/status".to_string()],
            expected: vec![entry("credit/1/status", "ENABLED")],
        },
        Row {
            case: "a history feature, a key it recorded twice",
            feature: "CREDIT_SHARING_HISTORY",
            values: vec![account.clone()],
            expected: vec![entry(&account, "DISABLED"), entry(&account, "ENABLED")],
        },
        Row {
            case: "a history feature, two keys it recorded once each",
            feature: "CREDIT_SHARING_HISTORY",
            values: vec!["key/0".to_string(), "key/1".to_string()],
            expected: vec![entry("key/0", "ENABLED"), entry("key/1", "ENABLED")],
        },
    ]
}

#[tokio::test]
async fn a_filter_that_omits_the_optional_name_narrows_by_its_values() {
    let router = seeded_emulator().await;
    for row in rows() {
        let named = read(
            &router,
            &request(row.feature, &row.values, true),
            row.case,
        )
        .await;
        assert_eq!(
            named, row.expected,
            "case {:?}: expected the named filter to select the entries it names, got {named:?}",
            row.case
        );
        let nameless = read(
            &router,
            &request(row.feature, &row.values, false),
            row.case,
        )
        .await;
        assert_eq!(
            nameless, row.expected,
            "case {:?}: expected a filter omitting its name to select the same entries, got {nameless:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_filter_carrying_no_value_narrows_nothing_whether_or_not_it_is_named() {
    let router = seeded_emulator().await;
    // A filter that names nothing to match narrows nothing, so the read reports
    // the feature's whole set — the rows are the forms such a filter takes.
    for feature in [
        "CREDIT_SHARING",
        "CREDIT_LEVEL_SHARING",
        "CREDIT_PREFERENCE_OPTIONS",
        "CREDIT_SHARING_HISTORY",
    ] {
        let unfiltered = read(
            &router,
            &json!({ "features": [feature] }),
            feature,
        )
        .await;
        assert!(
            !unfiltered.is_empty(),
            "case {feature:?}: expected the feature to hold entries, got {unfiltered:?}"
        );
        for (case, filter) in [
            ("the filter names nothing at all", json!({})),
            (
                "the filter names only its name",
                json!({ "name": "PREFERENCE_KEY" }),
            ),
        ] {
            let case = format!("{feature}: {case}");
            let reported = read(
                &router,
                &json!({ "features": [feature], "filters": [filter] }),
                &case,
            )
            .await;
            assert_eq!(
                reported, unfiltered,
                "case {case:?}: expected the whole set, got {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_filter_omitting_its_name_pages_the_selection_it_names() {
    let router = seeded_emulator().await;
    let values: Vec<String> = vec![
        "key/0".to_string(),
        "key/1".to_string(),
        "key/2".to_string(),
    ];
    let expected = vec![
        entry("key/0", "ENABLED"),
        entry("key/1", "ENABLED"),
        entry("key/2", "DISABLED"),
    ];

    // One entry to a page: the walk reports exactly the selected entries, so the
    // filter narrows every page of it, not just the first.
    for page_size in [1_u64, 2, 50] {
        let case = format!("{page_size} selected entries to a page");
        let mut body = request("CREDIT_SHARING", &values, false);
        body["maxResults"] = json!(page_size);
        let mut walked: Vec<(String, String)> = Vec::new();
        for taken in 0..MAX_PAGES {
            let (status, reply) = call(&router, "GetBillingPreferences", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the page to succeed, got {status} {reply:?}"
            );
            walked.extend(reported(&reply));
            match reply.get("nextToken").and_then(Value::as_str) {
                Some(token) => body["nextToken"] = json!(token),
                None => break,
            }
            assert!(
                taken + 1 < MAX_PAGES,
                "case {case:?}: the walk must terminate, got {MAX_PAGES} pages"
            );
        }
        assert_eq!(
            walked, expected,
            "case {case:?}: expected every selected entry exactly once in key order, got {walked:?}"
        );
    }

    // The two filter forms select the SAME entries, so a token one of them
    // minted continues the walk for the other: a page boundary is a position in
    // the selection, and both forms name one selection.
    let mut nameless = request("CREDIT_SHARING", &values, false);
    nameless["maxResults"] = json!(1);
    let (status, reply) = call(&router, "GetBillingPreferences", &nameless).await;
    assert_eq!(
        status, 200,
        "case \"the nameless first page\": expected the page to succeed, got {status} {reply:?}"
    );
    let token = reply
        .get("nextToken")
        .and_then(Value::as_str)
        .expect("a selection of three entries taken one to a page has a second page")
        .to_string();
    let mut named = request("CREDIT_SHARING", &values, true);
    named["maxResults"] = json!(1);
    named["nextToken"] = json!(token);
    let continued = read(&router, &named, "the named form continues the walk").await;
    assert_eq!(
        continued,
        vec![entry("key/1", "ENABLED")],
        "case \"the named form continues the walk\": expected the entry after the boundary, got {continued:?}"
    );
}

#[tokio::test]
async fn a_filter_naming_an_unsupported_filter_name_is_still_refused() {
    let router = seeded_emulator().await;
    // `BillingFeatureFilterName` declares exactly one value, so a name the model
    // does not declare is a bad request — including one that differs only in
    // case.
    for name in ["BOGUS", "preference_key", "PREFERENCE_KEYS", ""] {
        let (status, body) = call(
            &router,
            "GetBillingPreferences",
            &json!({
                "features": ["CREDIT_SHARING"],
                "filters": [{ "name": name, "value": ["key/0"] }],
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400 for a filter name this service does not support, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {name:?}: expected ValidationException, got {body:?}"
        );
    }
}
