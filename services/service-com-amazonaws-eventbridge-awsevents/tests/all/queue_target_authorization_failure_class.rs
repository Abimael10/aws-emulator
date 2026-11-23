// SPDX-License-Identifier: CC0-1.0

//! WHICH class a dead letter carries for a delivery the target queue's own resource
//! policy refused, and that the queue it refused is still THERE — end to end over the
//! wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through
//! the real `MultiServiceRouter` with Amazon SQS on the same engine, so each row is
//! read where a client reads one: off the message attributes of the dead letter the
//! target's `DeadLetterConfig` queue received.
//!
//! The RULE asserted here: a delivery a target's service refused because the
//! documents in force over that target do not authorize the call is reported in the
//! AUTHORIZATION class (`NO_PERMISSIONS`), carrying that service's own refusal of the
//! call — the code and status AWS answers an unauthorized call with, naming the
//! principal the call was made as, the action it asked for and the resource it asked
//! for; the RESOURCE class (`NO_RESOURCE`) is reserved for a target the delivery
//! could not address at all, which that service answers under its own not-found code.
//! For a queue target the authorizing document is the queue's own `Policy` alone
//! ("for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge
//! relies on resource-based policies", `PutTargets`), and a grant scoped by
//! `aws:SourceArn` authorizes the deliveries of the rule it names and no other. The
//! class and the sentence come from the ONE refusal, so they can never name two
//! different failures.
//!
//! `ERROR_CODE`/`ERROR_MESSAGE` are message attributes EventBridge adds to a
//! dead-letter message and no shape models (`Target$DeadLetterConfig` is documented
//! only as the queue dead-letter events are sent to:
//! `smithy_query(query="shape", shape="DeadLetterConfig")`), so the tokens are
//! EventBridge's own fixed vocabulary, which is why no part of either is taken from
//! the request.
//!
//! The first test is the two-rule case the class exists to tell apart: one bus, two
//! rules matching the same event, and two queues — one whose policy names its own
//! rule's `aws:SourceArn`, one whose policy names only the OTHER rule's. The
//! correctly scoped pair receives the event while the mis-scoped delivery is
//! dead-lettered in the permission class, so a class flipped for every failed
//! delivery, or one reached without reading the grant's scope, fails here.
//!
//! The second test spans the class over a TABLE whose rows differ along the FORM the
//! withholding takes (no document at all, a grant scoped to another rule, a grant to
//! another service principal, a grant of another action of the same service) and
//! includes a target queue that is NOT there, which is the other class. Every
//! repairable row then has the grant written and the SAME queue receives the next
//! event, with nothing else changed — which is what makes the class the right one: the
//! queue existed throughout and only the authorization had failed.
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

/// The id the one target of every rule below carries.
const TARGET_ID: &str = "queue";

/// The IAM action a delivery to a queue target performs on that queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The dead-letter message attributes this suite reads.
const ERROR_CODE_ATTRIBUTE: &str = "ERROR_CODE";
const ERROR_MESSAGE_ATTRIBUTE: &str = "ERROR_MESSAGE";
const RULE_ARN_ATTRIBUTE: &str = "RULE_ARN";
const TARGET_ARN_ATTRIBUTE: &str = "TARGET_ARN";

/// The error code and status AWS answers a call no document in force over the
/// resource permits, which is what a withheld send reads back.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// Amazon SQS's own code, status and sentence for a send on a queue that is not
/// there: `QueueDoesNotExist`'s `awsQueryError` code
/// (`smithy_query(query="shape", shape="QueueDoesNotExist",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`).
const QUEUE_DOES_NOT_EXIST: &str = "AWS.SimpleQueueService.NonExistentQueue";
const QUEUE_DOES_NOT_EXIST_STATUS: u16 = 400;

/// What closes the metadata block of a relayed answer, after the id of the one
/// refused call.
const ANSWER_TAIL: &str = "; Proxy: null)";

