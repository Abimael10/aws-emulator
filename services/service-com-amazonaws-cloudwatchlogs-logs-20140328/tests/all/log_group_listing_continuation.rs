// SPDX-License-Identifier: CC0-1.0

//! How `DescribeLogGroups` CONTINUES a listing, and how it treats the substring pattern a
//! listing selects with.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.DescribeLogGroups` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rules under test:
//!
//! * **A page that reported a log group hands back a `nextToken`**, whether or not groups
//!   follow it: a continuation is how a client learns the enumeration is finished, and the
//!   page that reports nothing is the one that carries no token. Walking the tokens covers the
//!   selected groups exactly once, in name order, and terminates.
//! * **`logGroupNamePattern` is LITERAL TEXT for a case-sensitive substring search**, not an
//!   expression: a value carrying a character a log group name can never hold matches no log
//!   group, which is a success whose `logGroups` is the EMPTY LIST rather than a refusal. The
//!   member's length is still a bound on what the service takes.
//!
//! The tables span what the rules generalise over — how many groups are selected, the page
//!   size, and which character outside a log group name's own class the pattern carries —
//! rather than one recorded query.

use serde_json::{json, Value};

use crate::harness::{error_code, Logs};

/// The operation under test.
const DESCRIBE: &str = "DescribeLogGroups";

/// The one client error this operation declares.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The longest `logGroupNamePattern` the member admits, from `LogGroupNamePattern`'s
/// `@length(min: 0, max: 512)`
/// (`smithy_query(query="shape",shape="LogGroupNamePattern")`). Stated here independently of
/// the crate under test.
const PATTERN_MAX_LEN: usize = 512;

/// The log groups every case is answered from, in the ASCII order the listing reports them in
/// ("The results are ASCII-sorted by log group name").
const GROUPS: [&str; 3] = [
    "/aws/lambda/beta",
    "/aws/opensearch/domains/alpha/audit-logs",
    "alpha-app-logs",
];

/// The names one page reports.
fn names(case: &str, page: &Value) -> Vec<String> {
    page.get("logGroups")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a served listing reports logGroups, got {page}"))
        .iter()
        .map(|group| {
            group
                .get("logGroupName")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: every group carries a name, got {group}"))
                .to_string()
        })
        .collect()
}

/// The groups of [`GROUPS`], created.
async fn account_with_groups(logs: &Logs) {
    for group in GROUPS {
        logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
            .await;
    }
}

/// Every page that reported a group carries a token, the walk covers the selection exactly
/// once in name order, and it ends at the empty page that carries no token — over a grid of
/// selection sizes and page sizes.
#[tokio::test]
async fn a_page_that_reported_a_group_hands_back_a_continuation() {
    let logs = Logs::new();
    account_with_groups(&logs).await;

    // (case, the selector, the names it selects in order)
    let selections: Vec<(&str, Value, Vec<&str>)> = vec![
        ("the whole account", json!({}), GROUPS.to_vec()),
        (
            "one group under a prefix",
            json!({"logGroupNamePrefix": "alpha"}),
            vec![GROUPS[2]],
        ),
        (
            "two groups under a prefix",
            json!({"logGroupNamePrefix": "/aws/"}),
            vec![GROUPS[0], GROUPS[1]],
        ),
        (
            "a substring pattern selecting one",
            json!({"logGroupNamePattern": "audit"}),
            vec![GROUPS[1]],
        ),
        (
            "a selection that matches nothing",
            json!({"logGroupNamePattern": "nothing-matches-this"}),
            vec![],
        ),
    ];

    for (case, selector, selected) in &selections {
        for limit in 1_usize..=3 {
            let case = format!("{case}, limit {limit}");
            let mut walked: Vec<String> = Vec::new();
            let mut token: Option<String> = None;
            let mut pages = 0usize;
            loop {
                pages += 1;
                assert!(
                    pages <= selected.len() + 2,
                    "case {case:?}: the walk must terminate, got {pages} pages"
                );
                let mut request = selector.clone();
                request["limit"] = json!(limit);
                if let Some(token) = &token {
                    request["nextToken"] = json!(token);
                }
                let page = logs.ok(DESCRIBE, &request).await;
                let reported = names(&case, &page);
                assert!(
                    reported.len() <= limit,
                    "case {case:?}: a page holds at most {limit} groups, got {page}"
                );
                let next = page.get("nextToken").and_then(Value::as_str);
                assert_eq!(
                    next.is_some(),
                    !reported.is_empty(),
                    "case {case:?}: a page carries a token exactly when it reported a group, \
                     got {page}"
                );
                walked.extend(reported.clone());
                match next {
                    Some(next) => token = Some(next.to_string()),
                    None => break,
                }
            }
            assert_eq!(
                walked,
                selected
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<String>>(),
                "case {case:?}: the walk covers the selection once, in name order"
            );
        }
    }
}

