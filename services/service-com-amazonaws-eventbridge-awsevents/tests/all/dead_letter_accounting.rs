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
//! The second suite below pins the ATTRIBUTION half of the same rule, read the way
//! a dashboard or an alarm reads a live series — through CloudWatch's
//! `GetMetricData` — over a table of failure conditions: the datapoint belongs to
//! the rule whose own target invocation failed, and the sibling rule matched by the
//! same events, whose target took them, has no datapoint on that metric at all.
//!
//! The harness (the router with EventBridge, Amazon SQS, Step Functions, IAM and
//! CloudWatch on one engine, and the queue and rule fixtures) is
//! `tests/all/target_dispatch.rs`'s.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    DEFAULT_EVENT_BUS_NAME, FAILED_INVOCATIONS_METRIC, INVOCATIONS_METRIC,
    INVOCATIONS_SENT_TO_DLQ_METRIC, METRIC_NAMESPACE, SERVICE_PRINCIPAL,
};

use crate::target_dispatch::{
    cloudwatch, drain, emulator, given_queue, given_queue_with_policy, given_role, given_rule,
    given_state_machine, metric_sum, metric_sum_over, ordered, publish, publish_details,
    published_series, rule_dimensions, rule_name_dimensions, rule_series, sfn, Emulator, REGION,
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

/// The two rules every attribution row wires on one bus: the one whose target
/// invocation fails, and the sibling the same events match whose target takes them.
const FAILING_RULE: &str = "dead-lettering";
const SIBLING_RULE: &str = "delivering";

/// The service principal a role that this service cannot assume trusts instead.
const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The action a state-machine target's invocation makes, and so the one its role
/// has to be allowed; and an action of the same service that is not it.
const TARGET_ACTION: &str = "states:StartExecution";
const ANOTHER_ACTION: &str = "states:DescribeStateMachine";

/// The window a metric read below spans, and the granularity it folds the series
/// into: one standard-resolution minute, the resolution these counters are
/// published at.
const WINDOW_SECONDS: f64 = 300.0;
const PERIOD_SECONDS: i64 = 60;

/// The id the one `MetricDataQuery` of a read below carries: "a short name used to
/// tie this object to the results in the response" (`MetricDataQuery$Id`).
const QUERY_ID: &str = "dlq";

/// WHY the failing rule's target invocation fails — the facts a target delivery is
/// refused for, each reached by a different step of the handover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Condition {
    /// A queue target whose own resource policy grants this service nothing: "for
    /// Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge
    /// relies on resource-based policies" (`PutTargets`), so the send is refused.
    TargetPolicyWithholdsTheSend,
    /// A target invoked under the role it names, whose policies allow another
    /// action of the same service instead of the call the invocation makes.
    RolePermissionsWithholdTheCall,
    /// A target naming a role that does not trust this service, so there is no
    /// session to invoke it under at all.
    RoleIsNotAssumable,
}

/// Where a sibling rule's delivery is read back from: the resource its target is.
enum Receipt {
    /// The queue the delivery is a message on.
    Queue(String),
    /// The state machine the delivery is an execution of.
    StateMachine(String),
}

/// One row: the failure condition, and how many matching events the round carries
/// — so a count is read as a count rather than as a flag.
struct AttributionRow {
    case: &'static str,
    condition: Condition,
    events: usize,
}

fn attribution_rows() -> Vec<AttributionRow> {
    vec![
        AttributionRow {
            case: "a queue target whose policy withholds the send, one event",
            condition: Condition::TargetPolicyWithholdsTheSend,
            events: 1,
        },
        AttributionRow {
            case: "a state machine whose role withholds the call, two events",
            condition: Condition::RolePermissionsWithholdTheCall,
            events: 2,
        },
        AttributionRow {
            case: "a state machine under a role this service cannot assume, three events",
            condition: Condition::RoleIsNotAssumable,
            events: 3,
        },
    ]
}

/// The two targets a row wires: the one whose invocation fails and the one that
/// takes the event, each with the role it is invoked under where its kind names
/// one, and where the sibling's delivery is read back from.
struct Wiring {
    failing_target: String,
    failing_role: Option<String>,
    sibling_target: String,
    sibling_role: Option<String>,
    sibling_receipt: Receipt,
}