/// The answer a send the queue's own documents withheld reads back, up to the id of
/// that one refused call: the refusal of the CALL, naming the principal, the action
/// and the queue — built here rather than read from the implementation's own helper,
/// so a wrong answer there cannot make this suite pass.
fn withheld_answer_head(queue_arn: &str) -> String {
    format!(
        "User: {SERVICE_PRINCIPAL} is not authorized to perform: {SEND_MESSAGE} on resource: \
         {queue_arn} because no resource-based policy allows the {SEND_MESSAGE} action (Service: \
         AmazonSQS; Status Code: {ACCESS_DENIED_STATUS}; Error Code: {ACCESS_DENIED}; Request ID: "
    )
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

/// A grant of the send to this service on `queue_arn`, scoped by `aws:SourceArn` to
/// the deliveries of the rule `rule` and no other — IAM's own condition key and
/// operator, a queue's `Policy` being an IAM policy document
/// (`smithy_query(query="doc", shape="com.amazonaws.sqs#QueueAttributeName")`).
fn granting_rule(queue_arn: &str, rule: &str) -> String {
    document(json!([grant(
        SERVICE_PRINCIPAL,
        SEND_MESSAGE,
        queue_arn,
        Some(json!({"ArnEquals": {"aws:SourceArn": rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, rule)}})),
    )]))
}

/// The rules the two-rule test publishes through: one whose queue names it, one whose
/// queue names the other.
const SCOPED_RULE: &str = "scoped-to-its-own-rule";
const MISSCOPED_RULE: &str = "scoped-to-the-other-rule";

/// A grant naming [`SCOPED_RULE`], which is the own rule of one queue and the OTHER
/// rule of the other.
fn granting_scoped_rule(queue_arn: &str) -> String {
    granting_rule(queue_arn, SCOPED_RULE)
}

/// The string value of the dead letter's message attribute `name`.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Assert the dead letter `message` reports the withheld send on `queue_arn` by the
/// rule `rule`: the permission class, and beside it that service's refusal of the one
/// call, whose id is the only part of the sentence the delivery did not state.
fn assert_withheld(case: &str, message: &Value, queue_arn: &str, rule: &str) {
    let class = attribute(message, ERROR_CODE_ATTRIBUTE);
    assert_eq!(
        class, NO_PERMISSIONS,
        "case {case:?}: expected the dead-letter {ERROR_CODE_ATTRIBUTE} {NO_PERMISSIONS:?}, got \
         {class:?} in {message:?}"
    );
    let head = withheld_answer_head(queue_arn);
    let reported = attribute(message, ERROR_MESSAGE_ATTRIBUTE);
    assert!(
        reported.starts_with(head.as_str()) && reported.ends_with(ANSWER_TAIL),
        "case {case:?}: expected {head:?} then the id of the refused call and {ANSWER_TAIL:?}, \
         got {reported:?}"
    );
    let request_id = reported
        .strip_prefix(head.as_str())
        .and_then(|rest| rest.strip_suffix(ANSWER_TAIL))
        .unwrap_or_default();
    // An AWS request id is a UUID in the canonical 8-4-4-4-12 form.
    let widths: Vec<usize> = request_id.split('-').map(str::len).collect();
    assert_eq!(
        widths,
        vec![8, 4, 4, 4, 12],
        "case {case:?}: expected a request id in the 8-4-4-4-12 form, got {request_id:?}"
    );
    // The class names neither the delivery nor the failure's own words, so the letter
    // names the rule and the target beside it.
    assert_eq!(
        attribute(message, RULE_ARN_ATTRIBUTE),
        rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, rule),
        "case {case:?}: expected the dead letter to name the rule, got {message:?}"
    );
    assert_eq!(
        attribute(message, TARGET_ARN_ATTRIBUTE),
        queue_arn,
        "case {case:?}: expected the dead letter to name the target, got {message:?}"
    );
}

/// Write `policy` on the queue at `url` as a client repairing the grant does.
async fn write_policy(emulator: &Emulator, case: &str, url: &str, policy: &str) {
    let (status, body) = sqs(
        emulator,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {body}"
    );
}