/// Every member BESIDES the literal-text pattern is still judged by its own constraint, so the
/// relaxation above is ONE member wide: a value outside a length bound, a numeric range, a
/// character class or an enum's value set is still refused, with that constraint stated.
#[tokio::test]
async fn every_other_selector_is_still_judged_by_its_own_constraint() {
    let logs = Logs::new();
    account_with_groups(&logs).await;

    // (case, the request, the constraint clause the refusal states)
    let refused: Vec<(&str, Value, &str)> = vec![
        (
            "a page limit below the range",
            json!({"limit": 0}),
            "Member must have value greater than or equal to 1",
        ),
        (
            "a page limit above the range",
            json!({"limit": 51}),
            "Member must have value less than or equal to 50",
        ),
        (
            "a zero-length continuation token",
            json!({"nextToken": ""}),
            "Member must have length greater than or equal to 1",
        ),
        (
            "an empty identifier list",
            json!({"logGroupIdentifiers": []}),
            "Member must have length greater than or equal to 1",
        ),
        (
            "a log group class the enum does not hold",
            json!({"logGroupClass": "ARCHIVE"}),
            "Member must satisfy enum value set: [DELIVERY, INFREQUENT_ACCESS, STANDARD]",
        ),
        (
            "a name prefix carrying a character a log group name cannot hold",
            json!({"logGroupNamePrefix": "alpha*"}),
            r"Member must satisfy regular expression pattern: [\.\-_/#A-Za-z0-9]+",
        ),
        (
            "an identifier carrying a character an identifier cannot hold",
            json!({"logGroupIdentifiers": ["alpha app logs"]}),
            r"Member must satisfy regular expression pattern: [\w#+=/:,.@-]*",
        ),
    ];
    for (case, request, clause) in &refused {
        let (status, parsed) = logs.call(DESCRIBE, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {request} must be refused, got {status} {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {parsed}"
        );
        let message = parsed
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: a refusal carries a message, got {parsed}"));
        assert!(
            message.contains(clause),
            "case {case:?}: expected the clause {clause:?}, got {message:?}"
        );
    }

    // The same members INSIDE their constraints are served, so the rows above are refused for
    // the value they carry rather than for the member they name.
    let served: Vec<(&str, Value)> = vec![
        ("the smallest page limit", json!({"limit": 1})),
        ("the largest page limit", json!({"limit": 50})),
        (
            "the one admitted class of these groups",
            json!({"logGroupClass": "STANDARD"}),
        ),
        (
            "a name prefix inside the class",
            json!({"logGroupNamePrefix": "alpha"}),
        ),
        (
            "an identifier inside the class",
            json!({"logGroupIdentifiers": [GROUPS[2]]}),
        ),
    ];
    for (case, request) in &served {
        let page = logs.ok(DESCRIBE, request).await;
        assert!(
            page.get("logGroups").and_then(Value::as_array).is_some(),
            "case {case:?}: a served listing reports logGroups, got {page}"
        );
    }
}

/// A `logGroupNamePattern` is matched as literal text: a value carrying a character a log
/// group name can never hold selects nothing and is SERVED with an empty list, and a value
/// that is a substring of a name selects exactly the names holding it.
#[tokio::test]
async fn a_pattern_that_cannot_match_is_an_empty_page_and_not_a_refusal() {
    let logs = Logs::new();
    account_with_groups(&logs).await;

    // Each row carries a character outside the class a log group name is built from —
    // `[\.\-_/#A-Za-z0-9]`, the expression this service publishes for `LogGroupName`
    // (`smithy_query(query="shape",shape="LogGroupName")`) — so no name can ever hold it.
    // (case, the pattern)
    let unmatchable: Vec<(&str, String)> = vec![
        ("a trailing wildcard", "alpha*".to_string()),
        ("a question mark", "alpha?logs".to_string()),
        ("a space", "alpha app logs".to_string()),
        (
            "an ARN, which carries colons",
            "arn:aws:logs:eu-west-2:000000000000:log-group:alpha-app-logs".to_string(),
        ),
        (
            "alternatives separated by a pipe",
            "audit-logs|app-logs".to_string(),
        ),
        ("a braced placeholder", "{domain}-app-logs".to_string()),
        ("a plus", "alpha+logs".to_string()),
        // The one row inside the class: the search is case-SENSITIVE, so a name's own text in
        // another case matches nothing either.
        ("the same text in another case", "AUDIT".to_string()),
        // One character past the published length bound is refused, not served, so the bound
        // itself is a served row and its neighbour a refusal (below).
        ("the longest admitted pattern", "z".repeat(PATTERN_MAX_LEN)),
    ];
    for (case, pattern) in &unmatchable {
        let (status, page) = logs
            .call(DESCRIBE, &json!({"logGroupNamePattern": pattern}))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a pattern that matches nothing is served, got {status} {page}"
        );
        assert_eq!(
            names(case, &page),
            Vec::<String>::new(),
            "case {case:?}: expected an empty list, got {page}"
        );
        assert!(
            page.get("logGroups").and_then(Value::as_array).is_some(),
            "case {case:?}: logGroups is reported as an empty list, not omitted, got {page}"
        );
    }

    // (case, the pattern, the names it selects)
    let matching: Vec<(&str, String, Vec<&str>)> = vec![
        ("a substring of one name", "audit".to_string(), vec![GROUPS[1]]),
        (
            "a substring of two names",
            "alpha".to_string(),
            vec![GROUPS[1], GROUPS[2]],
        ),
        (
            "a whole name",
            GROUPS[0].to_string(),
            vec![GROUPS[0]],
        ),
        // `@length(min: 0)`: a zero-length value is legal, and every name holds the empty
        // string.
        ("the empty pattern", String::new(), GROUPS.to_vec()),
    ];
    for (case, pattern, selected) in &matching {
        let page = logs
            .ok(DESCRIBE, &json!({"logGroupNamePattern": pattern}))
            .await;
        assert_eq!(
            names(case, &page),
            selected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>(),
            "case {case:?}: expected exactly the names holding the pattern, got {page}"
        );
    }

    // The member's LENGTH is still a bound on what the service takes: one character past the
    // maximum is refused, with the constraint the member declares.
    let (status, refusal) = logs
        .call(
            DESCRIBE,
            &json!({"logGroupNamePattern": "z".repeat(PATTERN_MAX_LEN + 1)}),
        )
        .await;
    assert_eq!(
        status, 400,
        "case \"one character past the length bound\": expected a refusal, got {status} {refusal}"
    );
    assert_eq!(
        error_code(&refusal),
        INVALID_PARAMETER,
        "case \"one character past the length bound\": expected {INVALID_PARAMETER}, got {refusal}"
    );
    assert_eq!(
        refusal.get("message").and_then(Value::as_str),
        Some(
            format!(
                "1 validation error detected: Value at 'logGroupNamePattern' failed to satisfy \
                 constraint: Member must have length less than or equal to {PATTERN_MAX_LEN}"
            )
            .as_str()
        ),
        "case \"one character past the length bound\": expected the member's length \
         constraint, got {refusal}"
    );
}
