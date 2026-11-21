// SPDX-License-Identifier: CC0-1.0

//! A continuation token a paginated listing of this service cannot resume from is IGNORED: the
//! listing serves the same first page a request carrying no token at all is served.
//!
//! The rule under test: a `NextToken` names a POSITION in the enumeration
//! (`DescribeAlarmsInput$NextToken` "The token returned by a previous call to indicate that
//! there is more data available.", `ListAlarmMuteRulesInput$NextToken` "The token returned from
//! a previous call to indicate where to continue retrieving results."), and a value this
//! service cannot read a cursor out of names no position of the enumeration at all: the
//! listings that SERVE such a token rather than refusing it with the `InvalidNextToken` their
//! model declares read the request as the one that carries no token, so the answer — the items
//! of the first page and whatever token that page owes — is identical to the answer of the
//! same request without the value.
//!
//! The rows span what the rule generalises over: both listings of this service that serve such
//! a token, the FORM the value takes (a word, a hexadecimal cursor whose bytes are no text, a
//! base64 blob of the form another service's cursor travels in, a minted cursor with a
//! character cut off and one with text appended), the page size the request asks for (none at
//! all, one, and more than the listing holds), and how many items the listing holds (one, and
//! several) — beside the two values that are no token at all, the empty string and an absent
//! member, which ask for the FIRST page too, and a cursor this service really minted, which
//! resumes where it points.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alarm_mute_rules::{listed_arns, put_request, with_members};
use crate::harness::{build_router, metric_alarm_request, ok};

/// The prefix every alarm this suite writes shares, so a listing narrowed by it holds exactly
/// the alarms a row created.
const PREFIX: &str = "cursor-page-alarm-";

/// The values a row offers as a `NextToken` that this service cannot read a cursor out of.
///
/// `cursor` is a token the listing really minted, so the last two rows are that cursor broken
/// rather than a value written out by hand; they are offered only where the listing owed a
/// cursor at all, since a listing that fits in one page mints none to break.
fn unresumable(cursor: &str) -> Vec<(String, String)> {
    let mut rows = vec![
        ("a word".to_string(), "not-a-cursor".to_string()),
        (
            "a hexadecimal cursor whose bytes are no text".to_string(),
            "fffe".to_string(),
        ),
        (
            "a base64 blob of another service's form".to_string(),
            "eyJuZXh0IjoiYWxhcm0ifQ==".to_string(),
        ),
    ];
    if !cursor.is_empty() {
        rows.push((
            "a minted cursor with its last character cut off".to_string(),
            cursor
                .get(..cursor.len().saturating_sub(1))
                .unwrap_or_default()
                .to_string(),
        ));
        rows.push((
            "a minted cursor with text appended".to_string(),
            format!("{cursor}zz"),
        ));
    }
    rows
}