/// A grant is read FOR THE RULE being invoked: of two rules matching one event, the
/// one its target queue's `aws:SourceArn` names is delivered, and the one whose queue
/// names only the other rule is dead-lettered in the permission class.
#[tokio::test]
async fn a_grant_scoped_to_another_rule_dead_letters_in_the_permission_class() {
    let case = "two rules matching one event, one queue scoped to the other rule";
    let emulator = emulator();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, "scoped-dlq").await;
    // Both queues carry the SAME document, naming SCOPED_RULE's ARN: for one queue
    // that is its own rule, for the other it is somebody else's.
    let (scoped_url, scoped_arn) =
        given_queue_with_policy(&emulator, "scoped-target", Some(&granting_scoped_rule)).await;
    let (misscoped_url, misscoped_arn) =
        given_queue_with_policy(&emulator, "misscoped-target", Some(&granting_scoped_rule)).await;
    for (rule, target_arn) in [
        (SCOPED_RULE, &scoped_arn),
        (MISSCOPED_RULE, &misscoped_arn),
    ] {
        given_rule(
            &emulator,
            rule,
            json!([{
                "Id": TARGET_ID,
                "Arn": target_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "RetryPolicy": { "MaximumRetryAttempts": 0, "MaximumEventAgeInSeconds": 60 },
            }]),
        )
        .await;
    }

    publish(&emulator).await;

    // The positive control: the pair whose grant names its own rule delivers.
    let delivered = drain(&emulator, &scoped_url).await;
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: the queue whose grant names its own rule receives the event, got \
         {delivered:?}"
    );
    // The queue the grant does not reach holds nothing, and its delivery is the one
    // dead letter.
    let withheld = drain(&emulator, &misscoped_url).await;
    assert!(
        withheld.is_empty(),
        "case {case:?}: the queue the grant does not reach receives nothing, got {withheld:?}"
    );
    let parked = drain(&emulator, &dead_letter_url).await;
    assert_eq!(
        parked.len(),
        1,
        "case {case:?}: expected exactly one dead letter, got {parked:?}"
    );
    assert_withheld(case, &parked[0], &misscoped_arn, MISSCOPED_RULE);

    // The repair: granting THIS rule the send makes the same queue receive the next
    // event, so the queue was there and addressed correctly all along.
    write_policy(
        &emulator,
        case,
        &misscoped_url,
        &granting_rule(&misscoped_arn, MISSCOPED_RULE),
    )
    .await;
    publish(&emulator).await;
    let repaired = drain(&emulator, &misscoped_url).await;
    assert_eq!(
        repaired.len(),
        1,
        "case {case:?}: the queue receives the next event once the grant names its rule, got \
         {repaired:?}"
    );
    let parked = drain(&emulator, &dead_letter_url).await;
    assert!(
        parked.is_empty(),
        "case {case:?}: a delivery that went through is not dead-lettered, got {parked:?}"
    );
}

