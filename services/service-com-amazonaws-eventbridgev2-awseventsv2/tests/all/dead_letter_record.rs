// SPDX-License-Identifier: CC0-1.0

//! The RECORD a subscriber's failed delivery leaves where its
//! `OnFailureConfiguration.Arn` names — "the ARN of the destination that
//! receives events that could not be delivered. An Amazon SQS queue is the
//! supported destination."
//!
//! RULE: a delivery that did not happen leaves exactly one JSON document on
//! that destination DESCRIBING the failure, never a copy of the event: the bus
//! the events were published to (`busArn`), the subscriber (`subscriberArn`),
//! the target its `InvokeConfiguration` names (`targetArn`), this service's own
//! classification of the failure (`errorCode`: a target that is not there, an
//! identity that was refused) with the sentence of that class (`errorMessage`),
//! the bound of the subscriber's `RetryPolicy` that ended the delivery
//! (`exhaustedRetryCondition`) beside the retries actually spent
//! (`retryAttempts`, which is none, whatever `MaxRetryAttempts` the subscriber
//! states — "Maximum number of retry attempts (0-185, default: 5)" — because
//! this slice attempts a delivery once and no record may report attempts nothing
//! made), and the identity of every event that was not delivered
//! (`failedMessages[].eventId`) — and none of the published event's own
//! envelope. WHICH bound the record names is pinned where a recording covers it,
//! on a subscriber that allows no retry at all, and asked of every other row as
//! the record naming a bound at all.
//!
//! The rows differ along what the rule generalises over: WHY the delivery failed
//! (the target queue is gone, the role may not make the call), what the
//! subscriber's `RetryPolicy` states (no retry, one, several, and no policy at
//! all, which is the member's default), and how many events the failed window
//! carries.
//!
//! The harness is [`crate::sns_subscriber_delivery`]'s: the same router with
//! EventBridge V2, Amazon SQS, Amazon SNS and IAM on ONE engine, so every
//! outcome is asserted where a client observes it. Nothing here waits on the
//! wall clock: a publish performs the deliveries it has made due before it
//! answers, and the one wait — for the destination to hold the record — polls
//! for that CONDITION under a generous deadline.

use serde_json::{json, Value};

use crate::sns_subscriber_delivery::{
    could_not_access, document, emulator, entry, given_active_bus, given_queue, given_role,
    given_subscriber, messages_on, publish, sqs_ok, Wanted, ACCESS_DENIED, SEND_MESSAGE_ACTION,
    SERVICE_PRINCIPAL,
};

/// The condition the record of a delivery whose target was not there carries.
/// Written out here rather than read from the service, so this suite pins the
/// value a client reads off the record.
const RESOURCE_NOT_FOUND: &str = "RESOURCE_NOT_FOUND";

/// The condition the ATTEMPTS of a failed delivery ended under: the bound of the
/// subscriber's `RetryPolicy` the delivery exhausted. Pinned here for the same
/// reason.
const MAXIMUM_RETRY_ATTEMPTS: &str = "MaximumRetryAttempts";

/// How many retries a record reports as spent: a delivery is attempted once, so
/// a record may report no retry it did not make, whatever the subscriber's
/// `RetryPolicy` allows.
const RETRIES_SPENT: i32 = 0;

/// The keys of the event's own envelope, which the record must carry NONE of:
/// it describes the failure, and the bus retains the events themselves. Source:
/// the members `PutEventsRequestEntry` carries and the envelope a delivery is
/// made in (`smithy_query(query="operation", operation="PutEvents")`).
const ENVELOPE_KEYS: [&str; 6] = ["source", "detail-type", "detail", "account", "time", "region"];

/// Why a row's delivery fails.
#[derive(Clone, Copy, Debug)]
enum Why {
    /// The target queue was deleted before the event was published, so the call
    /// the delivery makes addresses a queue that is not there.
    TargetIsGone,
    /// The role the subscriber invokes its target with is allowed nothing about
    /// the call its target takes.
    IdentityIsRefused,
}

impl Why {
    /// The condition a record of this failure states.
    fn error_code(self) -> &'static str {
        match self {
            Self::TargetIsGone => RESOURCE_NOT_FOUND,
            Self::IdentityIsRefused => ACCESS_DENIED,
        }
    }
}

