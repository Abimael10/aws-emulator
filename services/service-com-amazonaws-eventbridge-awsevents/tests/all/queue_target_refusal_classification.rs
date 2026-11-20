// SPDX-License-Identifier: CC0-1.0

//! WHICH class a failed delivery to an SQS target falls in, end to end over the
//! wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through
//! the real `MultiServiceRouter` with Amazon SQS on the same engine — so each row's
//! class is read where a client reads one: off the `ERROR_CODE` message attribute of
//! the dead letter the target's `DeadLetterConfig` queue received.
//!
//! The RULE asserted here: EventBridge assumes no role for an SQS target, so the
//! destination queue's own resource policy is what authorizes the delivery
//! (`PutTargets`: "for Lambda functions, Amazon SNS topics and Amazon SQS queues,
//! EventBridge relies on resource-based policies"), and the dead letter's
//! `ERROR_CODE` is EventBridge's classification OF THE ANSWER the target's service
//! gave — never a verdict this service reaches on its own. A queue that IS there and
//! refuses the send answers that service's own authorization refusal (HTTP 403 under
//! `AccessDenied`, the pair its gate answers a call the documents in force do not
//! permit with —
//! `service_com_amazonaws_sqs_amazonsqs::state::authorization::denied`), so every
//! form of withholding lands in the permission class (`NO_PERMISSIONS`); the
//! resource class (`NO_RESOURCE`) belongs to a queue that is not there at all, whose
//! not-found answer the delivery reads from `SendMessage` itself.
//!
//! The table spans the class rather than one case: the rows differ along the FORM
//! the withholding takes (no document at all, an explicit `Deny`, a grant scoped by
//! `aws:SourceArn` to another rule, a grant of another action, of another resource,
//! to another principal) and along the form a target that is not there takes (an ARN
//! no queue was created for, and a queue deleted after the target was written). Each
//! row is a FRESH emulator, so one row's document never decides another row's
//! delivery.
//!
//! The condition operators and keys the rows use are IAM's own, because a queue's
//! `Policy` is an IAM policy document
//! (`smithy_query(query="doc",shape="com.amazonaws.sqs#QueueAttributeName")`).
//!
//! The last test closes the loop the classification claims: the very queue a
//! withheld delivery was dead-lettered for receives the NEXT event once its policy
//! grants the send, with nothing else changed — so the queue was there all along and
//! only the authorization had failed, which is exactly what the class beside it
//! says.
//!
//! The harness (the router with EventBridge, Amazon SQS, Step Functions, IAM and
//! CloudWatch on one engine) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    rule_arn, DEFAULT_EVENT_BUS_NAME, NO_PERMISSIONS, NO_RESOURCE, SERVICE_PRINCIPAL,
};

use crate::target_dispatch::{
    drain, emulator, given_queue, given_queue_with_policy, given_rule, granting_events, publish,
    sqs, Emulator, REGION,
};

/// The rule every row publishes through.
const RULE: &str = "classified-queue-delivery";

/// The id the one target of that rule carries.
const TARGET_ID: &str = "queue";

/// The IAM action a delivery to a queue target performs on that queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The dead-letter message attributes this suite reads.
const ERROR_CODE_ATTRIBUTE: &str = "ERROR_CODE";
const ERROR_MESSAGE_ATTRIBUTE: &str = "ERROR_MESSAGE";
const RULE_ARN_ATTRIBUTE: &str = "RULE_ARN";
const TARGET_ARN_ATTRIBUTE: &str = "TARGET_ARN";

/// The account every resource below belongs to.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// A statement list wrapped as a policy document.
fn document(statements: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// An `Allow` of `action` to `principal` on `resource`, scoped by `condition` when
/// the row states one.
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

/// The ARN of a rule of this account and Region that is NOT the one publishing here,
/// which a row can scope its grant to so the grant does not reach this delivery.
fn another_rule_arn() -> String {
    rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, "somebody-elses-rule")
}

