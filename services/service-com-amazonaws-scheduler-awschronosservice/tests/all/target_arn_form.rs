// SPDX-License-Identifier: CC0-1.0

//! A schedule's target has to NAME something: `Target$Arn` must hold a well-formed ARN.
//!
//! The member is documented "the Amazon Resource Name (ARN) of the target" and the model
//! constrains it by length alone — `TargetArn` is a plain `@length 1..1600` string with no
//! `@pattern` (`smithy_query(query="shape", shape="com.amazonaws.scheduler#TargetArn")`) —
//! so the request boundary admits any text of that length and the ARN form is the
//! SERVICE's own check. A value that is no ARN names neither a resource nor an API
//! operation, so no occurrence of such a schedule could ever be delivered: the write is
//! refused with HTTP 400 `ValidationException`, the modelled error type on the wire, and a
//! message quoting the value the request carried.
//!
//! Both operations that write a schedule are held to it, and a refused write leaves the
//! store exactly as it was. Every request goes through the shared [`crate::wire_harness`],
//! the real restJson1 front door.

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{call, error_code, error_message, request, scheduler_router, REGION};

/// The execution role every write below names; it carries the modelled `@pattern` for
/// `Target$RoleArn`, so nothing in these cases is refused for the role.
const ROLE: &str = "arn:aws:iam::000000000000:role/r";

/// How the service states that a member documented as an ARN holds text that is not one.
/// Spelled out here rather than read from the crate, so the text is asserted against a
/// literal and not against itself.
fn not_an_arn(value: &str) -> String {
    format!("Parameter {value} is not valid. Reason: Provided Arn is not in correct format.")
}

/// A write body naming `target_arn`.
fn write_body(target_arn: &str) -> serde_json::Value {
    serde_json::json!({
        "ScheduleExpression": "rate(10 minutes)",
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "Target": { "Arn": target_arn, "RoleArn": ROLE }
    })
}

/// Dispatch and answer `(status, the x-amzn-errortype header, parsed body)`: the modelled
/// error type travels in that header, which is what an SDK reads the error shape from.
async fn call_reading_error_type(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (u16, String, serde_json::Value) {
    let response = router
        .dispatch(request(method, uri, REGION, body))
        .await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, error_type, parsed)
}

/// Values that are no ARN, spanning the ways text can fail the form: no separators at all,
/// too few fields, a first field that is not `arn`, and each of the three fields an ARN may
/// not leave empty. The shortest and longest the member admits are in there too
/// (`TargetArn` is `@length 1..1600`), so the refusal is not keyed to a length.
fn values_that_are_no_arn() -> Vec<(&'static str, String)> {
    vec![
        ("text with no separators", "not-an-arn".to_string()),
        (
            "a single character, the shortest the member admits",
            "x".to_string(),
        ),
        (
            "a URL rather than an ARN",
            "https://sqs.eu-west-2.amazonaws.com/000000000000/q".to_string(),
        ),
        (
            "too few fields",
            "arn:aws:sqs:eu-west-2:000000000000".to_string(),
        ),
        (
            "a first field that is not arn",
            "urn:aws:sqs:eu-west-2:000000000000:q".to_string(),
        ),
        (
            "an empty partition",
            "arn::sqs:eu-west-2:000000000000:q".to_string(),
        ),
        (
            "an empty vendor",
            "arn:aws::eu-west-2:000000000000:q".to_string(),
        ),
        (
            "an empty resource",
            "arn:aws:sqs:eu-west-2:000000000000:".to_string(),
        ),
        (
            // 1600 characters, the longest the member admits, none of them a separator.
            "the longest value the member admits",
            "a".repeat(1600),
        ),
    ]
}

/// ARNs a schedule may target, spanning the classes the rule must not refuse: templated
/// targets of several services, a resource field carrying a colon and one carrying a
/// slash, another partition, another account, a resource with no Region or account, and a
/// universal target, which names an API operation.
fn arns_a_target_may_name() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a queue", "arn:aws:sqs:eu-west-2:000000000000:q"),
        (
            "a function, whose resource field carries a colon",
            "arn:aws:lambda:eu-west-2:000000000000:function:fn",
        ),
        (
            "a stream, whose resource field carries a slash",
            "arn:aws:kinesis:eu-west-2:000000000000:stream/s",
        ),
        (
            "a state machine in another account",
            "arn:aws:states:eu-west-2:111122223333:stateMachine:sm",
        ),
        (
            "a resource in another partition",
            "arn:aws-cn:sqs:cn-north-1:000000000000:q",
        ),
        (
            "a universal target, with no Region or account",
            "arn:aws:scheduler:::aws-sdk:sqs:sendMessage",
        ),
    ]
}

#[tokio::test]
async fn a_create_whose_target_names_no_arn_is_refused_and_stores_nothing() {
    let router = scheduler_router();

    for (index, (case, value)) in values_that_are_no_arn().into_iter().enumerate() {
        let name = format!("no-arn-create-{index}");
        let (status, error_type, body) = call_reading_error_type(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(write_body(&value)),
        )
        .await;
        let message = error_message(&body);
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a validation refusal, got {status} {body}"
        );
        assert!(
            error_type.contains("ValidationException"),
            "case {case:?}: expected the modelled error type on the wire, got {error_type:?}"
        );
        assert_eq!(
            message,
            not_an_arn(&value),
            "case {case:?}: expected {:?}, got {message:?}",
            not_an_arn(&value)
        );

        // The refused create stored nothing: the name is still free.
        let (status, body) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: a refused create must store nothing, got {status} {body}"
        );
    }
}

#[tokio::test]
async fn an_update_whose_target_names_no_arn_is_refused_and_changes_nothing() {
    let router = scheduler_router();
    let name = "no-arn-update";
    let stored = "arn:aws:sqs:eu-west-2:000000000000:kept";

    let (status, body) = call(
        &router,
        "POST",
        &format!("/schedules/{name}"),
        Some(write_body(stored)),
    )
    .await;
    assert_eq!(status, 200, "the seeding create answered {status} {body}");

    for (case, value) in values_that_are_no_arn() {
        let (status, error_type, body) = call_reading_error_type(
            &router,
            "PUT",
            &format!("/schedules/{name}"),
            Some(write_body(&value)),
        )
        .await;
        let message = error_message(&body);
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a validation refusal, got {status} {body}"
        );
        assert!(
            error_type.contains("ValidationException"),
            "case {case:?}: expected the modelled error type on the wire, got {error_type:?}"
        );
        assert_eq!(
            message,
            not_an_arn(&value),
            "case {case:?}: expected {:?}, got {message:?}",
            not_an_arn(&value)
        );

        // The refused update changed nothing: the schedule still points where it did.
        let (status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        assert_eq!(
            (status, read["Target"]["Arn"].as_str()),
            (200, Some(stored)),
            "case {case:?}: a refused update must leave the target alone, got {read}"
        );
    }
}

#[tokio::test]
async fn every_arn_class_a_target_may_name_is_accepted_and_read_back() {
    let router = scheduler_router();

    for (index, (case, value)) in arns_a_target_may_name().into_iter().enumerate() {
        let name = format!("arn-class-{index}");
        let (status, body) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(write_body(value)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {body}"
        );

        let (status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        assert_eq!(
            (status, read["Target"]["Arn"].as_str()),
            (200, Some(value)),
            "case {case:?}: expected the target read back as {value}, got {read}"
        );
    }
}
