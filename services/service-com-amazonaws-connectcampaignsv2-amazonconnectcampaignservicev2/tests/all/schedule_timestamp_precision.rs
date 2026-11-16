// SPDX-License-Identifier: CC0-1.0

//! The PRECISION a campaign's schedule is stored and echoed at, end to end over the
//! wire.
//!
//! Every case drives real HTTP through the router the production entrypoint serves, so
//! each assertion exercises the restJson1 wire protocol, the modelled constraints and
//! the handlers together. The rule under test: this service keeps a `date-time`
//! timestamp to the MILLISECOND, so the sub-millisecond remainder of a submitted
//! `startTime` or `endTime` is dropped on the way in and every later read of the
//! campaign projects the truncated instant — truncated, never rounded up, so the
//! instant a value names never moves forward.
//!
//! The rows span what the rule generalises over: the size of the remainder (none, one
//! nanosecond, one microsecond, a whole number of milliseconds, the largest remainder
//! a nanosecond clock can carry), which of the two window members carries it, and
//! which call wrote the schedule — `CreateCampaign` or `UpdateCampaignSchedule`, the
//! two writers of the member.

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};

use super::common::{call, create_campaign, create_campaign_with, describe_campaign, router};

/// The region every case here signs for.
const REGION: &str = "ap-northeast-1";

/// Nanoseconds in one millisecond, the precision this service keeps.
const NANOS_PER_MILLI: u32 = 1_000_000;

/// A whole second well clear of any clock-owed transition, so no row is about a
/// campaign's state: the window opens in 2099.
const WINDOW_START_SECS: i64 = 4_070_908_800;

/// The instant one hour after [`WINDOW_START_SECS`], where a window ends.
const WINDOW_END_SECS: i64 = WINDOW_START_SECS + 3_600;

/// `secs`.`nanos` as the RFC 3339 string a `date-time` member carries on the wire.
fn wire(secs: i64, nanos: u32) -> String {
    DateTime::from_secs_and_nanos(secs, nanos)
        .fmt(Format::DateTime)
        .expect("a formattable instant")
}

/// The instant `secs`.`nanos` names, with its sub-millisecond remainder dropped.
fn truncated(secs: i64, nanos: u32) -> DateTime {
    DateTime::from_secs_and_nanos(secs, nanos / NANOS_PER_MILLI * NANOS_PER_MILLI)
}

/// The instant the `member` of `schedule` carries, parsed off the wire.
fn stored_instant(schedule: &Value, member: &str) -> DateTime {
    let text = schedule
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the stored schedule carries no {member}: {schedule}"));
    DateTime::from_str(text, Format::DateTime)
        .unwrap_or_else(|_| panic!("the stored {member} {text:?} is not a date-time"))
}

/// The sub-second remainders a row submits, as `(case, nanoseconds)`.
///
/// `nanos` is the fraction of the second the submitted instant carries: a whole number
/// of milliseconds survives untouched, anything finer loses its remainder.
fn remainders() -> Vec<(&'static str, u32)> {
    vec![
        ("no fraction at all", 0),
        ("one nanosecond", 1),
        ("one microsecond", 1_000),
        ("890 microseconds", 890_000),
        ("a whole millisecond", NANOS_PER_MILLI),
        ("a millisecond and a half", NANOS_PER_MILLI + 500_000),
        ("the largest remainder a nanosecond clock carries", 999_999_999),
    ]
}

/// Assert the window `schedule` carries is the submitted one with its sub-millisecond
/// remainder dropped.
fn assert_window(schedule: &Value, start_nanos: u32, end_nanos: u32, case: &str) {
    for (member, secs, nanos) in [
        ("startTime", WINDOW_START_SECS, start_nanos),
        ("endTime", WINDOW_END_SECS, end_nanos),
    ] {
        let expected = truncated(secs, nanos);
        let actual = stored_instant(schedule, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?} (the submitted instant to the \
             millisecond), got {actual:?}"
        );
        assert_eq!(
            actual.subsec_nanos() % NANOS_PER_MILLI,
            0,
            "case {case:?}: expected {member} to carry no sub-millisecond remainder, got {actual:?}"
        );
        assert!(
            actual <= DateTime::from_secs_and_nanos(secs, nanos),
            "case {case:?}: expected {member} not to move forward of the submitted instant, \
             got {actual:?}"
        );
    }
}

/// `UpdateCampaignSchedule` stores the window to the millisecond, and
/// `DescribeCampaign` projects what was stored.
#[tokio::test]
async fn an_updated_schedule_is_stored_to_the_millisecond() {
    let router = router();
    let instance = "schedule-precision-instance";
    let id = create_campaign(&router, REGION, instance, "updated-precision", None).await;

    for (case, nanos) in remainders() {
        // The remainder is put on BOTH members in one row and on one member alone in
        // the next, so the truncation is not an artefact of which member carries it.
        for (which, start_nanos, end_nanos) in [
            ("both members", nanos, nanos),
            ("startTime alone", nanos, 0),
            ("endTime alone", 0, nanos),
        ] {
            let case = format!("{case}, {which}");
            let (status, body) = call(
                &router,
                REGION,
                "POST",
                &format!("/v2/campaigns/{id}/schedule"),
                &json!({"schedule": {
                    "startTime": wire(WINDOW_START_SECS, start_nanos),
                    "endTime": wire(WINDOW_END_SECS, end_nanos),
                }}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: UpdateCampaignSchedule expected 200, got {status} {body}"
            );

            let campaign = describe_campaign(&router, REGION, &id, &case).await;
            assert_window(&campaign["schedule"], start_nanos, end_nanos, &case);

            // The record holds the truncated window, so reading it again answers the
            // same instants: the precision is a property of what was STORED and not of
            // one projection.
            let again = describe_campaign(&router, REGION, &id, &case).await;
            assert_eq!(
                again["schedule"], campaign["schedule"],
                "case {case:?}: expected the same stored schedule on a second read, got {} \
                 and {}",
                again["schedule"], campaign["schedule"]
            );
        }
    }
}

/// A campaign BORN with a schedule holds it at the same precision: the create is the
/// other writer of the member, so a campaign created with a fine-grained window and
/// one updated to it project the same instants.
#[tokio::test]
async fn a_created_schedule_is_stored_to_the_millisecond() {
    let router = router();
    let instance = "schedule-precision-create-instance";

    for (index, (case, nanos)) in remainders().into_iter().enumerate() {
        let submitted = json!({"schedule": {
            "startTime": wire(WINDOW_START_SECS, nanos),
            "endTime": wire(WINDOW_END_SECS, nanos),
        }});
        let created = create_campaign_with(
            &router,
            REGION,
            instance,
            &format!("created-precision-{index}"),
            submitted.clone(),
        )
        .await;
        let campaign = describe_campaign(&router, REGION, &created, case).await;
        assert_window(&campaign["schedule"], nanos, nanos, case);

        // The same window reached by an update leaves the same record, so the two
        // writers agree about the precision.
        let updated = create_campaign_with(
            &router,
            REGION,
            instance,
            &format!("updated-precision-{index}"),
            json!({}),
        )
        .await;
        let (status, body) = call(
            &router,
            REGION,
            "POST",
            &format!("/v2/campaigns/{updated}/schedule"),
            &submitted,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateCampaignSchedule expected 200, got {status} {body}"
        );
        let from_update = describe_campaign(&router, REGION, &updated, case).await;
        assert_eq!(
            from_update["schedule"], campaign["schedule"],
            "case {case:?}: expected the create and the update to store the same window, \
             got {} and {}",
            from_update["schedule"], campaign["schedule"]
        );
    }
}
