// SPDX-License-Identifier: CC0-1.0

//! WHICH class a dead letter carries when the destination queue's own resource policy
//! withheld the delivery, end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter` with
//! Amazon SQS on the same engine — so each row is read where a consumer of a
//! dead-letter queue reads it: off the message attributes of the dead letter the
//! target's `DeadLetterConfig` queue received.
//!
//! The RULE asserted here: EventBridge classifies a failed delivery by what the
//! target service's refusal is ABOUT, so a target that IS there and refused the CALL
//! is the PERMISSION class (`NO_PERMISSIONS`) and only a target that was not there to
//! be called is the RESOURCE class (`NO_RESOURCE`). A
//! queue target is reached with `sqs:SendMessage` as the service principal and no role
//! ("for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies
//! on resource-based policies", `PutTargets`), so a queue whose `Policy` does not allow
//! that principal that action on that queue answers a refusal of that call — the code
//! and status AWS answers an unauthorized call with, `AccessDenied` at HTTP 403, which
//! `SendMessage` declares no more than any other operation does because such a refusal
//! is answered ahead of the operation (`smithy_query(query="operation",
//! operation="SendMessage",
//! model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
//! — and the dead letter carries the permission class with
//! that answer beside it. A queue ARN nothing was created for is the other class, under
//! that service's `QueueDoesNotExist`.
//!
//! The table spans the class rather than the one case the defect was found on: its rows
//! differ along the FORM the withholding takes (no document at all, a document with no
//! statement, an explicit `Deny` beside the grant, a grant scoped by `aws:SourceArn` to
//! another rule, a grant of another action, a grant to another service principal) and
//! along whether the queue was there at all. Every row installs a SECOND queue target
//! on the same rule that DOES grant the send, and asserts it received the event — so a
//! row cannot pass by failing every delivery, and the two classes are told apart on one
//! publication.
//!
//! The condition operators and keys the rows use are IAM's own, because a queue's
//! `Policy` is an IAM policy document
//! (`smithy_query(query="doc",shape="com.amazonaws.sqs#QueueAttributeName")`).
//!
//! The harness (the router with EventBridge, Amazon SQS, Step Functions, IAM and
//! CloudWatch on one engine) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    rule_arn, DEFAULT_EVENT_BUS_NAME, NO_PERMISSIONS, NO_RESOURCE, SERVICE_PRINCIPAL,
};

use crate::target_dispatch::{
    drain, emulator, given_queue, given_queue_with_policy, given_rule, granting_events, publish,
    Emulator, REGION,
};

/// The rule every row below publishes through.
const RULE: &str = "classified-by-permission";

/// The ids the two queue targets of that rule carry.
const WITHHELD_TARGET_ID: &str = "withheld";
const GRANTED_TARGET_ID: &str = "granted";

/// The IAM action a delivery to a queue target performs on that queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The dead-letter message attributes this suite reads.
const ERROR_CODE_ATTRIBUTE: &str = "ERROR_CODE";
const ERROR_MESSAGE_ATTRIBUTE: &str = "ERROR_MESSAGE";
const RULE_ARN_ATTRIBUTE: &str = "RULE_ARN";
const TARGET_ARN_ATTRIBUTE: &str = "TARGET_ARN";

/// The error code and status Amazon SQS answers a send the documents in force over the
/// queue do not permit, and the code it answers for a queue that is not there — stated
/// here rather than read from the implementation, so a wrong answer there cannot make
/// this suite pass.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;
const QUEUE_DOES_NOT_EXIST: &str = "AWS.SimpleQueueService.NonExistentQueue";

/// A statement list wrapped as a policy document.
fn document(statements: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// An `Allow` of `action` to the service principal `principal` on `resource`, scoped by
/// `condition` when the row states one.
fn grant(principal: &str, action: &str, resource: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": {"Service": principal},
        "Action": action,
        "Resource": resource,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    statement
}

/// A document carrying no statement at all, which grants nothing.
fn no_statements(_queue_arn: &str) -> String {
    document(json!([]))
}

/// A grant of the send with an explicit `Deny` of it beside the grant.
fn granting_then_denying(queue_arn: &str) -> String {
    document(json!([
        grant(SERVICE_PRINCIPAL, SEND_MESSAGE, queue_arn, None),
        {
            "Effect": "Deny",
            "Principal": {"Service": SERVICE_PRINCIPAL},
            "Action": SEND_MESSAGE,
            "Resource": queue_arn,
        },
    ]))
}

/// A grant of the send scoped by `aws:SourceArn` to the deliveries of a rule other than
/// the one publishing here.
fn granting_another_rule(queue_arn: &str) -> String {
    let other = rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, "somebody-elses-rule");
    document(json!([grant(
        SERVICE_PRINCIPAL,
        SEND_MESSAGE,
        queue_arn,
        Some(json!({"ArnEquals": {"aws:SourceArn": other}})),
    )]))
}

