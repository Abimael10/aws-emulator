// SPDX-License-Identifier: CC0-1.0

//! What a delivery to an AWS AppSync GraphQL ENDPOINT target is ACCOUNTED as, read where a
//! client reads it: in the table the API's resolver writes to, on the target's dead-letter
//! queue, and on the rule's own AWS/Events counters through CloudWatch.
//!
//! The RULE asserted here: for every event a rule's `EventPattern` matches, EventBridge hands
//! the event over to each target and counts the handover on that rule's own AWS/Events
//! counters — `Invocations` (with `SuccessfulInvocationAttempts`) for a handover the target's
//! service ANSWERED, whatever that answer says about the work it did, and
//! `FailedInvocations` (with `InvocationsSentToDlq`) for one its role's permissions barred,
//! which is also the handover whose event is parked on `DeadLetterConfig.Arn` carrying the
//! rule, the target and the condition — while an event the pattern does not match (matching
//! is case-sensitive, so a value differing only in case matches nothing) is handed over to no
//! target at all, so it is neither executed, nor parked, nor counted.
//!
//! The rows span what the rule generalises over: the ANSWER the API gives a delivery it
//! accepted (a 200 that resolved the field, and a 200 carrying a resolver error instead of a
//! value), WHY a delivery is barred (a role holding another AppSync action rather than
//! `appsync:GraphQL`), the BUS the rule lives on (the default one and a custom one, whose
//! rules are separate metrics), and — in the second test — WHICH part of the event a
//! near-miss differs from the pattern in (the source, the detail type, a member of the
//! detail), each differing in CASE alone.
//!
//! The harness is the one [`crate::appsync_field_target_delivery`] owns: EventBridge with
//! AppSync, Amazon DynamoDB, IAM, the token service, Amazon SQS and CloudWatch mounted beside
//! it on one engine.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    DEFAULT_EVENT_BUS_NAME, EVENT_BUS_NAME_DIMENSION, FAILED_INVOCATIONS_METRIC,
    INVOCATIONS_METRIC, INVOCATIONS_SENT_TO_DLQ_METRIC, INVOCATION_ATTEMPTS_METRIC,
    MATCHED_EVENTS_METRIC, METRIC_NAMESPACE, NO_PERMISSIONS, RULE_NAME_DIMENSION,
    SERVICE_PRINCIPAL, SUCCESSFUL_INVOCATION_ATTEMPTS_METRIC,
};

use crate::appsync_field_target_delivery::{
    dead_letters, emulator, endpoint_arn, events, given_api, given_dead_letter_queue, given_role,
    given_table, item, rpc, Emulator, GraphqlApi, APPSYNC_PRINCIPAL, PUT_OPERATION,
    REFUSE_OPERATION, SOURCE, UNAUTHORIZED_ERROR_TYPE,
};

/// The IAM action the delivery performs, which the target's role is judged on, and the other
/// AppSync action a role may hold instead — one that says nothing about executing an
/// operation.
const GRAPHQL_ACTION: &str = "appsync:GraphQL";
const CONTROL_PLANE_ACTION: &str = "appsync:GetGraphqlApi";

/// The id of the one target every rule below carries.
const TARGET_ID: &str = "graphql";

/// One CloudWatch call (`GraniteServiceVersion20100801`, awsJson1_0) — the plane a client
/// reads a rule's counters through.
async fn cloudwatch(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        emulator,
        "monitoring",
        "GraniteServiceVersion20100801",
        "1.0",
        operation,
        body,
    )
    .await
}