/// The alarm names a `DescribeAlarms` answer reports.
fn listed_alarms(listed: &Value) -> Vec<String> {
    listed["MetricAlarms"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|alarm| alarm.get("AlarmName").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

/// The token an answer owes the page after it, if any.
fn next_token(listed: &Value) -> Option<String> {
    listed
        .get("NextToken")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Write `count` alarms whose names sort in the order they were created.
async fn given_alarms(router: &MultiServiceRouter, count: usize) -> Vec<String> {
    let mut names: Vec<String> = vec![];
    for index in 0..count {
        let name = format!("{PREFIX}{index:03}");
        ok(router, "PutMetricAlarm", &metric_alarm_request(&name)).await;
        names.push(name);
    }
    names
}

/// Write `count` mute rules whose names sort in the order they were created.
async fn given_rules(router: &MultiServiceRouter, count: usize) {
    for index in 0..count {
        ok(
            router,
            "PutAlarmMuteRule",
            &with_members(
                put_request(
                    &format!("cursor-page-rule-{index:03}"),
                    "cron(* * * * *)",
                    "PT1H",
                ),
                json!({}),
            ),
        )
        .await;
    }
}

#[tokio::test]
async fn an_unresumable_token_is_ignored_by_the_alarm_listing() {
    // (case, how many alarms the prefix holds, the page size the rows ask for)
    let rows: [(&str, usize, Option<i64>); 4] = [
        ("one alarm, no page size asked for", 1, None),
        ("three alarms, no page size asked for", 3, None),
        ("three alarms in pages of one", 3, Some(1)),
        ("three alarms in a page wider than the listing", 3, Some(50)),
    ];

    for (case, total, page_size) in rows {
        let router = build_router();
        let names = given_alarms(&router, total).await;

        // The page this request is served when it carries NO token: what the same request
        // carrying a token this service cannot read must be served too.
        let mut baseline_request = json!({"AlarmNamePrefix": PREFIX});
        if let Some(page_size) = page_size {
            baseline_request["MaxRecords"] = json!(page_size);
        }
        let baseline = ok(&router, "DescribeAlarms", &baseline_request).await;
        let baseline_alarms = listed_alarms(&baseline);
        let baseline_token = next_token(&baseline);

        // A cursor the listing really minted, taken from a page of one so one is always owed.
        let minted = ok(
            &router,
            "DescribeAlarms",
            &json!({"AlarmNamePrefix": PREFIX, "MaxRecords": 1}),
        )
        .await;
        let cursor = next_token(&minted).unwrap_or_default();

        for (token_case, token) in unresumable(&cursor) {
            let mut request = json!({"AlarmNamePrefix": PREFIX, "NextToken": token});
            if let Some(page_size) = page_size {
                request["MaxRecords"] = json!(page_size);
            }
            let listed = ok(&router, "DescribeAlarms", &request).await;
            let case = format!("{case} with {token_case} ({token:?})");
            assert_eq!(
                listed_alarms(&listed),
                baseline_alarms,
                "case {case:?}: expected the token ignored and the first page {baseline_alarms:?} \
                 served, got {listed}"
            );
            // The other two lists this read reports are materialised and empty here, and the
            // token the page owes is the one the first page owes.
            for member in ["CompositeAlarms", "LogAlarms"] {
                assert_eq!(
                    listed.get(member).and_then(Value::as_array),
                    Some(&Vec::new()),
                    "case {case:?}: expected {member} present and empty, got {listed}"
                );
            }
            assert_eq!(
                next_token(&listed),
                baseline_token,
                "case {case:?}: expected the first page's own token {baseline_token:?}, got \
                 {listed}"
            );
        }

        // The alarms themselves are untouched: a request carrying no token still reports every
        // one of them, so the pages above answered the TOKEN rather than a broken store.
        let whole = ok(
            &router,
            "DescribeAlarms",
            &json!({"AlarmNamePrefix": PREFIX}),
        )
        .await;
        assert_eq!(
            listed_alarms(&whole),
            names,
            "case {case:?}: expected the whole listing to survive, got {whole}"
        );

        // And the cursor the listing DID mint still resumes where it points, where it owed
        // one at all — a readable cursor is not ignored.
        if !cursor.is_empty() {
            let resumed = ok(
                &router,
                "DescribeAlarms",
                &json!({"AlarmNamePrefix": PREFIX, "NextToken": cursor}),
            )
            .await;
            assert_eq!(
                listed_alarms(&resumed),
                names.get(1..).unwrap_or_default(),
                "case {case:?}: expected a minted cursor to resume after the first alarm, got \
                 {resumed}"
            );
        }
    }
}

#[tokio::test]
async fn an_unresumable_token_is_ignored_by_the_mute_rule_listing() {
    // The same rule through the other listing of this service that serves such a token.
    for (case, total) in [("one rule", 1_usize), ("three rules", 3)] {
        let router = build_router();
        given_rules(&router, total).await;

        let minted = ok(&router, "ListAlarmMuteRules", &json!({"MaxRecords": 1})).await;
        let cursor = next_token(&minted).unwrap_or_default();
        let whole = listed_arns(&ok(&router, "ListAlarmMuteRules", &json!({})).await);
        assert_eq!(
            whole.len(),
            total,
            "case {case:?}: the rules must be stored before they are paged, got {whole:?}"
        );

        for (token_case, token) in unresumable(&cursor) {
            let listed = ok(
                &router,
                "ListAlarmMuteRules",
                &json!({"NextToken": token}),
            )
            .await;
            let case = format!("{case} with {token_case} ({token:?})");
            assert_eq!(
                listed_arns(&listed),
                whole,
                "case {case:?}: expected the token ignored and the first page {whole:?} served, \
                 got {listed}"
            );
        }

        // The two values that are no token at all ask for the FIRST page as well, and the
        // minted cursor resumes after the rule it names.
        for (token_case, request) in [
            ("the empty string", json!({"NextToken": ""})),
            ("no token at all", json!({})),
        ] {
            let listed = ok(&router, "ListAlarmMuteRules", &request).await;
            assert_eq!(
                listed_arns(&listed),
                whole,
                "case {case:?} with {token_case}: expected the first page, got {listed}"
            );
        }
        if !cursor.is_empty() {
            let resumed = ok(
                &router,
                "ListAlarmMuteRules",
                &json!({"NextToken": cursor}),
            )
            .await;
            assert_eq!(
                listed_arns(&resumed),
                whole.get(1..).unwrap_or_default(),
                "case {case:?}: expected a minted cursor to resume after the first rule, got \
                 {resumed}"
            );
        }
    }
}