/// One row of the record table.
#[derive(Clone, Copy, Debug)]
struct Row {
    case: &'static str,
    why: Why,
    /// The `MaxRetryAttempts` the subscriber states, or `None` for a subscriber
    /// stating no `RetryPolicy` at all.
    retry_attempts: Option<i32>,
    /// How many events the publish that fails carries.
    published: usize,
}

/// RULE: see the module docs — the record describes the failure, with every
/// field read off the subscriber's stored configuration, the failure and the
/// events that were not delivered.
#[tokio::test]
async fn a_failed_deliverys_record_describes_the_failure() {
    let rows: [Row; 6] = [
        Row {
            case: "the target queue is gone and no retry is allowed",
            why: Why::TargetIsGone,
            retry_attempts: Some(0),
            published: 1,
        },
        Row {
            case: "the target queue is gone, one retry is allowed and two events failed",
            why: Why::TargetIsGone,
            retry_attempts: Some(1),
            published: 2,
        },
        Row {
            case: "the target queue is gone and the subscriber states no retry policy",
            why: Why::TargetIsGone,
            retry_attempts: None,
            published: 1,
        },
        Row {
            case: "the role may not make the call and no retry is allowed",
            why: Why::IdentityIsRefused,
            retry_attempts: Some(0),
            published: 1,
        },
        Row {
            case: "the role may not make the call and three retries are allowed",
            why: Why::IdentityIsRefused,
            retry_attempts: Some(3),
            published: 2,
        },
        Row {
            case: "the role may not make the call and the subscriber states no retry policy",
            why: Why::IdentityIsRefused,
            retry_attempts: None,
            published: 1,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let router = emulator();
        let case = row.case;
        let (target_url, target_arn) =
            given_queue(&router, case, &format!("record-target-{index}")).await;
        let (record_url, record_arn) =
            given_queue(&router, case, &format!("record-dlq-{index}")).await;
        // The role allows the call the target takes, so a row whose target is
        // GONE fails at the target rather than at the identity; the other rows
        // state something that is not about that call at all, which is IAM's
        // default deny.
        let grants = match row.why {
            Why::TargetIsGone => json!([{
                "Effect": "Allow",
                "Action": SEND_MESSAGE_ACTION,
                "Resource": [target_arn.clone(), record_arn.clone()],
            }]),
            Why::IdentityIsRefused => json!([{
                "Effect": "Allow",
                "Action": "sqs:GetQueueAttributes",
                "Resource": "*",
            }]),
        };
        let role_arn = given_role(
            &router,
            case,
            &format!("record-role-{index}"),
            SERVICE_PRINCIPAL,
            Some(grants),
        )
        .await;
        let bus_arn = given_active_bus(&router, case, &format!("record-bus-{index}")).await;
        let subscriber_arn = given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("record-subscriber-{index}"),
                event_bus_arn: &bus_arn,
                role_arn: &role_arn,
                target_arn: &target_arn,
                on_failure_arn: Some(&record_arn),
                filter: None,
                retry_attempts: row.retry_attempts,
            },
        )
        .await;

        if matches!(row.why, Why::TargetIsGone) {
            sqs_ok(
                &router,
                "DeleteQueue",
                &json!({ "QueueUrl": target_url }),
                case,
            )
            .await;
        }

        let published = publish(
            &router,
            case,
            &bus_arn,
            (0..row.published).map(|at| entry(at, "ship")).collect(),
        )
        .await;

        // A window whose deliveries all failed the same way leaves ONE record
        // listing every event of it.
        let recorded = messages_on(&router, case, &record_url, 1).await;
        let body = recorded
            .first()
            .and_then(|message| message["Body"].as_str())
            .unwrap_or_else(|| panic!("case {case:?}: the record must arrive as a message body"));
        let record = document(case, body);

        assert_eq!(
            record["busArn"],
            json!(bus_arn),
            "case {case:?}: the record must name the bus the events were published to, got \
             {record}"
        );
        assert_eq!(
            record["subscriberArn"],
            json!(subscriber_arn),
            "case {case:?}: the record must name the subscriber whose delivery failed, got \
             {record}"
        );
        assert_eq!(
            record["targetArn"],
            json!(target_arn),
            "case {case:?}: the record must name the target the delivery was for, got {record}"
        );
        assert_eq!(
            record["errorCode"],
            json!(row.why.error_code()),
            "case {case:?}: expected the condition {:?}, got {record}",
            row.why.error_code()
        );
        assert_eq!(
            record["errorMessage"],
            // The sentence is the one AWS words for a target a delivery could
            // not reach, whichever of the two conditions decided it: its only
            // variable part is the target the delivery was for.
            json!(could_not_access(&target_arn)),
            "case {case:?}: expected the sentence of that condition, got {record}"
        );
        if row.retry_attempts == Some(0) {
            // The recorded case: a subscriber that allows NO retry has its one
            // attempt, so the bound that ended the delivery is the attempts
            // bound and the record names it.
            assert_eq!(
                record["exhaustedRetryCondition"],
                json!(MAXIMUM_RETRY_ATTEMPTS),
                "case {case:?}: the record must name the bound that ended the delivery, got \
                 {record}"
            );
        } else {
            // For a policy that allows retries no recording pins WHICH bound
            // is named, so what is required is that the record says which one
            // ended the delivery at all.
            assert!(
                record["exhaustedRetryCondition"]
                    .as_str()
                    .is_some_and(|named| !named.is_empty()),
                "case {case:?}: the record must name the bound that ended the delivery, got \
                 {record}"
            );
        }
        assert_eq!(
            record["retryAttempts"],
            json!(RETRIES_SPENT),
            "case {case:?}: the record may report no retry this delivery did not make, got \
             {record}"
        );
        let empty = Vec::new();
        let listed: Vec<&str> = record["failedMessages"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(|failed| failed["eventId"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            listed,
            published.iter().map(String::as_str).collect::<Vec<&str>>(),
            "case {case:?}: the record must point at every event that was not delivered, got \
             {record}"
        );
        for key in ENVELOPE_KEYS {
            assert_eq!(
                record.get(key),
                None,
                "case {case:?}: the record describes the failure and carries no {key:?} of the \
                 event itself, got {record}"
            );
        }
    }
}

/// RULE: the destination holds ONE record per failure and nothing else — the
/// events themselves are never written to it, whatever the window carried.
#[tokio::test]
async fn the_destination_holds_one_record_and_none_of_the_events() {
    let case = "two events of one window fail the same way";
    let router = emulator();
    let (target_url, target_arn) = given_queue(&router, case, "single-target").await;
    let (record_url, record_arn) = given_queue(&router, case, "single-dlq").await;
    let role_arn = given_role(
        &router,
        case,
        "single-role",
        SERVICE_PRINCIPAL,
        Some(json!([{
            "Effect": "Allow",
            "Action": SEND_MESSAGE_ACTION,
            "Resource": [target_arn.clone(), record_arn.clone()],
        }])),
    )
    .await;
    let bus_arn = given_active_bus(&router, case, "single-bus").await;
    given_subscriber(
        &router,
        case,
        Wanted {
            name: "single-subscriber",
            event_bus_arn: &bus_arn,
            role_arn: &role_arn,
            target_arn: &target_arn,
            on_failure_arn: Some(&record_arn),
            filter: None,
            retry_attempts: Some(0),
        },
    )
    .await;
    sqs_ok(
        &router,
        "DeleteQueue",
        &json!({ "QueueUrl": target_url }),
        case,
    )
    .await;

    let published = publish(
        &router,
        case,
        &bus_arn,
        (0..3).map(|at| entry(at, "ship")).collect(),
    )
    .await;

    // Exactly one message: `messages_on` takes one more pass and requires it to
    // find nothing.
    let recorded = messages_on(&router, case, &record_url, 1).await;
    let record = document(
        case,
        recorded
            .first()
            .and_then(|message| message["Body"].as_str())
            .unwrap_or_default(),
    );
    let listed = record["failedMessages"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        listed,
        published.len(),
        "case {case:?}: the one record must list every event of the window, got {record}"
    );
    let envelope: Vec<&Value> = ENVELOPE_KEYS
        .iter()
        .filter_map(|key| record.get(*key))
        .collect();
    assert!(
        envelope.is_empty(),
        "case {case:?}: the record carries none of the event's envelope, got {envelope:?} in \
         {record}"
    );
}