/// Seconds since the epoch, the form `GetMetricStatistics` takes its window in.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// The `Sum` of `metric` for the rule `rule` of the bus `bus`, in EventBridge's own
/// namespace, over a window wide enough around now that no assertion depends on how fast the
/// host runs.
async fn metric_sum(emulator: &Emulator, metric: &str, bus: &str, rule: &str) -> f64 {
    let now = epoch_seconds();
    let (status, stats) = cloudwatch(
        emulator,
        "GetMetricStatistics",
        &json!({
            "Namespace": METRIC_NAMESPACE,
            "MetricName": metric,
            "Dimensions": [
                {"Name": EVENT_BUS_NAME_DIMENSION, "Value": bus},
                {"Name": RULE_NAME_DIMENSION, "Value": rule},
            ],
            "StartTime": now - 300.0,
            "EndTime": now + 300.0,
            "Period": 600,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetMetricStatistics({metric}) answered {status} {stats}"
    );
    stats["Datapoints"]
        .as_array()
        .map(|points| {
            points
                .iter()
                .filter_map(|point| point["Sum"].as_f64())
                .sum::<f64>()
        })
        .unwrap_or_default()
}

/// Every handover counter of `rule` on `bus`, each asserted against what this delivery owes
/// it.
async fn assert_counted(emulator: &Emulator, case: &str, bus: &str, rule: &str, want: &Counted) {
    let rows = [
        (INVOCATIONS_METRIC, want.invocations),
        (
            SUCCESSFUL_INVOCATION_ATTEMPTS_METRIC,
            want.successful_attempts,
        ),
        (FAILED_INVOCATIONS_METRIC, want.failed_invocations),
        (INVOCATION_ATTEMPTS_METRIC, want.attempts),
        (INVOCATIONS_SENT_TO_DLQ_METRIC, want.sent_to_dlq),
    ];
    for (metric, expected) in rows {
        let got = metric_sum(emulator, metric, bus, rule).await;
        assert_eq!(
            got, expected,
            "case {case:?}: expected {metric} of rule {rule} on bus {bus} to sum to \
             {expected}, got {got}"
        );
    }
}

/// What a round owes each of the rule's handover counters.
struct Counted {
    invocations: f64,
    successful_attempts: f64,
    failed_invocations: f64,
    attempts: f64,
    sent_to_dlq: f64,
}

/// The counters a delivery the API ANSWERED owes: one invocation, counted as the one attempt
/// it was made in, and nothing failed or parked.
const ACCEPTED: Counted = Counted {
    invocations: 1.0,
    successful_attempts: 1.0,
    failed_invocations: 0.0,
    attempts: 1.0,
    sent_to_dlq: 0.0,
};

/// The counters a delivery the target's role barred owes: no invocation, one failed one, and
/// the event parked.
const BARRED: Counted = Counted {
    invocations: 0.0,
    successful_attempts: 0.0,
    failed_invocations: 1.0,
    attempts: 1.0,
    sent_to_dlq: 1.0,
};

/// The bus a row's rule lives on, created when it is not the default one.
async fn given_bus(emulator: &Emulator, case: &str, bus: &str) {
    if bus == DEFAULT_EVENT_BUS_NAME {
        return;
    }
    let (status, created) = events(emulator, "CreateEventBus", &json!({"Name": bus})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventBus answered {status} {created}"
    );
}

/// The rule one case wires up: where it lives, what it matches, and the one AppSync endpoint
/// target it carries.
struct RuleSpec<'a> {
    bus: &'a str,
    name: &'a str,
    pattern: Value,
    /// The endpoint ARN the target names and the role it is invoked under.
    target_arn: &'a str,
    role_arn: &'a str,
    /// The operation the target carries, and the variables its document declares.
    operation: &'a str,
    variables: &'a [&'a str],
    /// The queue the target parks an undeliverable event on.
    dead_letter_arn: &'a str,
}

