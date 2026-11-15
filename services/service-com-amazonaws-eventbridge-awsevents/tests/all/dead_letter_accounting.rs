// SPDX-License-Identifier: CC0-1.0

//! The events a failed target delivery PARKED are counted on the failing rule's own
//! `AWS/Events` `InvocationsSentToDlq`, end to end over the wire (`awsJson1_1`,
//! `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter`, with Amazon SQS and CloudWatch mounted on the same engine
//! — so the count is read where a client reads one: through CloudWatch's own
//! `GetMetricStatistics`, beside the dead-letter queue the event landed on.
//!
//! The RULE asserted here: **when a rule's target invocation fails and the event is
//! WRITTEN to the queue that target's `DeadLetterConfig` names, EventBridge
//! publishes one datapoint of value 1 on that rule's `InvocationsSentToDlq`, under
//! the rule's whole identity and under the rule name on its own — and publishes
//! none when nothing was parked: for a refused delivery whose target carries no
//! dead-letter queue at all, for one whose dead-letter queue refuses the write, and
//! for a delivery the target accepted.** The metric is a second, independent fact
//! about one refusal: `FailedInvocations` says the invocation failed, and this says
//! the event was parked rather than lost — which is the only thing that tells those
//! two apart.
//!
//! The rows differ along what the rule generalises over: WHETHER the delivery
//! failed at all, whether the failing target names a dead-letter queue, and whether
//! the queue it names accepts the write.
//!
//! The harness (the router with EventBridge, Amazon SQS, Step Functions, IAM and
//! CloudWatch on one engine, and the queue and rule fixtures) is
//! `tests/all/target_dispatch.rs`'s.

use serde_json::json;
use service_com_amazonaws_eventbridge_awsevents::state::{
    DEFAULT_EVENT_BUS_NAME, FAILED_INVOCATIONS_METRIC, INVOCATIONS_METRIC,
    INVOCATIONS_SENT_TO_DLQ_METRIC,
};

use crate::target_dispatch::{
    drain, emulator, given_queue, given_queue_with_policy, given_rule, metric_sum,
    metric_sum_over, publish, rule_name_dimensions,
};

/// The rule every row wires, and the id of its one target.
const RULE: &str = "parked";
const TARGET_ID: &str = "t1";

/// One row: what the target does with the event, what its dead-letter queue does
/// with the refusal, and what the rule's counters must then report.
struct Row {
    case: &'static str,
    /// Whether the TARGET queue's own policy admits the delivery.
    target_admits: bool,
    /// The dead-letter queue the target names: none at all, one that admits the
    /// write, or one that admits nothing.
    dead_letter: DeadLetter,
    /// How many messages the dead-letter queue must hold afterwards.
    parked: usize,
    /// What the rule's `InvocationsSentToDlq` must sum to.
    sent_to_dlq: f64,
    /// What its `FailedInvocations` must sum to.
    failed: f64,
    /// What its `Invocations` must sum to.
    invoked: f64,
}

/// Which dead-letter queue a row's target names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadLetter {
    /// The target names none, so there is nowhere to park the event.
    None,
    /// A queue whose policy admits this service's write.
    Admitting,
    /// A queue whose policy admits nothing, so the write is refused there too.
    Refusing,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a refused delivery whose target names a dead-letter queue",
            target_admits: false,
            dead_letter: DeadLetter::Admitting,
            parked: 1,
            sent_to_dlq: 1.0,
            failed: 1.0,
            invoked: 0.0,
        },
        Row {
            case: "a refused delivery whose target names no dead-letter queue",
            target_admits: false,
            dead_letter: DeadLetter::None,
            parked: 0,
            sent_to_dlq: 0.0,
            failed: 1.0,
            invoked: 0.0,
        },
        Row {
            case: "a refused delivery whose dead-letter queue admits nothing",
            target_admits: false,
            dead_letter: DeadLetter::Refusing,
            parked: 0,
            sent_to_dlq: 0.0,
            failed: 1.0,
            invoked: 0.0,
        },
        Row {
            case: "a delivery the target accepted",
            target_admits: true,
            dead_letter: DeadLetter::Admitting,
            parked: 0,
            sent_to_dlq: 0.0,
            failed: 0.0,
            invoked: 1.0,
        },
    ]
}

/// RULE: the events a failed delivery parked are what the rule's
/// `InvocationsSentToDlq` counts — and a refusal that parked nothing counts there
/// not at all.
#[tokio::test]
async fn the_events_a_refusal_parked_are_counted_on_the_rules_own_metric() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        // The target queue: one that admits this service's send, or one carrying a
        // document that admits nothing at all.
        let (target_url, target_arn) = if row.target_admits {
            given_queue(&emulator, &format!("parked-target-{index}")).await
        } else {
            given_queue_with_policy(&emulator, &format!("parked-target-{index}"), None).await
        };
        let dead_letter = match row.dead_letter {
            DeadLetter::None => None,
            DeadLetter::Admitting => Some(
                given_queue(&emulator, &format!("parked-dlq-{index}"))
                    .await,
            ),
            DeadLetter::Refusing => Some(
                given_queue_with_policy(&emulator, &format!("parked-dlq-{index}"), None).await,
            ),
        };
        let mut target = json!({ "Id": TARGET_ID, "Arn": target_arn });
        if let Some((_, dlq_arn)) = dead_letter.as_ref() {
            target["DeadLetterConfig"] = json!({ "Arn": dlq_arn });
        }
        given_rule(&emulator, RULE, json!([target])).await;
        publish(&emulator).await;

        // What the queues hold: the target's own copy, and the parked one.
        let delivered = drain(&emulator, &target_url).await;
        assert_eq!(
            delivered.len(),
            usize::from(row.target_admits),
            "case {case:?}: expected the target queue to hold {} message(s), got {delivered:?}",
            usize::from(row.target_admits)
        );
        if let Some((dlq_url, _)) = dead_letter.as_ref() {
            let parked = drain(&emulator, dlq_url).await;
            assert_eq!(
                parked.len(),
                row.parked,
                "case {case:?}: expected {} message(s) on the dead-letter queue, got {parked:?}",
                row.parked
            );
        }

        // And the counters the rule carries for it, under both dimension sets a
        // handover counter is published under.
        for (metric, want) in [
            (INVOCATIONS_SENT_TO_DLQ_METRIC, row.sent_to_dlq),
            (FAILED_INVOCATIONS_METRIC, row.failed),
            (INVOCATIONS_METRIC, row.invoked),
        ] {
            let got = metric_sum(&emulator, metric, DEFAULT_EVENT_BUS_NAME, RULE).await;
            assert_eq!(
                got, want,
                "case {case:?}: expected {metric} for the rule to sum to {want}, got {got}"
            );
            let by_name = metric_sum_over(&emulator, metric, &rule_name_dimensions(RULE)).await;
            assert_eq!(
                by_name, want,
                "case {case:?}: expected {metric} under the rule name alone to sum to {want}, \
                 got {by_name}"
            );
        }
    }
}
