// SPDX-License-Identifier: CC0-1.0

//! WHEN a CloudFormation stack-set status change reaches the account's default EventBridge
//! bus, observed where a client observes it: on the Amazon SQS queue a rule on that bus
//! delivers to.
//!
//! The rule pinned here: **a status change this service has RECORDED is on the bus by the
//! time the call that recorded it answers.** CloudFormation is an event SOURCE for its stack
//! sets, so publishing a change it has written is part of writing it — not work left for
//! something later to do. The change may be one the call itself makes (a stack set created,
//! a rollout started, a rollout stopped, a stack set deleted) or one the CLOCK had already
//! made due that a monitoring read materialises (the conclusion of a rollout): either way,
//! the call that records it is the call that owes the bus the event, so a client that only
//! listens is never behind a client that polls.
//!
//! Why it matters: a pipeline gating on a rollout, and an alarm on one failing, have nothing
//! to fall back on — such a client makes no further call of its own, so a publication that
//! was skipped is a change it never learns of, however long it waits. Each change a CALL
//! records is therefore read off the queue with ONE look and nothing waited for: what the
//! call owed the bus is already delivered when it answers, whatever else this emulator has
//! in flight. A conclusion the clock made due is waited for as the CONDITION it is, never
//! for a span of time.
//!
//! What this suite OWNS, beside `cloudformation_stackset_failed_instance_events.rs` and
//! `cloudformation_stack_set_status_change_events.rs`: those two pin WHAT a rollout
//! publishes and which of it a rule's pattern matches. This one pins WHEN each of those
//! publications has happened, over the calls that record a change — including the stack
//! set's own `ACTIVE` and `DELETED`, which no other suite reads.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_stackset_failed_instance_events::{
    assert_envelope, cloudformation, collect_until, drain, member, router, settled_operation,
    wired_queue, ACCOUNT, CREATE, INSTANCE_CHANGE, OPERATION_CHANGE, REGION, RUNNING, SOURCE,
    SUCCEEDED,
};
use crate::query_wire::opt_element;

/// The `detail-type` of a change to the STACK SET itself, beside the two the suites next to
/// this one read.
const STACK_SET_CHANGE: &str = "CloudFormation StackSet Status Change";

/// `StackSet$Status`: "a stack set that `CreateStackSet` has created is ACTIVE".
const ACTIVE: &str = "ACTIVE";

/// `StackSet$Status`: the terminal status of a stack set `DeleteStackSet` has deleted.
const DELETED: &str = "DELETED";

/// `StackSetOperation$Status`: "STOPPING: The operation is in the process of stopping, at
/// user request".
const STOPPING: &str = "STOPPING";

/// The logical id of the resource the template below waits for signals on.
const WAIT_CONDITION: &str = "Wait";

/// The pattern of a rule that observes every change of a stack set of this service: the
/// stack set's own, its operations' and its instances'.
fn every_change_of_a_stack_set() -> Value {
    json!({
        "source": [SOURCE],
        "detail-type": [STACK_SET_CHANGE, OPERATION_CHANGE, INSTANCE_CHANGE],
    })
}

/// The template every stack set of this suite deploys: a wait condition nothing signals of
/// its own accord, which holds the deployment of an instance's stack open for an hour.
///
/// The span is far longer than anything this suite waits for, so no assertion here turns on
/// how fast the host runs: the rollout a row STOPS is still in flight whenever that row gets
/// to it, and the rollout the second test concludes is concluded by the SIGNAL it sends
/// rather than by a timeout running out.
/// `AWS::CloudFormation::WaitCondition`'s `Timeout` is a number of seconds, of which "the
/// maximum time that can be specified for this property is 12 hours (43200 seconds)".
fn held_open_template() -> String {
    let mut template = json!({"Resources": {
        "Handle": {"Type": "AWS::CloudFormation::WaitConditionHandle"},
    }});
    template["Resources"][WAIT_CONDITION] = json!({
        "Type": "AWS::CloudFormation::WaitCondition",
        "Properties": {"Handle": {"Ref": "Handle"}, "Timeout": "3600", "Count": "1"},
    });
    template.to_string()
}

