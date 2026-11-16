// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `EnableImageDeprecation` and `DisableImageDeprecation`, driven over
//! the wire the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rule under test is that AN AMI'S DEPRECATION DATE IS ONE STORED MEMBER OF THAT
//! IMAGE: enabling deprecation writes the instant the request named — rounded to the
//! nearest minute, "if you specify a value for seconds, Amazon EC2 rounds the seconds to
//! the nearest minute" (`EnableImageDeprecationRequest$DeprecateAt`) — onto the AMI, where
//! every later read reports it as `Image$DeprecationTime`, and cancelling it takes that same
//! member away. The date a request may name is bounded by what the member publishes (not in
//! the past; at most 10 years from now, or 2 years from the creation date of a public AMI),
//! and only the AMI's owner may change it at all.
//!
//! Each table row is a distinct input along what the rule generalises over — which
//! sub-minute offset, which horizon, which prior state of the member, which published bound,
//! which refusal — so the one date a recorded client happened to send is a row rather than
//! the specification.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, MAX_IMAGE_DEPRECATION_YEARS,
    MAX_PUBLIC_IMAGE_DEPRECATION_YEARS, iso8601_utc,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, element, elements, error_code, error_message};

/// The Region every call here is signed for. The image store is per account rather than per
/// Region, so the Region only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// An account that is not the one this emulator serves, so an image of its own is another
/// account's.
const PARTNER_ACCOUNT: &str = "111122223333";

/// A well-formed long-form AMI id this store holds no image under.
const UNKNOWN_IMAGE_ID: &str = "ami-0123456789abcdef0";

/// A value that is no AMI id at all.
const MALFORMED_IMAGE_ID: &str = "not-an-ami";

/// The HTTP status EC2 answers a `DryRun` request with: 412, carrying `DryRunOperation`.
const DRY_RUN_STATUS: u16 = 412;

/// The seconds in a day, for laying a row's horizon out.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `EnableImageDeprecation` call, as its HTTP status and XML body.
async fn enable(
    router: &MultiServiceRouter,
    image_id: &str,
    deprecate_at: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![p("ImageId", image_id), p("DeprecateAt", deprecate_at)];
    params.extend_from_slice(extra);
    super::ec2_query::call(router, REGION, "EnableImageDeprecation", &params).await
}

/// One `DisableImageDeprecation` call, as its HTTP status and XML body.
async fn disable(
    router: &MultiServiceRouter,
    image_id: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![p("ImageId", image_id)];
    params.extend_from_slice(extra);
    super::ec2_query::call(router, REGION, "DisableImageDeprecation", &params).await
}

/// Assert that a successful deprecation change answered `<return>true</return>`.
fn assert_returned_true(case: &str, status: u16, body: &str) {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        element(body, "return").as_deref(),
        Some("true"),
        "case {case:?}: expected the request to report success, got {body}"
    );
}

/// The `deprecationTime` `DescribeImages` reports for one image, or `None` when the image
/// carries none. Read back through the describe, so what is asserted is what a later caller
/// sees rather than what this crate's state happens to hold.
async fn described_deprecation_time(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
) -> Option<String> {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeImages",
        &[p("ImageId.1", image_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeImages for {image_id} must succeed, got {status} with body \
         {body}"
    );
    let reported = elements(&body, "imageId");
    assert_eq!(
        reported,
        vec![image_id.to_string()],
        "case {case:?}: expected a report of exactly {image_id}, got {reported:?}"
    );
    elements(&body, "deprecationTime").into_iter().next()
}

/// Every image id a plain `DescribeImages` reports — the set an image whose deprecation time
/// has passed is left out of.
async fn described_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = super::ec2_query::call(router, REGION, "DescribeImages", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeImages must succeed, got {status} with body {body}"
    );
    elements(&body, "imageId")
}