/// The rule `spec` describes, created with its target; answers the ARN EventBridge reports
/// for it.
async fn given_rule(emulator: &Emulator, case: &str, spec: &RuleSpec<'_>) -> String {
    let RuleSpec {
        bus,
        name,
        pattern,
        target_arn,
        role_arn,
        operation,
        variables,
        dead_letter_arn,
    } = spec;
    let (status, rule) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": bus,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRule answered {status} {rule}"
    );
    let rule_arn = rule["RuleArn"].as_str().unwrap_or_default().to_string();
    assert!(
        !rule_arn.is_empty(),
        "case {case:?}: PutRule reports the ARN the rule is held under, got {rule}"
    );
    let (status, written) = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": bus,
            "Targets": [{
                "Id": TARGET_ID,
                "Arn": target_arn,
                "RoleArn": role_arn,
                "AppSyncParameters": {"GraphQLOperation": operation},
                "InputTransformer": input_transformer(variables),
                "RetryPolicy": {"MaximumRetryAttempts": 0, "MaximumEventAgeInSeconds": 60},
                "DeadLetterConfig": {"Arn": dead_letter_arn},
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {written}"
    );
    assert_eq!(
        written["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {written}"
    );
    rule_arn
}

/// The `InputTransformer` rendering the named members of the event's detail into the
/// variables object the operation's document declares — one path and one placeholder per
/// variable it takes.
fn input_transformer(variables: &[&str]) -> Value {
    let paths: Map<String, Value> = variables
        .iter()
        .map(|name| ((*name).to_string(), json!(format!("$.detail.{name}"))))
        .collect();
    let rendered = variables
        .iter()
        .map(|name| format!("\"{name}\": <{name}>"))
        .collect::<Vec<_>>()
        .join(", ");
    json!({"InputPathsMap": paths, "InputTemplate": format!("{{{rendered}}}")})
}

/// Publish ONE event onto `bus`, from `source`, of `detail_type`, carrying `detail`.
async fn publish(
    emulator: &Emulator,
    case: &str,
    bus: &str,
    source: &str,
    detail_type: &str,
    detail: &Value,
) {
    let (status, published) = events(
        emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": bus,
                "Source": source,
                "DetailType": detail_type,
                "Detail": detail.to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutEvents answered {status} {published}"
    );
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutEvents reported a failed entry: {published}"
    );
}

/// The API every case below delivers to, with the data source's own role, created once.
async fn given_the_api(emulator: &Emulator, case: &str) -> GraphqlApi {
    let table_arn = given_table(emulator, case).await;
    let data_source_role = given_role(
        emulator,
        case,
        "appsync-items",
        APPSYNC_PRINCIPAL,
        Some(("dynamodb:*", &table_arn)),
    )
    .await;
    given_api(emulator, case, &data_source_role).await
}

/// The detail type every rule of the accounting table matches on.
const DETAIL_TYPE: &str = "ItemArrived";

/// Which role a row's target names.
#[derive(Debug, Clone, Copy)]
enum Role {
    /// One `events.amazonaws.com` can assume, allowed `appsync:GraphQL`.
    Granted,
    /// One `events.amazonaws.com` can assume that holds another AppSync action instead, so it
    /// is allowed to perform no GraphQL operation at all.
    WithoutTheAction,
}

/// One row of the accounting table.
struct Row {
    case: &'static str,
    /// The bus the row's rule lives on.
    bus: &'static str,
    /// The operation the target carries and the variables its document declares.
    operation: &'static str,
    variables: &'static [&'static str],
    role: Role,
    /// The condition the delivery must fail with, `None` for one the API answered.
    failed_with: Option<&'static str>,
    /// Whether the table holds the row's item afterwards.
    writes: bool,
}

