// SPDX-License-Identifier: CC0-1.0

//! A rule on a CUSTOM bus delivers the one entry a client PROBES the wiring with,
//! whichever admissible form the request names the bus in and whichever admissible
//! form the queue's own policy grants the send in — end to end over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the
//! real `MultiServiceRouter`, with Amazon SQS mounted beside EventBridge on one
//! engine so a delivery is asserted where a client observes one: on the queue.
//!
//! Each row is one such probe through one variant of the same setup.
//!
//! The RULE asserted here: `EventBusName` "admits either a bare name or an
//! event-bus ARN" — its modelled pattern spells both out
//! (`smithy_query(query="shape", shape="EventBusNameOrArn,NonPartnerEventBusNameOrArn")`)
//! — so a rule written, targeted and published to under either spelling is one
//! rule on one bus; and "for Lambda functions, Amazon SNS topics and Amazon SQS
//! queues, EventBridge relies on resource-based policies" (`PutTargets`), so what
//! decides the delivery is whether the queue's own `Policy` states an `Allow`
//! about this service's principal, this action and this queue — in whatever
//! admissible spelling, since a queue policy is an IAM document and IAM compares
//! an action name without regard to case, matches a resource wildcard, and decides
//! a grant scoped by `aws:SourceArn` for the rule being invoked.
//!
//! This is the path an entry-count check depends on before it can exercise the
//! bound (`tests/all/put_events_entry_count_limit.rs`): if any of these setups
//! withheld the delivery, a client could not observe a published entry at all.
//!
//! The rows span the class rather than one setup: they differ along how the grant
//! names the queue (the queue's ARN, a wildcard), how it spells the action (the
//! model's own casing, the casing the AWS console writes), whether it is scoped by
//! the invoking rule's ARN, when it was written (with the queue, or afterwards),
//! and how the request spells the bus (its name, its ARN).
//!
//! The harness (the router with EventBridge, Amazon SQS and the services a target
//! lives in on one engine) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};

use crate::target_dispatch::{drain, emulator, events, sqs, Emulator, DETAIL_TYPE, REGION, SOURCE};

/// The bus and the rule every row below publishes through.
const BUS: &str = "variant-bus";
const RULE: &str = "variant-rule";

/// The service principal EventBridge delivers to a queue target as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The account every resource below belongs to.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// The ARN of the rule this suite publishes through, as EventBridge mints one for a
/// rule on a bus that is not the default: `rule/<bus>/<name>`.
fn rule_arn() -> String {
    format!(
        "arn:aws:events:{REGION}:{}:rule/{BUS}/{RULE}",
        account()
    )
}

/// A statement list wrapped as a policy document.
fn document(statement: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// An `Allow` of `action` on `resource` to the EventBridge service principal,
/// scoped by `condition` where the row states one.
fn grant(resource: &str, action: &str, condition: Option<Value>) -> String {
    let mut statement = json!({
        "Sid": "AllowEventBridge",
        "Effect": "Allow",
        "Principal": { "Service": EVENTS_PRINCIPAL },
        "Action": action,
        "Resource": resource,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    document(statement)
}

/// A queue called `name`; answers its url and the ARN a rule names it by. The
/// policy is installed with the queue when `at_create` (built from the wildcard
/// resource, which is all a document written before the queue exists can name) and
/// written afterwards otherwise.
async fn given_queue_with(
    emulator: &Emulator,
    case: &str,
    name: &str,
    policy: &dyn Fn(&str) -> String,
    at_create: bool,
) -> (String, String) {
    let mut request = json!({ "QueueName": name });
    if at_create {
        request["Attributes"] = json!({ "Policy": policy("*") });
    }
    let (status, body) = sqs(emulator, "CreateQueue", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {body}"
    );
    let url = body["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {body}"))
        .to_string();
    let (status, body) = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {body}"
    );
    let arn = body["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {body}"))
        .to_string();
    if !at_create {
        let (status, body) = sqs(
            emulator,
            "SetQueueAttributes",
            &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy(&arn) } }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: SetQueueAttributes answered {status} {body}"
        );
    }
    (url, arn)
}