/// `secs` as the ISO 8601 UTC instant a request names, with `millis` of sub-second precision
/// when the row carries any — the member takes a timestamp, so a client may send one.
fn requested_at(secs: i64, millis: u32) -> String {
    let whole = iso8601_utc(instant_of(secs));
    if millis == 0 {
        return whole;
    }
    format!("{}.{millis:03}Z", whole.trim_end_matches('Z'))
}

/// `secs` as a `SystemTime`.
fn instant_of(secs: i64) -> SystemTime {
    let seconds = u64::try_from(secs).expect("a test instant after the epoch");
    UNIX_EPOCH + Duration::from_secs(seconds)
}

/// Now, in epoch seconds, on the minute — the base every row's horizon is laid out from.
fn now_on_the_minute() -> i64 {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_secs(),
    )
    .expect("an epoch second that fits");
    now - now.rem_euclid(60)
}

/// `secs` rounded to the nearest minute, the way the member documents the service rounds it:
/// the test's own arithmetic, so the expectation is not read out of the implementation.
fn rounded_to_the_minute(secs: i64) -> i64 {
    (secs + 30).div_euclid(60) * 60
}

/// `from` plus `years` CALENDAR years, in epoch seconds — the two published upper limits are
/// stated in years ("the upper limit for DeprecateAt is 10 years from now, except for public
/// AMIs, where the upper limit is 2 years from the creation date"), so a row that sits ON one
/// of them is laid out on the calendar rather than on a fixed count of seconds.
fn plus_years(from: i64, years: u32) -> i64 {
    ::chrono::DateTime::from_timestamp(from, 0)
        .expect("a representable instant")
        .checked_add_months(::chrono::Months::new(years * 12))
        .expect("a representable limit")
        .timestamp()
}

/// Write one image into the store the way a register operation would, and answer its id.
fn seed_image(engine: &engine::InMemoryEngine, spec: ImageSpec) -> String {
    engine.state::<Ec2State>().lock().insert_image(spec)
}

/// A private AMI of the calling account, named `name`.
fn own_image(name: &str) -> ImageSpec {
    ImageSpec {
        name: name.to_string(),
        description: "An image of the calling account".to_string(),
        ..ImageSpec::default()
    }
}

/// How many images the store holds, so a refusal can be shown to have stored nothing.
fn stored_images(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().images.len()
}

// ---------------------------------------------------------------------------
// The date is stored, rounded to the minute, and reported by every later read
// ---------------------------------------------------------------------------

/// One row of the rounding table: a horizon and a sub-minute offset the request names.
#[derive(Debug, Clone)]
struct RoundingCase {
    label: &'static str,
    days_ahead: i64,
    seconds_past_the_minute: i64,
    millis: u32,
}

