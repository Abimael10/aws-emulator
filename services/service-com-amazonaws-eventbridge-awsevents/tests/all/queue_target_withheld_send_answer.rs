// SPDX-License-Identifier: CC0-1.0

//! WHAT a dead letter reports for a delivery the target queue's own resource policy
//! withheld, end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter` with
//! Amazon SQS on the same engine — so each row is read where a client reads one: off
//! the message attributes of the dead letter the target's `DeadLetterConfig` queue
//! received.
//!
//! The RULE asserted here: what EventBridge reports for a failed delivery is the
//! answer the TARGET's service gave it — the sentence that service worded, inside
//! the metadata block naming that service, the status and the error code it answered
//! with — and the `ERROR_CODE` beside it is this service's classification of that
//! code. Amazon SQS declares no authorization failure on `SendMessage` at all
//! (`smithy_query(query="operation", operation="SendMessage",
//! model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`
//! lists `InvalidAddress`, `InvalidMessageContents`, `InvalidSecurity`, the `Kms*`
//! ones, `QueueDoesNotExist`, `RequestThrottled` and `UnsupportedOperation`) and does
//! not disclose a queue to a caller which may not write to it: a send its queue's
//! policy does not authorize is answered under its not-found class
//! (`QueueDoesNotExist`, `awsQueryError` code
//! `AWS.SimpleQueueService.NonExistentQueue` at HTTP 400), whose sentence names both
//! facts that class covers. So the dead letter carries that sentence and that code's
//! class (`NO_RESOURCE`), and never a 403 `AccessDenied` sentence worded in that
//! service's name for an error it does not model.
//!
//! The first table spans the class rather than the one case the defect was found on:
//! its rows differ along the FORM the withholding takes (no document at all, a
//! document with no statement in it, an explicit `Deny` beside the grant, a grant
//! scoped by `aws:SourceArn` to another rule, a grant of another action of the same
//! service, a grant naming another queue as its resource, a grant to another service
//! principal) and include a target queue that is NOT there, which earns the same
//! class — because that is the same class that service answers — while carrying ITS
//! OWN sentence rather than this one.
//!
//! The second test pins what in the reported answer is per-call and what is fixed:
//! two refused deliveries carry two request ids, and nothing of the request, the
//! queue or the calling principal appears in the sentence.
//!
//! The refusal of an ENCRYPTED queue's key is the other class and keeps its own
//! answer (`KMS.AccessDeniedException` under `NO_PERMISSIONS`); that is
//! `tests/all/encrypted_queue_target_key_policy.rs`'s rule and is not re-asserted
//! here.
//!
//! The harness (the router with EventBridge, Amazon SQS, Step Functions, IAM and
//! CloudWatch on one engine) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    rule_arn, DEFAULT_EVENT_BUS_NAME, NO_RESOURCE, SERVICE_PRINCIPAL,
};

use crate::target_dispatch::{
    drain, emulator, given_queue, given_queue_with_policy, given_rule, publish, Emulator, REGION,
};

/// The rule every delivery below is made by.
const RULE: &str = "withheld-send";

/// The id the one target of that rule carries.
const TARGET_ID: &str = "queue";

/// The IAM action a delivery to a queue target performs on that queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The dead-letter message attributes this suite reads.
const ERROR_CODE_ATTRIBUTE: &str = "ERROR_CODE";
const ERROR_MESSAGE_ATTRIBUTE: &str = "ERROR_MESSAGE";

/// The fixed text the reported answer opens with, up to the id of the refused call:
/// Amazon SQS's own sentence for a queue it will not disclose, and the metadata block
/// naming that service, the status and the error code it answered with.
const ANSWER_HEAD: &str = "The specified queue does not exist or you do not have access to it. \
                           (Service: AmazonSQS; Status Code: 400; Error Code: \
                           AWS.SimpleQueueService.NonExistentQueue; Request ID: ";

/// What closes that block, after the id of the refused call.
const ANSWER_TAIL: &str = "; Proxy: null)";

/// The sentence Amazon SQS answers for a queue that is not there at all, which a
/// target naming one earns INSTEAD of the one above — the clause that covers access
/// is what tells the two apart.
const ABSENCE_CLAUSE: &str = "The specified queue does not exist";
const DISCLOSURE_CLAUSE: &str = "you do not have access to it";

