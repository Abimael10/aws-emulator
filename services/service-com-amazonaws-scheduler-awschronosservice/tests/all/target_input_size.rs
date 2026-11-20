// SPDX-License-Identifier: CC0-1.0

//! The SIZE of the payload a schedule may hand its target, driven over the real
//! `restJson1` front door.
//!
//! RULE: `Target$Input` — "the text, or well-formed JSON, passed to the target" — is
//! bounded above by the service and not by the model (`TargetInput` is `@length` min 1
//! with no maximum): a payload of at most [`MAX_TARGET_INPUT_BYTES`] bytes is accepted and
//! stored, and a larger one is refused 400 `ValidationException` stating the limit, with
//! nothing written. The rule holds for BOTH operations that write a schedule, because both
//! carry the member.
//!
//! The rows span the class rather than the one recorded payload: they differ along the
//! SIZE (no payload at all, one byte, a small document, one byte short of the maximum,
//! exactly the maximum, one byte past it, twice the maximum), along the FORM (a JSON
//! document, plain text) and along the TEXT (ASCII, and two-byte characters, which cross
//! the bound at half as many characters — so a payload is measured in bytes and not in
//! characters).

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{call, error_code, error_message, scheduler_router};

/// The largest `Target$Input` EventBridge Scheduler accepts, in bytes: its maximum target
/// payload. A fixed service bound, not a value from any request — the payload exactly at it
/// is accepted and one byte more is refused.
///
/// Stated here rather than read from the crate's own `state::MAX_TARGET_INPUT_BYTES` ON
/// PURPOSE, as is the sentence in [`expected_refusal`]: a suite that took both from the
/// implementation would still pass if the bound or the wording moved, so the wire contract
/// is pinned independently of the constant that serves it.
const MAX_TARGET_INPUT_BYTES: usize = 262_144;

/// A two-byte character, so a row can cross the byte bound well inside any character count.
const TWO_BYTE_CHARACTER: &str = "é";

/// The payload the schedule every refused update writes against carries, so an update that
/// must not apply is visibly unapplied.
const SEEDED_INPUT: &str = "{\"seeded\":true}";

/// A target carrying `input`, or none when `input` is `None`. The role is only SHAPED
/// like one: this router mounts no IAM, so nothing resolves it, and what is under test is
/// the payload.
fn target(input: Option<&str>) -> serde_json::Value {
    let mut target = serde_json::json!({
        "Arn": "arn:aws:sqs:eu-west-2:000000000000:q",
        "RoleArn": "arn:aws:iam::000000000000:role/r"
    });
    if let Some(input) = input {
        target["Input"] = serde_json::Value::String(input.to_string());
    }
    target
}

/// A `CreateSchedule`/`UpdateSchedule` body whose target carries `input`.
fn schedule_body(input: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "ScheduleExpression": "rate(5 minutes)",
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "Target": target(input)
    })
}

/// `GET /schedules/{Name}` as the parsed body it answers with.
async fn read_schedule(router: &MultiServiceRouter, name: &str) -> (u16, serde_json::Value) {
    call(router, "GET", &format!("/schedules/{name}"), None).await
}

/// The `Target.Input` a read answers with, or `None` when the stored target carries none.
fn stored_input(read: &serde_json::Value) -> Option<String> {
    read.get("Target")?
        .get("Input")?
        .as_str()
        .map(str::to_string)
}

/// The message a refused payload must carry: the service's statement about the target's
/// payload, naming the limit and the unit it is counted in.
fn expected_refusal() -> String {
    format!("The input size for the target cannot exceed {MAX_TARGET_INPUT_BYTES} bytes.")
}

/// One row: the payload a write carries, and whether the bound admits it.
struct PayloadCase {
    case: &'static str,
    payload: Option<String>,
    accepted: bool,
}

/// Every row of the class, built once and driven through both operations.
fn payload_cases() -> Vec<PayloadCase> {
    vec![
        PayloadCase {
            case: "no payload at all",
            payload: None,
            accepted: true,
        },
        PayloadCase {
            case: "the shortest payload the model admits",
            payload: Some("x".to_string()),
            accepted: true,
        },
        PayloadCase {
            case: "a small JSON document",
            payload: Some("{\"order\":{\"id\":7,\"items\":[1,2,3]}}".to_string()),
            accepted: true,
        },
        PayloadCase {
            case: "two-byte characters filling the maximum exactly",
            payload: Some(TWO_BYTE_CHARACTER.repeat(MAX_TARGET_INPUT_BYTES / 2)),
            accepted: true,
        },
        PayloadCase {
            case: "a payload one byte short of the maximum",
            payload: Some("x".repeat(MAX_TARGET_INPUT_BYTES - 1)),
            accepted: true,
        },
        PayloadCase {
            case: "a payload exactly at the maximum",
            payload: Some("x".repeat(MAX_TARGET_INPUT_BYTES)),
            accepted: true,
        },
        PayloadCase {
            case: "a payload one byte past the maximum",
            payload: Some("x".repeat(MAX_TARGET_INPUT_BYTES + 1)),
            accepted: false,
        },
        PayloadCase {
            case: "a JSON document past the maximum",
            payload: Some(format!(
                "{{\"blob\":\"{}\"}}",
                "x".repeat(MAX_TARGET_INPUT_BYTES)
            )),
            accepted: false,
        },
        PayloadCase {
            case: "a payload twice the maximum",
            payload: Some("x".repeat(MAX_TARGET_INPUT_BYTES * 2)),
            accepted: false,
        },
        PayloadCase {
            case: "two-byte characters one character past the maximum",
            payload: Some(TWO_BYTE_CHARACTER.repeat(MAX_TARGET_INPUT_BYTES / 2 + 1)),
            accepted: false,
        },
    ]
}

