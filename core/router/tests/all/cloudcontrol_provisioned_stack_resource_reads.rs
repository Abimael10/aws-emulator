// SPDX-License-Identifier: CC0-1.0

//! Whether CloudFormation's RESOURCE reads agree about the stack a Cloud Control
//! `CreateResource` provisions.
//!
//! The RULE under test: **while a resource operation on a registry type its owning service
//! provisions through an API of its own is still running, the resource is observable through
//! that service's own reads** — for `AWS::CloudFormation::Stack` that means every logical
//! resource the stack's template declares, reported at the status that service's lifecycle
//! has it in (`CREATE_IN_PROGRESS` for a gate waiting for its signals, `CREATE_COMPLETE` for
//! one whose creation finished) by EVERY one of CloudFormation's resource reads, so a
//! resource's status does not depend on which read a client uses: `DescribeStackResource`,
//! `DescribeStackResources` and `ListStackResources` are one stored record seen three ways.
//! Those reads are REPORTING operations, so looking at a held stack releases nothing.
//!
//! That is the join a client depends on: a `RequestToken` for work another service performs
//! is only useful if that service can be inspected and coordinated with while the work runs —
//! a gate that cannot be found cannot be signalled.
//!
//! The rows differ along what the rule generalises over: how many gates hold the create open,
//! how many signals each one waits for, and whether the template also declares a resource
//! that waits for nothing (which is complete while the gates are not).
//!
//! Scope: the LIFECYCLE this walk rides on — the create answered in a non-terminal status,
//! the modelled `ConcurrentOperationException` a second operation on the held resource is
//! refused with, the release, the resource model the completed request reports and the delete
//! CloudFormation carries out — is asserted by the suite that owns it, the cloudcontrol
//! crate's own `cloudformation_stack_provisioning`. Nothing here restates it: what is
//! asserted here is only which READS report a provisioned stack's resources, and that they
//! agree.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_api_schema_properties::{
    build_router, cloud_control, cloudformation,
};
use crate::query_wire::{element, pairs_of};

/// The registry type under test: a public type whose resources CloudFormation itself owns.
const STACK_TYPE: &str = "AWS::CloudFormation::Stack";

/// The `ResourceStatus` CloudFormation reports for a resource whose creation has started and
/// not finished — which is the state a wait condition waits for its signals in — and the one
/// it reports once that creation completed.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The non-terminal `OperationStatus` values a resource operation request reports: "the
/// resource operation hasn't yet started", and the operation running.
const STATUS_PENDING: &str = "PENDING";
const STATUS_IN_PROGRESS: &str = "IN_PROGRESS";

/// The terminal ones.
const STATUS_SUCCESS: &str = "SUCCESS";
const STATUS_FAILED: &str = "FAILED";
const STATUS_CANCEL_COMPLETE: &str = "CANCEL_COMPLETE";

/// How long a wait in this suite runs before the case fails.
///
/// Every wait here is for a status ANOTHER service reports — a stack's create runs that
/// service's own deployment and its own clock-owed transitions, each a further request of this
/// emulator — so the budget is far above anything that flow takes, including on a host running
/// the whole workspace's tests at once. No case waits for a span: each re-reads until what it
/// is after is there or this deadline passes.
const BUDGET: Duration = Duration::from_secs(180);

/// How often such a wait re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One gate of a row: the resource whose `CreationPolicy` waits for signals, and the
/// `UniqueId` of each signal its `Count` asks for.
struct Gate {
    logical_id: &'static str,
    signals: &'static [&'static str],
}

/// One row: the stack a create provisions, and the resources its template declares.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The resources that hold the create open until they are signalled.
    gates: &'static [Gate],
    /// The resources that wait for nothing: a wait-condition HANDLE, which is created
    /// without any service of this emulator being asked about it.
    free: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "one gate waiting for one signal",
        stack: "cc-reads-one",
        gates: &[Gate {
            logical_id: "Gate",
            signals: &["release"],
        }],
        free: &[],
    },
    Row {
        case: "one gate waiting for two signals, beside a resource that waits for nothing",
        stack: "cc-reads-count",
        gates: &[Gate {
            logical_id: "Gate",
            signals: &["host-one", "host-two"],
        }],
        free: &["Handle"],
    },
    Row {
        case: "two independent gates, each waiting for one signal",
        stack: "cc-reads-two",
        gates: &[
            Gate {
                logical_id: "Alpha",
                signals: &["release-a"],
            },
            Gate {
                logical_id: "Beta",
                signals: &["release-b"],
            },
        ],
        free: &[],
    },
];