/// A delivery to an AppSync endpoint target is counted on the matched rule's own AWS/Events
/// counters, and a delivery the target's role bars is the one that is parked on the target's
/// dead-letter queue naming the rule, the target and the condition.
#[tokio::test]
async fn an_appsync_endpoint_delivery_is_counted_on_the_rule_it_was_matched_by() {
    let setup = "the API every row of the accounting table delivers to";
    let emulator = emulator();
    let api = given_the_api(&emulator, setup).await;
    let granted = given_role(
        &emulator,
        setup,
        "events-graphql",
        SERVICE_PRINCIPAL,
        Some((GRAPHQL_ACTION, "*")),
    )
    .await;
    // The role of the recorded denial: trusted by the EventBridge service principal, holding
    // an AppSync action that is not the one a delivery performs.
    let without_the_action = given_role(
        &emulator,
        setup,
        "events-control-plane-only",
        SERVICE_PRINCIPAL,
        Some((CONTROL_PLANE_ACTION, &api.arn)),
    )
    .await;

    let rows: &[Row] = &[
        Row {
            case: "a mutation the role is granted, answered with the item it wrote",
            bus: DEFAULT_EVENT_BUS_NAME,
            operation: PUT_OPERATION,
            variables: &["id", "kind"],
            role: Role::Granted,
            failed_with: None,
            writes: true,
        },
        Row {
            // A GraphQL answer is a 200 whether or not every field resolved, so an answer
            // carrying a resolver error is an event the target ACCEPTED.
            case: "a 200 carrying a resolver error instead of a value",
            bus: "appsync-deliveries",
            operation: REFUSE_OPERATION,
            variables: &["id"],
            role: Role::Granted,
            failed_with: None,
            writes: false,
        },
        Row {
            case: "a role holding another AppSync action rather than appsync:GraphQL",
            bus: "appsync-denials",
            operation: PUT_OPERATION,
            variables: &["id", "kind"],
            role: Role::WithoutTheAction,
            failed_with: Some(NO_PERMISSIONS),
            writes: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let id = format!("accounted-{index}");
        let rule = format!("appsync-endpoint-{index}");
        let role = match row.role {
            Role::Granted => &granted,
            Role::WithoutTheAction => &without_the_action,
        };
        let target_arn = endpoint_arn(&api.api_id);
        let (queue_url, queue_arn) =
            given_dead_letter_queue(&emulator, case, &format!("accounting-dlq-{index}")).await;
        given_bus(&emulator, case, row.bus).await;
        let rule_arn = given_rule(
            &emulator,
            case,
            &RuleSpec {
                bus: row.bus,
                name: &rule,
                pattern: json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}),
                target_arn: &target_arn,
                role_arn: role,
                operation: row.operation,
                variables: row.variables,
                dead_letter_arn: &queue_arn,
            },
        )
        .await;

        publish(
            &emulator,
            case,
            row.bus,
            SOURCE,
            DETAIL_TYPE,
            &json!({"id": id, "kind": format!("kind-{index}")}),
        )
        .await;

        // What the delivery DID: the resolver's write, which only the operation the API
        // executed could have made.
        let held = item(&emulator, case, &id).await;
        assert_eq!(
            held.is_some(),
            row.writes,
            "case {case:?}: expected the table to hold the mapped item: {}, got {held:?}",
            row.writes
        );

        // What the delivery came to, as the rule's counters report it.
        let want = if row.failed_with.is_some() {
            &BARRED
        } else {
            &ACCEPTED
        };
        assert_counted(&emulator, case, row.bus, &rule, want).await;

        // And the dead letter: one per barred delivery, naming the rule EventBridge reported,
        // the target it could not reach and the condition; none for one the API answered.
        let parked = dead_letters(&emulator, case, &queue_url).await;
        match row.failed_with {
            None => assert!(
                parked.is_empty(),
                "case {case:?}: a delivery the API answered parks nothing, got {parked:?}"
            ),
            Some(expected) => {
                assert_eq!(
                    parked.len(),
                    1,
                    "case {case:?}: a barred delivery parks the event once, got {parked:?}"
                );
                let attributes = &parked[0]["MessageAttributes"];
                let attribute = |name: &str| {
                    attributes[name]["StringValue"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                };
                assert_eq!(
                    attribute("ERROR_CODE"),
                    expected,
                    "case {case:?}: the dead letter names the condition, got {:?}",
                    parked[0]
                );
                assert_eq!(
                    attribute("RULE_ARN"),
                    rule_arn,
                    "case {case:?}: the dead letter names the rule that matched, got {:?}",
                    parked[0]
                );
                assert_eq!(
                    attribute("TARGET_ARN"),
                    target_arn,
                    "case {case:?}: the dead letter names the target, got {:?}",
                    parked[0]
                );
                let reported = attribute("ERROR_MESSAGE");
                assert!(
                    reported.contains(UNAUTHORIZED_ERROR_TYPE),
                    "case {case:?}: the dead letter reports the answer the endpoint gives an \
                     identity it has not authorized, got {reported:?}"
                );
            }
        }
    }
}

