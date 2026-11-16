// SPDX-License-Identifier: CC0-1.0

//! Probes of the query strings the Logs Insights QL compiler must ACCEPT, by language feature:
//! the counterpart of the unknown-function and duplicate-field refusals in
//! [`crate::insights_semantic_refusal_spans`].
//!
//! The rule under test: a query string that calls only functions the language defines, and
//! writes only operators and literals the language defines, COMPILES — the compiler refuses a
//! query for a fault in the query, never for a feature it has not been taught. So every
//! aggregation, scalar, string, datetime, IP and general function the CloudWatch Logs Insights
//! query syntax publishes compiles, and so does every arithmetic and comparison operator it
//! publishes.
//!
//! The tables span what the rule generalises over rather than one query: which GROUP of
//! functions the call belongs to (the groups the query-syntax documentation lists them in),
//! whether the call takes no argument, one, several or another call, which command carries it,
//! and which operator joins two operands.

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::query_compile::compile;

/// RULE: a call to a function the language defines compiles, whichever group it belongs to and
/// whichever command carries it.
#[test]
fn every_published_function_compiles() {
    // (case, the query string) — one row per group of the query-syntax function list, written
    // the way a query of that group's functions is written.
    let cases: Vec<(&str, &str)> = vec![
        (
            "the aggregation functions a stats command computes",
            "stats count(*) as n, count_distinct(user) as u, sum(ms) as s, avg(ms) as a, \
             min(ms) as mn, max(ms) as mx, stddev(ms) as sd, pct(ms, 95) as p95 by svc",
        ),
        (
            "the aggregation spellings the documentation gives as synonyms",
            "stats average(ms) as a, mean(ms) as m, dc(user) as d, percentile(ms, 50) as p",
        ),
        (
            "the non-aggregation stats functions, which pick a value out of a group",
            "stats earliest(ms) as e, latest(ms) as l, sortsFirst(svc) as f, sortsLast(svc) as t",
        ),
        (
            "the arithmetic functions",
            "fields abs(ms) as a, ceil(ms) as c, floor(ms) as f, greatest(ms, 1) as g, \
             least(ms, 1) as l, log(ms) as lg, sqrt(ms) as q",
        ),
        (
            "the string functions",
            "fields concat(svc, '-') as k, isblank(svc) as b, isempty(svc) as e, \
             ltrim(svc) as lt, rtrim(svc) as rt, trim(svc) as tr, strlen(svc) as n, \
             substr(svc, 0, 2) as s, toupper(svc) as u, tolower(svc) as d, \
             replace(svc, 'a', 'b') as r, strcontains(svc, 'a') as c",
        ),
        (
            "the datetime functions",
            "fields bin(5m) as b, dateceil(@timestamp, 1h) as c, datefloor(@timestamp, 1h) as f, \
             fromMillis(@timestamp) as t, toMillis(@timestamp) as m",
        ),
        (
            "the IP-address functions",
            "fields isIpInSubnet(ip, '10.0.0.0/8') as a, isIpv4InSubnet(ip, '10.0.0.0/8') as b, \
             isIpv6InSubnet(ip, '::/0') as c, isValidIp(ip) as d, isValidIpV4(ip) as e, \
             isValidIpV6(ip) as f",
        ),
        (
            "the general functions",
            "fields coalesce(svc, 'none') as s, ispresent(svc) as p | unmask @message",
        ),
        (
            "a call whose argument is another call",
            "stats max(strlen(@message)) as longest by svc",
        ),
        (
            "a call inside a filter expression",
            "filter ispresent(svc) and strlen(@message) > 1 | limit 1",
        ),
    ];
    for (case, query) in &cases {
        let compiled = compile(query);
        assert!(
            compiled.is_ok(),
            "case {case:?}: expected {query:?} to compile, got {:?}",
            compiled.err()
        );
    }
}

/// RULE: every operator the language publishes compiles, in the command that carries it.
#[test]
fn every_published_operator_compiles() {
    // (case, the query string)
    let cases: Vec<(&str, &str)> = vec![
        (
            "the arithmetic operators",
            "fields ms + 1 as a, ms - 1 as b, ms * 2 as c, ms / 2 as d, ms % 7 as e",
        ),
        (
            "the comparison operators",
            "filter ms = 1 or ms != 2 and ms < 3 or ms <= 4 or ms > 5 or ms >= 6",
        ),
        (
            "the match operators, each against a pattern",
            r"filter @message like /WARN/ or @message not like /INFO/ or @message =~ /ERROR/",
        ),
        (
            "the membership operators",
            r#"filter svc in ["api", "db"] or svc not in ["cache"]"#,
        ),
        (
            "the boolean operators and their negation",
            "filter not ispresent(svc) and (ms > 1 or ms < 0)",
        ),
    ];
    for (case, query) in &cases {
        let compiled = compile(query);
        assert!(
            compiled.is_ok(),
            "case {case:?}: expected {query:?} to compile, got {:?}",
            compiled.err()
        );
    }
}