/// A SELF_MANAGED stack set over [`held_open_template`], with the administration and
/// execution roles such a stack set names: the stack set's ARN, which every event about it
/// states.
async fn create_stack_set(router: &MultiServiceRouter, case: &str, stack_set: &str) -> String {
    let created = cloudformation(
        router,
        case,
        "CreateStackSet",
        &[
            member("StackSetName", stack_set),
            member("TemplateBody", &held_open_template()),
            member("PermissionModel", "SELF_MANAGED"),
            member(
                "AdministrationRoleARN",
                &format!("arn:aws:iam::{ACCOUNT}:role/{stack_set}-admin"),
            ),
            member("ExecutionRoleName", "StackSetExecutionRole"),
        ],
    )
    .await;
    let stack_set_id = opt_element(&created, "StackSetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateStackSet answered no id: {created}"));
    format!("arn:aws:cloudformation:{REGION}:{ACCOUNT}:stackset/{stack_set_id}")
}

/// ONE stack instance of `stack_set`, deployed into the caller's own account and Region: the
/// `OperationId` that deployment answered, which is what a client monitors.
async fn create_stack_instances(
    router: &MultiServiceRouter,
    case: &str,
    stack_set: &str,
) -> String {
    let deployed = cloudformation(
        router,
        case,
        "CreateStackInstances",
        &[
            member("StackSetName", stack_set),
            member("Accounts.member.1", ACCOUNT),
            member("Regions.member.1", REGION),
        ],
    )
    .await;
    opt_element(&deployed, "OperationId").unwrap_or_else(|| {
        panic!("case {case:?}: CreateStackInstances answered no OperationId: {deployed}")
    })
}

/// WHICH call a row pins the publication of, named by the change that call records.
#[derive(Debug)]
enum Records {
    /// `CreateStackSet` records the stack set's own first status.
    StackSetActivated,
    /// `CreateStackInstances` records the rollout it started.
    RolloutStarted,
    /// `StopStackSetOperation` records the stop of a rollout still in flight.
    RolloutStopping,
    /// `DeleteStackSet` records the stack set's terminal status.
    StackSetDeleted,
}

/// The event the row's call must have published: the `detail-type` it carries, the status it
/// states and — for a change to an OPERATION — the action that operation is.
struct Expected {
    detail_type: &'static str,
    status: &'static str,
    action: Option<&'static str>,
}

/// One row: the case it is, the stack set it acts on and the call it pins.
struct Row {
    case: &'static str,
    stack_set: &'static str,
    records: Records,
}

/// The class the rule covers: every call of this service that records a stack-set status
/// change of its own, over both kinds of record such a change is about — the stack set
/// itself and one of its operations.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the status CreateStackSet records on the stack set it created",
            stack_set: "published-set-created",
            records: Records::StackSetActivated,
        },
        Row {
            case: "the rollout CreateStackInstances records",
            stack_set: "published-rollout-started",
            records: Records::RolloutStarted,
        },
        Row {
            case: "the stop StopStackSetOperation records on a rollout in flight",
            stack_set: "published-rollout-stopping",
            records: Records::RolloutStopping,
        },
        Row {
            case: "the status DeleteStackSet records on the stack set it deleted",
            stack_set: "published-set-deleted",
            records: Records::StackSetDeleted,
        },
    ]
}

/// Run `row` up to and including the call it pins: the stack set's ARN and what that call
/// owes the bus.
///
/// Everything published BEFORE that call is taken off the queue first, so what the look after
/// it finds is the call's own publication; the status each row expects is one no earlier call
/// of its row could have recorded either way.
async fn drive(
    router: &MultiServiceRouter,
    row: &Row,
    queue: &str,
) -> (String, Expected) {
    let (case, stack_set) = (row.case, row.stack_set);
    let stack_set_arn = create_stack_set(router, case, stack_set).await;
    let expected = match row.records {
        Records::StackSetActivated => Expected {
            detail_type: STACK_SET_CHANGE,
            status: ACTIVE,
            action: None,
        },
        Records::RolloutStarted => {
            drain(router, case, queue).await;
            create_stack_instances(router, case, stack_set).await;
            Expected {
                detail_type: OPERATION_CHANGE,
                status: RUNNING,
                action: Some(CREATE),
            }
        }
        Records::RolloutStopping => {
            let operation_id = create_stack_instances(router, case, stack_set).await;
            drain(router, case, queue).await;
            cloudformation(
                router,
                case,
                "StopStackSetOperation",
                &[
                    member("StackSetName", stack_set),
                    member("OperationId", &operation_id),
                ],
            )
            .await;
            Expected {
                detail_type: OPERATION_CHANGE,
                status: STOPPING,
                action: Some(CREATE),
            }
        }
        Records::StackSetDeleted => {
            drain(router, case, queue).await;
            cloudformation(
                router,
                case,
                "DeleteStackSet",
                &[member("StackSetName", stack_set)],
            )
            .await;
            Expected {
                detail_type: STACK_SET_CHANGE,
                status: DELETED,
                action: None,
            }
        }
    };
    (stack_set_arn, expected)
}