/// The template a row's stack is deployed from: one `AWS::CloudFormation::WaitCondition` per
/// gate, each with the `CreationPolicy.ResourceSignal` that makes it wait for as many signals
/// as the row names `UniqueId`s for, and one `AWS::CloudFormation::WaitConditionHandle` per
/// resource that waits for nothing.
fn template(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    for gate in row.gates {
        resources.insert(
            gate.logical_id.to_string(),
            json!({
                "Type": "AWS::CloudFormation::WaitCondition",
                "CreationPolicy": {
                    "ResourceSignal": { "Count": gate.signals.len(), "Timeout": "PT10M" }
                },
            }),
        );
    }
    for logical_id in row.free {
        resources.insert(
            (*logical_id).to_string(),
            json!({ "Type": "AWS::CloudFormation::WaitConditionHandle" }),
        );
    }
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// What every one of CloudFormation's resource reads must report for `row`'s stack while its
/// gates are waiting: each gate still being created, each free resource created.
fn held_statuses(row: &Row) -> BTreeMap<String, String> {
    row.gates
        .iter()
        .map(|gate| (gate.logical_id.to_string(), CREATE_IN_PROGRESS.to_string()))
        .chain(
            row.free
                .iter()
                .map(|logical_id| ((*logical_id).to_string(), CREATE_COMPLETE.to_string())),
        )
        .collect()
}

/// What they must report once the stack's create has completed: every resource created.
fn completed_statuses(row: &Row) -> BTreeMap<String, String> {
    row.gates
        .iter()
        .map(|gate| gate.logical_id)
        .chain(row.free.iter().copied())
        .map(|logical_id| (logical_id.to_string(), CREATE_COMPLETE.to_string()))
        .collect()
}

/// One Cloud Control operation, required to be served.
async fn cc_ok(router: &MultiServiceRouter, case: &str, operation: &str, payload: Value) -> Value {
    let (status, answered) = cloud_control(router, operation, payload).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {answered}"
    );
    answered
}