/// Enabling deprecation stores the instant the request named, rounded to the nearest minute,
/// and `DescribeImages` reports that instant for the image afterwards — twice, because
/// reading it changes nothing.
#[tokio::test]
async fn the_stored_deprecation_date_is_the_requested_instant_rounded_to_the_minute() {
    let (router, engine) = super::ec2_query::emulator();

    // Rows: a date on the minute, each side of the half minute the rounding turns on, the
    // last second of a minute, and a sub-second precision a client may send — over horizons
    // from an hour to nearly a decade ahead.
    let cases = [
        RoundingCase {
            label: "an hour ahead, on the minute",
            days_ahead: 0,
            seconds_past_the_minute: 60 * 60,
            millis: 0,
        },
        RoundingCase {
            label: "a day ahead, one second past the minute",
            days_ahead: 1,
            seconds_past_the_minute: 1,
            millis: 0,
        },
        RoundingCase {
            label: "a day ahead, 29 seconds past the minute",
            days_ahead: 1,
            seconds_past_the_minute: 29,
            millis: 0,
        },
        RoundingCase {
            label: "a week ahead, exactly half a minute past",
            days_ahead: 7,
            seconds_past_the_minute: 30,
            millis: 0,
        },
        RoundingCase {
            label: "a year ahead, 31 seconds past the minute",
            days_ahead: 365,
            seconds_past_the_minute: 31,
            millis: 0,
        },
        RoundingCase {
            label: "a year ahead, the last second of the minute",
            days_ahead: 365,
            seconds_past_the_minute: 59,
            millis: 0,
        },
        RoundingCase {
            label: "nine years ahead, 29.7 seconds past the minute",
            days_ahead: 9 * 365,
            seconds_past_the_minute: 29,
            millis: 700,
        },
        RoundingCase {
            label: "nine years ahead, 29.2 seconds past the minute",
            days_ahead: 9 * 365,
            seconds_past_the_minute: 29,
            millis: 200,
        },
    ];

    let base = now_on_the_minute();
    for (index, case) in cases.iter().enumerate() {
        let image_id = seed_image(&engine, own_image(&format!("deprecation-row-{index}")));
        let requested_secs = base + case.days_ahead * SECONDS_PER_DAY + case.seconds_past_the_minute;
        let requested = requested_at(requested_secs, case.millis);
        let expected_secs =
            rounded_to_the_minute(requested_secs + i64::from(case.millis >= 500));

        let (status, body) = enable(&router, &image_id, &requested, &[]).await;
        assert_returned_true(case.label, status, &body);

        let reported = described_deprecation_time(&router, case.label, &image_id)
            .await
            .unwrap_or_else(|| {
                panic!("case {:?}: the image must report a deprecation time", case.label)
            });
        let reported_secs = as_timestamp(&reported).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected a deprecation time that reads as an instant, got \
                 {reported:?}",
                case.label
            )
        });
        assert_eq!(
            reported_secs as i64, expected_secs,
            "case {:?}: expected the deprecation time {}, got {reported:?} for the request \
             {requested:?}",
            case.label,
            iso8601_utc(instant_of(expected_secs))
        );
        assert!(
            reported.ends_with('Z'),
            "case {:?}: a deprecation time is reported in UTC, got {reported:?}",
            case.label
        );
        assert_eq!(
            reported_secs as i64 % 60,
            0,
            "case {:?}: a deprecation time names a whole minute, got {reported:?}",
            case.label
        );

        // Reading the image again reports the same date: the describe is a read.
        let read_again = described_deprecation_time(&router, case.label, &image_id).await;
        assert_eq!(
            read_again.as_deref(),
            Some(reported.as_str()),
            "case {:?}: a second read must report the same deprecation time",
            case.label
        );
    }
}

/// A second enable REPLACES the date the image carried, and no other image moves with it.
#[tokio::test]
async fn a_second_enable_replaces_the_date_and_leaves_every_other_image_alone() {
    let (router, engine) = super::ec2_query::emulator();
    let changed = seed_image(&engine, own_image("deprecation-replaced"));
    let untouched = seed_image(&engine, own_image("deprecation-untouched"));
    let images_before = stored_images(&engine);

    let base = now_on_the_minute();
    // Rows: the date the image ends up with after each successive enable of the same AMI.
    let horizons = [30 * SECONDS_PER_DAY, 2 * SECONDS_PER_DAY, 400 * SECONDS_PER_DAY];
    for (index, horizon) in horizons.iter().enumerate() {
        let case = format!("enable {} of {}", index + 1, horizons.len());
        let requested_secs = base + horizon;
        let (status, body) = enable(
            &router,
            &changed,
            &requested_at(requested_secs, 0),
            &[],
        )
        .await;
        assert_returned_true(&case, status, &body);

        let reported = described_deprecation_time(&router, &case, &changed).await;
        let reported_secs = reported
            .as_deref()
            .and_then(as_timestamp)
            .unwrap_or_else(|| panic!("case {case:?}: expected a deprecation time, got {reported:?}"));
        assert_eq!(
            reported_secs as i64,
            rounded_to_the_minute(requested_secs),
            "case {case:?}: the latest enable fixes the date, got {reported:?}"
        );
        assert_eq!(
            described_deprecation_time(&router, &case, &untouched).await,
            None,
            "case {case:?}: an image nothing was asked about keeps no deprecation time"
        );
    }

    assert_eq!(
        stored_images(&engine),
        images_before,
        "changing a deprecation date must store no further image"
    );
}

