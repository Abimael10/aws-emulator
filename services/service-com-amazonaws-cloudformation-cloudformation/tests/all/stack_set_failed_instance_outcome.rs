// SPDX-License-Identifier: CC0-1.0

//! What a StackSet DEPLOYMENT's outcome is, end to end over the awsQuery wire: the stacks of
//! the instances it deploys.
//!
//! RULE under test: a `CREATE`/`UPDATE` StackSet operation IS the deployment of its
//! instances' own stacks, so
//!
//! * it is not concluded while one of those stacks is still working — "SUCCEEDED: The
//!   operation completed creating or updating all the specified stacks without exceeding the
//!   failure tolerance for the operation" (`StackSetOperation$Status`) — and
//! * its terminal status and its `StatusDetails$FailedStackInstancesCount` ("the number of
//!   stack instances for which the StackSet operation failed") are counted off what those
//!   stacks REACHED: "FAILED: The operation exceeded the specified failure tolerance … if the
//!   number of failed stacks within a Region exceeds the failure tolerance, the status of the
//!   operation in the Region is set to FAILED. This in turn sets the status of the operation
//!   as a whole to FAILED", against `StackSetOperationPreferences$FailureToleranceCount`
//!   ("by default, 0 is specified").
//!
//! An instance whose stack did not carry the deployment through is reported as the failure it
//! is: "OUTDATED: The stack isn't currently up to date with the StackSet because … the
//! associated stack failed during a CreateStackSet or UpdateStackSet operation"
//! (`StackInstanceSummary$Status`), with `StackInstanceStatus$DetailedStatus` `FAILED` and a
//! `StatusReason` naming the template resource that failed, its type and that resource's own
//! failure message.
//!
//! The table spans what the rule generalises over: WHY the instance's stack failed (a
//! resource-signal wait nothing signals, which only the clock can fail, and a template
//! CloudFormation refuses before any owning service is asked), how many instances fail (none,
//! one, two), and the failure tolerance the request asked for (none stated, and one that
//! covers the failure). The table is driven twice: once monitored from the deployment's own
//! Region, and once with the store settled only by calls in ANOTHER Region — calls that
//! cannot perform what a stack's expired wait owes the services that own its resources, so
//! the outcome a deployment concludes with cannot depend on WHICH call settles the store. The
//! last test is the other half of the rule: an operation whose instance's stack is held at a
//! wait is still RUNNING, and the SIGNAL — not the clock — is what lets it conclude.
//!
//! Every call goes through the real request path (form-encoded body in, XML envelope out), so
//! what is asserted is what a client sees.

use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_cloudformation_cloudformation::state::stack_sets::{
    detailed_status, instance_status, operation_status,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{call_in, member, router, xml_text, REGION};
use crate::wait_conditions::signal;

/// Target accounts beside the emulator's own (`Account` is `@pattern("^[0-9]{12}$")`).
const ACCOUNT_A: &str = "111111111111";
const ACCOUNT_B: &str = "222222222222";

/// A second Region, whose calls settle the one store without being calls on the Region the
/// deployment under test is in: CloudFormation is Regional, so the work a stack's expired
/// resource-signal wait owes the services that own its resources is performed for the Region
/// the REQUEST is in.
const OTHER_REGION: &str = "us-west-2";

/// The logical id of the wait condition the failing rows declare, and the id their
/// `StatusReason` therefore has to name.
const GATE: &str = "Gate";

/// A template whose create cannot complete: a wait condition nothing ever signals, with the
/// `Timeout` (in seconds) the row asks for.
fn unsignalled_wait(timeout: &str) -> String {
    json!({
        "Resources": {
            "Handle": {"Type": "AWS::CloudFormation::WaitConditionHandle"},
            GATE: {
                "Type": "AWS::CloudFormation::WaitCondition",
                "Properties": {"Handle": {"Ref": "Handle"}, "Timeout": timeout},
            },
        },
    })
    .to_string()
}

/// A template CloudFormation refuses before any owning service is asked for a resource: an
/// `Fn::ImportValue` of an export name nothing publishes in the instance's Region.
fn unresolvable_import(export: &str) -> String {
    json!({
        "Resources": {
            "Param": {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Type": "String",
                    "Value": {"Fn::ImportValue": export},
                },
            },
        },
    })
    .to_string()
}