/// How a row brings its target queue into the state the row needs.
enum Setup {
    /// A queue carrying the document the row's function builds from its own ARN, or
    /// none at all when the function is `None`.
    Queue(Option<fn(&str) -> String>),
    /// A queue ARN of this account and Region that no queue was ever created for.
    NeverCreated,
    /// A queue created, named by the target, and then deleted.
    Deleted,
}

/// One row: the case, how its target queue is brought about and the class the dead
/// letter is to report.
struct Row {
    case: &'static str,
    setup: Setup,
    class: &'static str,
}

/// A document stating nothing about this service: the send is granted to another
/// service principal only.
fn granting_another_principal(queue_arn: &str) -> String {
    document(json!([grant(
        "s3.amazonaws.com",
        SEND_MESSAGE,
        queue_arn,
        None
    )]))
}

/// A document granting this service a DIFFERENT action on the queue, so the action
/// the delivery performs is not among the ones allowed.
fn granting_another_action(queue_arn: &str) -> String {
    document(json!([grant(
        SERVICE_PRINCIPAL,
        "sqs:ReceiveMessage",
        queue_arn,
        None
    )]))
}

/// A document granting this service the send on ANOTHER queue, so no statement is
/// about the queue the delivery is made on.
fn granting_another_resource(queue_arn: &str) -> String {
    let other = format!("{queue_arn}-unrelated");
    document(json!([grant(SERVICE_PRINCIPAL, SEND_MESSAGE, &other, None)]))
}

/// A document granting this service the send, scoped by `aws:SourceArn` to the
/// deliveries of a rule other than the one publishing.
fn granting_another_rule(queue_arn: &str) -> String {
    document(json!([grant(
        SERVICE_PRINCIPAL,
        SEND_MESSAGE,
        queue_arn,
        Some(json!({"ArnEquals": {"aws:SourceArn": another_rule_arn()}})),
    )]))
}

/// A document granting this service the send and DENYING it beside the grant, which
/// refuses the delivery whatever is granted.
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

/// The ARN the row's target names, with whatever the row's state needs done to it
/// already done, and the queue's url when the row has one.
async fn given_target(
    emulator: &Emulator,
    case: &str,
    index: usize,
    setup: &Setup,
) -> (String, Option<String>) {
    let name = format!("classified-target-{index}");
    match setup {
        Setup::Queue(policy) => {
            let policy: Option<&dyn Fn(&str) -> String> =
                policy.as_ref().map(|written| written as &dyn Fn(&str) -> String);
            let (url, arn) = given_queue_with_policy(emulator, &name, policy).await;
            (arn, Some(url))
        }
        Setup::NeverCreated => (
            format!("arn:aws:sqs:{REGION}:{}:never-created-{index}", account()),
            None,
        ),
        Setup::Deleted => {
            let (url, arn) = given_queue(emulator, &name).await;
            let (status, body) = sqs(emulator, "DeleteQueue", &json!({ "QueueUrl": url })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteQueue answered {status} {body}"
            );
            (arn, None)
        }
    }
}