/// The account every resource below belongs to.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// A statement list wrapped as a policy document.
fn document(statements: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// An `Allow` of `action` to the service principal `principal` on `resource`, scoped
/// by `condition` when the row states one.
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

/// A document carrying no statement at all: it grants nothing, which is IAM's
/// implicit deny.
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

/// A grant of the send scoped by `aws:SourceArn` to the deliveries of a rule other
/// than the one publishing here.
fn granting_another_rule(queue_arn: &str) -> String {
    let other = rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, "somebody-elses-rule");
    document(json!([grant(
        SERVICE_PRINCIPAL,
        SEND_MESSAGE,
        queue_arn,
        Some(json!({"ArnEquals": {"aws:SourceArn": other}})),
    )]))
}

/// A grant of another action of the same service, so the action the delivery performs
/// is not among the ones allowed.
fn granting_another_action(queue_arn: &str) -> String {
    document(json!([grant(
        SERVICE_PRINCIPAL,
        "sqs:ReceiveMessage",
        queue_arn,
        None
    )]))
}

/// A grant of the send on ANOTHER queue, so no statement is about the queue the
/// delivery is made on.
fn granting_another_resource(queue_arn: &str) -> String {
    let other = format!("{queue_arn}-unrelated");
    document(json!([grant(SERVICE_PRINCIPAL, SEND_MESSAGE, &other, None)]))
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

/// How a row brings the queue its target names about.
enum Setup {
    /// A queue carrying the document the row's function builds from its own ARN, or
    /// none at all when the function is `None`.
    Queue(Option<fn(&str) -> String>),
    /// A queue ARN of this account and Region that no queue was ever created for.
    NeverCreated,
}

/// Which answer the row's delivery is to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The answer for a queue the delivery may not write to.
    NotDisclosed,
    /// The answer for a queue that is not there, which is the same CLASS and a
    /// different sentence.
    Absent,
}

/// One row: the case, how its target queue is brought about and the answer the dead
/// letter is to carry.
struct Row {
    case: &'static str,
    setup: Setup,
    answer: Answer,
}

/// The string value of the dead letter's message attribute `name`.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The ARN the row's target names, and the url of its queue when the row has one.
async fn given_target(emulator: &Emulator, index: usize, setup: &Setup) -> (String, Option<String>) {
    match setup {
        Setup::Queue(policy) => {
            let policy: Option<&dyn Fn(&str) -> String> = policy
                .as_ref()
                .map(|written| written as &dyn Fn(&str) -> String);
            let (url, arn) =
                given_queue_with_policy(emulator, &format!("withheld-target-{index}"), policy)
                    .await;
            (arn, Some(url))
        }
        Setup::NeverCreated => (
            format!("arn:aws:sqs:{REGION}:{}:never-created-{index}", account()),
            None,
        ),
    }
}

/// The one dead letter a row's refused delivery parked, with its attributes.
async fn one_dead_letter(emulator: &Emulator, case: &str, url: &str) -> Value {
    let parked = drain(emulator, url).await;
    assert_eq!(
        parked.len(),
        1,
        "case {case:?}: expected exactly one dead letter, got {parked:?}"
    );
    parked[0].clone()
}

