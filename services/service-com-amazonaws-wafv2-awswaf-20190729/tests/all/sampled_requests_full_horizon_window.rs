// SPDX-License-Identifier: CC0-1.0

//! The window a client asking for "everything WAF still holds" sends, end to end over the
//! wire through the same router the emulator serves from.
//!
//! RULE: `GetSampledRequests` NARROWS the `TimeWindow` it is given to the range it can
//! sample over and answers 200 — "you can specify any time range in the previous three
//! hours. If you specify a start time that's earlier than three hours ago, WAF sets it to
//! three hours ago" (`GetSampledRequests$TimeWindow`,
//! `smithy_query(query="operation", operation="GetSampledRequests")`) — so a start at or
//! before the horizon is not a parameter fault: the response's `TimeWindow` reports the
//! range that was sampled, starting at the horizon, and the modelled
//! `WAFInvalidParameterException` is kept for the faults the model names (a malformed ARN,
//! a `Scope` this endpoint does not serve).
//!
//! That is the window the documented poll loop computes: a caller that wants the whole
//! retained window takes `EndTime` now and `StartTime` three hours before it, and every
//! moment that passes between computing the pair and the call being served puts its start
//! further outside the horizon — so a bound refused strictly there refuses the natural
//! request, and refuses it again on every re-poll with a fresh `EndTime`.
//!
//! The rows span that boundary rather than one offset: the start exactly three hours
//! before the end, one second further back, a whole hour further back, and far outside
//! (ten hours, the shape of a caller asking for more than WAF retains) — each in both
//! scopes, and each with the end at the call instant and one with the end in the future.
//! Every row asserts the narrowed range it must come back with, naming the case and the
//! actual value.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::web_acl_lifecycle::{create_web_acl, rule, sqli_statement, Waf};

/// How far back a sampling window reaches, in seconds: "you can specify any time range in
/// the previous three hours", from the `TimeWindow` member's own doc. Written out here
/// rather than imported, so this suite does not restate the code under test.
const SAMPLING_HORIZON_SECS: f64 = 3.0 * 60.0 * 60.0;

/// How far the clock this test reads may differ from the clock the handler read: both are
/// the system clock, so the difference is the time the call took. Generous enough that a
/// loaded machine does not fail a row, and far smaller than any offset below.
const CLOCK_SLACK_SECS: f64 = 60.0;

/// The rule every web ACL below carries, and the dimension the sample is asked for.
const RULE_METRIC: &str = "horizon-rule";

/// Seconds since the Unix epoch, truncated to a whole second so every offset is exactly
/// representable on the wire.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs() as f64
}

/// One end of the reported window, in seconds since the epoch.
fn window_end(body: &Value, member: &str) -> f64 {
    body["TimeWindow"][member]
        .as_f64()
        .unwrap_or_else(|| panic!("TimeWindow.{member} must be an epoch-seconds number, got {body}"))
}

/// One row: the case, and the offsets of the requested window's ends from the call instant.
struct Row {
    case: &'static str,
    start_offset: f64,
    end_offset: f64,
}

/// The rows: every way the full-horizon window a poll loop computes can fall outside the
/// horizon by the time the call is served, plus one asking for far more than WAF retains.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the whole horizon, computed at the call instant",
            start_offset: -SAMPLING_HORIZON_SECS,
            end_offset: 0.0,
        },
        Row {
            case: "one second further back than the horizon",
            start_offset: -(SAMPLING_HORIZON_SECS + 1.0),
            end_offset: 0.0,
        },
        Row {
            case: "an hour further back than the horizon",
            start_offset: -(SAMPLING_HORIZON_SECS + 3600.0),
            end_offset: 0.0,
        },
        Row {
            case: "ten hours, far more than WAF retains",
            start_offset: -(10.0 * 60.0 * 60.0),
            end_offset: 0.0,
        },
        Row {
            case: "the whole horizon, re-polled with an end in the future",
            start_offset: -SAMPLING_HORIZON_SECS,
            end_offset: 300.0,
        },
    ]
}

#[tokio::test]
async fn a_full_horizon_window_is_served_and_narrowed_to_the_horizon() {
    let waf = Waf::new();
    let (_regional_id, regional_arn, _regional_lock) = create_web_acl(
        &waf,
        "REGIONAL",
        "horizon-regional",
        json!([rule(RULE_METRIC, sqli_statement())]),
    )
    .await;
    let (_cloudfront_id, cloudfront_arn, _cloudfront_lock) = create_web_acl(
        &waf,
        "CLOUDFRONT",
        "horizon-cloudfront",
        json!([rule(RULE_METRIC, sqli_statement())]),
    )
    .await;

    for (scope, arn) in [
        ("REGIONAL", &regional_arn),
        ("CLOUDFRONT", &cloudfront_arn),
    ] {
        for row in rows() {
            let case = format!("{scope} {}", row.case);
            let base = now_secs();
            let body = waf
                .ok(
                    "GetSampledRequests",
                    json!({
                        "WebAclArn": arn,
                        "RuleMetricName": RULE_METRIC,
                        "Scope": scope,
                        "TimeWindow": {
                            "StartTime": base + row.start_offset,
                            "EndTime": base + row.end_offset,
                        },
                        "MaxItems": 100,
                    }),
                )
                .await;

            let start = window_end(&body, "StartTime");
            let end = window_end(&body, "EndTime");
            let horizon_start = base - SAMPLING_HORIZON_SECS;
            assert!(
                (start - horizon_start).abs() <= CLOCK_SLACK_SECS,
                "case {case:?}: expected the reported start at the horizon ({horizon_start}), \
                 got {start}"
            );
            assert!(
                end <= base + CLOCK_SLACK_SECS && end >= start,
                "case {case:?}: expected the reported end no later than the call instant \
                 ({base}) and no earlier than {start}, got {end}"
            );
            assert_eq!(
                body["SampledRequests"],
                json!([]),
                "case {case:?}: expected a sample of the requests inspected in that window, \
                 got {body}"
            );
            assert_eq!(
                body["PopulationSize"].as_i64(),
                Some(0),
                "case {case:?}: expected PopulationSize 0, got {body}"
            );
        }
    }
}
