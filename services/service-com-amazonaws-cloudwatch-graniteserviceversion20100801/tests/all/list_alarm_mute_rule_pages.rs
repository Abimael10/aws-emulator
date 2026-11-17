// SPDX-License-Identifier: CC0-1.0

//! How `ListAlarmMuteRules` PAGES and what each page is made of — the half of the alarm mute
//! rule store `tests/all/alarm_mute_rules.rs` reads a single page of.
//!
//! The rule under test: **a listing reports the mute rules the Region's store holds, computed
//! from that state, one page at a time — an omitted `MaxRecords` asks for the documented default
//! of 50, a stated one for at most that many, a value outside the modelled range 1..100 is
//! refused, a walk reports every selected rule exactly once in one stable order, and the whole
//! listing changes nothing a later caller sees** (`ListAlarmMuteRules`,
//! `ListAlarmMuteRulesInput$MaxRecords` "The maximum number of mute rules to return in one call.
//! The default is 50.", `ListAlarmMuteRulesOutput$NextToken`).
//!
//! Rows differ along what the rule generalises over: how many rules the store holds beside the
//! page, which page size the request asks for (none, each published bound, one past each bound,
//! and sizes between them), and which members the rules carry — so the recorded single-page call
//! is one row among them rather than the specification.

// The mute-rule request builders and readers this suite needs already exist beside it, for the
// same resource: the sibling suite owns them and this one reads a listing through the same pair
// of helpers rather than a second copy.
use crate::alarm_mute_rules::{listed_arns, put_alarm, put_request, rule_arn, text, with_members};
use crate::harness::{build_router, call, error_code, ok};
use ::chrono::{TimeDelta, Utc};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the harness signs its requests for, and so the one a rule it writes lives in.
const REGION: &str = "us-east-1";

/// The page size an omitted `MaxRecords` asks for: "The default is 50."
/// (`ListAlarmMuteRulesInput$MaxRecords`).
const DEFAULT_PAGE_SIZE: usize = 50;

/// The modelled bounds of that member (`smithy_query(shape="MaxRecords")`: range 1..100).
const MIN_PAGE_SIZE: i64 = 1;
const MAX_PAGE_SIZE: i64 = 100;

/// The members `AlarmMuteRuleSummary` declares, and the only ones a summary may carry
/// (`smithy_query(shape="AlarmMuteRuleSummary")`).
const SUMMARY_MEMBERS: [&str; 5] = [
    "AlarmMuteRuleArn",
    "ExpireDate",
    "Status",
    "MuteType",
    "LastUpdatedTimestamp",
];

/// A `PutAlarmMuteRule` request for `name` whose window opens every minute and stays open an
/// hour — so the status the clock makes of the rule does not change while the suite runs —
/// carrying whatever `extra` states beside the members the model makes required.
fn mute_rule(name: &str, extra: Value) -> Value {
    with_members(put_request(name, "cron(* * * * *)", "PT1H"), extra)
}

/// The ARN of the rule `name` names in [`REGION`], which is how a listing identifies a rule:
/// `AlarmMuteRuleSummary` models no name member, only the ARN.
fn arn_of(name: &str) -> String {
    rule_arn(REGION, name)
}

/// The token an answer owes the next page, if any.
fn next_token(listed: &Value) -> Option<String> {
    text(listed, "NextToken")
}

/// Write `count` rules whose names sort in the order they were created, so the order a listing
/// reports them in is stated once and every row below expects it.
async fn given_rules(router: &MultiServiceRouter, count: usize) -> Vec<String> {
    let mut names: Vec<String> = vec![];
    for index in 0..count {
        let name = format!("page-rule-{index:03}");
        ok(router, "PutAlarmMuteRule", &mute_rule(&name, json!({}))).await;
        names.push(name);
    }
    names
}

/// Walk the whole listing at `max_records`, answering every ARN reported and the token the last
/// page carried (which must be none).
async fn walk(router: &MultiServiceRouter, max_records: Option<i64>, pages: usize) -> Vec<String> {
    let mut walked: Vec<String> = vec![];
    let mut token: Option<String> = None;
    for _ in 0..=pages {
        let mut request = match max_records {
            None => json!({}),
            Some(max_records) => json!({"MaxRecords": max_records}),
        };
        if let Some(token) = token.as_deref() {
            request = with_members(request, json!({"NextToken": token}));
        }
        let listed = ok(router, "ListAlarmMuteRules", &request).await;
        walked.extend(listed_arns(&listed));
        token = next_token(&listed);
        if token.is_none() {
            return walked;
        }
    }
    panic!("case {max_records:?}: the walk did not terminate within {pages} pages");
}