/// The one event among `seen` that carries `expected`'s `detail-type` and states its status,
/// or `None`.
fn published<'a>(seen: &'a [Value], expected: &Expected) -> Option<&'a Value> {
    seen.iter().find(|event| {
        event["detail-type"] == json!(expected.detail_type)
            && event["detail"]["status-details"]["status"] == json!(expected.status)
    })
}

/// What `published` could not find, stated as the assertion message every case shares.
fn assert_published(case: &str, seen: &[Value], expected: &Expected, stack_set_arn: &str) {
    let event = published(seen, expected).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the {:?} stating {:?} to have reached the rule's queue, \
             got {seen:?}",
            expected.detail_type, expected.status
        )
    });
    assert_envelope(case, event, stack_set_arn);
    if let Some(action) = expected.action {
        assert_eq!(
            event["detail"]["action"],
            json!(action),
            "case {case:?}: expected detail.action {action}, got {event}"
        );
    }
}

/// Every call that records a change of a stack set has published it by the time it answers.
#[tokio::test]
async fn a_change_a_call_records_is_on_the_bus_when_that_call_answers() {
    for row in rows() {
        // A fresh emulator per row, so one row's events never decide another's.
        let router = router();
        let queue = wired_queue(
            &router,
            row.case,
            row.stack_set,
            &every_change_of_a_stack_set(),
        )
        .await;
        let (stack_set_arn, expected) = drive(&router, &row, &queue).await;

        // ONE look, waiting for nothing: the call that recorded the change has answered, so
        // what it owed the bus has been delivered.
        let seen = drain(&router, row.case, &queue).await;
        assert_published(row.case, &seen, &expected, &stack_set_arn);
    }
}

/// The other kind of change: a conclusion the CLOCK made due, which the read monitoring the
/// operation materialises. It reaches the rule's queue without the client calling this
/// service again, so a client listening for the rollout to finish learns of it as the one
/// polling for it does.
#[tokio::test]
async fn a_conclusion_a_monitoring_read_materialises_reaches_the_rule() {
    let case = "a rollout concluded by the signal its wait condition was waiting for";
    let stack_set = "published-rollout-concluded";
    let router = router();
    let queue = wired_queue(&router, case, stack_set, &every_change_of_a_stack_set()).await;
    let stack_set_arn = create_stack_set(&router, case, stack_set).await;
    let operation_id = create_stack_instances(&router, case, stack_set).await;

    // The stack the instance deploys, which is what the rollout is waiting for.
    let listed = cloudformation(
        &router,
        case,
        "ListStackInstances",
        &[member("StackSetName", stack_set)],
    )
    .await;
    let stack_id = opt_element(&listed, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: the instance reports no StackId: {listed}"));

    // The signal the wait condition waits for: with it the deployment carries through, so the
    // operation's conclusion falls due without anything waiting for a timeout to run out.
    cloudformation(
        &router,
        case,
        "SignalResource",
        &[
            member("StackName", &stack_id),
            member("LogicalResourceId", WAIT_CONDITION),
            member("UniqueId", "signal-1"),
            member("Status", "SUCCESS"),
        ],
    )
    .await;

    let settled = settled_operation(&router, case, stack_set, &operation_id).await;
    let reported = opt_element(&settled, "Status").unwrap_or_default();
    assert_eq!(
        reported, SUCCEEDED,
        "case {case:?}: expected the signalled rollout to conclude {SUCCEEDED}, got {settled}"
    );

    // The conclusion the read above materialised, read off the queue the way a client that
    // never calls this service again reads it: the CONDITION is that it arrives, never the
    // time it took.
    let concluded = Expected {
        detail_type: OPERATION_CHANGE,
        status: SUCCEEDED,
        action: Some(CREATE),
    };
    let mut seen: Vec<Value> = Vec::new();
    collect_until(&router, case, &queue, &mut seen, |seen| {
        published(seen, &concluded).is_some()
    })
    .await;
    assert_published(case, &seen, &concluded, &stack_set_arn);
}