/// One row: the case, the document the queue carries (from its own ARN), whether it
/// is installed with the queue, and whether the request spells the bus as its ARN.
struct Row {
    case: &'static str,
    policy: fn(&str) -> String,
    at_create: bool,
    bus_as_arn: bool,
}

/// Every admissible setup of the same wiring delivers the probe.
#[tokio::test]
async fn every_admissible_setup_delivers_the_probe_to_the_rules_queue_target() {
    let rows = [
        Row {
            case: "a grant naming the queue and the modelled action casing",
            policy: |arn| grant(arn, "sqs:SendMessage", None),
            at_create: false,
            bus_as_arn: false,
        },
        Row {
            case: "a grant whose resource is a wildcard",
            policy: |_| grant("*", "sqs:SendMessage", None),
            at_create: false,
            bus_as_arn: false,
        },
        Row {
            case: "a grant written with the queue itself",
            policy: |_| grant("*", "sqs:SendMessage", None),
            at_create: true,
            bus_as_arn: false,
        },
        Row {
            case: "a grant spelling the action as the console writes it",
            policy: |arn| grant(arn, "SQS:SendMessage", None),
            at_create: false,
            bus_as_arn: false,
        },
        Row {
            case: "a grant scoped to the invoking rule's ARN",
            policy: |arn| {
                grant(
                    arn,
                    "sqs:SendMessage",
                    Some(json!({ "ArnEquals": { "aws:SourceArn": rule_arn() } })),
                )
            },
            at_create: false,
            bus_as_arn: false,
        },
        Row {
            case: "the bus named by its ARN throughout",
            policy: |arn| grant(arn, "sqs:SendMessage", None),
            at_create: false,
            bus_as_arn: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let (status, created) = events(&emulator, "CreateEventBus", &json!({ "Name": BUS })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateEventBus answered {status} {created}"
        );
        let bus_arn = created["EventBusArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no EventBusArn in {created}"))
            .to_string();
        let named_bus = if row.bus_as_arn {
            bus_arn
        } else {
            BUS.to_string()
        };

        let (url, queue_arn) = given_queue_with(
            &emulator,
            case,
            &format!("variant-queue-{index}"),
            &row.policy,
            row.at_create,
        )
        .await;
        let pattern = json!({ "source": [SOURCE] }).to_string();
        let (status, body) = events(
            &emulator,
            "PutRule",
            &json!({
                "Name": RULE,
                "EventBusName": &named_bus,
                "EventPattern": pattern,
                "State": "ENABLED",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutRule answered {status} {body}"
        );
        let (status, body) = events(
            &emulator,
            "PutTargets",
            &json!({
                "Rule": RULE,
                "EventBusName": &named_bus,
                "Targets": [{ "Id": "queue", "Arn": queue_arn }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {body}"
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case {case:?}: the queue target must be accepted, got {body}"
        );

        let (status, answer) = events(
            &emulator,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": r#"{"published":true}"#,
                    "EventBusName": &named_bus,
                }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutEvents answered {status} {answer}"
        );
        assert_eq!(
            answer["FailedEntryCount"],
            json!(0),
            "case {case:?}: the entry must be published, got {answer}"
        );

        let messages = drain(&emulator, &url).await;
        assert_eq!(
            messages.len(),
            1,
            "case {case:?}: expected the entry on the queue, got {messages:?}"
        );
        let envelope: Value = messages[0]["Body"]
            .as_str()
            .and_then(|body| serde_json::from_str(body).ok())
            .unwrap_or(Value::Null);
        for (pointer, expected) in [
            ("/source", json!(SOURCE)),
            ("/detail-type", json!(DETAIL_TYPE)),
            ("/detail/published", json!(true)),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {actual} in \
                 {envelope}"
            );
        }
    }
}
