// SPDX-License-Identifier: CC0-1.0

//! The grid `GetPredictiveScalingForecast` answers a FRESH policy over, and the sentence it
//! refuses a policy name nothing answers to with.
//!
//! The rules under test:
//!
//! 1. The hours a forecast is reported over are the hours of the WINDOW the request asked for,
//!    not a function of how much the account has observed: "Load forecasts are predictions of
//!    the hourly load values using historical load data from CloudWatch … A minimum of 24
//!    hours of data is required to create the initial forecasts"
//!    (`smithy_query(query="operation",operation="GetPredictiveScalingForecast")`) says what
//!    decides each hour's VALUE, and the output declares `LoadForecast`, `CapacityForecast`
//!    and, inside each, `Timestamps` and `Values` required
//!    (`smithy_query(query="shape",shape="LoadForecast,CapacityForecast")`). So a policy
//!    created moments ago, whose load series the account holds no observation of, is answered
//!    with the whole hours of the window in strictly increasing order, one value per instant
//!    in each series, the capacity series on exactly that same grid, and no value — not a
//!    number — at every point of both.
//! 2. A `PolicyName` the named group holds no policy under is refused with HTTP 400
//!    `ValidationError` and this operation's closed sentence about it, whose only variable part
//!    is the name taken verbatim from the request.
//!
//! The rows of rule 1 span where the window sits relative to the hour the read is taken in
//! (wholly behind it, straddling it, wholly ahead of it) and how many hours it holds (one, a
//! few, a day, two days) — which is what the rule generalises over, and the axis the sibling
//! suites do not cover. Rule 2's rows differ in the FORM of the name, the sentence's one
//! variable part, and the sibling operation that names a policy the same way is asserted to
//! keep its own sentence.
//!
//! The harness is the sibling suite's (`predictive_forecast_unobserved_hours.rs`, which pins
//! the hours a pattern says nothing about, per metric-specification form): the emulator, the
//! group, the policy and the readers of a reported series are reached as
//! `crate::predictive_forecast_unobserved_hours::<item>` rather than copied.

use std::time::{Duration, SystemTime};

use crate::predictive_forecast_unobserved_hours::{
    create_group, customized_load_metric, emulator, forecasts, get_forecast, hours_of, put_policy,
    whole_hour, PERIOD,
};
use crate::support::{query, tag_value, xml_unescape};

/// `base` shifted by `hours`, which may be negative, so a row can place its window behind the
/// hour the read is taken in as well as ahead of it.
fn shifted(base: SystemTime, hours: i64) -> SystemTime {
    let span = Duration::from_secs(PERIOD.as_secs() * hours.unsigned_abs());
    if hours < 0 {
        base - span
    } else {
        base + span
    }
}