/// Create the resources `condition` needs, as a client would: the queues and their
/// policies, the state machines, and the roles with their trust and identity
/// policies.
async fn given_wiring(emulator: &Emulator, index: usize, condition: Condition) -> Wiring {
    match condition {
        Condition::TargetPolicyWithholdsTheSend => {
            // A queue that has granted nothing receives nothing; the sibling's
            // queue grants this service the send.
            let (_, withholding) =
                given_queue_with_policy(emulator, &format!("withholding-{index}"), None).await;
            let (admitting_url, admitting) =
                given_queue(emulator, &format!("admitting-{index}")).await;
            Wiring {
                failing_target: withholding,
                failing_role: None,
                sibling_target: admitting,
                sibling_role: None,
                sibling_receipt: Receipt::Queue(admitting_url),
            }
        }
        Condition::RolePermissionsWithholdTheCall => {
            let failing = given_state_machine(emulator, &format!("refused-flow-{index}")).await;
            let sibling = given_state_machine(emulator, &format!("started-flow-{index}")).await;
            let failing_role = given_role(
                emulator,
                "withholding-role",
                SERVICE_PRINCIPAL,
                Some(ANOTHER_ACTION),
            )
            .await;
            let sibling_role = given_role(
                emulator,
                "granting-role",
                SERVICE_PRINCIPAL,
                Some(TARGET_ACTION),
            )
            .await;
            Wiring {
                failing_target: failing,
                failing_role: Some(failing_role),
                sibling_target: sibling.clone(),
                sibling_role: Some(sibling_role),
                sibling_receipt: Receipt::StateMachine(sibling),
            }
        }
        Condition::RoleIsNotAssumable => {
            let failing = given_state_machine(emulator, &format!("unassumed-flow-{index}")).await;
            let sibling = given_state_machine(emulator, &format!("started-flow-{index}")).await;
            let failing_role = given_role(
                emulator,
                "untrusting-role",
                OTHER_PRINCIPAL,
                Some(TARGET_ACTION),
            )
            .await;
            let sibling_role = given_role(
                emulator,
                "granting-role",
                SERVICE_PRINCIPAL,
                Some(TARGET_ACTION),
            )
            .await;
            Wiring {
                failing_target: failing,
                failing_role: Some(failing_role),
                sibling_target: sibling.clone(),
                sibling_role: Some(sibling_role),
                sibling_receipt: Receipt::StateMachine(sibling),
            }
        }
    }
}

/// One target of a rule: the resource it names, the role it is invoked under where
/// its kind names one, no retry at all, and the dead-letter queue the refusal is
/// parked on.
fn target(arn: &str, role: Option<&String>, dead_letter_arn: &str) -> Value {
    let mut target = json!({
        "Id": TARGET_ID,
        "Arn": arn,
        "RetryPolicy": { "MaximumRetryAttempts": 0, "MaximumEventAgeInSeconds": 60 },
        "DeadLetterConfig": { "Arn": dead_letter_arn },
    });
    if let Some(role) = role {
        target["RoleArn"] = json!(role);
    }
    target
}