/// One near-miss row: the case, and the source, detail type and detail-member value the event
/// carries — each row differing from the pattern in CASE alone, in one of the three.
struct NearMiss {
    case: &'static str,
    id: &'static str,
    source: &'static str,
    detail_type: &'static str,
    kind: &'static str,
}

/// The detail-member value the near-miss rule's pattern requires.
const MATCHED_KIND: &str = "widget";

/// An event that differs from the rule's pattern in case alone matches nothing, so it is
/// handed over to the AppSync target neither before nor after an event that DOES match: it
/// writes nothing, parks nothing and is counted nowhere.
#[tokio::test]
async fn a_near_miss_event_is_not_handed_over_to_the_appsync_target() {
    let case = "a live rule delivering to an AppSync endpoint";
    let bus = "near-miss-bus";
    let rule = "appsync-endpoint-near-miss";
    let emulator = emulator();
    let api = given_the_api(&emulator, case).await;
    let granted = given_role(
        &emulator,
        case,
        "events-graphql",
        SERVICE_PRINCIPAL,
        Some((GRAPHQL_ACTION, "*")),
    )
    .await;
    let (queue_url, queue_arn) = given_dead_letter_queue(&emulator, case, "near-miss-dlq").await;
    given_bus(&emulator, case, bus).await;
    given_rule(
        &emulator,
        case,
        &RuleSpec {
            bus,
            name: rule,
            pattern: json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "detail": {"kind": [MATCHED_KIND]},
            }),
            target_arn: &endpoint_arn(&api.api_id),
            role_arn: &granted,
            operation: PUT_OPERATION,
            variables: &["id", "kind"],
            dead_letter_arn: &queue_arn,
        },
    )
    .await;

    // Each near-miss differs from the pattern in the case of ONE of the three members the
    // pattern constrains.
    let near_misses: &[NearMiss] = &[
        NearMiss {
            case: "a source differing in case",
            id: "near-source",
            source: "COM.DEMO.APPSYNC",
            detail_type: DETAIL_TYPE,
            kind: MATCHED_KIND,
        },
        NearMiss {
            case: "a detail type differing in case",
            id: "near-detail-type",
            source: SOURCE,
            detail_type: "itemarrived",
            kind: MATCHED_KIND,
        },
        NearMiss {
            case: "a detail member differing in case",
            id: "near-detail-member",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            kind: "Widget",
        },
    ];

    // The near-miss events first, so the suppression is not the pipeline being cold…
    for near_miss in near_misses {
        publish(
            &emulator,
            near_miss.case,
            bus,
            near_miss.source,
            near_miss.detail_type,
            &json!({"id": near_miss.id, "kind": near_miss.kind}),
        )
        .await;
    }
    // …and then the one event the pattern matches, which the same rule delivers.
    publish(
        &emulator,
        case,
        bus,
        SOURCE,
        DETAIL_TYPE,
        &json!({"id": "matched", "kind": MATCHED_KIND}),
    )
    .await;

    let delivered = item(&emulator, case, "matched").await;
    assert!(
        delivered.is_some(),
        "case {case:?}: the matched event's operation writes its item, got {delivered:?}"
    );
    for near_miss in near_misses {
        let held = item(&emulator, near_miss.case, near_miss.id).await;
        assert!(
            held.is_none(),
            "case {:?}: an event the pattern does not match performs no operation, got {held:?}",
            near_miss.case
        );
    }
    let parked = dead_letters(&emulator, case, &queue_url).await;
    assert!(
        parked.is_empty(),
        "case {case:?}: an event matched by no rule is not undeliverable, so nothing is \
         parked, got {parked:?}"
    );

    // One handover, counted once: the matched event's. The near-misses are not counted even
    // as matches of this rule.
    assert_counted(&emulator, case, bus, rule, &ACCEPTED).await;
    let matched = metric_sum(&emulator, MATCHED_EVENTS_METRIC, bus, rule).await;
    assert_eq!(
        matched, 1.0,
        "case {case:?}: expected {MATCHED_EVENTS_METRIC} of rule {rule} to sum to 1, got \
         {matched}"
    );
}