/// A template every instance deploys without anything holding it back.
fn plain_topic(display_name: &str) -> String {
    json!({
        "Resources": {
            "Topic": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"DisplayName": display_name},
            },
        },
    })
    .to_string()
}

/// The `StatusReason` CloudFormation words for a stack instance whose own stack failed at a
/// resource: the resource's logical id, its type and that resource's own failure message.
///
/// Spelled out here rather than read from the crate: the wording is the requirement, so a
/// reworded one must fail this suite.
fn instance_reason(logical_id: &str, resource_type: &str, resource_reason: &str) -> String {
    format!(
        "ResourceLogicalId:{logical_id}, ResourceType:{resource_type}, \
         ResourceStatusReason:{resource_reason}"
    )
}

/// The reason a wait condition that received no signals reports when its `Timeout` ran out.
fn wait_timed_out(expected: usize) -> String {
    format!("WaitCondition timed out. Received 0 conditions when expecting {expected}")
}

/// The reason a stack reports for an export name nothing publishes.
fn no_export_reason(export: &str) -> String {
    format!("No export named {export} found.")
}

/// `CreateStackSet` from one template.
async fn create_stack_set(
    router: &MultiServiceRouter,
    stack_set: &str,
    template: &str,
) -> (u16, String) {
    create_stack_set_in(router, REGION, stack_set, template).await
}

/// [`create_stack_set`] in `region`.
async fn create_stack_set_in(
    router: &MultiServiceRouter,
    region: &str,
    stack_set: &str,
    template: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "CreateStackSet",
        &[
            member("StackSetName", stack_set),
            member("TemplateBody", template),
        ],
    )
    .await
}

/// `CreateStackInstances` over `accounts` in the request's own Region, with the failure
/// tolerance the row asks for.
async fn deploy(
    router: &MultiServiceRouter,
    stack_set: &str,
    accounts: &[&str],
    tolerance: Option<i32>,
) -> (u16, String) {
    deploy_in(router, REGION, stack_set, accounts, tolerance).await
}

/// [`deploy`] in `region`, over that Region's own instances.
async fn deploy_in(
    router: &MultiServiceRouter,
    region: &str,
    stack_set: &str,
    accounts: &[&str],
    tolerance: Option<i32>,
) -> (u16, String) {
    let mut members = vec![
        member("StackSetName", stack_set),
        member("Regions.member.1", region),
    ];
    for (index, account) in accounts.iter().enumerate() {
        members.push(member(&format!("Accounts.member.{}", index + 1), account));
    }
    if let Some(tolerance) = tolerance {
        members.push(member(
            "OperationPreferences.FailureToleranceCount",
            &tolerance.to_string(),
        ));
        members.push(member("OperationPreferences.MaxConcurrentCount", "1"));
    }
    call_in(router, region, "CreateStackInstances", &members).await
}

/// `DescribeStackSetOperation`, which is what a caller monitors a deployment through.
async fn describe_operation(
    router: &MultiServiceRouter,
    stack_set: &str,
    operation_id: &str,
) -> (u16, String) {
    describe_operation_in(router, REGION, stack_set, operation_id).await
}

/// [`describe_operation`] in `region`.
async fn describe_operation_in(
    router: &MultiServiceRouter,
    region: &str,
    stack_set: &str,
    operation_id: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "DescribeStackSetOperation",
        &[
            member("StackSetName", stack_set),
            member("OperationId", operation_id),
        ],
    )
    .await
}

/// `ListStackInstances` for ONE account and Region pair, so the answer carries that
/// instance's summary alone.
async fn list_instance(
    router: &MultiServiceRouter,
    stack_set: &str,
    account: &str,
) -> (u16, String) {
    list_instance_in(router, REGION, stack_set, account).await
}

/// [`list_instance`] in `region`, for that Region's own instance.
async fn list_instance_in(
    router: &MultiServiceRouter,
    region: &str,
    stack_set: &str,
    account: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "ListStackInstances",
        &[
            member("StackSetName", stack_set),
            member("StackInstanceAccount", account),
            member("StackInstanceRegion", region),
        ],
    )
    .await
}

/// The `OperationId` the call that started the deployment answered with.
fn operation_id_of(case: &str, http_status: u16, body: &str) -> String {
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateStackInstances to succeed, got {http_status} {body}"
    );
    xml_text(body, "OperationId")
        .unwrap_or_else(|| panic!("case {case:?}: no OperationId in {body}"))
}

