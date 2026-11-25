// SPDX-License-Identifier: CC0-1.0

//! `GetScalingPlanResourceForecastData` over the real wire path.
//!
//! Four rules are pinned here, each over a TABLE of requests rather than the one that
//! exposed them:
//!
//! * the forecast WINDOW is checked against its documented limits before any plan is
//!   looked up, so the same window earns the same answer whether or not the named plan
//!   exists — `EndTime` has to be later than `StartTime` and the window may span at most
//!   `MAX_FORECAST_WINDOW_DAYS` days, the width measured in the whole hourly forecast
//!   periods the data exists at, whatever sub-second precision the window's endpoints
//!   carry. Nothing bounds WHERE the window sits: `MAX_FORECAST_HISTORY_DAYS` is how far
//!   back forecast DATA is available, so a window outside it — like one reaching into the
//!   future — is served with the data points it holds rather than refused;
//! * a request that passes those checks is looked up by (`ScalingPlanName`,
//!   `ScalingPlanVersion`), and a lookup that finds no stored plan has no forecast to
//!   report: HTTP 200 with an empty `Datapoints` list, because this operation models no
//!   not-found error (`crate::absent_plan_reads` pins that rule across both reads, and
//!   `crate::plan_version_selects_a_plan` the version half of the coordinate);
//! * the resource coordinates are refused only where the `(ServiceNamespace, ResourceId,
//!   ScalableDimension)` triple describes no one scalable resource — a dimension owned by
//!   another service namespace, or a resource id that identifies no resource of the kind
//!   its dimension scales — which earns the modelled `ValidationException` (HTTP 400);
//!   which resources this service holds a forecast FOR is decided by the stored plan, and
//!   the served side of that rule is pinned in
//!   [`crate::forecast_resource_triple_consistency`];
//! * an accepted request answers HTTP 200 with the required `Datapoints` member;
//! * a window's answer — refused, or served with the forecast it holds — is decided by the
//!   two window members ALONE: the same envelope earns the same answer for every
//!   `ForecastDataType`, for a plan that forecasts the named resource and one that does
//!   not, and for a name this service does not hold. A served envelope's `Datapoints` list
//!   is then whatever the window really holds: the forecast of every interval inside the
//!   span the data exists over, which is empty exactly when the window holds none of them.
//!
//! Every plan these tests store configures DYNAMIC scaling only — its instruction carries
//! no load metric specification, which is what "configur[es] predictive scaling" — so it
//! has no forecast and its accepted answers carry no data point, whatever the window or
//! the forecast data type. What a plan that DOES configure predictive scaling forecasts is
//! pinned in [`crate::forecast_datapoints`].
//!
//! The harness (router, dispatch, error-body readers) is reused from
//! [`crate::scaling_plan_lifecycle`]; nothing here builds a second one.

use serde_json::{json, Value};

use service_com_amazonaws_autoscalingplans_anyscalescalingplannerfrontendservice::ops::get_scaling_plan_resource_forecast_data::{
    FORECAST_HORIZON_DAYS, FORECAST_INTERVAL_SECONDS, FORECAST_SCALABLE_DIMENSION,
    FORECAST_SERVICE_NAMESPACE, MAX_FORECAST_HISTORY_DAYS, MAX_FORECAST_WINDOW_DAYS,
};
use service_com_amazonaws_autoscalingplans_anyscalescalingplannerfrontendservice::ops::types::FORECAST_DATA_TYPE_VALUES;

use crate::scaling_plan_lifecycle::{
    error_code, error_message, instruction, now_secs, simple_instructions, stack_source, Emulator,
    DAY_SECONDS as DAY,
};

/// The operation under test.
const OPERATION: &str = "GetScalingPlanResourceForecastData";

/// One forecast period in seconds: the granularity forecast data exists at, so it is both
/// the offset a window's endpoints are placed inside and the span the documented day
/// bounds are measured in.
const HOUR: i64 = FORECAST_INTERVAL_SECONDS;

/// The Auto Scaling group the stored plan of these tests scales. Its id carries the
/// documented prefix: `ResourceId` "consists of a prefix (autoScalingGroup) followed by
/// the name of a specified Auto Scaling group (my-asg)".
const COVERED_RESOURCE: &str = "autoScalingGroup/my-asg";

/// An Auto Scaling group no stored plan of these tests covers.
const UNCOVERED_RESOURCE: &str = "autoScalingGroup/other-asg";

/// The plan the emulator holds in these tests.
const STORED_PLAN: &str = "forecast plan";

/// A plan name this service does not hold.
const ABSENT_PLAN: &str = "no such plan";

/// The only scaling-plan version the model documents as valid.
///
/// `pub(crate)` so a suite driving the same operation names the same version.
pub(crate) const ONLY_VERSION: i64 = 1;

/// How far from a documented bound a row is placed when the verdict depends on the
/// SERVING clock: the service reads its own `now` a few milliseconds after this test
/// builds the request, so a row sits a whole minute inside or past the bound and the
/// elapsed time cannot flip it.
const CLOCK_MARGIN_SECONDS: i64 = 60;

/// The coordinates of one scalable resource: `(ServiceNamespace, ResourceId,
/// ScalableDimension)`.
type Resource<'a> = (&'a str, &'a str, &'a str);

