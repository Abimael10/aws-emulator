// SPDX-License-Identifier: CC0-1.0

//! A dead letter's `ERROR_CODE` and `ERROR_MESSAGE` come from ONE answer, end to end
//! over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`)
//! through the real `MultiServiceRouter` with Amazon SQS on the same engine — so the
//! pair is read where a client reads it: off the message attributes of the dead letter
//! the target's `DeadLetterConfig` queue received.
//!
//! The RULE asserted here: EventBridge classifies a failed delivery from the refusal
//! the target's own service answered, and the `ERROR_MESSAGE` beside the class is that
//! same answer — so the class is always the class of the error code INSIDE the
//! message, and the two can never name two different failures. For a queue target the
//! send is authorized by the queue's own resource policy alone ("for Lambda functions,
//! Amazon SNS topics and Amazon SQS queues, EventBridge relies on resource-based
//! policies", `PutTargets`), and that service answers:
//!
//! * a call the documents in force do not permit — the queue IS there and refused the
//!   principal — with its authorization refusal, HTTP 403 under `AccessDenied`, the
//!   pair its own gate answers such a call with
//!   (`service_com_amazonaws_sqs_amazonsqs::state::authorization::denied`), which is
//!   the permission class;
//! * a send on a queue that is NOT there with `QueueDoesNotExist`, HTTP 400 under
//!   `AWS.SimpleQueueService.NonExistentQueue`
//!   (`smithy_query(query="shape", shape="QueueDoesNotExist",
//!   model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`),
//!   which is the resource class.
//!
//! The table spans that rule rather than one case: its rows differ along the FORM a
//! refusal takes (no document at all, an explicit `Deny`, a grant scoped by
//! `aws:SourceArn` to another rule, one scoped by `aws:SourceAccount` to another
//! account, a grant of another action, a grant to another service principal) and
//! along the FORM an absence takes (an ARN no queue was created for, a queue deleted
//! after the target was written). The expected class of each row is DERIVED from the
//! error code the row's message carries, through the one table below, so a row whose
//! two attributes disagreed would fail whichever of them was wrong — and the row also
//! states the class it expects, so the derivation cannot pass vacuously.
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
    drain, emulator, given_queue, given_queue_with_policy, given_rule, publish, sqs, Emulator,
    REGION,
};

/// The rule every row below publishes through.
const RULE: &str = "paired-refusal";

/// The id the one target of that rule carries.
const TARGET_ID: &str = "queue";

/// The IAM action a delivery to a queue target performs on that queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The dead-letter message attributes this suite reads.
const ERROR_CODE_ATTRIBUTE: &str = "ERROR_CODE";
const ERROR_MESSAGE_ATTRIBUTE: &str = "ERROR_MESSAGE";
const RULE_ARN_ATTRIBUTE: &str = "RULE_ARN";
const TARGET_ARN_ATTRIBUTE: &str = "TARGET_ARN";

/// Amazon SQS's own error code for a call its documents do not permit, and the status
/// it answers under.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// Amazon SQS's own error code for a queue that is not there: `QueueDoesNotExist`'s
/// `awsQueryError` code.
const QUEUE_DOES_NOT_EXIST: &str = "AWS.SimpleQueueService.NonExistentQueue";

/// Which class each error code a queue target's service answers belongs to. Source:
/// the two refusals above, as that service publishes them.
const CLASS_OF_CODE: [(&str, &str); 2] = [
    (ACCESS_DENIED, NO_PERMISSIONS),
    (QUEUE_DOES_NOT_EXIST, NO_RESOURCE),
];

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

/// A document that denies the send outright, beside a grant of it.
fn denying(queue_arn: &str) -> String {
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

/// A grant scoped by `aws:SourceArn` to the deliveries of a rule other than the one
/// publishing here.
fn granting_another_rule(queue_arn: &str) -> String {
    let other = rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, "somebody-elses-rule");
    document(json!([grant(
        SERVICE_PRINCIPAL,
        SEND_MESSAGE,
        queue_arn,
        Some(json!({"ArnEquals": {"aws:SourceArn": other}})),
    )]))
}

/// A grant scoped by `aws:SourceAccount` to another account's rules.
fn granting_another_account(queue_arn: &str) -> String {
    document(json!([grant(
        SERVICE_PRINCIPAL,
        SEND_MESSAGE,
        queue_arn,
        Some(json!({"StringEquals": {"aws:SourceAccount": "111122223333"}})),
    )]))
}

/// A grant of another action of the same service, so no statement is about the call
/// the delivery makes.
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

/// How a row brings the queue its target names about.
enum Setup {
    /// A queue carrying the document the row's function builds from its own ARN, or
    /// none at all when the function is `None`.
    Queue(Option<fn(&str) -> String>),
    /// A queue ARN of this account and Region that no queue was ever created for.
    NeverCreated,
    /// A queue created, named by the target, and then deleted.
    Deleted,
}

/// One row: the case, how its target queue is brought about, and the answer that
/// state earns — the error code the target's service gives, and the class this
/// service puts that code in.
struct Row {
    case: &'static str,
    setup: Setup,
    code: &'static str,
    class: &'static str,
}

/// The string value of the dead letter's message attribute `name`.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The class [`CLASS_OF_CODE`] puts the error code named inside `reported` in — or
/// `None` where that message names no code the table knows, or more than one, in
/// which case the answer does not place the failure at all.
fn class_of(reported: &str) -> Option<&'static str> {
    let mut named = CLASS_OF_CODE
        .into_iter()
        .filter(|(code, _)| reported.contains(code));
    let found = named.next()?;
    if named.next().is_some() {
        return None;
    }
    Some(found.1)
}