/// A grant of another action of the same service on the queue.
fn granting_another_action(queue_arn: &str) -> String {
    document(json!([grant(
        SERVICE_PRINCIPAL,
        "sqs:ReceiveMessage",
        queue_arn,
        None
    )]))
}

/// A grant of the send to a different service principal.
fn granting_another_principal(queue_arn: &str) -> String {
    document(json!([grant(
        "s3.amazonaws.com",
        SEND_MESSAGE,
        queue_arn,
        None
    )]))
}

/// How a row brings the queue its withheld target names about.
enum Setup {
    /// A queue carrying the document the row's function builds from its own ARN, or none
    /// at all when the function is `None`.
    Queue(Option<fn(&str) -> String>),
    /// A queue ARN of this account and Region that no queue was ever created for.
    NeverCreated,
}

/// One row: the case, how its withheld target is brought about, the class the dead
/// letter is to carry and the error code the answer beside it must name.
struct Row {
    case: &'static str,
    setup: Setup,
    class: &'static str,
    code: &'static str,
}

/// The string value of the dead letter's message attribute `name`.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The ARN the row's withheld target names, and the url of its queue when the row has
/// one.
async fn given_withheld_target(
    emulator: &Emulator,
    index: usize,
    setup: &Setup,
) -> (String, Option<String>) {
    match setup {
        Setup::Queue(policy) => {
            let policy: Option<&dyn Fn(&str) -> String> = policy
                .as_ref()
                .map(|written| written as &dyn Fn(&str) -> String);
            let (url, arn) =
                given_queue_with_policy(emulator, &format!("permission-target-{index}"), policy)
                    .await;
            (arn, Some(url))
        }
        Setup::NeverCreated => (
            format!(
                "arn:aws:sqs:{REGION}:{}:never-created-{index}",
                engine::emulator_account_id()
            ),
            None,
        ),
    }
}

/// The class of a delivery the destination queue's own documents withheld is the
/// permission one, over every form the withholding takes — and only a queue that was
/// not there earns the resource one.
#[tokio::test]
async fn a_queue_policy_refusal_is_dead_lettered_as_a_permission_failure() {
    let rows = [
        Row {
            case: "a queue carrying no policy at all",
            setup: Setup::Queue(None),
            class: NO_PERMISSIONS,
            code: ACCESS_DENIED,
        },
        Row {
            case: "a queue whose document states no statement",
            setup: Setup::Queue(Some(no_statements)),
            class: NO_PERMISSIONS,
            code: ACCESS_DENIED,
        },
        Row {
            case: "a queue whose policy denies the send beside granting it",
            setup: Setup::Queue(Some(granting_then_denying)),
            class: NO_PERMISSIONS,
            code: ACCESS_DENIED,
        },
        Row {
            case: "a queue whose grant is scoped by aws:SourceArn to another rule",
            setup: Setup::Queue(Some(granting_another_rule)),
            class: NO_PERMISSIONS,
            code: ACCESS_DENIED,
        },
        Row {
            case: "a queue granting this service another action",
            setup: Setup::Queue(Some(granting_another_action)),
            class: NO_PERMISSIONS,
            code: ACCESS_DENIED,
        },
        Row {
            case: "a queue granting the send to another service principal",
            setup: Setup::Queue(Some(granting_another_principal)),
            class: NO_PERMISSIONS,
            code: ACCESS_DENIED,
        },
        Row {
            case: "a queue ARN no queue was created for",
            setup: Setup::NeverCreated,
            class: NO_RESOURCE,
            code: QUEUE_DOES_NOT_EXIST,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, &format!("permission-dlq-{index}")).await;
        let (withheld_arn, withheld_url) =
            given_withheld_target(&emulator, index, &row.setup).await;
        // The sibling target of the same rule, whose own policy DOES grant the send:
        // one publication, two queues, and only the one that granted it receives the
        // event.
        let (granted_url, granted_arn) = given_queue_with_policy(
            &emulator,
            &format!("permission-sibling-{index}"),
            Some(&granting_events),
        )
        .await;
        given_rule(
            &emulator,
            RULE,
            json!([
                {
                    "Id": WITHHELD_TARGET_ID,
                    "Arn": withheld_arn,
                    "DeadLetterConfig": { "Arn": dead_letter_arn },
                    "RetryPolicy": { "MaximumRetryAttempts": 0, "MaximumEventAgeInSeconds": 60 },
                },
                {
                    "Id": GRANTED_TARGET_ID,
                    "Arn": granted_arn,
                },
            ]),
        )
        .await;

        publish(&emulator).await;

        // The sibling that granted the send received the whole envelope: the rule
        // matched and the delivery path ran.
        let delivered = drain(&emulator, &granted_url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: expected the granting queue to receive the event, got {delivered:?}"
        );
        if let Some(url) = &withheld_url {
            let leaked = drain(&emulator, url).await;
            assert!(
                leaked.is_empty(),
                "case {case:?}: a queue that did not authorize the send receives nothing, got \
                 {leaked:?}"
            );
        }

        let parked = drain(&emulator, &dead_letter_url).await;
        assert_eq!(
            parked.len(),
            1,
            "case {case:?}: expected exactly one dead letter, got {parked:?}"
        );
        let message = &parked[0];
        let class = attribute(message, ERROR_CODE_ATTRIBUTE);
        assert_eq!(
            class, row.class,
            "case {case:?}: expected the dead-letter {ERROR_CODE_ATTRIBUTE} {:?}, got {class:?} \
             in {message:?}",
            row.class
        );
        // The answer beside the class is the one the refused send read, and the class is
        // the class of the code INSIDE it, so the two cannot name two failures.
        let reported = attribute(message, ERROR_MESSAGE_ATTRIBUTE);
        assert!(
            reported.contains(row.code),
            "case {case:?}: expected the answering service's code {:?} in {reported:?}",
            row.code
        );
        if row.class == NO_PERMISSIONS {
            // An authorization refusal names what was refused: the principal the call
            // was made as, the action it asked for and the queue it asked for.
            let expected = format!(
                "User: {SERVICE_PRINCIPAL} is not authorized to perform: {SEND_MESSAGE} on \
                 resource: {withheld_arn} because no resource-based policy allows the \
                 {SEND_MESSAGE} action (Service: AmazonSQS; Status Code: \
                 {ACCESS_DENIED_STATUS}; Error Code: {ACCESS_DENIED}; Request ID: "
            );
            assert!(
                reported.starts_with(expected.as_str()) && reported.ends_with("; Proxy: null)"),
                "case {case:?}: expected {expected:?} then the id of the refused call, got \
                 {reported:?}"
            );
        }
        // The class names neither the rule nor the target, so the letter names both.
        assert_eq!(
            attribute(message, RULE_ARN_ATTRIBUTE),
            rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, RULE),
            "case {case:?}: expected the dead letter to name the rule, got {message:?}"
        );
        assert_eq!(
            attribute(message, TARGET_ARN_ATTRIBUTE),
            withheld_arn,
            "case {case:?}: expected the dead letter to name the target, got {message:?}"
        );
    }
}