/// The Auto Scaling group resource the stored plan of these tests covers.
const ASG_RESOURCE: Resource<'static> = (
    FORECAST_SERVICE_NAMESPACE,
    COVERED_RESOURCE,
    FORECAST_SCALABLE_DIMENSION,
);

/// One forecast request, with every member the model marks required: a plan name and
/// version, a scalable resource, a forecast data type and the window `[start, end)`.
///
/// The window endpoints are generic over how they go on the wire, because the `awsJson1_1`
/// `TimestampType` is carried as epoch SECONDS and a caller may send either a whole count
/// (`i64`) or a fractional one (`f64`): both reach the same handler, so both are driven
/// through this one request builder.
pub(crate) fn forecast_request<T: Into<Value>>(
    plan: &str,
    version: i64,
    resource: Resource<'_>,
    data_type: &str,
    window: (T, T),
) -> Value {
    let (namespace, resource_id, dimension) = resource;
    let (start, end) = window;
    let (start, end) = (start.into(), end.into());
    json!({
        "ScalingPlanName": plan,
        "ScalingPlanVersion": version,
        "ServiceNamespace": namespace,
        "ResourceId": resource_id,
        "ScalableDimension": dimension,
        "ForecastDataType": data_type,
        "StartTime": start,
        "EndTime": end,
    })
}

/// A forecast request for the Auto Scaling group resource, over `[start, end)`.
///
/// `pub(crate)` so every suite driving this operation builds its request here rather
/// than beside this one.
pub(crate) fn asg_request<T: Into<Value>>(
    plan: &str,
    version: i64,
    data_type: &str,
    window: (T, T),
) -> Value {
    forecast_request(plan, version, ASG_RESOURCE, data_type, window)
}

/// An emulator holding one `Active` plan that covers [`COVERED_RESOURCE`].
async fn with_stored_plan() -> Emulator {
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateScalingPlan",
            json!({
                "ScalingPlanName": STORED_PLAN,
                "ApplicationSource": stack_source(1),
                "ScalingInstructions": simple_instructions(),
            }),
        )
        .await;
    emulator.make_all_due();
    emulator
}

/// The `Datapoints` member of a successful response, which the model marks required.
///
/// Asserted in two parts, because they are two different claims: the member is PRESENT
/// and carries the modelled list shape (an array), and separately — at each call site —
/// how many data points that list holds.
///
/// `pub(crate)` so every suite reads the member the same way.
pub(crate) fn datapoints(body: &Value) -> &Vec<Value> {
    assert!(
        !body["Datapoints"].is_null(),
        "the required Datapoints member must be present, got {body}"
    );
    body.get("Datapoints")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected Datapoints to be a list, got {body}"))
}

/// What the window rules answer for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// HTTP 200 carrying the required `Datapoints` list, which is empty because the
    /// stored plan configures no predictive scaling.
    Accepted,
    /// HTTP 400 `ValidationException`, because the window spans more than the documented
    /// maximum duration.
    RefusedAsTooWide,
    /// HTTP 400 `ValidationException`, because the end is not after the start.
    RefusedAsOutOfOrder,
}

/// The start of the clock hour containing now.
///
/// Every width row below places its two endpoints at a KNOWN offset inside an hour, so a
/// row that used to be accepted only because both endpoints fell in hour-aligned periods
/// seven days apart is exercised whatever time the test happens to run at.
pub(crate) fn current_hour_start() -> i64 {
    now_secs().div_euclid(HOUR) * HOUR
}

/// Assert the verdict of one window on one plan name, the window carried as the whole
/// epoch seconds most rows here use.
async fn assert_window_verdict(
    emulator: &Emulator,
    label: &str,
    held: &str,
    plan: &str,
    window: (i64, i64),
    verdict: Verdict,
) {
    let body = asg_request(plan, ONLY_VERSION, "CapacityForecast", window);
    assert_verdict(emulator, label, held, &format!("{window:?}"), &body, verdict).await;
}

/// Assert the verdict of one forecast request, however its window went on the wire.
///
/// `window` is that window as the row sent it, so a failure names the request that earned
/// the wrong answer.
async fn assert_verdict(
    emulator: &Emulator,
    label: &str,
    held: &str,
    window: &str,
    body: &Value,
    verdict: Verdict,
) {
    let (status, parsed) = emulator.call(OPERATION, body).await;
    if verdict == Verdict::Accepted {
        assert_eq!(
            status, 200,
            "case {label:?} on {held} ({window}): expected HTTP 200, got {status} \
             with {parsed}"
        );
        assert!(
            datapoints(&parsed).is_empty(),
            "case {label:?} on {held} ({window}): expected no data points, got {parsed}"
        );
        return;
    }

    assert_eq!(
        (status, error_code(&parsed)),
        (400, "ValidationException"),
        "case {label:?} on {held} ({window}): expected HTTP 400 ValidationException, \
         got {status} with {parsed}"
    );
    let message = error_message(&parsed);

    // The sentence is read only to tell WHICH relation refused the row — the width or the
    // endpoint order — not because a message text is the rule.
    let named = message.contains(&format!("{MAX_FORECAST_WINDOW_DAYS} days"));
    let expected = match verdict {
        Verdict::RefusedAsTooWide => true,
        Verdict::Accepted | Verdict::RefusedAsOutOfOrder => false,
    };
    assert_eq!(
        named, expected,
        "case {label:?} on {held} ({window}): expected the refusal for {verdict:?}, \
         got {message:?}"
    );
}

