// SPDX-License-Identifier: CC0-1.0

//! How a failure the DEPLOYING CALL itself found is reported to a stack instance, end to end
//! over the awsQuery wire.
//!
//! RULE under test: **a stack instance has ONE field for why its deployment failed —
//! `StatusReason`, "the explanation for the specific status code assigned to this stack
//! instance" — and it names the RESOURCE of the instance's stack that the deployment could
//! not carry through (its logical id, its type and that resource's own failure message),
//! whoever observed the failure: the walk the deploying call made in the services that own
//! the resources, or the clock afterwards.** The instance is `OUTDATED` with `DetailedStatus`
//! `FAILED` either way, the operation concludes `FAILED`, and `DescribeStackInstance` and
//! `ListStackInstances` report the one explanation.
//!
//! Why it matters: a client acting on a failed rollout cannot reach the instance's own stack
//! events, so that one field is all it has; two paths wording it differently make it
//! unparseable, and a failure the deploying call found is the common case.
//!
//! What this suite OWNS, beside `stack_set_failed_instance_outcome.rs`: that suite pins WHEN
//! a deployment concludes and WHAT it concludes in — the conclusion waiting for the instances'
//! stacks, the failure tolerance, the counts, and the clock-driven wait-condition failure. The
//! rows here are the ones it does not reach: a failure the deploying call's OWN walk found in
//! the owning service, and a `CreationPolicy`'s `ResourceSignal` (the other declaration that
//! asks for signals) — the two paths that record an instance's failure themselves — read back
//! through both instance reads and against the instance's own stack.
//!
//! Every observation is a client call — `DescribeStackSetOperation`,
//! `DescribeStackInstance`, `ListStackInstances` and `DescribeStacks` on the instance's own
//! stack id — so what is asserted is what a client sees rather than what this store recorded.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::stack_sets::{
    detailed_status, instance_status, operation_status,
};

use crate::stack_lifecycle::{call, member, router, xml_blocks, xml_text, ACCOUNT_ID, REGION};

/// How long a row waits for an operation or a stack to reach a terminal status, and how often
/// it looks.
///
/// The deadline is far longer than any span this emulator gives one of them, so a loaded host
/// cannot fail a case: what every row asserts is the CONDITION — a status that is not an
/// operation in flight — never the time it took.
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
/// See [`SETTLE_DEADLINE`].
const LOOK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// The fixed labels a stack instance's explanation of a failed resource carries, in order.
///
/// Spelled out here rather than read from the crate: the wording IS the requirement, so a
/// reworded one must fail this suite.
fn resource_failure_prefix(logical_id: &str, resource_type: &str) -> String {
    format!("ResourceLogicalId:{logical_id}, ResourceType:{resource_type}, ResourceStatusReason:")
}

/// One row: the stack set it creates, the template its instance deploys, and the resource
/// whose failure the instance's one explanation must name.
struct Row {
    case: &'static str,
    stack_set: &'static str,
    template: String,
    /// The status the operation concludes in ([`operation_status`]).
    operation: &'static str,
    /// The instance's `Status` and `DetailedStatus` once it has.
    instance: (&'static str, &'static str),
    /// The resource the explanation must name: its logical id and its type.
    names: Option<(&'static str, &'static str)>,
    /// A fragment the explanation must carry whatever else it says — the diagnosis of the
    /// failure itself; `None` where the owning service's own wording is that service's to
    /// choose and this row asserts only the resource the explanation names.
    diagnosis: Option<&'static str>,
}

