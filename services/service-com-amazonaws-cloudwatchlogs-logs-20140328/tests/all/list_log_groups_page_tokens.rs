// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the page tokens `ListLogGroups` hands back.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.ListLogGroups` — so a response that is not computed from the
//! shared store cannot pass. WHICH log groups the filters select is the subject of
//! `tests/all/log_group_name_pattern_lists.rs`; this suite is about the walk.
//!
//! The rule: `nextToken` is "the token for the next set of items to return", so it comes back
//! exactly while log groups remain beyond the page just reported, and carrying it back resumes
//! the SAME enumeration where that page ended — which is what lets a client enumerating the
//! account's log groups see every one of them exactly once.
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

/// A page carries a `nextToken` exactly while log groups remain beyond it, and walking the
/// token reports every selected group exactly once.
#[tokio::test]
async fn a_page_carries_a_token_exactly_while_log_groups_remain() {
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
                walked.extend(names);
                pages += 1;
                assert!(
                    pages <= selected.len() + 1,
                    "case {case:?}: the walk terminates, got {pages} pages"
                );
                assert_eq!(
                    next.is_some(),
                    walked.len() < selected.len(),
                    "case {case:?}: expected a token exactly while groups remain, after \
                     {walked:?} of {selected:?}"
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