/// The ARN the row's target names, and the url of its queue when the row has one.
async fn given_target(
    emulator: &Emulator,
    case: &str,
    index: usize,
    setup: &Setup,
) -> (String, Option<String>) {
    let name = format!("paired-target-{index}");
    match setup {
        Setup::Queue(policy) => {
            let policy: Option<&dyn Fn(&str) -> String> = policy
                .as_ref()
                .map(|written| written as &dyn Fn(&str) -> String);
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

/// Every form a failed delivery to a queue target takes reports the answer that
/// service gave, and the class beside it is the class of the code inside that answer.
#[tokio::test]
async fn a_dead_letters_class_and_message_come_from_one_answer() {
    let rows = [
        Row {
            case: "a queue carrying no policy at all",
            setup: Setup::Queue(None),
            code: ACCESS_DENIED,
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue whose policy denies the send beside granting it",
            setup: Setup::Queue(Some(denying)),
            code: ACCESS_DENIED,
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue whose grant is scoped by aws:SourceArn to another rule",
            setup: Setup::Queue(Some(granting_another_rule)),
            code: ACCESS_DENIED,
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue whose grant is scoped by aws:SourceAccount to another account",
            setup: Setup::Queue(Some(granting_another_account)),
            code: ACCESS_DENIED,
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue granting this service another action",
            setup: Setup::Queue(Some(granting_another_action)),
            code: ACCESS_DENIED,
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue granting the send to another service principal",
            setup: Setup::Queue(Some(granting_another_principal)),
            code: ACCESS_DENIED,
            class: NO_PERMISSIONS,
        },
        Row {
            case: "a queue ARN no queue was created for",
            setup: Setup::NeverCreated,
            code: QUEUE_DOES_NOT_EXIST,
            class: NO_RESOURCE,
        },
        Row {
            case: "a queue deleted after the target was written",
            setup: Setup::Deleted,
            code: QUEUE_DOES_NOT_EXIST,
            class: NO_RESOURCE,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, &format!("paired-dlq-{index}")).await;
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
                "case {case:?}: a failed delivery leaves the target queue empty, got {delivered:?}"
            );
        }
        let parked = drain(&emulator, &dead_letter_url).await;
        assert_eq!(
            parked.len(),
            1,
            "case {case:?}: expected exactly one dead letter, got {parked:?}"
        );
        let message = &parked[0];
        let reported = attribute(message, ERROR_MESSAGE_ATTRIBUTE);
        // The answer is the target service's own: the code that service answered
        // under, as the delivery read it.
        assert!(
            reported.contains(row.code),
            "case {case:?}: expected the answer that service gave to name {:?}, got {reported:?}",
            row.code
        );
        // The class is the class of the code inside that very answer, so the pair
        // cannot name two failures.
        let derived = class_of(&reported);
        let class = attribute(message, ERROR_CODE_ATTRIBUTE);
        assert_eq!(
            derived,
            Some(row.class),
            "case {case:?}: expected the answer's own code to be the {:?} class, got \
             {derived:?} from {reported:?}",
            row.class
        );
        assert_eq!(
            class,
            row.class,
            "case {case:?}: expected the dead-letter {ERROR_CODE_ATTRIBUTE} {:?}, got {class:?} \
             in {message:?}",
            row.class
        );
        // The class says nothing about which delivery failed, so the letter names the
        // rule and the target beside it.
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
    }
}

/// The permission-class answer names what WAS refused: the principal the send was
/// made as, the action it performs and the queue it was refused on — each the
/// delivery's own, so a second rule's delivery to the same queue reports that rule's
/// own refused call and a fresh id for it.
#[tokio::test]
async fn the_permission_class_answer_names_the_refused_call() {
    let case = "a queue granting this service nothing";
    let emulator = emulator();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, "named-dlq").await;
    let (target_url, target_arn) =
        given_queue_with_policy(&emulator, "named-target", None).await;
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
    publish(&emulator).await;

    let parked = drain(&emulator, &dead_letter_url).await;
    assert_eq!(
        parked.len(),
        2,
        "case {case:?}: expected one dead letter per refused delivery, got {parked:?}"
    );
    let head = format!(
        "User: {SERVICE_PRINCIPAL} is not authorized to perform: {SEND_MESSAGE} on resource: \
         {target_arn} because no resource-based policy allows the {SEND_MESSAGE} action (Service: \
         AmazonSQS; Status Code: {ACCESS_DENIED_STATUS}; Error Code: {ACCESS_DENIED}; Request ID: "
    );
    let mut ids = Vec::new();
    for message in &parked {
        let reported = attribute(message, ERROR_MESSAGE_ATTRIBUTE);
        assert!(
            reported.starts_with(head.as_str()) && reported.ends_with("; Proxy: null)"),
            "case {case:?}: expected {head:?} then the id of the refused call, got {reported:?}"
        );
        let id = reported
            .strip_prefix(head.as_str())
            .and_then(|rest| rest.strip_suffix("; Proxy: null)"))
            .unwrap_or_default()
            .to_string();
        // An AWS request id is a UUID in the canonical 8-4-4-4-12 form.
        let widths: Vec<usize> = id.split('-').map(str::len).collect();
        assert_eq!(
            widths,
            vec![8, 4, 4, 4, 12],
            "case {case:?}: expected a request id in the 8-4-4-4-12 form, got {id:?}"
        );
        ids.push(id);
    }
    assert_ne!(
        ids[0], ids[1],
        "case {case:?}: expected each refused call its own id, got {:?} twice",
        ids[0]
    );
    let delivered = drain(&emulator, &target_url).await;
    assert!(
        delivered.is_empty(),
        "case {case:?}: the queue that refused the send receives nothing, got {delivered:?}"
    );
}