#[tokio::test]
async fn every_page_size_walks_every_stored_rule_exactly_once() {
    let router = build_router();
    // One rule more than the widest page the member can ask for, so every row below has rules
    // left over after its first page and the walk has to carry a token.
    let total = usize::try_from(MAX_PAGE_SIZE).unwrap_or_default() + 1;
    let names = given_rules(&router, total).await;
    let every_rule: Vec<String> = names.iter().map(|name| arn_of(name)).collect();

    // (case, the page size the request asks for, the rules its first page must hold).
    //
    // Rows differ along the page size: none at all (the documented default), each published
    // bound of the member, and two sizes between them.
    let cases: Vec<(&str, Option<i64>, usize)> = vec![
        ("no MaxRecords at all", None, DEFAULT_PAGE_SIZE),
        ("the modelled minimum", Some(MIN_PAGE_SIZE), 1),
        ("a page of seven", Some(7), 7),
        (
            "the default asked for outright",
            Some(50),
            DEFAULT_PAGE_SIZE,
        ),
        (
            "the modelled maximum",
            Some(MAX_PAGE_SIZE),
            usize::try_from(MAX_PAGE_SIZE).unwrap_or_default(),
        ),
    ];

    for (case, max_records, first_page) in cases {
        let request = match max_records {
            None => json!({}),
            Some(max_records) => json!({"MaxRecords": max_records}),
        };
        let listed = ok(&router, "ListAlarmMuteRules", &request).await;
        let page = listed_arns(&listed);
        assert_eq!(
            page.as_slice(),
            every_rule.get(..first_page).unwrap_or_default(),
            "case {case:?}: expected the first {first_page} rules in name order, got {page:?}"
        );
        assert!(
            next_token(&listed).is_some(),
            "case {case:?}: {} rules are left over, so a token is owed, got {listed}",
            total - first_page
        );

        let walked = walk(&router, max_records, total + 1).await;
        assert_eq!(
            walked, every_rule,
            "case {case:?}: expected every stored rule once in name order, got {} ARNs",
            walked.len()
        );
    }
}

#[tokio::test]
async fn a_page_size_outside_the_modelled_range_is_refused() {
    let router = build_router();
    let names = given_rules(&router, 3).await;
    let every_rule: Vec<String> = names.iter().map(|name| arn_of(name)).collect();

    // (case, the page size, the page it must answer with — `None` for a refusal).
    //
    // Rows differ along where the asked-for size falls relative to the modelled range 1..100:
    // each published bound, one past each of them, and a value no count can be.
    let cases: Vec<(&str, i64, Option<usize>)> = vec![
        ("one below the modelled minimum", MIN_PAGE_SIZE - 1, None),
        ("the modelled minimum", MIN_PAGE_SIZE, Some(1)),
        (
            "the modelled maximum",
            MAX_PAGE_SIZE,
            Some(every_rule.len()),
        ),
        ("one past the modelled maximum", MAX_PAGE_SIZE + 1, None),
        ("a negative count", -7, None),
    ];

    for (case, max_records, expected) in cases {
        let (status, body) = call(
            &router,
            "ListAlarmMuteRules",
            &json!({"MaxRecords": max_records}),
        )
        .await;
        match expected {
            None => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: expected the modelled range to refuse it, \
                     got {status} with {body}"
                );
            }
            Some(len) => {
                assert_eq!(
                    (status, listed_arns(&body).as_slice()),
                    (200, every_rule.get(..len).unwrap_or_default()),
                    "case {case:?}: expected the first {len} rules, got {status} with {body}"
                );
            }
        }
    }

    // The refusals left the store exactly as it was: every rule is still listed.
    let listed = ok(&router, "ListAlarmMuteRules", &json!({})).await;
    assert_eq!(
        listed_arns(&listed),
        every_rule,
        "case \"after the refusals\": expected every stored rule, got {listed}"
    );
}