/// How many deliveries the sibling rule's target really holds — read through the
/// owning service's own API, which is where a client of the pipeline looks.
async fn received(emulator: &Emulator, case: &str, receipt: &Receipt) -> usize {
    match receipt {
        Receipt::Queue(url) => drain(emulator, url).await.len(),
        Receipt::StateMachine(arn) => {
            let (status, listed) = sfn(
                emulator,
                "ListExecutions",
                &json!({ "stateMachineArn": arn }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListExecutions answered {status} {listed}"
            );
            listed["executions"].as_array().map_or(0, Vec::len)
        }
    }
}

/// The values `GetMetricData` answers for the ONE series `dimensions` names of
/// `metric` in EventBridge's namespace, over a window around now folded into
/// `PERIOD_SECONDS`-wide buckets.
///
/// The other reader of the same store, beside the `GetMetricStatistics` the suite
/// above asks: a series a dashboard or an alarm reads is read through this one.
async fn metric_data_values(
    emulator: &Emulator,
    case: &str,
    metric: &str,
    dimensions: &Value,
) -> Vec<f64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let (status, body) = cloudwatch(
        emulator,
        "GetMetricData",
        &json!({
            "StartTime": now - WINDOW_SECONDS,
            "EndTime": now + WINDOW_SECONDS,
            "MetricDataQueries": [{
                "Id": QUERY_ID,
                "MetricStat": {
                    "Metric": {
                        "Namespace": METRIC_NAMESPACE,
                        "MetricName": metric,
                        "Dimensions": dimensions,
                    },
                    "Period": PERIOD_SECONDS,
                    "Stat": "Sum",
                },
                "ReturnData": true,
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricData answered {status} {body}"
    );
    let results = body["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let result = results
        .iter()
        .find(|result| result["Id"] == json!(QUERY_ID))
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetMetricData answered no result for the query, got {body}")
        });
    result["Values"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_f64)
        .collect()
}

/// RULE: the `InvocationsSentToDlq` datapoint a parked event publishes belongs to
/// the rule whose OWN target invocation failed — whatever refused that invocation
/// and however many events it refused — so a sibling rule matched by the same
/// events, whose target took them, carries no datapoint on that metric at all; and
/// the series is the same one through every reader of the store.
#[tokio::test]
async fn the_parked_events_are_attributed_to_the_rule_whose_target_failed() {
    for (index, row) in attribution_rows().iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        // Each rule gets its own dead-letter queue, so WHERE an event was parked
        // cannot stand in for which rule parked it.
        let (failing_dlq_url, failing_dlq_arn) =
            given_queue(&emulator, &format!("failing-dlq-{index}")).await;
        let (sibling_dlq_url, sibling_dlq_arn) =
            given_queue(&emulator, &format!("sibling-dlq-{index}")).await;
        let wiring = given_wiring(&emulator, index, row.condition).await;
        given_rule(
            &emulator,
            FAILING_RULE,
            json!([target(
                &wiring.failing_target,
                wiring.failing_role.as_ref(),
                &failing_dlq_arn
            )]),
        )
        .await;
        given_rule(
            &emulator,
            SIBLING_RULE,
            json!([target(
                &wiring.sibling_target,
                wiring.sibling_role.as_ref(),
                &sibling_dlq_arn
            )]),
        )
        .await;

        let details: Vec<String> = (0..row.events)
            .map(|round| json!({ "round": round }).to_string())
            .collect();
        let published = publish_details(&emulator, &details).await;
        assert_eq!(
            published.len(),
            row.events,
            "case {case:?}: expected {} published event id(s), got {published:?}",
            row.events
        );

        // The sibling rule's target took every event, and parked none.
        let taken = received(&emulator, case, &wiring.sibling_receipt).await;
        assert_eq!(
            taken, row.events,
            "case {case:?}: expected the sibling rule's target to hold {} delivery(ies), got \
             {taken}",
            row.events
        );
        let sibling_parked = drain(&emulator, &sibling_dlq_url).await;
        assert!(
            sibling_parked.is_empty(),
            "case {case:?}: a delivery the target took parks nothing, got {sibling_parked:?}"
        );

        // The failing rule's refusals are parked on its own queue, each naming the
        // rule the metric is then attributed to.
        let parked = drain(&emulator, &failing_dlq_url).await;
        assert_eq!(
            parked.len(),
            row.events,
            "case {case:?}: expected {} parked message(s), got {parked:?}",
            row.events
        );
        let failing_rule_arn = format!(
            "arn:aws:events:{REGION}:{}:rule/{FAILING_RULE}",
            engine::emulator_account_id()
        );
        for message in &parked {
            assert_eq!(
                message["MessageAttributes"]["RULE_ARN"]["StringValue"],
                json!(failing_rule_arn),
                "case {case:?}: the parked message names the rule that parked it, got {message}"
            );
        }

        // The datapoints: one per parked event on the failing rule's series, under
        // both dimension sets a handover counter is published under, and none on the
        // sibling's — read through `GetMetricData`, and the same through
        // `GetMetricStatistics`.
        let want = f64::from(u32::try_from(row.events).unwrap_or(u32::MAX));
        for dimensions in [
            rule_dimensions(DEFAULT_EVENT_BUS_NAME, FAILING_RULE),
            rule_name_dimensions(FAILING_RULE),
        ] {
            let values =
                metric_data_values(&emulator, case, INVOCATIONS_SENT_TO_DLQ_METRIC, &dimensions)
                    .await;
            let got: f64 = values.iter().sum();
            assert_eq!(
                got, want,
                "case {case:?}: expected GetMetricData over {dimensions} to sum to {want}, got \
                 {values:?}"
            );
            let through_statistics =
                metric_sum_over(&emulator, INVOCATIONS_SENT_TO_DLQ_METRIC, &dimensions).await;
            assert_eq!(
                through_statistics, want,
                "case {case:?}: expected GetMetricStatistics over {dimensions} to sum to {want}, \
                 got {through_statistics}"
            );
        }
        for dimensions in [
            rule_dimensions(DEFAULT_EVENT_BUS_NAME, SIBLING_RULE),
            rule_name_dimensions(SIBLING_RULE),
        ] {
            let values =
                metric_data_values(&emulator, case, INVOCATIONS_SENT_TO_DLQ_METRIC, &dimensions)
                    .await;
            assert!(
                values.is_empty(),
                "case {case:?}: expected no datapoint over {dimensions}, got {values:?}"
            );
        }

        // And the failing rule's delivery is counted as the failure it was, while
        // the sibling's is counted as the invocation it was: the parked-event
        // counter is a fact about one rule's handover, not about the bus.
        for (metric, rule, expected) in [
            (FAILED_INVOCATIONS_METRIC, FAILING_RULE, want),
            (FAILED_INVOCATIONS_METRIC, SIBLING_RULE, 0.0),
            (INVOCATIONS_METRIC, SIBLING_RULE, want),
            (INVOCATIONS_METRIC, FAILING_RULE, 0.0),
        ] {
            let got = metric_sum(&emulator, metric, DEFAULT_EVENT_BUS_NAME, rule).await;
            assert_eq!(
                got, expected,
                "case {case:?}: expected {metric} for rule {rule} to sum to {expected}, got {got}"
            );
        }

        // The series CloudWatch holds for this counter are the failing rule's and
        // nothing else: a reader naming the sibling rule finds no series at all for
        // it, which is what "no datapoint" means to a client.
        let held: Vec<Value> = published_series(&emulator)
            .await
            .into_iter()
            .filter(|series| series["MetricName"] == json!(INVOCATIONS_SENT_TO_DLQ_METRIC))
            .collect();
        let expected_series = rule_series(
            DEFAULT_EVENT_BUS_NAME,
            FAILING_RULE,
            &[INVOCATIONS_SENT_TO_DLQ_METRIC],
        );
        assert_eq!(
            ordered(&held),
            ordered(&expected_series),
            "case {case:?}: expected the parked-event counter to be published for the failing \
             rule alone, got {held:?}"
        );
    }
}