/// How long a case gives a deployment to reach its terminal status: far longer than anything
/// a row asks for, so a loaded machine cannot fail one, and the condition — not the clock —
/// is what ends the wait.
const SETTLED_DEADLINE: Duration = Duration::from_secs(120);

/// How often the loops below look again.
const POLL_EVERY: Duration = Duration::from_millis(50);

/// The `DescribeStackSetOperation` body of `operation_id` once its `Status` is terminal,
/// looked for again until it is.
async fn settled_operation(
    case: &str,
    router: &MultiServiceRouter,
    stack_set: &str,
    operation_id: &str,
) -> String {
    settled_operation_in(case, router, REGION, stack_set, operation_id).await
}

/// [`settled_operation`], monitored in `region`.
async fn settled_operation_in(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    stack_set: &str,
    operation_id: &str,
) -> String {
    let deadline = Instant::now() + SETTLED_DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        let (http_status, body) =
            describe_operation_in(router, region, stack_set, operation_id).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected DescribeStackSetOperation to succeed, got {http_status} \
             {body}"
        );
        let reported = xml_text(&body, "Status").unwrap_or_default();
        if !operation_status::is_in_flight(&reported) {
            return body;
        }
        last = body;
        tokio::time::sleep(POLL_EVERY).await;
    }
    panic!("case {case:?}: the deployment never left its in-flight status: {last}")
}

/// One row of the outcome table: a StackSet whose template decides what its instances' stacks
/// reach, and the outcome the deployment therefore has.
struct Outcome {
    case: &'static str,
    stack_set: &'static str,
    template: String,
    /// The accounts the deployment targets, one instance each in the request's Region.
    accounts: &'static [&'static str],
    /// `OperationPreferences$FailureToleranceCount`, where the row states one.
    tolerance: Option<i32>,
    /// The terminal `StackSetOperation$Status` the deployment reaches.
    status: &'static str,
    /// `StatusDetails$FailedStackInstancesCount`.
    failed_count: &'static str,
    /// The `StackInstanceSummary$Status` and `StackInstanceStatus$DetailedStatus` every
    /// instance of the deployment reports.
    instance: (&'static str, &'static str),
    /// The `StatusReason` every instance reports, or `None` for a deployment that left none.
    reason: Option<String>,
}

/// The class the rule covers: the ways a deployment's instances' stacks can turn out.
fn outcomes() -> Vec<Outcome> {
    vec![
        Outcome {
            case: "one instance whose wait condition nothing signals",
            stack_set: "unsignalled-one",
            template: unsignalled_wait("1"),
            accounts: &[ACCOUNT_A],
            tolerance: None,
            status: operation_status::FAILED,
            failed_count: "1",
            instance: (instance_status::OUTDATED, detailed_status::FAILED),
            reason: Some(instance_reason(
                GATE,
                "AWS::CloudFormation::WaitCondition",
                &wait_timed_out(1),
            )),
        },
        Outcome {
            case: "two instances whose wait conditions nothing signals",
            stack_set: "unsignalled-two",
            template: unsignalled_wait("1"),
            accounts: &[ACCOUNT_A, ACCOUNT_B],
            tolerance: None,
            status: operation_status::FAILED,
            failed_count: "2",
            instance: (instance_status::OUTDATED, detailed_status::FAILED),
            reason: Some(instance_reason(
                GATE,
                "AWS::CloudFormation::WaitCondition",
                &wait_timed_out(1),
            )),
        },
        Outcome {
            case: "one failing instance within the failure tolerance the request asked for",
            stack_set: "tolerated-one",
            template: unsignalled_wait("1"),
            accounts: &[ACCOUNT_A],
            tolerance: Some(1),
            status: operation_status::SUCCEEDED,
            failed_count: "1",
            instance: (instance_status::OUTDATED, detailed_status::FAILED),
            reason: Some(instance_reason(
                GATE,
                "AWS::CloudFormation::WaitCondition",
                &wait_timed_out(1),
            )),
        },
        Outcome {
            case: "one instance whose template imports an export nothing publishes",
            stack_set: "unresolvable-import",
            template: unresolvable_import("never-published-for-an-instance"),
            accounts: &[ACCOUNT_A],
            tolerance: None,
            status: operation_status::FAILED,
            failed_count: "1",
            instance: (instance_status::OUTDATED, detailed_status::FAILED),
            reason: Some(no_export_reason("never-published-for-an-instance")),
        },
        Outcome {
            case: "two instances nothing holds back",
            stack_set: "deployed-two",
            template: plain_topic("deployed-two"),
            accounts: &[ACCOUNT_A, ACCOUNT_B],
            tolerance: None,
            status: operation_status::SUCCEEDED,
            failed_count: "0",
            instance: (instance_status::CURRENT, detailed_status::SUCCEEDED),
            reason: None,
        },
    ]
}