/// The class this suite covers: the two deployments that record an instance's failure
/// THEMSELVES rather than leaving it to the clock.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a resource the owning service refuses",
            stack_set: "instance-refused-resource",
            // A FIFO topic whose name does not carry the suffix Amazon SNS requires: that
            // service refuses the create, so the failure is the one the deployment's own walk
            // found, with that service's diagnosis on it.
            template: json!({"Resources": {"Notifications": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": "instance-refused-topic", "FifoTopic": true},
            }}})
            .to_string(),
            operation: operation_status::FAILED,
            instance: (instance_status::OUTDATED, detailed_status::FAILED),
            names: Some(("Notifications", "AWS::SNS::Topic")),
            diagnosis: None,
        },
        Row {
            // The other declaration that asks for signals: a `CreationPolicy`'s
            // `ResourceSignal` on a resource of any type, whose expiry is worded as that
            // declaration words it rather than as a wait condition's is.
            case: "a creation policy whose resource signal never arrives",
            stack_set: "instance-creation-policy",
            template: json!({"Resources": {"Gated": {
                "Type": "AWS::SNS::Topic",
                "CreationPolicy": {"ResourceSignal": {"Count": 1, "Timeout": "PT1S"}},
                "Properties": {"DisplayName": "gated-by-a-signal"},
            }}})
            .to_string(),
            operation: operation_status::FAILED,
            instance: (instance_status::OUTDATED, detailed_status::FAILED),
            names: Some(("Gated", "AWS::SNS::Topic")),
            diagnosis: Some("resource signal"),
        },
    ]
}

