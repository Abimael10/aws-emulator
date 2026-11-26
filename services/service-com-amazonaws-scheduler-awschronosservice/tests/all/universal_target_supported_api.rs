// SPDX-License-Identifier: CC0-1.0

//! Which API operation a UNIVERSAL target may name, driven over the real `restJson1`
//! front door on BOTH operations that write a schedule.
//!
//! RULE: a universal target's `Target$Arn`
//! (`arn:<partition>:scheduler:::aws-sdk:<service>:<operation>`) names the CALL each
//! occurrence makes, so the operation has to be one there is a point in a schedule
//! making: an operation whose only effect is its own ANSWER changes nothing when it is
//! performed and has no caller for that answer, so EventBridge Scheduler supports no
//! schedule for one and refuses the write 400 `ValidationException` with `Invalid
//! RequestJson provided. Reason <operation> API is not supported.`, naming the
//! operation the ARN carried. An operation that changes the resource it addresses is
//! accepted and the target round-trips.
//!
//! WHICH operations only read is the called service's own fact, read from the tree's
//! single statement of the APIs a universal target may name
//! (`service_common::universal_target_apis`), so the table below walks several of its
//! services and both kinds of operation in each — including the operations whose
//! spelling says the opposite of what they do — rather than the one recorded
//! operation. A service that statement holds nothing for is not judged at all.

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{call, error_code, error_message, scheduler_router};

/// The expression a seeded schedule carries, which a refused update must leave on it.
const SEEDED_EXPRESSION: &str = "rate(1 hour)";

/// The universal target naming `service`'s `operation`.
fn universal_target(service: &str, operation: &str) -> String {
    format!("arn:aws:scheduler:::aws-sdk:{service}:{operation}")
}

/// A write body whose target names `arn` and hands it `Input`.
fn body(arn: &str) -> serde_json::Value {
    serde_json::json!({
        "ScheduleExpression": SEEDED_EXPRESSION,
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "State": "DISABLED",
        "Target": {
            "Arn": arn,
            "RoleArn": "arn:aws:iam::000000000000:role/r",
            "Input": "{\"QueueName\":\"orders\"}"
        }
    })
}

/// The refusal an operation this service supports no schedule for carries.
fn unsupported(operation: &str) -> String {
    format!("Invalid RequestJson provided. Reason {operation} API is not supported.")
}

/// Create the schedule a row's update is applied to, and answer its name.
async fn seeded(router: &MultiServiceRouter, name: &str) -> String {
    let (status, answer) = call(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(body(&universal_target("sqs", "sendMessage"))),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSchedule {name} must succeed, got {status} {answer}"
    );
    name.to_string()
}

#[tokio::test]
async fn a_universal_target_naming_a_reading_operation_is_refused() {
    // (case, the service, the operation, whether the write is refused)
    let rows: [(&str, &str, &str, bool); 14] = [
        ("a queue's own lookup", "sqs", "getQueueUrl", true),
        ("a queue attribute read", "sqs", "getQueueAttributes", true),
        ("a queue listing", "sqs", "listQueues", true),
        ("an item read", "dynamodb", "getItem", true),
        ("a transactional read", "dynamodb", "transactGetItems", true),
        ("an object read", "s3", "getObject", true),
        ("an object query that changes nothing", "s3", "selectObjectContent", true),
        // The operations that change what they address, including the two whose
        // spelling reads like a read and the one whose spelling reads like a write.
        ("a message sent", "sqs", "sendMessage", false),
        ("a receive, which starts a visibility timeout", "sqs", "receiveMessage", false),
        ("a purge, which deletes every message", "sqs", "purgeQueue", false),
        ("an item written", "dynamodb", "putItem", false),
        ("a restore, which starts one", "s3", "restoreObject", false),
        // A service the shared statement holds no APIs for is not judged: AWS's
        // universal targets reach far more services than this emulator hosts, so a
        // schedule naming one is written and left to its occurrence to report.
        ("a read of a service it holds nothing for", "sfn", "describeExecution", false),
        ("a write of a service it holds nothing for", "sfn", "startExecution", false),
    ];

    let router = scheduler_router();
    let held = seeded(&router, "held.universal_schedule").await;

    for (index, (case, service, operation, refused)) in rows.iter().enumerate() {
        let name = format!("universal-{index}");
        let arn = universal_target(service, operation);
        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(body(&arn)),
        )
        .await;
        let (read_status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;

        if *refused {
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (400, "ValidationException"),
                "case {case:?}: CreateSchedule must refuse {arn} 400 ValidationException, \
                 got {status} {answer}"
            );
            assert_eq!(
                error_message(&answer),
                unsupported(operation),
                "case {case:?}: expected {:?}, got {:?}",
                unsupported(operation),
                error_message(&answer)
            );
            assert_eq!(
                (read_status, error_code(&read).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: a refused create persists nothing, got {read_status} {read}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: CreateSchedule must accept {arn}, got {status} {answer}"
            );
            assert_eq!(
                read.get("Target")
                    .and_then(|target| target.get("Arn"))
                    .and_then(serde_json::Value::as_str),
                Some(arn.as_str()),
                "case {case:?}: the target must round-trip, got {read}"
            );
        }

        // The same rule on the other write path.
        let (status, answer) = call(
            &router,
            "PUT",
            &format!("/schedules/{held}"),
            Some(body(&arn)),
        )
        .await;
        let (_, held_read) = call(&router, "GET", &format!("/schedules/{held}"), None).await;
        let stored = held_read
            .get("Target")
            .and_then(|target| target.get("Arn"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if *refused {
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (400, "ValidationException"),
                "case {case:?}: UpdateSchedule must refuse {arn} 400 ValidationException, \
                 got {status} {answer}"
            );
            assert_eq!(
                error_message(&answer),
                unsupported(operation),
                "case {case:?}: expected the same sentence from the update, got {:?}",
                error_message(&answer)
            );
            assert_ne!(
                stored, arn,
                "case {case:?}: a refused update changes nothing, got {held_read}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: UpdateSchedule must accept {arn}, got {status} {answer}"
            );
            assert_eq!(
                stored, arn,
                "case {case:?}: an accepted update must be applied, got {held_read}"
            );
            let (status, answer) = call(
                &router,
                "PUT",
                &format!("/schedules/{held}"),
                Some(body(&universal_target("sqs", "sendMessage"))),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: restoring the held schedule, got {status} {answer}"
            );
        }
    }
}