/// Assert the outcome `row` states: the deployment's own terminal status and failed-instance
/// count, and what each of its instances reports.
///
/// The ONE place this suite words those assertions, so the case that monitors the deployment
/// from its own Region and the one that lets ANOTHER Region's calls settle the store cannot
/// hold it to different outcomes.
async fn assert_the_outcome(
    case: &str,
    router: &MultiServiceRouter,
    row: &Outcome,
    operation_id: &str,
) {
    // The operation's own terminal status and its failed-instance count are what its
    // instances' stacks reached.
    let settled = settled_operation(case, router, row.stack_set, operation_id).await;
    let reported = xml_text(&settled, "Status").unwrap_or_default();
    assert_eq!(
        reported, row.status,
        "case {case:?}: expected the deployment to conclude {:?}, got {reported:?} in \
         {settled}",
        row.status
    );
    let counted = xml_text(&settled, "FailedStackInstancesCount").unwrap_or_default();
    assert_eq!(
        counted, row.failed_count,
        "case {case:?}: expected FailedStackInstancesCount {:?}, got {counted:?} in {settled}",
        row.failed_count
    );

    // ...and every instance reports what its own stack did.
    for account in row.accounts {
        let (http_status, listed) = list_instance(router, row.stack_set, account).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected ListStackInstances({account}) to succeed, got \
             {http_status} {listed}"
        );
        let (expected_status, expected_detailed) = row.instance;
        let instance_status = xml_text(&listed, "Status").unwrap_or_default();
        assert_eq!(
            instance_status, expected_status,
            "case {case:?}: expected instance {account} to report Status \
             {expected_status:?}, got {instance_status:?} in {listed}"
        );
        let detailed = xml_text(&listed, "DetailedStatus").unwrap_or_default();
        assert_eq!(
            detailed, expected_detailed,
            "case {case:?}: expected instance {account} to report DetailedStatus \
             {expected_detailed:?}, got {detailed:?} in {listed}"
        );
        let reason = xml_text(&listed, "StatusReason");
        assert_eq!(
            reason, row.reason,
            "case {case:?}: expected instance {account} to report StatusReason {:?}, got \
             {reason:?} in {listed}",
            row.reason
        );
    }
}

#[tokio::test]
async fn a_deployment_concludes_as_its_instances_stacks_did() {
    for row in outcomes() {
        let case = row.case;
        let router = router();
        let (http_status, body) = create_stack_set(&router, row.stack_set, &row.template).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStackSet to succeed, got {http_status} {body}"
        );
        let (http_status, body) =
            deploy(&router, row.stack_set, row.accounts, row.tolerance).await;
        let operation_id = operation_id_of(case, http_status, &body);
        assert_the_outcome(case, &router, &row, &operation_id).await;
    }
}

