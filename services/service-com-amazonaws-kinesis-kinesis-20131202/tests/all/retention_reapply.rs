// SPDX-License-Identifier: CC0-1.0

//! RE-APPLYING the retention window a Kinesis data stream already carries, end to end over
//! the wire: `CreateStream` -> `IncreaseStreamRetentionPeriod` /
//! `DecreaseStreamRetentionPeriod` -> `DescribeStreamSummary` / `DescribeStream`.
//!
//! RULE: each retention operation guards its own DIRECTION only, and strictly — `Increase`
//! refuses a window shorter than the stored one and `Decrease` refuses a longer one — so the
//! window the stream already carries, which moves it neither way, is accepted by BOTH and
//! stores what is already stored: HTTP 200 with an empty body, the stream still `ACTIVE` and
//! still reporting that window. Re-applying a desired configuration is therefore idempotent,
//! which is what a converge loop that re-sends it on every run depends on.
//!
//! The recorded flow this was derived from re-applies the 24-hour default to one stream
//! through both operations, which is two rows of the table below. The rows differ along what
//! the rule generalises over: the window the stream carries when the call arrives (the
//! default, both published bounds, one step inside each of them and values between), the
//! direction the call comes from, how many times the same window is re-applied, and the
//! identifier the request resolves the stream by.
//!
//! Every case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the statuses,
//! bodies and modelled errors asserted here are the ones a client sees.

use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    DEFAULT_RETENTION_PERIOD_HOURS, MAX_RETENTION_PERIOD_HOURS, MIN_RETENTION_PERIOD_HOURS,
    STREAM_ACTIVE,
};

use crate::stream_tags::{error_code, poll_until_active, stream_arn, Emulator, REGION};

/// The two operations the rule covers, which is also the order a row calls them in.
const BOTH_DIRECTIONS: [&str; 2] = [
    "IncreaseStreamRetentionPeriod",
    "DecreaseStreamRetentionPeriod",
];

/// How a row addresses the stream: by `StreamName`, by `StreamARN`, or by both together —
/// the three forms every stream operation of this service resolves.
#[derive(Debug, Clone, Copy)]
enum AddressedBy {
    Name,
    Arn,
    Both,
}

/// A retention request for `name` asking for `hours`, addressed the row's way.
fn retention_body(name: &str, hours: i32, addressed_by: AddressedBy) -> Value {
    let mut body = json!({ "RetentionPeriodHours": hours });
    match addressed_by {
        AddressedBy::Name => body["StreamName"] = json!(name),
        AddressedBy::Arn => body["StreamARN"] = json!(stream_arn(REGION, name)),
        AddressedBy::Both => {
            body["StreamName"] = json!(name);
            body["StreamARN"] = json!(stream_arn(REGION, name));
        }
    }
    body
}

/// The `StreamStatus` and `RetentionPeriodHours` `DescribeStreamSummary` reports for `name`.
async fn reported(emulator: &Emulator, case: &str, name: &str) -> (String, i64) {
    let described = emulator
        .ok(case, "DescribeStreamSummary", &json!({ "StreamName": name }))
        .await;
    let summary = &described["StreamDescriptionSummary"];
    let status = summary["StreamStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: the summary must carry StreamStatus, got {described}"))
        .to_string();
    let hours = summary
        .get("RetentionPeriodHours")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            panic!("case {case}: the summary must carry RetentionPeriodHours, got {described}")
        });
    (status, hours)
}

/// One row: the window the stream carries when the calls arrive, and how they address it.
struct Reapply {
    case: &'static str,
    /// The window the stream carries when the re-applications arrive. A row whose window is
    /// not the default a create gives is lengthened to it first.
    carried: i32,
    addressed_by: AddressedBy,
}

