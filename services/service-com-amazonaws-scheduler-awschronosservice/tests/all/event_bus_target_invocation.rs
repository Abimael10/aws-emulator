// SPDX-License-Identifier: CC0-1.0

//! A schedule whose `Target.Arn` names an EVENT BUS is invoked: end to end over the real
//! `MultiServiceRouter`, with Amazon EventBridge, Amazon SQS and IAM mounted beside
//! EventBridge Scheduler on ONE engine — so the invocation is asserted where a client
//! observes it, on the queue the bus's own rule delivers to.
//!
//! RULE: `Target.EventBridgeParameters` is "the templated target type for the EventBridge
//! `PutEvents` API operation", so at each occurrence EventBridge Scheduler acts as
//! `Target.RoleArn` and makes exactly that call against the bus `Target.Arn` names, with
//! ONE entry: `Source` and `DetailType` as those parameters state them and `Detail` the
//! rendered `Target.Input` — "the text, or well-formed JSON, passed to the target", which
//! for a templated EventBridge target is the event's own detail. The published event is
//! then routed by the bus like any other, so every rule whose `EventPattern` matches that
//! envelope hands it to its own targets, and a rule whose pattern matches nothing of it is
//! handed nothing.
//!
//! SECOND RULE, asserted in [`a_source_outside_the_modelled_expression_is_refused`]: the
//! `Source` such a target carries is constrained by the `@pattern`
//! `EventBridgeParameters$Source` declares, and a value outside it — one claiming the
//! namespace AWS's own services publish under, or one opening outside the expression's
//! character class — is refused when the schedule is written, with the constraint clause
//! this service states every such refusal in.
//!
//! The rows span the class rather than one recorded case: they differ along WHICH bus the
//! target names (a bus of the account's own, the account's default bus), along the
//! `Source`/`DetailType` pair, along the FORM of the input the detail is (a flat object, a
//! nested document carrying a list, one naming a schedule context attribute) and along
//! what the rule's `EventPattern` filters on (the envelope's source and detail type, a
//! member inside the detail).
//!
//! Nothing here sleeps for a span: each wait is for the CONDITION it is about — the
//! message on the queue — looked at again until it holds or a generous deadline passes,
//! and no assertion reads the wall clock.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::invocation;
use smithy_server_runtime::MultiServiceRouter;

use crate::dead_letter_delivery::{body_document, messages_once, PATIENCE};
use crate::schedule_invocation::{
    at_expression_body, bodies, bodies_once, given_execution_role_for, given_queue, iam, peek,
    scheduler, seconds_from_now, sqs, POLL_INTERVAL, REGION,
};
use crate::wire_harness::{account_id, error_code, error_message};

/// How far ahead of the clock every schedule below states its one occurrence: room for
/// the whole table to be written before the instant passes, since a one-time schedule
/// written after its instant names no occurrence at all.
const OCCURRENCE_IN: u64 = 12;

/// How long a queue that must stay empty is watched for, once the delivery that proves
/// the clock has run past the occurrence has already been observed.
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// The service principal a rule's queue target grants the delivery to: "for Lambda
/// functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
/// resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The IAM action a bus target's invocation is authorized by, which is the one API a
/// templated EventBridge target is invoked by.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The members of the `PutEvents` request a bus target's FAILED event is reported as:
/// that API takes its events as a list of entries, each naming the bus, the source, the
/// detail type and the detail (`smithy_query` query="operation" operation="PutEvents" on
/// Amazon EventBridge's model: `Entries` of `PutEventsRequestEntry{EventBusName, Source,
/// DetailType, Detail}`).
const ENTRIES_MEMBER: &str = "Entries";
/// See [`ENTRIES_MEMBER`].
const EVENT_BUS_NAME_MEMBER: &str = "EventBusName";
/// See [`ENTRIES_MEMBER`].
const SOURCE_MEMBER: &str = "Source";
/// See [`ENTRIES_MEMBER`].
const DETAIL_TYPE_MEMBER: &str = "DetailType";
/// See [`ENTRIES_MEMBER`].
const DETAIL_MEMBER: &str = "Detail";