#[tokio::test]
async fn a_summary_carries_only_the_members_the_shape_models() {
    let router = build_router();
    let now = Utc::now();

    // Rows differ along which optional members the rule carries, because a summary reports the
    // subset `AlarmMuteRuleSummary` models whatever the rule beside it holds: "This field is only
    // present if an expiration date was configured." (`AlarmMuteRuleSummary$ExpireDate`).
    let cases: Vec<(&str, &str, Value, bool)> = vec![
        ("a rule with no expiry at all", "summary-no-expiry", json!({}), false),
        (
            "a rule that expires",
            "summary-with-expiry",
            json!({"ExpireDate": (now + TimeDelta::days(1)).timestamp()}),
            true,
        ),
        (
            "a rule carrying every optional member",
            "summary-with-everything",
            json!({
                "Description": "the suite's own rule",
                "StartDate": (now - TimeDelta::days(1)).timestamp(),
                "ExpireDate": (now + TimeDelta::days(2)).timestamp(),
                "MuteTargets": {"AlarmNames": ["SummaryAlarm"]},
                "Tags": [{"Key": "owner", "Value": "suite"}],
            }),
            true,
        ),
    ];

    for (_, name, extra, _) in &cases {
        ok(&router, "PutAlarmMuteRule", &mute_rule(name, extra.clone())).await;
    }

    let listed = ok(&router, "ListAlarmMuteRules", &json!({})).await;
    let summaries = listed["AlarmMuteRuleSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    for (case, name, _, expires) in &cases {
        let arn = arn_of(name);
        let summary = summaries
            .iter()
            .find(|summary| summary.get("AlarmMuteRuleArn").and_then(Value::as_str) == Some(&arn))
            .unwrap_or_else(|| panic!("case {case:?}: {arn} must be listed, got {listed}"));
        let reported: Vec<String> = summary
            .as_object()
            .map(|members| members.keys().cloned().collect())
            .unwrap_or_default();
        let unmodelled: Vec<&String> = reported
            .iter()
            .filter(|member| !SUMMARY_MEMBERS.contains(&member.as_str()))
            .collect();
        assert!(
            unmodelled.is_empty(),
            "case {case:?}: a summary carries only the members the shape models, \
             got {unmodelled:?} beside them in {summary}"
        );
        assert_eq!(
            summary.get("ExpireDate").is_some(),
            *expires,
            "case {case:?}: expected the expiry member present={expires}, got {summary}"
        );
        // The members the shape DOES model are reported off the rule its own reader reports,
        // never invented by the listing.
        let read = ok(
            &router,
            "GetAlarmMuteRule",
            &json!({"AlarmMuteRuleName": name}),
        )
        .await;
        for member in SUMMARY_MEMBERS {
            assert_eq!(
                summary.get(member),
                read.get(member),
                "case {case:?}: expected {member} to be the rule's own value, \
                 got {summary} beside {read}"
            );
        }
    }
}

#[tokio::test]
async fn listing_the_rules_changes_nothing_a_later_caller_sees() {
    let router = build_router();
    let now = Utc::now();
    ok(&router, "PutMetricAlarm", &put_alarm("ReadAlarm")).await;

    // One rule in each status the clock decides, each in a status that cannot change while the
    // suite runs, so a difference between two readings can only come from the reads themselves.
    let rules: Vec<(&str, Value)> = vec![
        ("read-active", json!({})),
        (
            "read-scheduled",
            json!({"StartDate": (now + TimeDelta::days(1)).timestamp()}),
        ),
        (
            "read-expired",
            json!({"ExpireDate": (now - TimeDelta::days(1)).timestamp()}),
        ),
        (
            "read-targeted",
            json!({"MuteTargets": {"AlarmNames": ["ReadAlarm"]}}),
        ),
    ];
    for (name, extra) in &rules {
        ok(&router, "PutAlarmMuteRule", &mute_rule(name, extra.clone())).await;
    }

    let before_listing = ok(&router, "ListAlarmMuteRules", &json!({})).await;
    let mut before_reads: Vec<Value> = vec![];
    for (name, _) in &rules {
        before_reads.push(
            ok(
                &router,
                "GetAlarmMuteRule",
                &json!({"AlarmMuteRuleName": name}),
            )
            .await,
        );
    }

    // Rows differ along what the reading asks for: a filter by alarm, by status, a paged walk, a
    // refused token and a refused alarm name — every shape of request this listing serves or
    // refuses, none of which may leave a mark.
    let readings: Vec<(&str, Value, u16)> = vec![
        ("the whole listing again", json!({}), 200),
        (
            "filtered by the alarm they target",
            json!({"AlarmName": "ReadAlarm"}),
            200,
        ),
        ("filtered by status", json!({"Statuses": ["ACTIVE"]}), 200),
        ("a page of one", json!({"MaxRecords": 1}), 200),
        (
            "a token this listing cannot resume from",
            json!({"NextToken": "not-a-token"}),
            200,
        ),
        (
            "an alarm name nothing was created under",
            json!({"AlarmName": "absent-alarm"}),
            404,
        ),
    ];
    for (case, request, expected) in readings {
        let (status, body) = call(&router, "ListAlarmMuteRules", &request).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected}, got {status} with {body}"
        );
    }
    // And a full walk, which is the reading that reports every rule.
    walk(&router, Some(1), rules.len() + 1).await;

    let after_listing = ok(&router, "ListAlarmMuteRules", &json!({})).await;
    assert_eq!(
        after_listing, before_listing,
        "case \"after every reading\": expected the listing unchanged, \
         got {after_listing} rather than {before_listing}"
    );
    for ((name, _), before) in rules.iter().zip(before_reads) {
        let after = ok(
            &router,
            "GetAlarmMuteRule",
            &json!({"AlarmMuteRuleName": name}),
        )
        .await;
        assert_eq!(
            after, before,
            "case {name:?}: expected the rule unchanged by the readings, \
             got {after} rather than {before}"
        );
    }
}