/// The string value of the dead letter's message attribute `name`.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The dead-letter `ERROR_CODE` is the class the target service's OWN answer puts
/// the failure in, over every form a withheld delivery and an absent target take.
#[tokio::test]
async fn a_queue_targets_failure_class_is_the_one_its_own_state_puts_it_in() {
    let rows = [
        Row {
            case: "a queue carrying no policy at all",
            setup: Setup::Queue(None),
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue whose policy denies the send beside granting it",
            setup: Setup::Queue(Some(granting_then_denying)),
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue whose grant is scoped by aws:SourceArn to another rule",
            setup: Setup::Queue(Some(granting_another_rule)),
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue granting this service another action",
            setup: Setup::Queue(Some(granting_another_action)),
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue whose grant names another queue as its resource",
            setup: Setup::Queue(Some(granting_another_resource)),
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue granting the send to another service principal",
            setup: Setup::Queue(Some(granting_another_principal)),
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue ARN no queue was created for",
            setup: Setup::NeverCreated,
            class: NO_RESOURCE,
        },
        Row {
            case: "a queue deleted after the target was written",
            setup: Setup::Deleted,
            class: NO_RESOURCE,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, &format!("classified-dlq-{index}")).await;
        let (target_arn, target_url) = given_target(&emulator, case, index, &row.setup).await;
        given_rule(
            &emulator,
            RULE,
            json!([{
                "Id": TARGET_ID,
                "Arn": target_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "RetryPolicy": { "MaximumRetryAttempts": 0, "MaximumEventAgeInSeconds": 60 },
            }]),
        )
        .await;

        publish(&emulator).await;

        if let Some(url) = &target_url {
            let delivered = drain(&emulator, url).await;
            assert!(
                delivered.is_empty(),
                "case {case:?}: a withheld delivery leaves the target queue empty, got \
                 {delivered:?}"
            );
        }
        let dead_lettered = drain(&emulator, &dead_letter_url).await;
        assert_eq!(
            dead_lettered.len(),
            1,
            "case {case:?}: expected exactly one dead letter, got {dead_lettered:?}"
        );
        let message = &dead_lettered[0];
        let code = attribute(message, ERROR_CODE_ATTRIBUTE);
        assert_eq!(
            code, row.class,
            "case {case:?}: expected the dead-letter {ERROR_CODE_ATTRIBUTE} {:?}, got {code:?} \
             in {message:?}",
            row.class
        );
        // The class says nothing about the target the failure was on, so the dead
        // letter names both the rule and the target beside it, and reports the
        // answer the delivery read in full.
        assert_eq!(
            attribute(message, RULE_ARN_ATTRIBUTE),
            rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, RULE),
            "case {case:?}: expected the dead letter to name the rule, got {message:?}"
        );
        assert_eq!(
            attribute(message, TARGET_ARN_ATTRIBUTE),
            target_arn,
            "case {case:?}: expected the dead letter to name the target, got {message:?}"
        );
        assert!(
            !attribute(message, ERROR_MESSAGE_ATTRIBUTE).is_empty(),
            "case {case:?}: expected the target's own answer as the \
             {ERROR_MESSAGE_ATTRIBUTE}, got {message:?}"
        );
    }
}

/// The queue a withheld delivery was dead-lettered for IS there: the same queue,
/// with nothing changed but its policy, receives the next event — which is why the
/// class beside it names the permission rather than claiming the queue is gone, and
/// what an operator reading the letter has to repair.
#[tokio::test]
async fn the_queue_a_withheld_delivery_names_receives_once_its_policy_grants_the_send() {
    let case = "a target queue granted the send after a withheld delivery";
    let emulator = emulator();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, "regranted-dlq").await;
    let (target_url, target_arn) =
        given_queue_with_policy(&emulator, "regranted-target", None).await;
    given_rule(
        &emulator,
        RULE,
        json!([{
            "Id": TARGET_ID,
            "Arn": target_arn,
            "DeadLetterConfig": { "Arn": dead_letter_arn },
            "RetryPolicy": { "MaximumRetryAttempts": 0, "MaximumEventAgeInSeconds": 60 },
        }]),
    )
    .await;

    publish(&emulator).await;

    let dead_lettered = drain(&emulator, &dead_letter_url).await;
    assert_eq!(
        dead_lettered.len(),
        1,
        "case {case:?}: expected one dead letter before the grant, got {dead_lettered:?}"
    );
    let code = attribute(&dead_lettered[0], ERROR_CODE_ATTRIBUTE);
    assert_eq!(
        code, NO_PERMISSIONS,
        "case {case:?}: expected the dead-letter {ERROR_CODE_ATTRIBUTE} {NO_PERMISSIONS:?}, got \
         {code:?} in {:?}",
        dead_lettered[0]
    );

    let (status, body) = sqs(
        &emulator,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": target_url,
            "Attributes": { "Policy": granting_events(&target_arn) },
        }),
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
        "case {case:?}: the granted queue receives the next event, got {delivered:?}"
    );
    let dead_lettered = drain(&emulator, &dead_letter_url).await;
    assert!(
        dead_lettered.is_empty(),
        "case {case:?}: a delivery that went through is not dead-lettered, got {dead_lettered:?}"
    );
}