/// `CreateStackSet` with the row's template, which must be accepted.
async fn create_stack_set(router: &MultiServiceRouter, case: &str, row: &Row) {
    let (status, body) = call(
        router,
        "CreateStackSet",
        &[
            member("StackSetName", row.stack_set),
            member("TemplateBody", &row.template),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateStackSet to succeed, got {status} {body}"
    );
}

/// `CreateStackInstances` for the caller's own account and Region, answered with the
/// `OperationId` the caller then monitors.
async fn create_stack_instances(router: &MultiServiceRouter, case: &str, row: &Row) -> String {
    let (status, body) = call(
        router,
        "CreateStackInstances",
        &[
            member("StackSetName", row.stack_set),
            member("Accounts.member.1", ACCOUNT_ID),
            member("Regions.member.1", REGION),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateStackInstances to succeed, got {status} {body}"
    );
    xml_text(&body, "OperationId")
        .unwrap_or_else(|| panic!("case {case:?}: expected an OperationId, got {body}"))
}

/// Poll `DescribeStackSetOperation` the way a client's waiter does and answer the BODY of the
/// first read whose `Status` is terminal.
async fn settled_operation(
    router: &MultiServiceRouter,
    case: &str,
    stack_set: &str,
    operation_id: &str,
) -> String {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (status, body) = call(
            router,
            "DescribeStackSetOperation",
            &[
                member("StackSetName", stack_set),
                member("OperationId", operation_id),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeStackSetOperation to succeed, got {status} {body}"
        );
        let reported = xml_text(&body, "Status").unwrap_or_default();
        if reported != operation_status::RUNNING && reported != operation_status::STOPPING {
            return body;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the operation to reach a terminal status, saw {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// `DescribeStackInstance` for the instance this suite deploys.
async fn describe_instance(router: &MultiServiceRouter, case: &str, stack_set: &str) -> String {
    let (status, body) = call(
        router,
        "DescribeStackInstance",
        &[
            member("StackSetName", stack_set),
            member("StackInstanceAccount", ACCOUNT_ID),
            member("StackInstanceRegion", REGION),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeStackInstance to succeed, got {status} {body}"
    );
    body
}

/// The one `StackInstanceSummary` `ListStackInstances` reports for that stack set.
async fn list_instance(router: &MultiServiceRouter, case: &str, stack_set: &str) -> String {
    let (status, body) = call(
        router,
        "ListStackInstances",
        &[member("StackSetName", stack_set)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListStackInstances to succeed, got {status} {body}"
    );
    let summaries = xml_blocks(&body, "member");
    assert_eq!(
        summaries.len(),
        1,
        "case {case:?}: expected one stack instance summary, got {body}"
    );
    summaries.into_iter().next().expect("checked just above")
}

/// The `StackStatus` of the stack the instance deploys, polled to a terminal one: what the
/// service that OWNS that stack says became of it.
async fn settled_instance_stack(router: &MultiServiceRouter, case: &str, stack_id: &str) -> String {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (status, body) = call(router, "DescribeStacks", &[member("StackName", stack_id)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeStacks on the instance's stack to succeed, got \
             {status} {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return reported;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the instance's stack to settle, saw {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The whole rule, over every row of the table.
#[tokio::test]
async fn a_failure_the_deploying_call_found_is_worded_for_the_instance() {
    let router = router();
    for row in rows() {
        let case = row.case;
        create_stack_set(&router, case, &row).await;
        let operation_id = create_stack_instances(&router, case, &row).await;

        let settled = settled_operation(&router, case, row.stack_set, &operation_id).await;
        let reported = xml_text(&settled, "Status").unwrap_or_default();
        assert_eq!(
            reported, row.operation,
            "case {case:?}: expected the operation to conclude {:?}, got {reported:?} in \
             {settled}",
            row.operation
        );

        let described = describe_instance(&router, case, row.stack_set).await;
        let instance_status = xml_text(&described, "Status").unwrap_or_default();
        let detailed = xml_text(&described, "DetailedStatus").unwrap_or_default();
        assert_eq!(
            (instance_status.as_str(), detailed.as_str()),
            row.instance,
            "case {case:?}: expected the instance to report {:?}, got {described}",
            row.instance
        );

        // The instance's stack is the record the instance reports off: its own terminal status
        // is a failed one exactly where the operation concluded FAILED.
        let stack_id = xml_text(&described, "StackId")
            .unwrap_or_else(|| panic!("case {case:?}: expected a StackId, got {described}"));
        let stack_status = settled_instance_stack(&router, case, &stack_id).await;
        let carried_through = stack_status == "CREATE_COMPLETE";
        assert_eq!(
            carried_through,
            row.operation == operation_status::SUCCEEDED,
            "case {case:?}: expected the instance's stack to have carried the deployment \
             through only where the operation succeeded, got {stack_status:?}"
        );

        // What the instance says beside its status, and what `ListStackInstances` reports for
        // the same instance: ONE explanation, read through either call.
        let reason = xml_text(&described, "StatusReason");
        let listed = list_instance(&router, case, row.stack_set).await;
        assert_eq!(
            xml_text(&listed, "StatusReason"),
            reason,
            "case {case:?}: expected both reads to report one explanation, got {listed}"
        );
        if row.operation == operation_status::SUCCEEDED {
            assert_eq!(
                reason, None,
                "case {case:?}: a deployment that worked explains nothing, got {described}"
            );
            continue;
        }
        let reason =
            reason.unwrap_or_else(|| panic!("case {case:?}: expected a StatusReason, got {described}"));
        if let Some((logical_id, resource_type)) = row.names {
            let prefix = resource_failure_prefix(logical_id, resource_type);
            assert!(
                reason.starts_with(&prefix),
                "case {case:?}: expected the explanation to open with {prefix:?}, got {reason:?}"
            );
        }
        if let Some(diagnosis) = row.diagnosis {
            assert!(
                reason.contains(diagnosis),
                "case {case:?}: expected the explanation to carry {diagnosis:?}, got {reason:?}"
            );
        }
        // The operation COUNTS the targets it failed in and carries the explanation of the
        // first of them ([`crate::state::stack_set_instance_outcomes`]).
        assert_eq!(
            xml_text(&settled, "FailedStackInstancesCount").as_deref(),
            Some("1"),
            "case {case:?}: expected the one failed instance to be counted, got {settled}"
        );
        assert_eq!(
            xml_text(&settled, "StatusReason").as_deref(),
            Some(reason.as_str()),
            "case {case:?}: expected the operation to report the failed target's own \
             explanation, got {settled}"
        );
    }
}