#[tokio::test]
async fn the_forecast_window_width_is_measured_in_whole_forecast_periods() {
    let week = MAX_FORECAST_WINDOW_DAYS * DAY;
    // Rows differ along what the rule generalises over: the WIDTH of the window, the
    // ORDER of its two endpoints, where inside its clock hour each endpoint sits, and
    // whether the window lies ahead of or behind now. The documented maximum duration is
    // probed inside, exactly at, part of a forecast period past, a whole period past and
    // far past the bound — the overshoots shorter than one period ask for no forecast
    // period beyond the bound, so they are served.
    //
    // `(label, offset of the start inside the current hour, width in seconds, verdict)`.
    let cases: [(&str, i64, i64, Verdict); 17] = [
        ("a one-second window", 600, 1, Verdict::Accepted),
        (
            "a ten-minute window inside a single hour",
            600,
            600,
            Verdict::Accepted,
        ),
        (
            "an hour-long window starting on the hour",
            0,
            HOUR,
            Verdict::Accepted,
        ),
        (
            "a window one second inside the documented maximum width",
            0,
            week - 1,
            Verdict::Accepted,
        ),
        (
            "a window exactly at the documented maximum width",
            0,
            week,
            Verdict::Accepted,
        ),
        (
            "a maximum-width window starting mid-hour",
            1_777,
            week,
            Verdict::Accepted,
        ),
        (
            "a window one second past the documented maximum width",
            0,
            week + 1,
            Verdict::Accepted,
        ),
        (
            "a minute past the maximum width, both endpoints inside one hour",
            800,
            week + 60,
            Verdict::Accepted,
        ),
        (
            "half an hour past the maximum width, both endpoints inside one hour",
            1_000,
            week + 1_800,
            Verdict::Accepted,
        ),
        (
            "the last second of the period past the maximum width",
            0,
            week + HOUR - 1,
            Verdict::Accepted,
        ),
        (
            "a whole forecast period past the maximum width",
            0,
            week + HOUR,
            Verdict::RefusedAsTooWide,
        ),
        (
            "a day past the maximum width",
            0,
            week + DAY,
            Verdict::RefusedAsTooWide,
        ),
        (
            "a window ten days wide",
            800,
            10 * DAY,
            Verdict::RefusedAsTooWide,
        ),
        (
            "an end time equal to the start time",
            600,
            0,
            Verdict::RefusedAsOutOfOrder,
        ),
        (
            "an end time a second before the start time",
            600,
            -1,
            Verdict::RefusedAsOutOfOrder,
        ),
        (
            "an end time half an hour before the start time",
            600,
            -1_800,
            Verdict::RefusedAsOutOfOrder,
        ),
        (
            "an end time a week before the start time",
            600,
            -week,
            Verdict::RefusedAsOutOfOrder,
        ),
    ];

    let emulator = with_stored_plan().await;
    // Every row is checked twice: against the plan the emulator holds and against a name
    // it does not hold. Both earn the SAME verdict: the window checks are request-only, so
    // a row they refuse is refused either way, and a row they accept is served either way
    // — the empty forecast of a plan that is not held reads exactly like the empty
    // forecast of a held plan with nothing to forecast.
    for (plan, held) in [(STORED_PLAN, "a stored plan"), (ABSENT_PLAN, "an absent plan")] {
        for (label, start_offset, width, verdict) in cases {
            let start = current_hour_start() + start_offset;
            assert_window_verdict(&emulator, label, held, plan, (start, start + width), verdict)
                .await;
        }
    }
}

#[tokio::test]
async fn a_window_further_back_than_the_documented_data_span_is_served() {
    // One width — a day, well inside the documented maximum — at every position the
    // request can put it: ahead of now, around now, just inside the span forecast data is
    // available over, just past it, and far past it on both sides. Every one of them is
    // SERVED: the 56 days `StartTime`'s prose names is how far back forecast DATA exists
    // ("Data points are available for up to 56 days"), so a window outside it carries the
    // data points that exist in it — none — rather than being refused, and `EndTime` "can
    // accept a date and time that is more than two days in the future", so neither
    // direction is bounded.
    //
    // `(label, offset of the start from now in seconds, verdict)`.
    let history = MAX_FORECAST_HISTORY_DAYS * DAY;
    let cases: [(&str, i64, Verdict); 14] = [
        ("a window starting now", 0, Verdict::Accepted),
        (
            "a window two days ahead, the documented forecast horizon",
            2 * DAY,
            Verdict::Accepted,
        ),
        ("a window a week ahead", 7 * DAY, Verdict::Accepted),
        ("a window a year ahead", 365 * DAY, Verdict::Accepted),
        ("a window an hour in the past", -HOUR, Verdict::Accepted),
        ("a window a week in the past", -7 * DAY, Verdict::Accepted),
        ("a window thirty days in the past", -30 * DAY, Verdict::Accepted),
        (
            "a window an hour inside the documented span",
            -history + HOUR,
            Verdict::Accepted,
        ),
        (
            "a window a minute inside the documented span",
            -history + CLOCK_MARGIN_SECONDS,
            Verdict::Accepted,
        ),
        (
            "a window a minute past the documented span",
            -history - CLOCK_MARGIN_SECONDS,
            Verdict::Accepted,
        ),
        (
            "a window a whole forecast period past the documented span",
            -history - HOUR,
            Verdict::Accepted,
        ),
        (
            "a window a hundred days in the past",
            -100 * DAY,
            Verdict::Accepted,
        ),
        (
            "a window two hundred days in the past",
            -200 * DAY,
            Verdict::Accepted,
        ),
        (
            "a window a year in the past",
            -365 * DAY,
            Verdict::Accepted,
        ),
    ];

    let emulator = with_stored_plan().await;
    for (plan, held) in [(STORED_PLAN, "a stored plan"), (ABSENT_PLAN, "an absent plan")] {
        for (label, start_offset, verdict) in cases {
            let start = now_secs() + start_offset;
            assert_window_verdict(&emulator, label, held, plan, (start, start + DAY), verdict)
                .await;
        }
    }
}