// ---------------------------------------------------------------------------
// Cancelling the deprecation takes the same member away
// ---------------------------------------------------------------------------

/// `DisableImageDeprecation` leaves the AMI carrying no deprecation time, whatever it
/// carried before — a date this API set, a date that has already passed, or none at all —
/// and an image that was deprecated is reported again by a plain `DescribeImages`.
#[tokio::test]
async fn disabling_deprecation_takes_the_date_away_whatever_the_image_carried() {
    let (router, engine) = super::ec2_query::emulator();
    let base = now_on_the_minute();

    // Rows: the three prior states of the member a cancellation can meet.
    let enabled = seed_image(&engine, own_image("deprecation-cancel-enabled"));
    let (status, body) = enable(
        &router,
        &enabled,
        &requested_at(base + 90 * SECONDS_PER_DAY, 0),
        &[],
    )
    .await;
    assert_returned_true("a date this API set", status, &body);

    let already_deprecated = seed_image(
        &engine,
        ImageSpec {
            // An image whose deprecation time has already passed: it is a deprecated AMI, so
            // a plain describe leaves it out until the deprecation is cancelled.
            deprecation_time: Some(iso8601_utc(instant_of(base - 365 * SECONDS_PER_DAY))),
            ..own_image("deprecation-cancel-passed")
        },
    );
    let never_deprecated = seed_image(&engine, own_image("deprecation-cancel-absent"));

    let listed_before = described_ids(&router, "before the cancellations").await;
    assert!(
        !listed_before.contains(&already_deprecated),
        "a deprecated image is left out of a plain describe, got {listed_before:?}"
    );

    for (case, image_id) in [
        ("a date this API set", &enabled),
        ("a date that has passed", &already_deprecated),
        ("no date at all", &never_deprecated),
    ] {
        let (status, body) = disable(&router, image_id, &[]).await;
        assert_returned_true(case, status, &body);
        assert_eq!(
            described_deprecation_time(&router, case, image_id).await,
            None,
            "case {case:?}: the image must carry no deprecation time after the cancellation"
        );
    }

    let listed_after = described_ids(&router, "after the cancellations").await;
    assert!(
        listed_after.contains(&already_deprecated),
        "case \"a date that has passed\": the image is no longer deprecated, so a plain \
         describe reports it, got {listed_after:?}"
    );
    assert!(
        listed_after.contains(&enabled) && listed_after.contains(&never_deprecated),
        "every image of the account stays reported, got {listed_after:?}"
    );
}

// ---------------------------------------------------------------------------
// What a request may ask for
// ---------------------------------------------------------------------------