/// The same table, with the store settled by calls that CANNOT reach what its instances'
/// stacks are still waiting on.
///
/// A stack whose resource-signal wait has run out failed at the instant it ran out, and that
/// failure owes the services that own the stack's resources a rollback — calls made on the
/// request path, for the Region the request is in, and never from under the store's lock. So
/// a call in another Region settles the whole store without reaching them, exactly as the
/// background task that waits for a deployment's conclusion does. The outcome a deployment
/// concludes with is still its instances' stacks': whichever call settles the store, a
/// deployment is never concluded as the success nothing reached.
#[tokio::test]
async fn a_deployment_concludes_as_its_stacks_did_whichever_call_settles_the_store() {
    /// The stack set whose deployment is the clock here: a deployment in ANOTHER Region, of
    /// the same shape as the rows that are held at a wait, so polling it settles the store
    /// over and over without any call on the Region under test.
    const CLOCK: &str = "clock-in-another-region";
    for row in outcomes() {
        let case = row.case;
        let router = router();
        let (http_status, body) = create_stack_set(&router, row.stack_set, &row.template).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStackSet to succeed, got {http_status} {body}"
        );
        let (http_status, body) =
            deploy(&router, row.stack_set, row.accounts, row.tolerance).await;
        let operation_id = operation_id_of(case, http_status, &body);

        // The clock: a deployment of the same shape in another Region, started after the one
        // under test and so running out of wait after it.
        let (http_status, body) =
            create_stack_set_in(&router, OTHER_REGION, CLOCK, &unsignalled_wait("1")).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStackSet in {OTHER_REGION} to succeed, got \
             {http_status} {body}"
        );
        let (http_status, body) =
            deploy_in(&router, OTHER_REGION, CLOCK, &[ACCOUNT_A], None).await;
        let clock_operation = operation_id_of(case, http_status, &body);

        // Waited for in that other Region alone — the CONDITION being that deployment's own
        // conclusion, which its stack's wait running out is what brings about. So by the time
        // it concludes, the wait of the deployment under test has run out too and its own
        // conclusion has been due for at least as long, with nothing but those other Region's
        // calls having settled the store in between.
        let settled =
            settled_operation_in(case, &router, OTHER_REGION, CLOCK, &clock_operation).await;
        let reported = xml_text(&settled, "Status").unwrap_or_default();
        assert_eq!(
            reported,
            operation_status::FAILED,
            "case {case:?}: expected the clock deployment in {OTHER_REGION} to conclude \
             FAILED, got {reported:?} in {settled}"
        );

        assert_the_outcome(case, &router, &row, &operation_id).await;
    }
}

#[tokio::test]
async fn a_deployment_held_at_a_wait_condition_concludes_on_the_signal() {
    let case = "an instance's stack held at a wait condition that is then signalled";
    let stack_set = "signalled-instance";
    let router = router();
    // A span far longer than this store's own transition delay, so what holds the deployment
    // open is the WAIT rather than a race with the clock.
    let (http_status, body) =
        create_stack_set(&router, stack_set, &unsignalled_wait("600")).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateStackSet to succeed, got {http_status} {body}"
    );
    let (http_status, body) = deploy(&router, stack_set, &[ACCOUNT_A], None).await;
    let operation_id = operation_id_of(case, http_status, &body);

    // The instance's stack is waiting for a signal, so the deployment has not concluded:
    // nothing the clock owes can complete an operation whose stack is still working.
    let (http_status, listed) = list_instance(&router, stack_set, ACCOUNT_A).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected ListStackInstances to succeed, got {http_status} {listed}"
    );
    let stack_id = xml_text(&listed, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: the instance carries no StackId: {listed}"));
    let (http_status, held) = describe_operation(&router, stack_set, &operation_id).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DescribeStackSetOperation to succeed, got {http_status} \
         {held}"
    );
    let reported = xml_text(&held, "Status").unwrap_or_default();
    assert_eq!(
        reported,
        operation_status::RUNNING,
        "case {case:?}: expected the deployment to be RUNNING while its instance's stack \
         waits, got {reported:?} in {held}"
    );

    // The signal releases the wait, so the instance's stack completes and the deployment
    // concludes as the success it is.
    let (http_status, body) = signal(&router, &stack_id, GATE, "instance-host", "SUCCESS").await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected the signal to be accepted, got {http_status} {body}"
    );
    let settled = settled_operation(case, &router, stack_set, &operation_id).await;
    let reported = xml_text(&settled, "Status").unwrap_or_default();
    assert_eq!(
        reported,
        operation_status::SUCCEEDED,
        "case {case:?}: expected the signalled deployment to succeed, got {reported:?} in \
         {settled}"
    );
    let counted = xml_text(&settled, "FailedStackInstancesCount").unwrap_or_default();
    assert_eq!(
        counted, "0",
        "case {case:?}: expected no failed instances, got {counted:?} in {settled}"
    );
    let (http_status, listed) = list_instance(&router, stack_set, ACCOUNT_A).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected ListStackInstances to succeed, got {http_status} {listed}"
    );
    let instance_status = xml_text(&listed, "Status").unwrap_or_default();
    assert_eq!(
        instance_status,
        instance_status::CURRENT,
        "case {case:?}: expected the deployed instance to be CURRENT, got {instance_status:?} \
         in {listed}"
    );
    let detailed = xml_text(&listed, "DetailedStatus").unwrap_or_default();
    assert_eq!(
        detailed,
        detailed_status::SUCCEEDED,
        "case {case:?}: expected the deployed instance's deployment to have SUCCEEDED, got \
         {detailed:?} in {listed}"
    );
}