/// A withheld delivery reports the answer Amazon SQS gave it, over every form the
/// withholding takes — and a target that is not there earns the same class with that
/// service's own sentence for an absence.
#[tokio::test]
async fn a_withheld_queue_delivery_reports_the_answer_that_service_gave() {
    let rows = [
        Row {
            case: "a queue carrying no policy at all",
            setup: Setup::Queue(None),
            answer: Answer::NotDisclosed,
        },
        Row {
            case: "a queue whose document states no statement",
            setup: Setup::Queue(Some(no_statements)),
            answer: Answer::NotDisclosed,
        },
        Row {
            case: "a queue whose policy denies the send beside granting it",
            setup: Setup::Queue(Some(granting_then_denying)),
            answer: Answer::NotDisclosed,
        },
        Row {
            case: "a queue whose grant is scoped by aws:SourceArn to another rule",
            setup: Setup::Queue(Some(granting_another_rule)),
            answer: Answer::NotDisclosed,
        },
        Row {
            case: "a queue granting this service another action",
            setup: Setup::Queue(Some(granting_another_action)),
            answer: Answer::NotDisclosed,
        },
        Row {
            case: "a queue whose grant names another queue as its resource",
            setup: Setup::Queue(Some(granting_another_resource)),
            answer: Answer::NotDisclosed,
        },
        Row {
            case: "a queue granting the send to another service principal",
            setup: Setup::Queue(Some(granting_another_principal)),
            answer: Answer::NotDisclosed,
        },
        Row {
            case: "a queue ARN no queue was created for",
            setup: Setup::NeverCreated,
            answer: Answer::Absent,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, &format!("withheld-dlq-{index}")).await;
        let (target_arn, target_url) = given_target(&emulator, index, &row.setup).await;
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
        let parked = one_dead_letter(&emulator, case, &dead_letter_url).await;
        let class = attribute(&parked, ERROR_CODE_ATTRIBUTE);
        assert_eq!(
            class, NO_RESOURCE,
            "case {case:?}: expected the dead-letter {ERROR_CODE_ATTRIBUTE} {NO_RESOURCE:?}, got \
             {class:?} in {parked:?}"
        );
        let reported = attribute(&parked, ERROR_MESSAGE_ATTRIBUTE);
        match row.answer {
            Answer::NotDisclosed => {
                assert!(
                    reported.starts_with(ANSWER_HEAD) && reported.ends_with(ANSWER_TAIL),
                    "case {case:?}: expected {ANSWER_HEAD:?} then the id of the refused call and \
                     {ANSWER_TAIL:?}, got {reported:?}"
                );
                let request_id = reported
                    .strip_prefix(ANSWER_HEAD)
                    .and_then(|rest| rest.strip_suffix(ANSWER_TAIL))
                    .unwrap_or_default();
                // An AWS request id is a UUID in the canonical 8-4-4-4-12 form.
                let widths: Vec<usize> = request_id.split('-').map(str::len).collect();
                assert_eq!(
                    widths,
                    vec![8, 4, 4, 4, 12],
                    "case {case:?}: expected a request id in the 8-4-4-4-12 form, got \
                     {request_id:?}"
                );
            }
            Answer::Absent => {
                assert!(
                    reported.contains(ABSENCE_CLAUSE) && !reported.contains(DISCLOSURE_CLAUSE),
                    "case {case:?}: expected that service's own sentence for an absent queue, \
                     got {reported:?}"
                );
            }
        }
        // Whichever form it took, the reported answer is that service's own: it names
        // no status and no code of a refusal Amazon SQS does not answer a send with.
        for absent in ["AccessDenied", "Status Code: 403"] {
            assert!(
                !reported.contains(absent),
                "case {case:?}: expected no {absent:?} in the reported answer, got {reported:?}"
            );
        }
    }
}

/// What varies in the reported answer and what does not: the id belongs to the ONE
/// refused call, so two refused deliveries report two ids — and the sentence carries
/// nothing of the request, so neither the queue it was refused on nor the principal
/// it was refused for appears in it. Those are the dead letter's own `TARGET_ARN` and
/// `RULE_ARN` attributes instead.
#[tokio::test]
async fn the_reported_answer_states_one_call_and_nothing_of_the_request() {
    let case = "two events withheld by the same queue";
    let emulator = emulator();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, "per-call-dlq").await;
    let (_target_url, target_arn) =
        given_queue_with_policy(&emulator, "per-call-target", None).await;
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
    let first = attribute(
        &one_dead_letter(&emulator, case, &dead_letter_url).await,
        ERROR_MESSAGE_ATTRIBUTE,
    );
    publish(&emulator).await;
    let second = attribute(
        &one_dead_letter(&emulator, case, &dead_letter_url).await,
        ERROR_MESSAGE_ATTRIBUTE,
    );

    assert_ne!(
        first, second,
        "case {case:?}: expected each refused call its own request id, got {first:?} twice"
    );
    for reported in [&first, &second] {
        assert!(
            reported.starts_with(ANSWER_HEAD) && reported.ends_with(ANSWER_TAIL),
            "case {case:?}: expected the answer that service gave, got {reported:?}"
        );
        for echoed in [target_arn.as_str(), SERVICE_PRINCIPAL] {
            assert!(
                !reported.contains(echoed),
                "case {case:?}: expected no {echoed:?} in the reported answer, got {reported:?}"
            );
        }
    }
}