/// One CloudFormation operation, required to be served. Addressed through the wire helper of
/// the module this suite's router mounting comes from, so both services are called in the one
/// Region the stack belongs to.
async fn cfn_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// A string member of an answered document.
fn text(value: &Value, member: &str) -> String {
    value
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `ProgressEvent` the request `token` names reports right now.
async fn reported(router: &MultiServiceRouter, case: &str, token: &str) -> Value {
    let answered = cc_ok(
        router,
        case,
        "GetResourceRequestStatus",
        json!({ "RequestToken": token }),
    )
    .await;
    answered["ProgressEvent"].clone()
}

/// Read the request `token` names until it names the identifier the OWNING service assigned
/// the resource, answering it.
///
/// A request that has not started yet reports `PENDING` and one whose work is running reports
/// `IN_PROGRESS`, so the wait holds the request to that non-terminal pair rather than to one
/// of them — what the rule forbids is a create the owning service's gates hold open
/// TERMINATING, and that is asserted once the identifier is there.
async fn awaited_identifier(router: &MultiServiceRouter, case: &str, token: &str) -> String {
    let started = Instant::now();
    loop {
        let event = reported(router, case, token).await;
        let reached = text(&event, "OperationStatus");
        assert!(
            reached == STATUS_PENDING || reached == STATUS_IN_PROGRESS,
            "case {case:?}: expected the create to be held open by the stack's own lifecycle, \
             got {event}"
        );
        let identifier = text(&event, "Identifier");
        if !identifier.is_empty() {
            assert_eq!(
                reached, STATUS_IN_PROGRESS,
                "case {case:?}: expected the create that named its resource to be running, got \
                 {event}"
            );
            return identifier;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the create to name the stack it provisions within \
             {BUDGET:?}, it still reports {event}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read the request `token` names until it reports a TERMINAL status, which must be the
/// SUCCESS the released stack's own lifecycle gives it.
async fn settled(router: &MultiServiceRouter, case: &str, token: &str) {
    let started = Instant::now();
    loop {
        let event = reported(router, case, token).await;
        let reached = text(&event, "OperationStatus");
        if [STATUS_SUCCESS, STATUS_FAILED, STATUS_CANCEL_COMPLETE].contains(&reached.as_str()) {
            assert_eq!(
                reached, STATUS_SUCCESS,
                "case {case:?}: expected the released create to succeed, got {event}"
            );
            return;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the request to reach a terminal status within {BUDGET:?}, \
             it is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ResourceStatus` EVERY one of CloudFormation's resource reads reports for the stack
/// `stack` names, as the logical-id -> status map a client reads it as.
///
/// The three reads are the same stored record seen three ways, so this asserts they AGREE —
/// a resource's status may not depend on which read a client uses — and answers the one map
/// they report.
async fn resource_statuses(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let described = cfn_ok(
        router,
        case,
        "DescribeStackResources",
        &[("StackName", stack)],
    )
    .await;
    let by_describe = pairs_of(&described, "LogicalResourceId", "ResourceStatus");
    let listed = cfn_ok(router, case, "ListStackResources", &[("StackName", stack)]).await;
    let by_list = pairs_of(&listed, "LogicalResourceId", "ResourceStatus");
    assert_eq!(
        by_list, by_describe,
        "case {case:?}: expected ListStackResources and DescribeStackResources to report one \
         status per resource, got {by_list:?} against {by_describe:?}"
    );
    for (logical_id, status) in &by_describe {
        let detail = cfn_ok(
            router,
            case,
            "DescribeStackResource",
            &[("StackName", stack), ("LogicalResourceId", logical_id)],
        )
        .await;
        let reported = element(&detail, "ResourceStatus");
        assert_eq!(
            &reported, status,
            "case {case:?}: expected DescribeStackResource to report {logical_id:?} as \
             {status:?}, got {reported:?}"
        );
    }
    by_describe
}

/// The `StackStatus` CloudFormation's own read reports for the stack `name_or_id` names.
async fn stack_status(router: &MultiServiceRouter, case: &str, name_or_id: &str) -> String {
    let described = cfn_ok(router, case, "DescribeStacks", &[("StackName", name_or_id)]).await;
    element(&described, "StackStatus")
}

/// Start the create of `row`'s stack, answering `(the request token, the identifier the
/// owning service assigned)`.
async fn started(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let desired = json!({ "StackName": row.stack, "TemplateBody": template(row) });
    let accepted = cc_ok(
        router,
        row.case,
        "CreateResource",
        json!({ "TypeName": STACK_TYPE, "DesiredState": desired.to_string() }),
    )
    .await;
    let token = text(&accepted["ProgressEvent"], "RequestToken");
    let identifier = awaited_identifier(router, row.case, &token).await;
    (token, identifier)
}

/// RULE: every logical resource the template of a Cloud-Control-provisioned stack declares is
/// reported, at the status the owning service's lifecycle has it in, by each of that service's
/// resource reads — which agree with each other — both while the stack's gates hold the create
/// open and once the signals they wait for have released it.
#[tokio::test]
async fn every_cloudformation_resource_read_reports_a_provisioned_stacks_resources() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let (token, identifier) = started(&router, row).await;

        // The stack is CloudFormation's own: its read reports it under the name the desired
        // state carried, being created, with the stack id the request named.
        let described = cfn_ok(&router, case, "DescribeStacks", &[("StackName", row.stack)]).await;
        assert_eq!(
            element(&described, "StackStatus"),
            CREATE_IN_PROGRESS,
            "case {case:?}: expected CloudFormation to report the stack being created, got \
             {described}"
        );
        assert_eq!(
            element(&described, "StackId"),
            identifier,
            "case {case:?}: expected the request to name the stack id CloudFormation assigned, \
             got {identifier:?} against {described}"
        );

        // ...and so is every logical resource its template declares, at the status that
        // service's own lifecycle has it in: a gate waiting for its signals is being created,
        // a resource that waits for nothing is created.
        let held = resource_statuses(&router, case, row.stack).await;
        assert_eq!(
            held,
            held_statuses(row),
            "case {case:?}: expected CloudFormation's resource reads to report each declared \
             resource at its own status, got {held:?}"
        );

        // The signals the template asked for release the gates, and with them the request.
        for gate in row.gates {
            for unique_id in gate.signals {
                cfn_ok(
                    &router,
                    case,
                    "SignalResource",
                    &[
                        ("StackName", row.stack),
                        ("LogicalResourceId", gate.logical_id),
                        ("UniqueId", unique_id),
                        ("Status", "SUCCESS"),
                    ],
                )
                .await;
            }
        }
        settled(&router, case, &token).await;

        // The stack the request followed is complete THERE, and so is every resource of it —
        // by each of the three reads again.
        let reached = stack_status(&router, case, row.stack).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected CloudFormation to report the released stack complete, got \
             {reached:?}"
        );
        let statuses = resource_statuses(&router, case, row.stack).await;
        assert_eq!(
            statuses,
            completed_statuses(row),
            "case {case:?}: expected every declared resource to be reported created, got \
             {statuses:?}"
        );
    }
}

/// How many times the no-op case re-reads the held stack.
///
/// The reads are made back to back, with nothing between them: what is asserted is that
/// READING changes nothing, so there is nothing to wait for.
const REREADS: usize = 5;

/// RULE: those reads are REPORTING operations and a gate is released by its signals and by
/// nothing else — so a client that polls CloudFormation while the Cloud Control request is
/// held sees the same statuses however often it looks, and the request stays non-terminal.
#[tokio::test]
async fn reading_a_held_stack_releases_nothing() {
    let case = "a gate that is never signalled, read again and again";
    let row = Row {
        case,
        stack: "cc-reads-unsignalled",
        gates: &[Gate {
            logical_id: "Gate",
            signals: &["never-sent"],
        }],
        free: &["Handle"],
    };
    let router = build_router();
    let (token, _) = started(&router, &row).await;

    for read in 1..=REREADS {
        let statuses = resource_statuses(&router, case, row.stack).await;
        assert_eq!(
            statuses,
            held_statuses(&row),
            "case {case:?}: expected read {read} to report the unsignalled gate still waiting, \
             got {statuses:?}"
        );
        let reached = stack_status(&router, case, row.stack).await;
        assert_eq!(
            reached, CREATE_IN_PROGRESS,
            "case {case:?}: expected read {read} to report the stack still being created, got \
             {reached:?}"
        );
        let event = reported(&router, case, &token).await;
        assert_eq!(
            text(&event, "OperationStatus"),
            STATUS_IN_PROGRESS,
            "case {case:?}: expected read {read} to leave the request running, got {event}"
        );
    }
}
