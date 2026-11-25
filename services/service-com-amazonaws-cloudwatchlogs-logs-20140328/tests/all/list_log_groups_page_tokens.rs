// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the page tokens `ListLogGroups` hands back.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.ListLogGroups` — so a response that is not computed from the
//! shared store cannot pass. WHICH log groups the filters select is the subject of
//! `tests/all/log_group_name_pattern_lists.rs`; this suite is about the walk.
//!
//! The rule: `nextToken` is "the token for the next set of items to return", and this
//! operation "is paginated" — "your first use of this operation returns 50 results, and includes
//! a token to use in a subsequent operation to return more results": a page that REPORTED log
//! groups hands back the token that continues the SAME enumeration where it ended, and only a
//! page that reported none ends the walk. Carrying a token back resumes that enumeration after
//! the last group the page named — which is what lets a client enumerating the account's log
//! groups see every one of them exactly once, and makes the walk terminate.
//!
//! The table spans what the rule generalises over: the page size, from one group per page up
//! to more than the account holds, and whether the enumeration is the whole account's or the
//! one a name pattern selects.

use serde_json::{json, Value};

use crate::harness::Logs;

/// The operation every case calls.
const OP: &str = "ListLogGroups";

/// The log groups the cases enumerate: three under one prefix and two under another, so a
/// pattern selects a proper subset of the account's groups.
const GROUPS: [&str; 5] = [
    "/orders/api",
    "/orders/worker",
    "/orders/worker-two",
    "/payments/api",
    "/payments/worker",
];

/// The prefix pattern a case narrows the enumeration with, and the groups it selects.
const PREFIX_PATTERN: &str = "^/orders";

/// Create every log group of [`GROUPS`].
async fn account_with_groups(logs: &Logs) {
    for group in GROUPS {
        logs.ok("CreateLogGroup", &json!({"logGroupName": group})).await;
    }
}

/// The log group names one page reports, in the order it reported them, and its `nextToken`.
async fn page(logs: &Logs, case: &str, request: &Value) -> (Vec<String>, Option<String>) {
    let answer = logs.ok(OP, request).await;
    let names = answer["logGroups"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: {OP} reports logGroups, got {answer}"))
        .iter()
        .map(|group| {
            group["logGroupName"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every reported group carries a name, got {group}")
                })
                .to_string()
        })
        .collect();
    let token = answer
        .get("nextToken")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    (names, token)
}

/// A page carries a `nextToken` exactly while it reported log groups, and walking the token
/// reports every selected group exactly once before an empty page ends the walk.
#[tokio::test]
async fn a_page_carries_a_token_exactly_while_it_reports_log_groups() {
    let logs = Logs::new();
    account_with_groups(&logs).await;

    let all: Vec<String> = {
        let mut names: Vec<String> = GROUPS.iter().map(|name| (*name).to_string()).collect();
        names.sort();
        names
    };
    let under_prefix: Vec<String> = all
        .iter()
        .filter(|name| name.starts_with("/orders"))
        .cloned()
        .collect();

    // (case, the filter the enumeration carries, the groups it selects)
    let enumerations: [(&str, Value, &Vec<String>); 2] = [
        ("the whole account", json!({}), &all),
        (
            "the groups a name pattern selects",
            json!({"logGroupNamePattern": PREFIX_PATTERN}),
            &under_prefix,
        ),
    ];

    for (enumeration, filter, selected) in enumerations {
        // One row per page size: every size under the number selected, the exact number, and
        // one more than the account holds at all.
        for page_size in 1..=(GROUPS.len() + 1) {
            let case = format!("{enumeration}, limit {page_size}");
            let mut walked: Vec<String> = Vec::new();
            let mut token: Option<String> = None;
            let mut pages = 0usize;
            loop {
                let mut request = filter.clone();
                request["limit"] = json!(page_size);
                if let Some(token) = &token {
                    request["nextToken"] = json!(token);
                }
                let (names, next) = page(&logs, &case, &request).await;
                assert!(
                    names.len() <= page_size,
                    "case {case:?}: a page holds at most limit groups, got {names:?}"
                );
                let reported_any = !names.is_empty();
                walked.extend(names);
                pages += 1;
                assert!(
                    pages <= selected.len() + 1,
                    "case {case:?}: the walk terminates, got {pages} pages"
                );
                assert_eq!(
                    next.is_some(),
                    reported_any,
                    "case {case:?}: expected a token exactly while a page reports groups, \
                     after {walked:?} of {selected:?}"
                );
                match next {
                    Some(next) => token = Some(next),
                    None => break,
                }
            }
            assert_eq!(
                &walked, selected,
                "case {case:?}: expected every selected group exactly once, got {walked:?}"
            );
        }
    }
}