/// The wire code and message one refusal reports, as a client reads them.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// RULE 1: a policy created moments ago, over a series the account holds no observation of, is
/// answered over every whole hour of the window asked for — wherever that window sits relative
/// to the hour of the read, and however many hours it holds — with both series on that one
/// strictly increasing grid and no value at any of its points.
#[tokio::test]
async fn a_fresh_policy_is_forecast_over_every_hour_of_the_window() {
    let router = emulator();
    let group = "asg-forecast-window";
    let policy = "forecast-window-policy";
    create_group(&router, group).await;
    put_policy(&router, group, policy, &customized_load_metric()).await;

    // The hour the read is taken in, so each row states where its window sits relative to it.
    let anchor = whole_hour(SystemTime::now());
    // (case, the window's first hour relative to the anchor, how many hours it holds).
    let rows: [(&str, i64, i64); 6] = [
        ("a window wholly behind the read", -48, 24),
        ("a window of a day either side of the read", -24, 48),
        ("a window straddling the read narrowly", -3, 6),
        ("a window opening at the hour of the read", 0, 3),
        ("a window wholly ahead of the read", 2, 12),
        ("a window of one hour", 5, 1),
    ];
    for (case, first_hour, hours) in rows {
        let start = shifted(anchor, first_hour);
        let end = shifted(start, hours);
        let (status, body) = get_forecast(&router, group, policy, start, end).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the forecast must be served, got {status} with {body}"
        );

        // "The time the forecast was made" is reported for every answer.
        let update_time = tag_value(&body, "UpdateTime");
        assert!(
            ::aws_smithy_types::DateTime::from_str(
                &update_time,
                ::aws_smithy_types::date_time::Format::DateTime,
            )
            .is_ok(),
            "case {case:?}: expected the instant the forecast was made, got {update_time:?} in \
             {body}"
        );

        let expected = hours_of(start, end);
        let ((load_at, loads), (capacity_at, capacities)) = forecasts(&body);
        assert_eq!(
            load_at, expected,
            "case {case:?}: expected the whole hours of the window, got {load_at:?} in {body}"
        );
        assert_eq!(
            capacity_at, expected,
            "case {case:?}: the capacity series must sit on the load series' grid, got \
             {capacity_at:?} against {load_at:?}"
        );
        assert_eq!(
            (loads.len(), capacities.len()),
            (expected.len(), expected.len()),
            "case {case:?}: expected one value per instant of a {}-hour window, got {loads:?} \
             and {capacities:?}",
            expected.len()
        );
        assert!(
            !loads.is_empty(),
            "case {case:?}: the window holds whole hours, so the series must carry them, got \
             {body}"
        );
        // Strictly increasing, by exactly one forecast period per step.
        let step = i64::try_from(PERIOD.as_secs()).expect("an hour fits");
        for pair in load_at.windows(2) {
            assert_eq!(
                pair[1] - pair[0],
                step,
                "case {case:?}: the grid must rise by one hour per point, got {pair:?} in \
                 {load_at:?}"
            );
        }
        for (slot, (load, capacity)) in loads.iter().zip(&capacities).enumerate() {
            assert!(
                load.is_nan() && capacity.is_nan(),
                "case {case:?}: hour {slot} of a series nobody published into must carry no \
                 value in either series, got the load {load} needing {capacity}"
            );
        }
    }
}

/// RULE 2: a `PolicyName` the named group holds no policy under is refused with this
/// operation's closed sentence, whose only variable part is the name as the request stated it
/// — while the group goes on answering for the policy it does hold, and the sibling operation
/// that names a policy the same way keeps its own sentence.
#[tokio::test]
async fn an_absent_policy_name_is_refused_with_a_closed_sentence() {
    let router = emulator();
    let group = "asg-forecast-absent-policy";
    let kept = "forecast-kept-policy";
    create_group(&router, group).await;
    put_policy(&router, group, kept, &customized_load_metric()).await;

    let start = whole_hour(SystemTime::now()) + PERIOD;
    let end = shifted(start, 4);

    // The rows differ in the FORM of the sentence's one variable part: "The name of the
    // policy" (`GetPredictiveScalingForecastType$PolicyName`) is a string of 1..255
    // characters whose pattern admits spaces and punctuation, so a short name, a name
    // carrying either, an ARN spelling and a near-miss of the name the group DOES hold are
    // all names the group holds no policy under.
    let names = [
        "nonesuch",
        "a",
        "scale out (peak hours)",
        "arn:aws:autoscaling:us-west-2:000000000000:scalingPolicy:abc:\
         autoScalingGroupName/asg-forecast-absent-policy:policyName/ghost",
        "forecast-kept-policy-2",
    ];
    for name in names {
        let (status, body) = get_forecast(&router, group, name, start, end).await;
        let (code, message) = refusal(&body);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                400,
                "ValidationError",
                format!("Policy {name} not found.").as_str()
            ),
            "case {name:?}: expected the absent-policy refusal, got {status} with {body}"
        );
    }

    // The group still answers for the policy it DOES hold, so the refusals above are about the
    // name and not about this group or this operation.
    let (status, body) = get_forecast(&router, group, kept, start, end).await;
    assert_eq!(
        status, 200,
        "the policy the group holds must still be forecast, got {status} with {body}"
    );

    // The sibling operation that names a policy the same way states its OWN sentence about a
    // name nothing answers to, so the two refusals are not one wording.
    let (status, body) = query(
        &router,
        "ExecutePolicy",
        &[("AutoScalingGroupName", group), ("PolicyName", "nonesuch")],
    )
    .await;
    let (code, message) = refusal(&body);
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, "ValidationError", "Policy nonesuch not found"),
        "the sibling operation must keep its own absent-policy sentence, got {status} with \
         {body}"
    );
}