/// The two published upper limits bound the date: a request ON the limit is served and one a
/// minute past it is refused, with the limit that holds decided by the image — 10 years from
/// now for a private AMI, 2 years from the creation date for a public one.
#[tokio::test]
async fn the_published_upper_limits_bound_the_date_a_request_may_name() {
    let (router, engine) = super::ec2_query::emulator();
    let base = now_on_the_minute();

    // A public AMI of this account, created a day ago, so its own limit is measured from a
    // creation date the test fixes rather than from the call.
    let created_at = base - SECONDS_PER_DAY;
    let public_limit = plus_years(created_at, MAX_PUBLIC_IMAGE_DEPRECATION_YEARS);
    let private_limit = plus_years(base, MAX_IMAGE_DEPRECATION_YEARS);

    // Rows: each published bound, and one minute past each.
    let cases: [(&str, bool, i64, bool); 4] = [
        ("a private AMI, on the ten-year limit", false, private_limit, true),
        ("a private AMI, a minute past it", false, private_limit + 60, false),
        ("a public AMI, two years from its creation date", true, public_limit, true),
        ("a public AMI, a minute past that", true, public_limit + 60, false),
    ];

    for (index, (case, public, requested_secs, served)) in cases.iter().enumerate() {
        let image_id = seed_image(
            &engine,
            ImageSpec {
                public: *public,
                creation_date: iso8601_utc(instant_of(created_at)),
                ..own_image(&format!("deprecation-limit-{index}"))
            },
        );
        let requested = requested_at(*requested_secs, 0);
        let (status, body) = enable(&router, &image_id, &requested, &[]).await;
        if *served {
            assert_returned_true(case, status, &body);
            let reported = described_deprecation_time(&router, case, &image_id).await;
            assert_eq!(
                reported.as_deref().and_then(as_timestamp).map(|at| at as i64),
                Some(rounded_to_the_minute(*requested_secs)),
                "case {case:?}: a date on the published limit is stored, got {reported:?}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidParameterValue",
                "case {case:?}: expected InvalidParameterValue, got {body}"
            );
            assert!(
                error_message(&body).contains("specified deprecation time"),
                "case {case:?}: the refusal must name the date as its cause, got {}",
                error_message(&body)
            );
            assert_eq!(
                described_deprecation_time(&router, case, &image_id).await,
                None,
                "case {case:?}: a refused request must store no deprecation date"
            );
        }
    }
}

/// A date in the past is refused — "you can't specify a date in the past" — and the image is
/// left exactly as it was.
#[tokio::test]
async fn a_date_in_the_past_is_refused_and_nothing_is_stored() {
    let (router, engine) = super::ec2_query::emulator();
    let base = now_on_the_minute();

    // Rows: a minute ago, a day ago, and a decade ago.
    let cases = [
        ("a minute ago", base - 60),
        ("a day ago", base - SECONDS_PER_DAY),
        ("ten years ago", base - 3650 * SECONDS_PER_DAY),
    ];
    for (index, (case, requested_secs)) in cases.iter().enumerate() {
        let image_id = seed_image(&engine, own_image(&format!("deprecation-past-{index}")));
        let (status, body) = enable(&router, &image_id, &requested_at(*requested_secs, 0), &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {case:?}: expected InvalidParameterValue, got {body}"
        );
        assert_eq!(
            error_message(&body),
            "EnableImageDeprecation failed. Caused by specified deprecation time in the past",
            "case {case:?}: expected the refusal EC2 words for a past date, got {body}"
        );
        assert_eq!(
            described_deprecation_time(&router, case, &image_id).await,
            None,
            "case {case:?}: a refused request must store no deprecation date"
        );
    }
}

// ---------------------------------------------------------------------------
// Which AMIs this account may change at all
// ---------------------------------------------------------------------------