/// A refusal of a payload: the status, the code and the whole sentence.
fn assert_refusal(case: &str, operation: &str, status: u16, answer: &serde_json::Value) {
    assert_eq!(
        (status, error_code(answer).as_str()),
        (400, "ValidationException"),
        "case {case:?}: {operation} must refuse it 400 ValidationException, got {status} \
         {:?}",
        error_code(answer)
    );
    let message = error_message(answer);
    assert_eq!(
        message,
        expected_refusal(),
        "case {case:?}: {operation} must state the limit, got {message:?}"
    );
}

/// `CreateSchedule`: a payload inside the bound is stored and reads back verbatim; one
/// past it is refused and nothing is persisted.
#[tokio::test]
async fn create_schedule_bounds_the_target_payload() {
    let router = scheduler_router();

    for (index, row) in payload_cases().iter().enumerate() {
        let name = format!("created-{index}");
        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(schedule_body(row.payload.as_deref())),
        )
        .await;
        let (read_status, read) = read_schedule(&router, &name).await;

        if row.accepted {
            assert_eq!(
                status, 200,
                "case {:?}: CreateSchedule must accept a payload of {:?} bytes, got {status} \
                 {:?}",
                row.case,
                row.payload.as_ref().map(String::len),
                error_message(&answer)
            );
            assert_eq!(
                read_status, 200,
                "case {:?}: the schedule must be persisted, got {read_status} {:?}",
                row.case,
                error_code(&read)
            );
            assert_eq!(
                stored_input(&read),
                row.payload,
                "case {:?}: expected the payload stored verbatim, got {:?} bytes",
                row.case,
                stored_input(&read).map(|stored| stored.len())
            );
        } else {
            assert_refusal(row.case, "CreateSchedule", status, &answer);
            assert_eq!(
                (read_status, error_code(&read).as_str()),
                (404, "ResourceNotFoundException"),
                "case {:?}: a refused create must persist nothing, got {read_status} {:?}",
                row.case,
                error_code(&read)
            );
        }
    }
}

/// `UpdateSchedule`: the same bound, on a schedule that already exists — an accepted
/// payload replaces the stored one, a refused payload leaves it exactly as it was.
#[tokio::test]
async fn update_schedule_bounds_the_target_payload() {
    let router = scheduler_router();

    for (index, row) in payload_cases().iter().enumerate() {
        let name = format!("updated-{index}");
        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(schedule_body(Some(SEEDED_INPUT))),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup for case {:?}: CreateSchedule must succeed, got {status} {:?}",
            row.case,
            error_message(&answer)
        );

        let (status, answer) = call(
            &router,
            "PUT",
            &format!("/schedules/{name}"),
            Some(schedule_body(row.payload.as_deref())),
        )
        .await;
        let (read_status, read) = read_schedule(&router, &name).await;
        assert_eq!(
            read_status, 200,
            "case {:?}: the schedule written in setup must still be readable, got \
             {read_status} {:?}",
            row.case,
            error_code(&read)
        );

        if row.accepted {
            assert_eq!(
                status, 200,
                "case {:?}: UpdateSchedule must accept a payload of {:?} bytes, got {status} \
                 {:?}",
                row.case,
                row.payload.as_ref().map(String::len),
                error_message(&answer)
            );
            assert_eq!(
                stored_input(&read),
                row.payload,
                "case {:?}: expected the update applied verbatim, got {:?} bytes",
                row.case,
                stored_input(&read).map(|stored| stored.len())
            );
        } else {
            assert_refusal(row.case, "UpdateSchedule", status, &answer);
            assert_eq!(
                stored_input(&read).as_deref(),
                Some(SEEDED_INPUT),
                "case {:?}: a refused update must leave the stored payload alone, got {:?} \
                 bytes",
                row.case,
                stored_input(&read).map(|stored| stored.len())
            );
        }
    }
}
