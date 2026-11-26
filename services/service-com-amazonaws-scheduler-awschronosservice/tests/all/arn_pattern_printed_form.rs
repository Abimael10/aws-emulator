// SPDX-License-Identifier: CC0-1.0

//! The EXPRESSION a constraint refusal echoes for an ARN-valued member, driven over
//! the real `restJson1` front door.
//!
//! RULE: a `@pattern` refusal states the expression as this service's own validator
//! PRINTS it, not as the public Smithy model spells it: the whole-value anchors the
//! model writes around it are dropped, and an ARN's partition — which every ARN
//! pattern of this model writes as the optional group `aws(-[a-z]+)?` — is printed as
//! the character class `aws[a-z-]*`. The echoed expression is the only
//! machine-readable part of a constraint refusal, so a caller that lifts it to check
//! candidate values locally reads the service's own text.
//!
//! The rest of the clause is the one every constraint refusal of this service carries
//! (`1 validation error detected: Value '<value>' at '<member path>' failed to satisfy
//! constraint: Member must satisfy regular expression pattern: <expression>`), and the
//! table drives several ARN-valued members of two operations rather than one recorded
//! request.

use crate::wire_harness::{call, error_code, error_message, group_arn, scheduler_router, REGION};

/// The expression every row's schedule carries.
const EXPRESSION: &str = "rate(5 minutes)";

/// A well-formed role for the target, so a row fails on the member under test alone.
const ROLE: &str = "arn:aws:iam::000000000000:role/r";

/// How the service prints the partition of an ARN expression.
const PRINTED_PARTITION: &str = "aws[a-z-]*";

/// The clause a value outside its member's expression is refused with.
fn pattern_clause(value: &str, path: &str, expression: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {expression}"
    )
}

#[tokio::test]
async fn an_arn_patterns_partition_is_echoed_as_the_service_prints_it() {
    let router = scheduler_router();

    // (case, the member's modelled expression without its anchors, the member path, a
    // value outside it, the body that carries it). Source: `smithy_query(query="shape",
    // shape="com.amazonaws.scheduler#<Shape>")` for each member.
    let rows: [(&str, &str, &str, &str, serde_json::Value); 3] = [
        (
            // The dead-letter queue: a FIFO queue's ARN ends `.fifo`, which the
            // expression's resource class does not admit.
            "a FIFO queue as the dead-letter queue",
            r"arn:aws[a-z-]*:sqs:[a-z0-9\-]+:\d{12}:[a-zA-Z0-9\-_]+",
            "target.deadLetterConfig.arn",
            "arn:aws:sqs:eu-west-2:000000000000:orders.fifo",
            serde_json::json!({
                "ScheduleExpression": EXPRESSION,
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": {
                    "Arn": "arn:aws:sqs:eu-west-2:000000000000:q",
                    "RoleArn": ROLE,
                    "DeadLetterConfig": {
                        "Arn": "arn:aws:sqs:eu-west-2:000000000000:orders.fifo"
                    }
                }
            }),
        ),
        (
            "an execution role that is no role ARN",
            r"arn:aws[a-z-]*:iam::\d{12}:role\/[\w+=,.@\/-]+",
            "target.roleArn",
            "arn:aws:iam::000000000000:user/u",
            serde_json::json!({
                "ScheduleExpression": EXPRESSION,
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": {
                    "Arn": "arn:aws:sqs:eu-west-2:000000000000:q",
                    "RoleArn": "arn:aws:iam::000000000000:user/u"
                }
            }),
        ),
        (
            "a key that is no KMS key ARN",
            r"arn:aws[a-z-]*:kms:[a-z0-9\-]+:\d{12}:(key|alias)\/[0-9a-zA-Z-_]*",
            "kmsKeyArn",
            "arn:aws:kms:eu-west-2:000000000000:notakey/1",
            serde_json::json!({
                "ScheduleExpression": EXPRESSION,
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "KmsKeyArn": "arn:aws:kms:eu-west-2:000000000000:notakey/1",
                "Target": {
                    "Arn": "arn:aws:sqs:eu-west-2:000000000000:q",
                    "RoleArn": ROLE
                }
            }),
        ),
    ];

    for (index, (case, expression, path, value, body)) in rows.iter().enumerate() {
        assert!(
            expression.contains(PRINTED_PARTITION),
            "case {case:?}: the row's own expression must carry the printed partition"
        );
        let name = format!("pattern-{index}");
        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {answer}"
        );
        assert_eq!(
            error_message(&answer),
            pattern_clause(value, path, expression),
            "case {case:?}: expected {:?}, got {:?}",
            pattern_clause(value, path, expression),
            error_message(&answer)
        );
        // The same member, the same clause, on the other write path.
        let (status, answer) = call(
            &router,
            "PUT",
            &format!("/schedules/{name}"),
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the update refused the same way, got {status} {answer}"
        );
        assert_eq!(
            error_message(&answer),
            pattern_clause(value, path, expression),
            "case {case:?}: expected the same clause from the update, got {:?}",
            error_message(&answer)
        );
    }
}

#[tokio::test]
async fn a_tagging_arn_pattern_is_echoed_in_the_same_printed_form() {
    let router = scheduler_router();
    // `TagResource$ResourceArn` is modelled with the printed partition already
    // (`^arn:aws[a-z-]*:scheduler:[a-z0-9\-]+:\d{12}:schedule-group\/[0-9a-zA-Z-_.]+$`),
    // so the clause states that expression unchanged — the printing is of the
    // partition, not of a particular member's spelling.
    const RESOURCE_ARN: &str =
        r"arn:aws[a-z-]*:scheduler:[a-z0-9\-]+:\d{12}:schedule-group\/[0-9a-zA-Z-_.]+";

    // A schedule's ARN is an ARN of this service that the member's expression does not
    // admit: the resource type is `schedule`, not `schedule-group`.
    let value = format!(
        "arn:aws:scheduler:{REGION}:{}:schedule/default/nightly",
        crate::wire_harness::account_id()
    );
    let (status, answer) = call(
        &router,
        "POST",
        &format!("/tags/{}", crate::wire_harness::encode_label(&value)),
        Some(serde_json::json!({ "Tags": [{ "Key": "k", "Value": "v" }] })),
    )
    .await;
    assert_eq!(
        (status, error_code(&answer).as_str()),
        (400, "ValidationException"),
        "case a schedule's ARN as the tagged resource: expected 400 ValidationException, \
         got {status} {answer}"
    );
    assert_eq!(
        error_message(&answer),
        pattern_clause(&value, "resourceArn", RESOURCE_ARN),
        "case a schedule's ARN as the tagged resource: expected {:?}, got {:?}",
        pattern_clause(&value, "resourceArn", RESOURCE_ARN),
        error_message(&answer)
    );

    // A group's own ARN satisfies the expression, so the same call reaches the handler.
    let (status, created) = call(&router, "POST", "/schedule-groups/tagged", None).await;
    assert_eq!(
        status, 200,
        "setup: CreateScheduleGroup must succeed, got {status} {created}"
    );
    let arn = group_arn(REGION, "tagged");
    let (status, answer) = call(
        &router,
        "POST",
        &format!("/tags/{}", crate::wire_harness::encode_label(&arn)),
        Some(serde_json::json!({ "Tags": [{ "Key": "k", "Value": "v" }] })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a group's own ARN: expected the tagging to succeed, got {status} {answer}"
    );
}