/// How a row brings the queue its target names about.
enum Setup {
    /// A queue carrying the document the row's function builds from its own ARN, or
    /// none at all when the function is `None`.
    Queue(Option<fn(&str) -> String>),
    /// A queue ARN of this account and Region that no queue was ever created for.
    NeverCreated,
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

/// A grant of another action of the same service.
fn granting_another_action(queue_arn: &str) -> String {
    document(json!([grant(
        SERVICE_PRINCIPAL,
        "sqs:ReceiveMessage",
        queue_arn,
        None
    )]))
}

/// A grant scoped by `aws:SourceArn` to a rule other than the one publishing.
fn granting_another_rule(queue_arn: &str) -> String {
    granting_rule(queue_arn, "somebody-elses-rule")
}

/// One row: the case, how its target queue is brought about, and whether writing the
/// grant afterwards is to make that same queue receive the next event.
struct Row {
    case: &'static str,
    setup: Setup,
    class: &'static str,
    repairable: bool,
}

/// The rule the table below publishes through.
const TABLE_RULE: &str = "withheld-then-granted";

/// Every form of withholding earns the permission class with that service's refusal
/// of the call, and the queue it refused receives the next event once the grant is
/// written — while a queue that is not THERE earns the resource class instead.
#[tokio::test]
async fn the_withholding_class_is_the_permission_one_and_the_queue_is_still_there() {
    let rows = [
        Row {
            case: "a queue carrying no policy at all",
            setup: Setup::Queue(None),
            class: NO_PERMISSIONS,
            repairable: true,
        },
        Row {
            case: "a queue whose grant is scoped by aws:SourceArn to another rule",
            setup: Setup::Queue(Some(granting_another_rule)),
            class: NO_PERMISSIONS,
            repairable: true,
        },
        Row {
            case: "a queue granting the send to another service principal",
            setup: Setup::Queue(Some(granting_another_principal)),
            class: NO_PERMISSIONS,
            repairable: true,
        },
        Row {
            case: "a queue granting this service another action",
            setup: Setup::Queue(Some(granting_another_action)),
            class: NO_PERMISSIONS,
            repairable: true,
        },
        Row {
            case: "a queue ARN no queue was created for",
            setup: Setup::NeverCreated,
            class: NO_RESOURCE,
            repairable: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, &format!("withheld-class-dlq-{index}")).await;
        let (target_url, target_arn) = match &row.setup {
            Setup::Queue(policy) => {
                let policy: Option<&dyn Fn(&str) -> String> = policy
                    .as_ref()
                    .map(|written| written as &dyn Fn(&str) -> String);
                let (url, arn) = given_queue_with_policy(
                    &emulator,
                    &format!("withheld-class-target-{index}"),
                    policy,
                )
                .await;
                (Some(url), arn)
            }
            Setup::NeverCreated => (
                None,
                format!(
                    "arn:aws:sqs:{REGION}:{}:withheld-class-absent-{index}",
                    ::engine::emulator_account_id()
                ),
            ),
        };
        given_rule(
            &emulator,
            TABLE_RULE,
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
                "case {case:?}: a refused delivery leaves the target queue empty, got \
                 {delivered:?}"
            );
        }
        let parked = drain(&emulator, &dead_letter_url).await;
        assert_eq!(
            parked.len(),
            1,
            "case {case:?}: expected exactly one dead letter, got {parked:?}"
        );
        let message = &parked[0];
        if row.class == NO_PERMISSIONS {
            assert_withheld(case, message, &target_arn, TABLE_RULE);
        } else {
            let class = attribute(message, ERROR_CODE_ATTRIBUTE);
            assert_eq!(
                class, NO_RESOURCE,
                "case {case:?}: expected the dead-letter {ERROR_CODE_ATTRIBUTE} \
                 {NO_RESOURCE:?}, got {class:?} in {message:?}"
            );
            // The sentence agrees with the class: that service's own not-found answer,
            // under its own code and status.
            let reported = attribute(message, ERROR_MESSAGE_ATTRIBUTE);
            for stated in [
                QUEUE_DOES_NOT_EXIST,
                &QUEUE_DOES_NOT_EXIST_STATUS.to_string(),
            ] {
                assert!(
                    reported.contains(stated),
                    "case {case:?}: expected the absent queue's own answer to state {stated:?}, \
                     got {reported:?}"
                );
            }
            assert!(
                !reported.contains(ACCESS_DENIED),
                "case {case:?}: a target that is not there reports no authorization refusal, got \
                 {reported:?}"
            );
        }

        if !row.repairable {
            continue;
        }
        let url = target_url
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: expected a queue to repair"));
        write_policy(&emulator, case, &url, &granting_events(&target_arn)).await;
        publish(&emulator).await;
        let delivered = drain(&emulator, &url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the same queue receives the next event once its policy grants the \
             send, got {delivered:?}"
        );
    }
}