/// A page that reported the LAST of the matching log groups still carries a token — the
/// service hands one back with the results it reported rather than looking past them — and
/// continuing from that token answers the groups left, which is none, and ends the walk.
///
/// The rows span what the rule generalises over: a page bounded by no `limit` at all, one
/// bounded at exactly the number matching, one bounded above it, and a page of one group out of
/// one matching.
#[tokio::test]
async fn a_complete_page_still_carries_a_token_and_the_next_page_ends_the_walk() {
    let logs = Logs::new();
    account_with_groups(&logs).await;

    // (case, the request, the groups it reports)
    let rows: Vec<(&str, Value, usize)> = vec![
        ("no limit at all", json!({}), GROUPS.len()),
        (
            "a limit at exactly the number matching",
            json!({"limit": GROUPS.len()}),
            GROUPS.len(),
        ),
        (
            "a limit above the number matching",
            json!({"limit": GROUPS.len() + 5}),
            GROUPS.len(),
        ),
        (
            // One group matches the pattern and the page takes it, so the page is both the
            // first and the last of that enumeration.
            "one group out of one matching",
            json!({"logGroupNamePattern": "^/payments/api", "limit": 1}),
            1,
        ),
    ];

    for (case, request, reported) in rows {
        let (names, token) = page(&logs, case, &request).await;
        assert_eq!(
            names.len(),
            reported,
            "case {case:?}: expected {reported} group(s), got {names:?}"
        );
        let token = token.unwrap_or_else(|| {
            panic!("case {case:?}: expected a token with the groups reported, got none")
        });

        let mut next_request = request.clone();
        next_request["nextToken"] = json!(token);
        let (left, next_token) = page(&logs, case, &next_request).await;
        assert!(
            left.is_empty(),
            "case {case:?}: expected nothing left past the last group, got {left:?}"
        );
        assert_eq!(
            next_token, None,
            "case {case:?}: expected the empty page to end the walk, got {next_token:?}"
        );
    }
}

/// A page that reported NO log group carries no token: there is nothing to continue from, so
/// the enumeration ends there.
#[tokio::test]
async fn a_page_reporting_no_log_group_carries_no_token() {
    let logs = Logs::new();
    account_with_groups(&logs).await;

    // (case, a request matching none of the account's log groups)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a name pattern no group matches",
            json!({"logGroupNamePattern": "^/absent"}),
        ),
        (
            "a name pattern no group matches, with a page bound",
            json!({"logGroupNamePattern": "^/absent", "limit": 1}),
        ),
        (
            // "An array of tag filters to return only log groups that have specific tags":
            // none of these groups is tagged at all.
            "a tag filter no group matches",
            json!({"logGroupTags": [{"key": "stage", "values": ["prod"]}]}),
        ),
    ];

    for (case, request) in rows {
        let (names, token) = page(&logs, case, &request).await;
        assert!(
            names.is_empty(),
            "case {case:?}: expected no group reported, got {names:?}"
        );
        assert_eq!(
            token, None,
            "case {case:?}: expected no token with an empty page, got {token:?}"
        );
    }
}