/// Milliseconds in a second, for the rows that place a window endpoint INSIDE a second.
///
/// `pub(crate)` so every suite placing an endpoint inside a second measures it the same
/// way.
pub(crate) const MILLIS_PER_SECOND: i64 = 1_000;

/// `millis` since the epoch as the fractional epoch seconds an `awsJson1_1` timestamp is
/// carried as: the finest precision a caller can ask a window with over this protocol.
///
/// `pub(crate)` for the same reason: one owner for the sub-second conversion.
pub(crate) fn epoch_seconds(millis: i64) -> f64 {
    millis as f64 / MILLIS_PER_SECOND as f64
}

/// Assert the verdict of one window whose endpoints are given in epoch MILLISECONDS, so
/// each one may sit inside a second.
async fn assert_sub_second_window_verdict(
    emulator: &Emulator,
    label: &str,
    held: &str,
    plan: &str,
    window_millis: (i64, i64),
    verdict: Verdict,
) {
    let (start, end) = window_millis;
    let window = (epoch_seconds(start), epoch_seconds(end));
    let body = asg_request(plan, ONLY_VERSION, "CapacityForecast", window);
    let shown = format!("[{}, {})", window.0, window.1);
    assert_verdict(emulator, label, held, &shown, &body, verdict).await;
}