/// The class follows the DOCUMENT in force at the instant of the delivery: the very
/// queue a withheld delivery was dead-lettered for receives the next event once its
/// policy grants the send, with nothing else changed — which is what makes the
/// permission class the one an operator acts on.
#[tokio::test]
async fn the_permission_class_clears_when_the_queues_policy_grants_the_send() {
    let case = "a target queue granted the send after a withheld delivery";
    let emulator = emulator();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, "regranted-dlq").await;
    let (target_url, target_arn) =
        given_queue_with_policy(&emulator, "regranted-target", None).await;
    given_rule(
        &emulator,
        RULE,
        json!([{
            "Id": WITHHELD_TARGET_ID,
            "Arn": target_arn,
            "DeadLetterConfig": { "Arn": dead_letter_arn },
            "RetryPolicy": { "MaximumRetryAttempts": 0, "MaximumEventAgeInSeconds": 60 },
        }]),
    )
    .await;

    publish(&emulator).await;

    let parked = drain(&emulator, &dead_letter_url).await;
    assert_eq!(
        parked.len(),
        1,
        "case {case:?}: expected one dead letter before the grant, got {parked:?}"
    );
    let class = attribute(&parked[0], ERROR_CODE_ATTRIBUTE);
    assert_eq!(
        class, NO_PERMISSIONS,
        "case {case:?}: expected the dead-letter {ERROR_CODE_ATTRIBUTE} {NO_PERMISSIONS:?}, got \
         {class:?} in {:?}",
        parked[0]
    );

    // Nothing but the queue's own document changes.
    let (status, body) = crate::target_dispatch::sqs(
        &emulator,
        "SetQueueAttributes",
        &json!({ "QueueUrl": target_url, "Attributes": { "Policy": granting_events(&target_arn) } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {body}"
    );

    publish(&emulator).await;

    let delivered = drain(&emulator, &target_url).await;
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: expected the regranted queue to receive the next event, got {delivered:?}"
    );
    let parked = drain(&emulator, &dead_letter_url).await;
    assert!(
        parked.is_empty(),
        "case {case:?}: a delivery the policy now grants parks nothing, got {parked:?}"
    );
}