/// Re-applying the window a stream already carries is accepted as a no-op by BOTH retention
/// operations, and the stream reads `ACTIVE` with that same window after each call.
#[tokio::test]
async fn re_applying_the_window_a_stream_carries_is_a_no_op() {
    let rows: Vec<Reapply> = vec![
        Reapply {
            // The recorded case: a stream still at the documented default, which is also the
            // published FLOOR of the window.
            case: "reapply-the-default",
            carried: DEFAULT_RETENTION_PERIOD_HOURS,
            addressed_by: AddressedBy::Name,
        },
        Reapply {
            case: "reapply-one-step-above-the-floor",
            carried: MIN_RETENTION_PERIOD_HOURS + 1,
            addressed_by: AddressedBy::Arn,
        },
        Reapply {
            case: "reapply-a-week",
            carried: 168,
            addressed_by: AddressedBy::Both,
        },
        Reapply {
            case: "reapply-one-step-below-the-ceiling",
            carried: MAX_RETENTION_PERIOD_HOURS - 1,
            addressed_by: AddressedBy::Name,
        },
        Reapply {
            // The published CEILING, where `Increase` has nowhere left to go.
            case: "reapply-the-ceiling",
            carried: MAX_RETENTION_PERIOD_HOURS,
            addressed_by: AddressedBy::Arn,
        },
    ];

    let emulator = Emulator::new();
    for row in &rows {
        emulator
            .ok(
                row.case,
                "CreateStream",
                &json!({ "StreamName": row.case, "ShardCount": 1 }),
            )
            .await;
    }
    // The CONDITION each stream reaches on the emulator's own clock, looked for again until
    // it holds — never a duration this case measures.
    for row in &rows {
        poll_until_active(&emulator, row.case, row.case).await;
    }

    for row in &rows {
        let case = row.case;
        if row.carried != DEFAULT_RETENTION_PERIOD_HOURS {
            emulator
                .ok(
                    case,
                    "IncreaseStreamRetentionPeriod",
                    &json!({ "StreamName": case, "RetentionPeriodHours": row.carried }),
                )
                .await;
        }

        // Twice through both operations, so the row also pins that re-applying the same
        // window again changes nothing.
        for round in 1..=2 {
            for operation in BOTH_DIRECTIONS {
                let label = format!("{case}-{operation}-round-{round}");
                let served = emulator
                    .ok(
                        &label,
                        operation,
                        &retention_body(case, row.carried, row.addressed_by),
                    )
                    .await;
                assert!(
                    served.is_null() || served == json!({}),
                    "case {label}: the operation models no output members, got {served}"
                );
                let (status, hours) = reported(&emulator, &label, case).await;
                assert_eq!(
                    (status.as_str(), hours),
                    (STREAM_ACTIVE, i64::from(row.carried)),
                    "case {label}: expected the stream to stay ({STREAM_ACTIVE}, {}), got \
                     ({status}, {hours})",
                    row.carried
                );
            }
        }

        // The full description agrees with the summary, which is the read a converge loop
        // compares against.
        let described = emulator
            .ok(case, "DescribeStream", &json!({ "StreamName": case }))
            .await;
        assert_eq!(
            described["StreamDescription"]
                .get("RetentionPeriodHours")
                .and_then(Value::as_i64),
            Some(i64::from(row.carried)),
            "case {case}: expected the description to carry the window unchanged, got \
             {described}"
        );
    }
}

/// The equal boundary is accepted only for a stream that EXISTS and is `ACTIVE`: the no-op is
/// a served change, so every other guard of the two operations still stands in front of it.
#[tokio::test]
async fn a_no_op_retention_change_still_needs_a_stream_to_apply_to() {
    let emulator = Emulator::new();
    let absent = "reapply-no-such-stream";
    for operation in BOTH_DIRECTIONS {
        let (status, response) = emulator
            .call(
                operation,
                &json!({
                    "StreamName": absent,
                    "RetentionPeriodHours": DEFAULT_RETENTION_PERIOD_HOURS,
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ResourceNotFoundException"),
            "case {operation}: a no-op against a stream nothing holds is still refused, got \
             {status} with {response}"
        );
    }

    // Once that stream reaches the status its create owes it, the same no-op is served: the
    // not-ACTIVE guard in front of it is the one `stream_not_active_refusal` pins for both
    // operations, so this case only shows the no-op does not get past the guards.
    let creating = "reapply-after-the-create-settles";
    emulator
        .ok(
            creating,
            "CreateStream",
            &json!({ "StreamName": creating, "ShardCount": 1 }),
        )
        .await;
    poll_until_active(&emulator, creating, creating).await;
    for operation in BOTH_DIRECTIONS {
        emulator
            .ok(
                creating,
                operation,
                &json!({
                    "StreamName": creating,
                    "RetentionPeriodHours": DEFAULT_RETENTION_PERIOD_HOURS,
                }),
            )
            .await;
    }
    let (status, hours) = reported(&emulator, creating, creating).await;
    assert_eq!(
        (status.as_str(), hours),
        (STREAM_ACTIVE, i64::from(DEFAULT_RETENTION_PERIOD_HOURS)),
        "case {creating}: expected ({STREAM_ACTIVE}, {DEFAULT_RETENTION_PERIOD_HOURS}), got \
         ({status}, {hours})"
    );
}