#[tokio::test]
async fn the_sub_second_part_of_a_window_adds_no_forecast_period_to_its_width() {
    let week = MAX_FORECAST_WINDOW_DAYS * DAY * MILLIS_PER_SECOND;
    let period = HOUR * MILLIS_PER_SECOND;
    // The same width rule as the whole-second rows above, driven at the finest precision
    // the wire carries: an `awsJson1_1` `TimestampType` is epoch SECONDS, so a fractional
    // number is a window endpoint inside a second. Rows differ along the WIDTH (shorter
    // than a second, at the documented maximum, part of a forecast period past it, a
    // period past it), along the ORDER of the two endpoints, and along where inside its
    // second and its clock hour each endpoint sits — the sub-second part never decides a
    // row, because the bound is measured in whole forecast periods.
    //
    // A row whose verdict turns on a boundary sits a whole SECOND clear of it: a
    // present-day instant carried as fractional epoch seconds resolves to about a
    // microsecond, so a row balanced exactly on a period boundary would be deciding on
    // the float's rounding rather than on the rule.
    //
    // `(label, offset of the start inside the current hour in millis, width in millis,
    // verdict)`.
    let cases: [(&str, i64, i64, Verdict); 11] = [
        ("a one-millisecond window", 250, 1, Verdict::Accepted),
        ("a half-second window", 500, 500, Verdict::Accepted),
        (
            "a window whose endpoints are the same instant to the millisecond",
            250,
            0,
            Verdict::RefusedAsOutOfOrder,
        ),
        (
            "a window ending a millisecond before it starts",
            250,
            -1,
            Verdict::RefusedAsOutOfOrder,
        ),
        (
            "a window ending half a second before it starts",
            500,
            -500,
            Verdict::RefusedAsOutOfOrder,
        ),
        (
            "the documented maximum width, both endpoints a quarter second into a second",
            250,
            week,
            Verdict::Accepted,
        ),
        (
            "half a second past the maximum width",
            0,
            week + 500,
            Verdict::Accepted,
        ),
        (
            "a minute and a half second past the maximum width",
            125,
            week + 60 * MILLIS_PER_SECOND + 500,
            Verdict::Accepted,
        ),
        (
            "half a second short of a whole forecast period past the maximum width",
            750,
            week + period - 500,
            Verdict::Accepted,
        ),
        (
            "a second past a whole forecast period past the maximum width",
            250,
            week + period + MILLIS_PER_SECOND,
            Verdict::RefusedAsTooWide,
        ),
        (
            "ten days and half a second",
            400,
            10 * DAY * MILLIS_PER_SECOND + 500,
            Verdict::RefusedAsTooWide,
        ),
    ];

    let emulator = with_stored_plan().await;
    // Both plan names again: the window checks read request members only, so a row earns
    // the same verdict whether or not the named plan is held.
    for (plan, held) in [(STORED_PLAN, "a stored plan"), (ABSENT_PLAN, "an absent plan")] {
        for (label, start_offset, width, verdict) in cases {
            let start = current_hour_start() * MILLIS_PER_SECOND + start_offset;
            assert_sub_second_window_verdict(
                &emulator,
                label,
                held,
                plan,
                (start, start + width),
                verdict,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn a_start_outside_the_documented_data_span_is_served_whatever_its_sub_second_part() {
    let history = MAX_FORECAST_HISTORY_DAYS * DAY * MILLIS_PER_SECOND;
    let period = HOUR * MILLIS_PER_SECOND;
    // The span forecast data is available over bounds the DATA, not the request, so a
    // start outside it is served at every precision a caller can ask with. Rows differ
    // along how far past the span the start sits and along its sub-second part. Each row's
    // width is a day, well inside the maximum width, so the width rule cannot be what
    // answers it.
    //
    // `(label, offset of the start from now in millis, verdict)`.
    let cases: [(&str, i64, Verdict); 5] = [
        (
            "a start half a second inside the documented span",
            -history + 500,
            Verdict::Accepted,
        ),
        (
            "a start half a second past the documented span",
            -history - 500,
            Verdict::Accepted,
        ),
        (
            "a start a minute and a half second past the documented span",
            -history - CLOCK_MARGIN_SECONDS * MILLIS_PER_SECOND - 500,
            Verdict::Accepted,
        ),
        (
            "a start a whole forecast period and a minute past the documented span",
            -history - period - CLOCK_MARGIN_SECONDS * MILLIS_PER_SECOND,
            Verdict::Accepted,
        ),
        (
            "a start a day and half a second past the documented span",
            -history - DAY * MILLIS_PER_SECOND - 500,
            Verdict::Accepted,
        ),
    ];

    let emulator = with_stored_plan().await;
    for (plan, held) in [(STORED_PLAN, "a stored plan"), (ABSENT_PLAN, "an absent plan")] {
        for (label, start_offset, verdict) in cases {
            let start = now_secs() * MILLIS_PER_SECOND + start_offset;
            assert_sub_second_window_verdict(
                &emulator,
                label,
                held,
                plan,
                (start, start + DAY * MILLIS_PER_SECOND),
                verdict,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn a_too_wide_window_is_refused_on_its_width_wherever_it_sits() {
    // The two window rules are independent: a window that breaks the width rule is
    // refused on the width whether it lies ahead of now, around now, or further back than
    // the documented span — so neither rule hides the other.
    let week = MAX_FORECAST_WINDOW_DAYS * DAY;
    let history = MAX_FORECAST_HISTORY_DAYS * DAY;
    let cases: [(&str, i64); 4] = [
        ("a too-wide window starting a year ahead", 365 * DAY),
        ("a too-wide window starting now", 0),
        ("a too-wide window inside the documented span", -history / 2),
        ("a too-wide window past the documented span", -300 * DAY),
    ];

    let emulator = with_stored_plan().await;
    for (label, start_offset) in cases {
        let start = now_secs() + start_offset;
        assert_window_verdict(
            &emulator,
            label,
            "a stored plan",
            STORED_PLAN,
            (start, start + week + HOUR),
            Verdict::RefusedAsTooWide,
        )
        .await;
    }
}

#[tokio::test]
async fn the_scaling_plan_version_selects_the_plan_to_forecast_from() {
    // `ScalingPlanVersion` is a long with no `@range` and no enum
    // (`smithy_query(query="shape",shape="ScalingPlanVersion")`), so its value selects the
    // plan a forecast is issued from rather than gating the request: the rows differ in
    // version MAGNITUDE and SIGN across that type's whole range — zero, the next version
    // up, a negative version, a version past 32 bits and both extremes of the type — and
    // every one of them is answered, not refused.
    let cases: [(&str, i64); 7] = [
        ("the version a created plan carries", ONLY_VERSION),
        ("version zero", 0),
        ("the next version up", ONLY_VERSION + 1),
        ("a negative version", -7),
        ("a version past 32 bits", 4_294_967_296),
        ("the largest long", i64::MAX),
        ("the smallest long", i64::MIN),
    ];

    let emulator = with_stored_plan().await;
    // Checked against a plan the emulator holds and a name it does not: either way the
    // coordinate selects a plan that configures no predictive scaling, or no plan at all,
    // and both have no forecast — the empty `Datapoints` list, not a refusal.
    for (plan, held) in [(STORED_PLAN, "a stored plan"), (ABSENT_PLAN, "an absent plan")] {
        for (label, version) in cases {
            let base = now_secs();
            let body = asg_request(plan, version, "CapacityForecast", (base, base + HOUR));
            let (status, parsed) = emulator.call(OPERATION, &body).await;
            assert_eq!(
                status, 200,
                "case {label:?} ({version}) on {held}: expected HTTP 200, got {status} \
                 with {parsed}"
            );
            assert!(
                datapoints(&parsed).is_empty(),
                "case {label:?} ({version}) on {held}: expected no data points, got {parsed}"
            );
        }
    }
}

#[tokio::test]
async fn a_triple_that_describes_no_scalable_resource_is_refused() {
    // The coordinates are refused for being INCONSISTENT, never for naming a resource
    // kind other than an Auto Scaling group: rows differ in which half of the triple
    // contradicts the other — a listed dimension under a namespace that does not own it
    // (in both directions), a dimension the model's enum does not list at all, and a
    // resource id carrying no resource type of the dimension beside it. Source of the
    // namespace prefix of each dimension: `smithy_query(query="shape",
    // shape="ScalableDimension")`, mirrored as `SCALABLE_DIMENSION_VALUES`.
    let cases: [(&str, &str, &str, &str); 6] = [
        (
            "the Auto Scaling dimension under the DynamoDB namespace",
            "dynamodb",
            COVERED_RESOURCE,
            FORECAST_SCALABLE_DIMENSION,
        ),
        (
            "a DynamoDB dimension under the Auto Scaling namespace",
            FORECAST_SERVICE_NAMESPACE,
            "table/my-table",
            "dynamodb:table:ReadCapacityUnits",
        ),
        (
            "an ECS dimension under the EC2 namespace",
            "ec2",
            "service/default/sample-webapp",
            "ecs:service:DesiredCount",
        ),
        (
            "a scalable dimension outside the modelled enum",
            FORECAST_SERVICE_NAMESPACE,
            COVERED_RESOURCE,
            "autoscaling:autoScalingGroup:Nonsense",
        ),
        (
            "an Auto Scaling group id without the documented prefix",
            FORECAST_SERVICE_NAMESPACE,
            "my-asg",
            FORECAST_SCALABLE_DIMENSION,
        ),
        (
            "a DynamoDB table id under a DynamoDB index dimension",
            "dynamodb",
            "table/my-table",
            "dynamodb:index:ReadCapacityUnits",
        ),
    ];

    let emulator = with_stored_plan().await;
    for (label, namespace, resource_id, dimension) in cases {
        let base = now_secs();
        let body = forecast_request(
            STORED_PLAN,
            ONLY_VERSION,
            (namespace, resource_id, dimension),
            "CapacityForecast",
            (base, base + HOUR),
        );
        let (status, parsed) = emulator.call(OPERATION, &body).await;
        assert_eq!(
            (status, error_code(&parsed)),
            (400, "ValidationException"),
            "case {label:?}: expected HTTP 400 ValidationException, got {status} with {parsed}"
        );
    }
}

#[tokio::test]
async fn a_forecast_data_type_outside_the_modelled_enum_is_refused() {
    let emulator = with_stored_plan().await;
    // Rows differ in how far they miss the enum: an empty value, a near-miss in case, a
    // value from another member's enum, and a plausible but unmodelled name.
    for label in [
        "",
        "capacityforecast",
        "ASGAverageCPUUtilization",
        "CapacityForecasts",
    ] {
        let base = now_secs();
        let body = asg_request(STORED_PLAN, ONLY_VERSION, label, (base, base + HOUR));
        let (status, parsed) = emulator.call(OPERATION, &body).await;
        assert_eq!(
            (status, error_code(&parsed)),
            (400, "ValidationException"),
            "case {label:?}: expected HTTP 400 ValidationException, got {status} with {parsed}"
        );
    }
}

#[tokio::test]
async fn a_plan_configuring_no_predictive_scaling_reports_an_empty_forecast() {
    let emulator = with_stored_plan().await;
    // A second plan, covering a DIFFERENT Auto Scaling group, so a request for the
    // covered resource reaches a plan that does not cover it.
    emulator
        .ok(
            "CreateScalingPlan",
            json!({
                "ScalingPlanName": "other plan",
                "ApplicationSource": stack_source(2),
                "ScalingInstructions": [instruction(
                    FORECAST_SERVICE_NAMESPACE,
                    UNCOVERED_RESOURCE,
                    FORECAST_SCALABLE_DIMENSION,
                    1,
                    10,
                    "ASGAverageCPUUtilization",
                    40.0,
                )],
            }),
        )
        .await;
    emulator.make_all_due();

    // Rows differ along what the rule generalises over: whether the plan that is held
    // covers the named resource. Neither plan's instruction carries a load metric
    // specification, so neither configures predictive scaling and neither has a forecast
    // — the covered resource included.
    let lookups: [(&str, &str); 2] = [
        ("a plan that covers the resource", STORED_PLAN),
        ("a plan that covers another resource", "other plan"),
    ];

    // Source: `smithy_query(query="shape",shape="ForecastDataType")`, mirrored in
    // `src/ops/types/shared_types.rs` as `FORECAST_DATA_TYPE_VALUES` — every value,
    // including the two scheduled-action ones the issue did not cite.
    for data_type in FORECAST_DATA_TYPE_VALUES {
        for (label, plan) in lookups {
            let base = now_secs();
            let parsed = emulator
                .ok(
                    OPERATION,
                    asg_request(plan, ONLY_VERSION, data_type, (base, base + DAY)),
                )
                .await;
            assert!(
                datapoints(&parsed).is_empty(),
                "case {label:?} / {data_type}: expected no data points, got {parsed}"
            );
        }

        // A plan the service does not hold reads the same way: nothing to forecast is the
        // empty list, not a refusal — this operation models no not-found error.
        let base = now_secs();
        let parsed = emulator
            .ok(
                OPERATION,
                asg_request(ABSENT_PLAN, ONLY_VERSION, data_type, (base, base + DAY)),
            )
            .await;
        assert!(
            datapoints(&parsed).is_empty(),
            "case a plan this service does not hold / {data_type}: expected no data \
             points, got {parsed}"
        );
    }
}

#[tokio::test]
async fn a_forecast_read_leaves_the_stored_plan_as_it_was() {
    let emulator = with_stored_plan().await;
    let before = emulator
        .ok(
            "DescribeScalingPlans",
            json!({ "ScalingPlanNames": [STORED_PLAN] }),
        )
        .await;

    let base = now_secs();
    for data_type in FORECAST_DATA_TYPE_VALUES {
        emulator
            .ok(
                OPERATION,
                asg_request(STORED_PLAN, ONLY_VERSION, data_type, (base, base + DAY)),
            )
            .await;
    }

    let after = emulator
        .ok(
            "DescribeScalingPlans",
            json!({ "ScalingPlanNames": [STORED_PLAN] }),
        )
        .await;
    assert_eq!(
        after, before,
        "reading the forecast must not change what a later caller sees: {before} became \
         {after}"
    );
}

/// Which instant an envelope's `StartTime` is measured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// The start of the clock hour containing now: a forecast period boundary, so a row
    /// measured from it places both endpoints at a known phase inside a period whatever
    /// time the test runs at.
    PeriodBoundary,
    /// Now itself, for a row whose data content turns on where the window sits relative to
    /// the span forecast data exists over.
    Now,
}

impl Anchor {
    /// The whole epoch second `offset` seconds from this anchor.
    fn at(self, offset: i64) -> i64 {
        match self {
            Self::PeriodBoundary => current_hour_start() + offset,
            Self::Now => now_secs() + offset,
        }
    }
}

/// What one window envelope earns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// HTTP 200 carrying the required `Datapoints` member: the forecast of every interval
    /// the window holds inside the span forecast data exists over — at least one of them
    /// where `points` is true, none of them where it is false.
    Served { points: bool },
    /// HTTP 400 `ValidationException`, for the relation the inner verdict names.
    Refused(Verdict),
}

/// One window envelope — `[anchor + offset, anchor + offset + width)` — and the answer the
/// rule gives it.
#[derive(Debug, Clone, Copy)]
struct Envelope {
    label: &'static str,
    anchor: Anchor,
    offset: i64,
    width: i64,
    answer: Answer,
}

/// Who a forecast is asked of: one emulator, one plan name, one scalable resource — and
/// whether that pair has a forecast at all, which is what decides whether a served window
/// can carry a data point.
struct Subject<'a> {
    label: &'static str,
    emulator: &'a Emulator,
    plan: &'a str,
    resource: Resource<'a>,
    has_forecast: bool,
}

/// Assert one envelope earns its answer when asked of one subject for one forecast data
/// type.
async fn assert_envelope(subject: &Subject<'_>, envelope: &Envelope, data_type: &str) {
    let start = envelope.anchor.at(envelope.offset);
    let window = (start, start + envelope.width);
    let body = forecast_request(
        subject.plan,
        ONLY_VERSION,
        subject.resource,
        data_type,
        window,
    );
    let shown = format!("[{}, {}) / {data_type}", window.0, window.1);
    match envelope.answer {
        Answer::Refused(verdict) => {
            assert_verdict(
                subject.emulator,
                envelope.label,
                subject.label,
                &shown,
                &body,
                verdict,
            )
            .await;
        }
        Answer::Served { points } => {
            let (status, parsed) = subject.emulator.call(OPERATION, &body).await;
            assert_eq!(
                status,
                200,
                "case {:?} on {} ({shown}): expected HTTP 200, got {status} with {parsed}",
                envelope.label,
                subject.label
            );
            // A subject with nothing to forecast reports the empty list for every window;
            // one with a forecast reports the intervals the window really holds.
            let expected = points && subject.has_forecast;
            let carried = !datapoints(&parsed).is_empty();
            assert_eq!(
                carried,
                expected,
                "case {:?} on {} ({shown}): expected {} data point, got {parsed}",
                envelope.label,
                subject.label,
                if expected { "at least one" } else { "no" }
            );
        }
    }
}

#[tokio::test]
async fn one_window_envelope_earns_one_answer_whatever_lies_behind_it() {
    let week = MAX_FORECAST_WINDOW_DAYS * DAY;
    let history = MAX_FORECAST_HISTORY_DAYS * DAY;
    let horizon = FORECAST_HORIZON_DAYS * DAY;
    // Rows walk the envelope family around every documented boundary, differing along what
    // the rule generalises over: the WIDTH (a second, a forecast period, inside the
    // documented maximum, exactly at it, part of a period past it, a whole period past it,
    // days past it), the ORDER of the two endpoints, the PHASE of the start inside its
    // forecast period, and WHERE the window sits relative to now — at the far edge of the
    // span forecast data is available over, before that edge, past the documented forecast
    // horizon and far ahead of it. Only the first two decide the refusal; the position
    // decides how much forecast a served window carries, which is the empty list when the
    // window holds no interval the service has data for.
    let cases: [Envelope; 16] = [
        Envelope {
            label: "a window of one forecast period starting on a period boundary",
            anchor: Anchor::PeriodBoundary,
            offset: 0,
            width: HOUR,
            answer: Answer::Served { points: true },
        },
        Envelope {
            label: "a one-second window containing a period boundary",
            anchor: Anchor::PeriodBoundary,
            offset: 0,
            width: 1,
            answer: Answer::Served { points: true },
        },
        Envelope {
            label: "a one-second window containing no period boundary",
            anchor: Anchor::PeriodBoundary,
            offset: 600,
            width: 1,
            answer: Answer::Served { points: false },
        },
        Envelope {
            label: "a ten-minute window inside a single forecast period",
            anchor: Anchor::PeriodBoundary,
            offset: 600,
            width: 600,
            answer: Answer::Served { points: false },
        },
        Envelope {
            label: "a window a second inside the documented maximum width",
            anchor: Anchor::PeriodBoundary,
            offset: 0,
            width: week - 1,
            answer: Answer::Served { points: true },
        },
        Envelope {
            label: "a window exactly at the documented maximum width",
            anchor: Anchor::PeriodBoundary,
            offset: 0,
            width: week,
            answer: Answer::Served { points: true },
        },
        Envelope {
            label: "a maximum-width window starting mid-period",
            anchor: Anchor::PeriodBoundary,
            offset: 1_777,
            width: week,
            answer: Answer::Served { points: true },
        },
        Envelope {
            label: "the last second of the forecast period past the maximum width",
            anchor: Anchor::PeriodBoundary,
            offset: 0,
            width: week + HOUR - 1,
            answer: Answer::Served { points: true },
        },
        Envelope {
            label: "a whole forecast period past the documented maximum width",
            anchor: Anchor::PeriodBoundary,
            offset: 0,
            width: week + HOUR,
            answer: Answer::Refused(Verdict::RefusedAsTooWide),
        },
        Envelope {
            label: "three days past the documented maximum width",
            anchor: Anchor::PeriodBoundary,
            offset: 0,
            width: week + 3 * DAY,
            answer: Answer::Refused(Verdict::RefusedAsTooWide),
        },
        Envelope {
            label: "an end time equal to the start time",
            anchor: Anchor::PeriodBoundary,
            offset: 600,
            width: 0,
            answer: Answer::Refused(Verdict::RefusedAsOutOfOrder),
        },
        Envelope {
            label: "an end time a forecast period before the start time",
            anchor: Anchor::PeriodBoundary,
            offset: 600,
            width: -HOUR,
            answer: Answer::Refused(Verdict::RefusedAsOutOfOrder),
        },
        Envelope {
            label: "a window starting at the far edge of the documented data span",
            anchor: Anchor::Now,
            offset: -history,
            width: DAY,
            answer: Answer::Served { points: true },
        },
        Envelope {
            label: "a window starting a forecast period before that edge",
            anchor: Anchor::Now,
            offset: -history - HOUR,
            width: DAY,
            answer: Answer::Served { points: true },
        },
        Envelope {
            label: "a window starting two hundred days before now, well past that span",
            anchor: Anchor::Now,
            offset: -200 * DAY,
            width: DAY,
            answer: Answer::Served { points: false },
        },
        Envelope {
            label: "a window starting a day past the documented forecast horizon ahead of now",
            anchor: Anchor::Now,
            offset: horizon + DAY,
            width: DAY,
            answer: Answer::Served { points: false },
        },
    ];

    // Two emulators, so the same envelope is asked of a plan that HAS a forecast and of one
    // that has none: the predictive plan of `crate::forecast_window_precision` (an
    // instruction with a load metric specification, with its history published) and the
    // dynamic-only plan of this suite.
    let predictive = crate::forecast_window_precision::with_predictive_plan().await;
    let dynamic = with_stored_plan().await;
    let other_resource: Resource<'_> = (
        FORECAST_SERVICE_NAMESPACE,
        UNCOVERED_RESOURCE,
        FORECAST_SCALABLE_DIMENSION,
    );
    let subjects = [
        Subject {
            label: "a plan that forecasts the named resource",
            emulator: &predictive,
            plan: crate::forecast_window_precision::STORED_PLAN,
            resource: ASG_RESOURCE,
            has_forecast: true,
        },
        Subject {
            label: "a plan that instructs another resource",
            emulator: &predictive,
            plan: crate::forecast_window_precision::STORED_PLAN,
            resource: other_resource,
            has_forecast: false,
        },
        Subject {
            label: "a plan this service does not hold",
            emulator: &predictive,
            plan: ABSENT_PLAN,
            resource: ASG_RESOURCE,
            has_forecast: false,
        },
        Subject {
            label: "a plan configuring no predictive scaling",
            emulator: &dynamic,
            plan: STORED_PLAN,
            resource: ASG_RESOURCE,
            has_forecast: false,
        },
    ];

    for subject in &subjects {
        // Source of the data types: `smithy_query(query="shape",shape="ForecastDataType")`,
        // mirrored as `FORECAST_DATA_TYPE_VALUES` — the window answer is the same for every
        // one of them, the two scheduled-action types included.
        for data_type in FORECAST_DATA_TYPE_VALUES {
            for envelope in &cases {
                assert_envelope(subject, envelope, data_type).await;
            }
        }
    }
}