/// An image id this account cannot change is refused by BOTH ends of the member, with EC2's
/// own code: a value that is no AMI id is malformed, an id the account can see no image for
/// does not exist, and an image it can see but does not own is not one it is authorized for.
#[tokio::test]
async fn an_image_this_account_cannot_change_is_refused_by_both_ends() {
    let (router, engine) = super::ec2_query::emulator();
    let base = now_on_the_minute();

    let partner_public = seed_image(
        &engine,
        ImageSpec {
            name: "partner-public-image".to_string(),
            public: true,
            owner_id: PARTNER_ACCOUNT.to_string(),
            ..ImageSpec::default()
        },
    );
    // A published Amazon AMI: an image every account can see and none of them owns.
    let published = engine
        .state::<Ec2State>()
        .lock()
        .images
        .values()
        .find(|image| image.image_owner_alias.as_deref() == Some("amazon"))
        .and_then(|image| image.image_id.clone())
        .expect("the store publishes Amazon's own images");

    // Rows: each unusable id, with the code EC2 answers for it, and — where its wording is
    // the one this surface is recorded answering — the message too.
    let cases: [(&str, String, &str, Option<String>); 4] = [
        (
            "a value that is no AMI id",
            MALFORMED_IMAGE_ID.to_string(),
            "InvalidAMIID.Malformed",
            None,
        ),
        (
            "a well-formed id with no image",
            UNKNOWN_IMAGE_ID.to_string(),
            "InvalidAMIID.NotFound",
            Some(format!("Invalid ImageId: {UNKNOWN_IMAGE_ID}")),
        ),
        (
            "another account's public image",
            partner_public.clone(),
            "AuthFailure",
            None,
        ),
        (
            "a published Amazon image",
            published.clone(),
            "AuthFailure",
            None,
        ),
    ];

    let deprecate_at = requested_at(base + 30 * SECONDS_PER_DAY, 0);
    for (case, image_id, expected_code, expected_message) in &cases {
        let (status, body) = enable(&router, image_id, &deprecate_at, &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?} (enable): expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            &error_code(&body),
            expected_code,
            "case {case:?} (enable): expected {expected_code}, got {body}"
        );
        if let Some(expected_message) = expected_message {
            assert_eq!(
                &error_message(&body),
                expected_message,
                "case {case:?} (enable): expected the message {expected_message:?}, got {body}"
            );
        }

        let (status, body) = disable(&router, image_id, &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?} (disable): expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            &error_code(&body),
            expected_code,
            "case {case:?} (disable): expected {expected_code}, got {body}"
        );
        if let Some(expected_message) = expected_message {
            assert_eq!(
                &error_message(&body),
                expected_message,
                "case {case:?} (disable): expected the message {expected_message:?}, got {body}"
            );
        }
    }

    // Nothing the account cannot change was changed: both visible images still carry no
    // deprecation date.
    for (case, image_id) in [
        ("another account's public image", &partner_public),
        ("a published Amazon image", &published),
    ] {
        assert_eq!(
            described_deprecation_time(&router, case, image_id).await,
            None,
            "case {case:?}: a refused request must store no deprecation date"
        );
    }
}

/// A `DryRun` request is answered `DryRunOperation` by both ends and performs nothing: the
/// member is exactly as it was, whether it carried a date or not.
#[tokio::test]
async fn a_dry_run_answers_dry_run_operation_and_changes_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    let base = now_on_the_minute();
    let dry_run = [p("DryRun", "true")];

    let without_a_date = seed_image(&engine, own_image("deprecation-dry-run-absent"));
    let with_a_date = seed_image(&engine, own_image("deprecation-dry-run-present"));
    let stored_secs = base + 45 * SECONDS_PER_DAY;
    let (status, body) = enable(&router, &with_a_date, &requested_at(stored_secs, 0), &[]).await;
    assert_returned_true("the date a dry run must not disturb", status, &body);

    // Rows: the enable of an image that carries no date, and the cancellation of one that
    // does — the two changes a dry run must refuse to make.
    let (status, body) = enable(
        &router,
        &without_a_date,
        &requested_at(base + 10 * SECONDS_PER_DAY, 0),
        &dry_run,
    )
    .await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case \"a dry run enable\": expected HTTP {DRY_RUN_STATUS}, got {status} with body \
         {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case \"a dry run enable\": expected DryRunOperation, got {body}"
    );
    assert_eq!(
        described_deprecation_time(&router, "a dry run enable", &without_a_date).await,
        None,
        "case \"a dry run enable\": nothing may be stored"
    );

    let (status, body) = disable(&router, &with_a_date, &dry_run).await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case \"a dry run cancellation\": expected HTTP {DRY_RUN_STATUS}, got {status} with \
         body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case \"a dry run cancellation\": expected DryRunOperation, got {body}"
    );
    let reported = described_deprecation_time(&router, "a dry run cancellation", &with_a_date).await;
    assert_eq!(
        reported.as_deref().and_then(as_timestamp).map(|at| at as i64),
        Some(rounded_to_the_minute(stored_secs)),
        "case \"a dry run cancellation\": the stored date must be left alone, got {reported:?}"
    );
}
