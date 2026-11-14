// SPDX-License-Identifier: CC0-1.0

//! The `detail-type` an email event carries on the event bus a configuration set's
//! EventBridge event destination names — the name a rule over ONE email event matches on.
//!
//! The RULE these cases pin: the event a send publishes onto that bus carries, as its
//! `detail-type`, the name Amazon SES's event-publishing documentation gives THAT event type
//! for an EventBridge destination — the name of what happened to the message ("Email Sent",
//! "Email Delivered", "Email Bounced", "Email Complaint Received"), not a repetition of the
//! `MatchingEventTypes` spelling — so a rule whose pattern names that one detail-type
//! receives exactly the one event of that type the send produced, and a rule naming any
//! other spelling of the same event receives nothing.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service,
//! Amazon EventBridge and Amazon SQS on ONE engine — and reads each event where a client
//! observes one: on the queue the rule's target enqueues the whole envelope into. The
//! harness is the one [`crate::event_bridge_event_destination_delivery`] already drives these
//! sends through.
//!
//! The table spans the event types an accepted send produces, each selected by a rule naming
//! only its own detail-type: the acceptance, the delivery, the failure and the complaint. The
//! recorded case — a rule on the delivery's detail-type over a send to the mailbox
//! simulator's accepting address — is one row of it.

use serde_json::json;

use crate::event_bridge_event_destination_delivery::{
    assert_event, queue_arn, settled_envelopes, Emulator, Expected, BOUNCING, COMPLAINING,
    EVERY_EVENT_TYPE, SES_EVENT_SOURCE, SUCCESS,
};

/// One row of the table: the send it makes, the rule that selects one of its events by that
/// event's own detail-type, and what the one selected envelope must carry.
struct Row {
    case: &'static str,
    /// The configuration set the row creates and sends through.
    set: &'static str,
    /// The queue the row's rule hands its envelopes to.
    queue: &'static str,
    /// The rule naming the one detail-type the row is about.
    rule: &'static str,
    /// The one recipient of the send, whose reaction produces the row's event.
    recipient: &'static str,
    /// The event the rule must receive: its documented `detail-type`, the `eventType` its
    /// detail is of, and the addresses that detail names.
    expected: Expected,
}

const ROWS: &[Row] = &[
    Row {
        case: "the acceptance of a message",
        set: "sent-detail-type-set",
        queue: "sent-detail-type-queue",
        rule: "sent-detail-type-rule",
        recipient: SUCCESS,
        expected: Expected {
            detail_type: "Email Sent",
            event_type: "Send",
            addresses: &[SUCCESS],
            bounce: None,
        },
    },
    Row {
        case: "a message that reached its recipient",
        set: "delivered-detail-type-set",
        queue: "delivered-detail-type-queue",
        rule: "delivered-detail-type-rule",
        recipient: SUCCESS,
        expected: Expected {
            detail_type: "Email Delivered",
            event_type: "Delivery",
            addresses: &[SUCCESS],
            bounce: None,
        },
    },
    Row {
        case: "a message that failed for its recipient",
        set: "bounced-detail-type-set",
        queue: "bounced-detail-type-queue",
        rule: "bounced-detail-type-rule",
        recipient: BOUNCING,
        expected: Expected {
            detail_type: "Email Bounced",
            event_type: "Bounce",
            addresses: &[BOUNCING],
            bounce: Some(("Permanent", "General")),
        },
    },
    Row {
        case: "a message a recipient reported as unwanted",
        set: "complaint-detail-type-set",
        queue: "complaint-detail-type-queue",
        rule: "complaint-detail-type-rule",
        recipient: COMPLAINING,
        expected: Expected {
            detail_type: "Email Complaint Received",
            event_type: "Complaint",
            addresses: &[COMPLAINING],
            bounce: None,
        },
    },
];

/// The addresses the published document's `mail` object names, which is where a consumer
/// keyed on one message reads the recipients the send addressed.
fn mail_destination(envelope: &serde_json::Value) -> serde_json::Value {
    envelope["detail"]["mail"]["destination"].clone()
}

#[tokio::test]
async fn a_rule_naming_one_documented_detail_type_receives_that_email_event() {
    for row in ROWS {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_verified_sender(case).await;
        // The destination matches EVERY event type, so the send publishes every event it
        // produces onto the bus and the rule's own pattern is the only thing that selects
        // one of them.
        emulator
            .given_destination(case, row.set, true, EVERY_EVENT_TYPE)
            .await;
        let url = emulator.given_queue(case, row.queue).await;
        emulator
            .given_rule(
                case,
                row.rule,
                &json!({
                    "source": [SES_EVENT_SOURCE],
                    "detail-type": [row.expected.detail_type],
                }),
                &queue_arn(row.queue),
            )
            .await;

        let message_id = emulator.send(case, row.set, &[row.recipient]).await;
        let held = settled_envelopes(&emulator, case, &url, 1).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly one {:?} event, got {held:?}",
            row.expected.detail_type
        );
        assert_event(case, &row.expected, &held[0], row.set, &message_id);
        let destination = mail_destination(&held[0]);
        assert_eq!(
            destination,
            json!([row.recipient]),
            "case {case:?}: expected the event's mail.destination to name {:?}, got \
             {destination:?}",
            row.recipient
        );
    }
}

#[tokio::test]
async fn a_rule_naming_another_spelling_of_the_same_event_receives_nothing() {
    // The detail-type is the documented name of what happened to the message, so no event
    // carries the event type's own `MatchingEventTypes` spelling, and none carries a
    // detail-type that merely repeats it: a rule naming either receives nothing, while the
    // rule naming the documented name receives the event.
    let case = "rules naming the event type's own spelling beside the documented name";
    let set = "spelling-detail-type-set";
    let emulator = Emulator::new();
    emulator.given_verified_sender(case).await;
    emulator
        .given_destination(case, set, true, EVERY_EVENT_TYPE)
        .await;

    let documented = emulator.given_queue(case, "documented-queue").await;
    emulator
        .given_rule(
            case,
            "documented-rule",
            &json!({"source": [SES_EVENT_SOURCE], "detail-type": ["Email Delivered"]}),
            &queue_arn("documented-queue"),
        )
        .await;
    let other = emulator.given_queue(case, "other-spelling-queue").await;
    emulator
        .given_rule(
            case,
            "other-spelling-rule",
            &json!({
                "source": [SES_EVENT_SOURCE],
                "detail-type": ["DELIVERY", "Email Delivery", "Delivery"],
            }),
            &queue_arn("other-spelling-queue"),
        )
        .await;

    let message_id = emulator.send(case, set, &[SUCCESS]).await;
    let held = settled_envelopes(&emulator, case, &documented, 1).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected exactly the delivery on the documented rule's queue, got \
         {held:?}"
    );
    assert_event(
        case,
        &Expected {
            detail_type: "Email Delivered",
            event_type: "Delivery",
            addresses: &[SUCCESS],
            bounce: None,
        },
        &held[0],
        set,
        &message_id,
    );
    let stray = emulator.received(case, &other).await;
    assert!(
        stray.is_empty(),
        "case {case:?}: no event may carry another spelling of the delivery, got {stray:?}"
    );
}