/// The emulator: EventBridge Scheduler, Amazon EventBridge, Amazon SQS and IAM on ONE
/// engine — the shape `core/router/src/lib.rs` mounts them in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call against Amazon EventBridge (`AWSEvents.<Operation>`).
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A bus of the account's own called `name`, and the ARN EventBridge reports for it.
async fn given_bus(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = events(router, "CreateEventBus", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventBus answered {status} {created}"
    );
    created["EventBusArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no EventBusArn in {created}"))
        .to_string()
}

/// The ARN of the account's DEFAULT bus, which every account has without creating one.
fn default_bus_arn() -> String {
    format!(
        "arn:aws:events:{REGION}:{}:event-bus/default",
        account_id()
    )
}

/// The bus NAME an event-bus ARN addresses: the whole remainder after the `event-bus/`
/// resource-type segment, which is how `EventBusNameOrArn` reduces an ARN.
fn bus_name_of(bus_arn: &str) -> String {
    bus_arn
        .split_once(":event-bus/")
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| panic!("{bus_arn} names no event bus"))
}

/// Let the rule `rule` of the bus `bus` deliver to the queue `queue_arn`: the rule
/// itself, with `pattern` as its `EventPattern`, the target, and the queue policy that
/// admits the delivery.
async fn given_rule_to_queue(
    router: &MultiServiceRouter,
    case: &str,
    bus: &str,
    rule: &str,
    pattern: &Value,
    queue_url: &str,
    queue_arn: &str,
) {
    let (status, written) = events(
        router,
        "PutRule",
        &json!({
            "Name": rule,
            "EventBusName": bus,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRule answered {status} {written}"
    );
    let (status, targeted) = events(
        router,
        "PutTargets",
        &json!({
            "Rule": rule,
            "EventBusName": bus,
            "Targets": [{ "Id": format!("{rule}-queue"), "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {targeted}"
    );
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: PutTargets reported a failed entry: {targeted}"
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string();
    let (status, set) = sqs(
        router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {set}"
    );
}

/// Grant the role `role` the `action` on `resource`, beside the trust policy it was
/// created with: the pair AWS documents for a schedule's execution role is that trust
/// plus the permission the schedule's own call needs.
async fn given_grant(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    action: &str,
    resource: &str,
) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", "invoke-the-target"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy answered {status}: {written}"
    );
}

/// A schedule called `name` whose one occurrence is `occurrence`, pointed at the event
/// bus `bus_arn` with the source and detail type `EventBridgeParameters` states.
async fn given_bus_schedule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    occurrence: SystemTime,
    target: Value,
) -> String {
    let (status, created) = scheduler(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(json!({
            "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": target,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSchedule answered {status} {created}"
    );
    // The schedule is read back through the service's own reader, so what is invoked is
    // the record a client sees.
    let (status, read) = scheduler(
        router,
        "GET",
        &format!("/schedules/{name}?groupName=default"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetSchedule answered {status} {read}"
    );
    assert_eq!(
        read["State"].as_str(),
        Some("ENABLED"),
        "case {case:?}: GetSchedule reports another State: {read}"
    );
    created["ScheduleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ScheduleArn in {created}"))
        .to_string()
}

/// One row: which bus the target names, the pair its parameters state, the input the
/// event's detail is, and what the bus's rule filters on.
struct Row {
    case: &'static str,
    /// Whether the row's target names a bus of its OWN, or the account's default bus.
    own_bus: bool,
    source: &'static str,
    detail_type: &'static str,
    /// `Target.Input`, built from the row's own probe value.
    input: fn(&str) -> Value,
    /// The `detail` the delivered envelope must carry: the input with the schedule's
    /// context attributes substituted, over the probe and the occurrence's scheduled
    /// time.
    expected_detail: fn(&str, &str) -> Value,
    /// A `(member, value)` pair inside `detail` the rule's pattern filters on as well,
    /// or `None` for a pattern on the source and detail type alone.
    detail_filter: Option<(&'static str, &'static str)>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a bus of the account's own, matched on the source and detail type",
        own_bus: true,
        source: "orders.nightly",
        detail_type: "Nightly Roll-Up",
        input: |probe| json!({ "probe": probe }),
        expected_detail: |probe, _| json!({ "probe": probe }),
        detail_filter: None,
    },
    Row {
        case: "the account's default bus, with a nested detail carrying a list",
        own_bus: false,
        source: "inventory.sweep",
        detail_type: "Inventory Sweep",
        input: |probe| {
            json!({ "probe": probe, "items": [1, 2, 3], "nested": { "deep": true } })
        },
        expected_detail: |probe, _| {
            json!({ "probe": probe, "items": [1, 2, 3], "nested": { "deep": true } })
        },
        detail_filter: None,
    },
    Row {
        case: "a pattern reaching into the event's own detail",
        own_bus: true,
        source: "billing.cycle",
        detail_type: "Billing Cycle Closed",
        input: |probe| json!({ "probe": probe, "stage": "closed" }),
        expected_detail: |probe, _| json!({ "probe": probe, "stage": "closed" }),
        detail_filter: Some(("stage", "closed")),
    },
    Row {
        case: "an input naming the occurrence's scheduled time",
        own_bus: true,
        source: "audit.trail",
        detail_type: "Audit Checkpoint",
        input: |probe| json!({ "probe": probe, "at": "<aws.scheduler.scheduled-time>" }),
        expected_detail: |probe, at| json!({ "probe": probe, "at": at }),
        detail_filter: None,
    },
];

/// What one row left behind, read once its occurrence has passed.
struct Prepared<'row> {
    row: &'row Row,
    probe: String,
    queue_url: String,
    /// The occurrence this row's schedule names, as its context attributes carry it: each
    /// row's instant is taken when that row's schedule is WRITTEN, so a slow or loaded
    /// host cannot let the whole table's instant pass during the setup of the rows after
    /// the first.
    scheduled_time: String,
}

/// Every row: the occurrence publishes ONE event onto the bus the target names, carrying
/// the source and detail type the target's parameters state and the rendered input as its
/// detail, and the bus's matching rule hands that envelope to its own queue target.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bus_target_publishes_the_occurrence_onto_the_bus_it_names() {
    let emulator = emulator();
    // Every row's schedule is written on ONE emulator, each with its own bus, rule, queue
    // and role, and each with its own occurrence taken as that schedule is written.
    let mut prepared: Vec<Prepared<'_>> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let bus_arn = if row.own_bus {
            given_bus(&emulator, case, &format!("bus-target-bus-{index}")).await
        } else {
            default_bus_arn()
        };
        let bus = bus_name_of(&bus_arn);
        let (queue_url, queue_arn) =
            given_queue(&emulator, case, &format!("bus-target-queue-{index}")).await;
        let mut pattern = json!({
            "source": [row.source],
            "detail-type": [row.detail_type],
        });
        if let Some((member, value)) = row.detail_filter {
            pattern["detail"] = json!({ member: [value] });
        }
        given_rule_to_queue(
            &emulator,
            case,
            &bus,
            &format!("bus-target-rule-{index}"),
            &pattern,
            &queue_url,
            &queue_arn,
        )
        .await;
        let role_name = format!("bus-target-role-{index}");
        // The role is created with the scheduler trust policy and no permission of its
        // own, and then granted the one call this schedule makes.
        let role = given_execution_role_for(&emulator, case, &role_name, &[]).await;
        given_grant(&emulator, case, &role_name, PUT_EVENTS_ACTION, &bus_arn).await;
        let probe = format!("probe-{index}");
        let occurrence = seconds_from_now(OCCURRENCE_IN);
        given_bus_schedule(
            &emulator,
            case,
            &format!("bus-target-{index}"),
            occurrence,
            json!({
                "Arn": bus_arn,
                "RoleArn": role,
                "Input": (row.input)(&probe).to_string(),
                "EventBridgeParameters": {
                    "Source": row.source,
                    "DetailType": row.detail_type,
                },
            }),
        )
        .await;
        prepared.push(Prepared {
            row,
            probe,
            queue_url,
            scheduled_time: invocation::scheduled_time_text(occurrence),
        });
    }

    for entry in &prepared {
        let case = entry.row.case;
        let held = bodies_once(&emulator, &entry.queue_url, 1, PATIENCE).await;
        let first = held.first().unwrap_or_else(|| {
            panic!("case {case:?}: no event carrying {:?} reached the rule's queue", entry.probe)
        });
        let envelope: Value = serde_json::from_str(first).unwrap_or_else(|error| {
            panic!("case {case:?}: the delivered body is not JSON ({error}): {first:?}")
        });
        assert_eq!(
            envelope["source"].as_str(),
            Some(entry.row.source),
            "case {case:?}: expected the Source the target's parameters state, got {envelope}"
        );
        assert_eq!(
            envelope["detail-type"].as_str(),
            Some(entry.row.detail_type),
            "case {case:?}: expected the DetailType the target's parameters state, got {envelope}"
        );
        let expected = (entry.row.expected_detail)(&entry.probe, &entry.scheduled_time);
        assert_eq!(
            envelope["detail"], expected,
            "case {case:?}: expected the detail {expected}, got {envelope}"
        );
        // The bus published the event in its own Region and account, which is what makes
        // the envelope a real publication rather than the payload echoed on.
        assert_eq!(
            envelope["region"].as_str(),
            Some(REGION),
            "case {case:?}: expected the bus's own Region, got {envelope}"
        );
        assert_eq!(
            envelope["account"].as_str(),
            Some(account_id()),
            "case {case:?}: expected the bus's own account, got {envelope}"
        );
        // The schedule names ONE occurrence, so exactly one event is published for it —
        // looked at again over the quiet window, since the wait above returns as soon as
        // the first one lands and a second would arrive a moment later.
        let deadline = Instant::now() + QUIET_WINDOW;
        while Instant::now() < deadline {
            let since = bodies(&emulator, &entry.queue_url).await;
            assert_eq!(
                since.len(),
                1,
                "case {case:?}: expected one published event, got {since:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// A bus target reaches only the rules the published envelope MATCHES: the event carries
/// the source and detail type the target's parameters state, so a rule of the same bus
/// whose pattern names another source is handed nothing, while the rule that matches is
/// handed the envelope. The matching rule's delivery is the row's own control: once it
/// has arrived the clock has already published the occurrence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bus_target_reaches_only_the_rules_its_envelope_matches() {
    let case = "two rules of one bus, one of which names another source";
    let emulator = emulator();
    let bus_arn = given_bus(&emulator, case, "selective-bus").await;
    let bus = bus_name_of(&bus_arn);
    let source = "shipments.dispatch";
    let detail_type = "Shipment Dispatched";

    let (matching_url, matching_arn) = given_queue(&emulator, case, "selective-matching").await;
    given_rule_to_queue(
        &emulator,
        case,
        &bus,
        "selective-matching-rule",
        &json!({ "source": [source], "detail-type": [detail_type] }),
        &matching_url,
        &matching_arn,
    )
    .await;
    let (other_url, other_arn) = given_queue(&emulator, case, "selective-other").await;
    given_rule_to_queue(
        &emulator,
        case,
        &bus,
        "selective-other-rule",
        &json!({ "source": ["something.else"], "detail-type": [detail_type] }),
        &other_url,
        &other_arn,
    )
    .await;

    let role_name = "selective-role";
    let role = given_execution_role_for(&emulator, case, role_name, &[]).await;
    given_grant(&emulator, case, role_name, PUT_EVENTS_ACTION, &bus_arn).await;
    given_bus_schedule(
        &emulator,
        case,
        "selective",
        seconds_from_now(OCCURRENCE_IN),
        json!({
            "Arn": bus_arn,
            "RoleArn": role,
            "Input": json!({ "probe": "selective" }).to_string(),
            "EventBridgeParameters": { "Source": source, "DetailType": detail_type },
        }),
    )
    .await;

    let held = bodies_once(&emulator, &matching_url, 1, PATIENCE).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the matching rule's queue to hold the envelope, got {held:?}"
    );
    // The envelope was dispatched, so a queue still empty here was never a target of it.
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        let other = peek(&emulator, &other_url).await;
        assert!(
            other.is_empty(),
            "case {case:?}: expected the unmatched rule's queue to hold nothing, got {other:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An invocation of a bus target the execution role may not make is reported to the
/// schedule's dead-letter queue as the REQUEST it attempted: the `PutEvents` request with
/// the one entry that publication would have carried, so a consumer replays it by
/// resubmitting that document.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withheld_bus_publication_is_dead_lettered_as_its_put_events_request() {
    let case = "a role granted nothing on the bus";
    let emulator = emulator();
    let bus_arn = given_bus(&emulator, case, "dead-letter-bus").await;
    let (dead_letter_url, dead_letter_arn) =
        given_queue(&emulator, case, "bus-target-dlq").await;
    // The role may write to the dead-letter queue and nothing else, so the publication is
    // withheld and its report is still delivered.
    let role = given_execution_role_for(
        &emulator,
        case,
        "bus-dead-letter-role",
        &[dead_letter_arn.as_str()],
    )
    .await;
    let source = "returns.intake";
    let detail_type = "Return Received";
    let input = json!({ "probe": "withheld" }).to_string();
    given_bus_schedule(
        &emulator,
        case,
        "bus-dead-letter",
        seconds_from_now(OCCURRENCE_IN),
        json!({
            "Arn": bus_arn,
            "RoleArn": role,
            "Input": input,
            "EventBridgeParameters": { "Source": source, "DetailType": detail_type },
            // A refusal of the request is the invocation's last attempt in any case; the
            // policy is stated so the report is of an invocation that retried nothing.
            "RetryPolicy": { "MaximumRetryAttempts": 0 },
            "DeadLetterConfig": { "Arn": dead_letter_arn },
        }),
    )
    .await;

    let reported = messages_once(&emulator, &dead_letter_url, 1, PATIENCE).await;
    let message = reported
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: nothing was dead-lettered"));
    let document = body_document(message);
    let expected = json!({
        ENTRIES_MEMBER: [{
            EVENT_BUS_NAME_MEMBER: bus_arn,
            SOURCE_MEMBER: source,
            DETAIL_TYPE_MEMBER: detail_type,
            DETAIL_MEMBER: input,
        }],
    });
    assert_eq!(
        document, expected,
        "case {case:?}: expected the dead-lettered request {expected}, got {message}"
    );
}

/// The `Source` such a target carries is held to the `@pattern` its own member declares:
/// a value outside that expression is refused when the schedule is WRITTEN — by
/// `CreateSchedule` and by `UpdateSchedule` alike — and nothing is stored or changed for
/// it, while every value the expression admits is accepted.
///
/// The rows span the expression rather than one value: they differ along which branch of
/// it the value satisfies (its opening character class, the reference-path branch), and
/// run the near misses of the namespace its look-ahead excludes, which it does NOT
/// exclude.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_outside_the_modelled_expression_is_refused() {
    // The expression this service PUBLISHES for the member: the modelled `@pattern` of
    // `EventBridgeParameters$Source` (`smithy_query(query="shape",
    // shape="com.amazonaws.scheduler#EventBridgeParameters")`) without the whole-value
    // anchors the model writes it with, which is how this service quotes a pattern.
    const PUBLISHED_SOURCE_PATTERN: &str =
        r"(?=[/\.\-_A-Za-z0-9]+)((?!aws\.).*)|(\$(\.[\w_-]+(\[(\d+|\*)\])*)*)";
    let case = "a source held to the expression its member declares";
    let emulator = emulator();
    let bus_arn = given_bus(&emulator, case, "source-pattern-bus").await;
    let role = given_execution_role_for(&emulator, case, "source-pattern-role", &[]).await;
    let occurrence = seconds_from_now(3_600);
    let schedule = |source: &str| {
        json!({
            "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "DISABLED",
            "Target": {
                "Arn": bus_arn,
                "RoleArn": role,
                "Input": json!({ "probe": "pattern" }).to_string(),
                "EventBridgeParameters": { "Source": source, "DetailType": "Pattern Probe" },
            },
        })
    };

    // The two values the UPDATE half below is driven with, named rather than picked out of
    // the table by position: a source the expression admits, and one it does not.
    const ADMITTED_SOURCE: &str = "orders.nightly";
    const REFUSED_SOURCE: &str = "aws.scheduler";
    // (case, the Source, whether the expression admits it)
    let rows: [(&str, &str, bool); 8] = [
        ("a custom namespace", ADMITTED_SOURCE, true),
        ("a path-like namespace", "/orders/nightly", true),
        // The look-ahead is anchored at the start, so these are not the reserved prefix.
        ("the reserved letters without the dot", "aws", true),
        ("a word opening with the reserved letters", "awsome.orders", true),
        ("the reserved namespace inside the value", "my.aws.orders", true),
        ("a reference path", "$.detail.source[0]", true),
        ("the reserved namespace", REFUSED_SOURCE, false),
        ("a value opening outside the character class", " orders", false),
    ];

    for (index, (row, source, admitted)) in rows.iter().enumerate() {
        let name = format!("source-pattern-{index}");
        let (status, body) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/{name}"),
            Some(schedule(source)),
        )
        .await;
        if *admitted {
            assert_eq!(
                status, 200,
                "case {row:?}: expected the source {source:?} accepted, got {status} {body}"
            );
            continue;
        }
        let expected = format!(
            "1 validation error detected: Value '{source}' at \
             'target.eventBridgeParameters.source' failed to satisfy constraint: Member must \
             satisfy regular expression pattern: {PUBLISHED_SOURCE_PATTERN}"
        );
        assert_eq!(
            (status, error_code(&body).as_str(), error_message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {row:?}: expected the constraint clause for {source:?}, got {status} {body}"
        );
        // Nothing was stored for a refused write: the schedule does not resolve.
        let (status, read) = scheduler(
            &emulator,
            "GET",
            &format!("/schedules/{name}?groupName=default"),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {row:?}: expected no schedule stored, got {status} {read}"
        );
    }

    // The same constraint on the UPDATE path: the schedule written with the admitted source
    // keeps it, and the refused update changes nothing. The schedule updated is the one the
    // first row wrote, which is the row stating `ADMITTED_SOURCE`.
    let written = rows
        .iter()
        .position(|(_, source, admitted)| *admitted && *source == ADMITTED_SOURCE)
        .expect("a row states the admitted source");
    let (status, updated) = scheduler(
        &emulator,
        "PUT",
        &format!("/schedules/source-pattern-{written}"),
        Some(schedule(REFUSED_SOURCE)),
    )
    .await;
    let expected = format!(
        "1 validation error detected: Value '{REFUSED_SOURCE}' at \
         'target.eventBridgeParameters.source' failed to satisfy constraint: Member must \
         satisfy regular expression pattern: {PUBLISHED_SOURCE_PATTERN}"
    );
    assert_eq!(
        (status, error_code(&updated).as_str(), error_message(&updated)),
        (400, "ValidationException", expected),
        "case an update to a refused source: got {status} {updated}"
    );
    let (status, read) = scheduler(
        &emulator,
        "GET",
        &format!("/schedules/source-pattern-{written}?groupName=default"),
        None,
    )
    .await;
    assert_eq!(
        (status, read["Target"]["EventBridgeParameters"]["Source"].as_str()),
        (200, Some(ADMITTED_SOURCE)),
        "case an update to a refused source: expected the stored source unchanged, got \
         {status} {read}"
    );
}
