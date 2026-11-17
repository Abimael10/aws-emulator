// SPDX-License-Identifier: CC0-1.0

//! What a stack whose last operation FAILED and was LEFT STANDING takes, and what a change
//! set of such a stack is computed against.
//!
//! Three rules, each held over the class it covers rather than over one recorded call:
//!
//! 1. **A drift detection is carried out only on a stack in a STABLE state.** A detection
//!    "compares the actual configuration of the resource with its expected template
//!    configuration", so it needs a stack whose resources ARE the configuration its template
//!    declares — the state the stack's last operation settled it in. Asked of a stack in any
//!    other status it is refused with HTTP 400 `ValidationError` naming the stack and that
//!    status, and that holds for the whole-stack detection and for the detection of ONE
//!    resource alike.
//! 2. **A call that DEPLOYS a configuration onto such a stack is accepted only while the
//!    caller asks for the failed state to go on being preserved.** `DisableRollback`
//!    "preserves the state of previously provisioned resources when an operation fails", so
//!    the stack takes an `UpdateStack` or an `ExecuteChangeSet` while that is what the
//!    request asks for — stated on the call, or as the change set's own documented
//!    equivalent `OnStackFailure=DO_NOTHING` — and otherwise answers the non-terminal-state
//!    refusal, which leaves the stack and the change set exactly where they were. The status
//!    the refusal brackets is the one the stack is really in, whichever operation parked it
//!    there (a create's failure or an update's), and the second way out of that state — the
//!    stack's own `RollbackStack` — takes the stack to a status that holds no configuration an
//!    update could change, so from there even a request that asks for the failed state to be
//!    kept is refused, by the sentence that names the stack and its status.
//! 3. **A change set is computed against the configuration the stack's resources CARRY.** A
//!    resource whose change the failed operation could not carry through was put back to the
//!    configuration in force before it, which the template in force no longer declares — so
//!    a change set that restates what that resource already holds proposes nothing and
//!    settles `FAILED` with CloudFormation's no-changes reason (and an execution of it is
//!    refused with the modelled `InvalidChangeSetStatus`), while one that restates the
//!    template in force proposes putting the failed value back.
//!
//! 4. **A resource whose create the owning service REFUSED does not exist there, so the next
//!    deployment that declares it CREATES it** rather than asking that service to change a
//!    resource it does not hold. That is what the fix-forward update rule 2 admits is for: the
//!    stack kept the estate the failed operation had made, and the resources it could NOT make
//!    are the outstanding work. It holds for every type this emulator provisions, whichever
//!    module that type's own create and update arms live in.
//!
//! The rows span what each rule generalises over: every `StackStatus` a live stack carries
//! for rule 1, the ways a request can and cannot ask for the failed state to be kept and the
//! operation whose failure parked the stack for rule 2, how many resources the failed
//! operation broke — beside a change it carried through — for rule 3, and the resource TYPE
//! whose create was refused for rule 4. Every call goes over the awsQuery wire, and each
//! resource is read back through the OWNING service's own wire (AWS Lambda, Amazon CloudWatch
//! Logs), so what is asserted is what a client sees.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, describe_change_set, settle_the_store};
use crate::executed_change_set_create_outcomes::execute_change_set;
use crate::failed_update_resource_rollback::{
    alias_logical_id, alias_version, function_description, function_name, template,
    CREATED_DESCRIPTION, HEALTHY_VERSION, UPDATED_DESCRIPTION,
};
use crate::harness::{
    call, create_stack, member, refusal, router_with_engine, store, xml_blocks, xml_text,
    xml_texts, REGION,
};
use crate::logs_group_children_resources::group_exists;
use crate::stack_lifecycle::{acknowledging_iam, describe_stack, resource_event_statuses};

/// The code every refusal under test carries: the detections model no error of their own and
/// the deployment gate is not one of `ExecuteChangeSet`'s modelled errors either, so both are
/// the service-common one.
const VALIDATION_ERROR: &str = "ValidationError";

/// The modelled error an execution of a change set with no plan to apply answers
/// (`InvalidChangeSetStatusException`: "the specified change set can't be used to update the
/// stack").
const INVALID_CHANGE_SET_STATUS: &str = "InvalidChangeSetStatus";

/// CloudFormation's own `StatusReason` on a change set that would change nothing.
const NO_CHANGES_REASON: &str = "The submitted information didn't contain changes. Submit \
                                 different information to create a change set.";

/// How long a case waits for a stack operation to settle before giving up: generous, since
/// nothing here is timed — every wait below polls for the CONDITION it needs.
const SETTLING_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Every `StackStatus` a LIVE stack carries, with whether a drift detection is carried out
/// on a stack in it.
///
/// Source: the model's `StackStatus` enum,
/// `smithy_query(query="shape",shape="StackStatus")` — all 23 values, less `DELETE_COMPLETE`,
/// which is not a stack a request can address by name at all (`stack_lifecycle.rs` holds that
/// refusal).
const DRIFT_DETECTION_BY_STATUS: [(&str, bool); 22] = [
    ("CREATE_IN_PROGRESS", false),
    ("CREATE_FAILED", false),
    ("CREATE_COMPLETE", true),
    ("ROLLBACK_IN_PROGRESS", false),
    ("ROLLBACK_FAILED", false),
    ("ROLLBACK_COMPLETE", false),
    ("DELETE_IN_PROGRESS", false),
    ("DELETE_FAILED", false),
    ("UPDATE_IN_PROGRESS", false),
    ("UPDATE_COMPLETE_CLEANUP_IN_PROGRESS", false),
    ("UPDATE_COMPLETE", true),
    ("UPDATE_FAILED", false),
    ("UPDATE_ROLLBACK_IN_PROGRESS", false),
    ("UPDATE_ROLLBACK_FAILED", false),
    ("UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS", false),
    ("UPDATE_ROLLBACK_COMPLETE", true),
    ("REVIEW_IN_PROGRESS", false),
    ("IMPORT_IN_PROGRESS", false),
    ("IMPORT_COMPLETE", true),
    ("IMPORT_ROLLBACK_IN_PROGRESS", false),
    ("IMPORT_ROLLBACK_FAILED", false),
    ("IMPORT_ROLLBACK_COMPLETE", true),
];

/// The one bucket the drift rows are asked about.
const DRIFT_RESOURCE: &str = "Store";

/// The `StackStatus` the stack reports right now.
async fn stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status_code, body) = describe_stack(router, stack).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected DescribeStacks to answer, got {status_code} {body}"
    );
    xml_text(&body, "StackStatus").unwrap_or_default()
}

/// The stack's own `StackId`, which is how a refusal about the stack addresses it.
async fn stack_id(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status_code, body) = describe_stack(router, stack).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected DescribeStacks to answer, got {status_code} {body}"
    );
    xml_text(&body, "StackId").unwrap_or_default()
}

/// Wait for the stack's operation to SETTLE and answer the status it settled in.
///
/// The transition is due at an instant, so the store is settled and read again until the
/// stack is no longer mid-operation — the condition, never a fixed wait.
async fn settled_status(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
) -> String {
    let deadline = Instant::now() + SETTLING_DEADLINE;
    loop {
        settle_the_store(router, engine).await;
        let status = stack_status(router, case, stack).await;
        if !status.ends_with("_IN_PROGRESS") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never settled, still {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Deploy the healthy revision of the Lambda estate and then break it: every alias is pointed
/// at a version its function never published, with the failed state kept.
///
/// `description` is the function `Description` the broken revision declares, so a case can
/// carry a change the owning service APPLIES beside the ones it refuses.
///
/// Answers once the stack rests in `UPDATE_FAILED`, having asserted that AWS Lambda holds
/// each alias at the version it had before — the live configuration the rules below are
/// about.
async fn parked_in_update_failed(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    missing_versions: &[&str],
    description: &str,
) {
    let healthy: Vec<&str> = missing_versions.iter().map(|_| HEALTHY_VERSION).collect();
    let deployed = template(stack, &healthy, CREATED_DESCRIPTION);
    let (status_code, body) =
        create_stack(router, stack, &deployed, &acknowledging_iam(&[])).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected CreateStack to be accepted, got {status_code} {body}"
    );
    let created = settled_status(router, engine, case, stack).await;
    assert_eq!(
        created, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {created:?}"
    );

    let broken = template(stack, missing_versions, description);
    let mut members = vec![
        member("StackName", stack),
        member("TemplateBody", &broken),
        member("DisableRollback", "true"),
    ];
    members.extend(acknowledging_iam(&[]));
    let (status_code, body) = call(router, "UpdateStack", &members).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected UpdateStack to be accepted, got {status_code} {body}"
    );
    let settled = settled_status(router, engine, case, stack).await;
    assert_eq!(
        settled, "UPDATE_FAILED",
        "case {case:?}: expected the failed update to park the stack, got {settled:?}"
    );
    for index in 0..missing_versions.len() {
        let function = function_name(stack, index);
        let invoked = alias_version(router, &function).await;
        assert_eq!(
            invoked.as_deref(),
            Some(HEALTHY_VERSION),
            "case {case:?}: expected AWS Lambda to still hold the alias of {function:?} \
             invoking {HEALTHY_VERSION:?}, got {invoked:?}"
        );
    }
}

/// Put the stack in the status a row is about.
///
/// Rule 1 is about the GATE, so the rows state the status directly rather than driving the
/// stack into each of them: that is what lets one table cover every status the model declares,
/// including the ones no flow of this emulator rests in.
fn put_in_status(engine: &Arc<engine::InMemoryEngine>, stack: &str, status: &str) {
    let state = store(engine);
    let mut held = state.lock();
    let held_stack = held
        .resolve_mut(REGION, stack)
        .expect("the stack the case created");
    held_stack.pending = None;
    held_stack.status = status.to_string();
}

/// RULE 1: a drift detection is carried out only on a stack in a stable state, and is refused
/// about the stack and the status in every other one.
#[tokio::test]
async fn a_drift_detection_is_refused_unless_the_stack_is_stable() {
    let (router, engine) = router_with_engine();
    let stack = "drift-gate";
    let body = format!(
        r#"{{"Resources":{{"{DRIFT_RESOURCE}":{{"Type":"AWS::S3::Bucket","Properties":{{"BucketName":"{stack}-store"}}}}}}}}"#
    );
    let (status_code, created) = create_stack(&router, stack, &body, &[]).await;
    assert_eq!(
        status_code, 200,
        "expected CreateStack to be accepted, got {status_code} {created}"
    );
    let settled = settled_status(&router, &engine, "the drift gate", stack).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "expected the create to complete, got {settled:?}"
    );
    let stack_id = stack_id(&router, "the drift gate", stack).await;

    for (status, accepted) in DRIFT_DETECTION_BY_STATUS {
        let case = status;
        put_in_status(&engine, stack, status);
        let whole_stack = call(&router, "DetectStackDrift", &[member("StackName", stack)]).await;
        let one_resource = call(
            &router,
            "DetectStackResourceDrift",
            &[
                member("StackName", stack),
                member("LogicalResourceId", DRIFT_RESOURCE),
            ],
        )
        .await;
        if accepted {
            let (status_code, body) = whole_stack;
            assert_eq!(
                status_code, 200,
                "case {case:?}: expected DetectStackDrift to be accepted, got {status_code} \
                 {body}"
            );
            let detection = xml_text(&body, "StackDriftDetectionId").unwrap_or_default();
            assert!(
                !detection.is_empty(),
                "case {case:?}: expected a StackDriftDetectionId, got {body}"
            );
            let (status_code, body) = one_resource;
            assert_eq!(
                status_code, 200,
                "case {case:?}: expected DetectStackResourceDrift to be accepted, got \
                 {status_code} {body}"
            );
            let reported = xml_text(&body, "StackResourceDriftStatus").unwrap_or_default();
            assert!(
                !reported.is_empty(),
                "case {case:?}: expected a drift status for {DRIFT_RESOURCE:?}, got {body}"
            );
            continue;
        }
        let expected = format!(
            "Drift detection cannot be performed on stack [{stack_id}] while it is in \
             [{status}] state"
        );
        for (operation, (status_code, body)) in [
            ("DetectStackDrift", whole_stack),
            ("DetectStackResourceDrift", one_resource),
        ] {
            assert_eq!(
                status_code, 400,
                "case {case:?}: expected {operation} to be refused, got {status_code} {body}"
            );
            let (code, message) = refusal(&body);
            assert_eq!(
                code, VALIDATION_ERROR,
                "case {case:?}: expected {operation} to answer {VALIDATION_ERROR:?}, got \
                 {code:?}"
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected {operation} to answer {expected:?}, got {message:?}"
            );
        }
    }

    // Which refusal wins when the request is wrong about both: the status is a precondition
    // of the STACK, which is resolved before the resource the request names, so a detection
    // aimed at a logical id the stack never deployed still answers the status refusal.
    let status = "UPDATE_FAILED";
    put_in_status(&engine, stack, status);
    let (status_code, body) = call(
        &router,
        "DetectStackResourceDrift",
        &[
            member("StackName", stack),
            member("LogicalResourceId", "NeverDeployed"),
        ],
    )
    .await;
    assert_eq!(
        status_code, 400,
        "expected a detection of an undeployed resource to be refused, got {status_code} {body}"
    );
    let (code, message) = refusal(&body);
    let expected = format!(
        "Drift detection cannot be performed on stack [{stack_id}] while it is in [{status}] \
         state"
    );
    assert_eq!(
        code, VALIDATION_ERROR,
        "expected {VALIDATION_ERROR:?}, got {code:?}"
    );
    assert_eq!(
        message, expected,
        "expected the stack's own status to refuse the call ({expected:?}), got {message:?}"
    );
}

/// One row of the deployment-gate table: how the execution asks — or fails to ask — for the
/// failed state to go on being preserved.
struct ExecuteRow {
    case: &'static str,
    stack: &'static str,
    /// `OnStackFailure` the change set is created with, where the row states it there.
    on_stack_failure: Option<&'static str>,
    /// `DisableRollback` the execution states, where the row states it on the call.
    disable_rollback: Option<&'static str>,
    /// Whether the execution asks for the failed state to be kept, and so is accepted.
    accepted: bool,
}

/// The class rule 2 covers on `ExecuteChangeSet`: nothing asking at all, each member that can
/// ask, and each of them stating the opposite.
const EXECUTE_ROWS: &[ExecuteRow] = &[
    ExecuteRow {
        case: "nothing asks for the failed state to be kept",
        stack: "cfnuf-exec-unasked",
        on_stack_failure: None,
        disable_rollback: None,
        accepted: false,
    },
    ExecuteRow {
        case: "the execution states DisableRollback=true",
        stack: "cfnuf-exec-flagged",
        on_stack_failure: None,
        disable_rollback: Some("true"),
        accepted: true,
    },
    ExecuteRow {
        case: "the execution states DisableRollback=false",
        stack: "cfnuf-exec-unflagged",
        on_stack_failure: None,
        disable_rollback: Some("false"),
        accepted: false,
    },
    ExecuteRow {
        // "DO_NOTHING … This is equivalent to specifying true for the DisableRollback
        // parameter" — stated on the change set rather than on the execution.
        case: "the change set was created with OnStackFailure=DO_NOTHING",
        stack: "cfnuf-exec-do-nothing",
        on_stack_failure: Some("DO_NOTHING"),
        disable_rollback: None,
        accepted: true,
    },
    ExecuteRow {
        case: "the change set was created with OnStackFailure=ROLLBACK",
        stack: "cfnuf-exec-rollback",
        on_stack_failure: Some("ROLLBACK"),
        disable_rollback: None,
        accepted: false,
    },
];

/// The change set the rows of rule 2 execute: the alias back at the version it still invokes,
/// and a change to the function the owning service will apply — so there is a plan to carry
/// out whichever way the row asks for it.
fn correcting_change_set(stack: &str) -> String {
    template(stack, &[HEALTHY_VERSION], UPDATED_DESCRIPTION)
}

/// The `ExecutionStatus` the change set reports right now.
async fn execution_status_of(
    router: &MultiServiceRouter,
    case: &str,
    change_set: &str,
    stack: &str,
) -> String {
    let (status_code, body) = describe_change_set(router, change_set, Some(stack)).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected DescribeChangeSet to answer, got {status_code} {body}"
    );
    xml_text(&body, "ExecutionStatus").unwrap_or_default()
}

/// RULE 2 on `ExecuteChangeSet`: executing a change set deploys its configuration onto the
/// stack, so a stack resting in a failed status takes one only while the request asks for that
/// state to be kept — and a refused execution changes nothing.
#[tokio::test]
async fn an_execution_onto_a_parked_stack_has_to_ask_for_the_failed_state_to_be_kept() {
    for row in EXECUTE_ROWS {
        let case = row.case;
        let (router, engine) = router_with_engine();
        parked_in_update_failed(
            &router,
            &engine,
            case,
            row.stack,
            &["593"],
            CREATED_DESCRIPTION,
        )
        .await;
        let parked_id = stack_id(&router, case, row.stack).await;

        let change_set = "the-correction";
        let body = correcting_change_set(row.stack);
        let mut extra = vec![
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &body),
        ];
        if let Some(on_stack_failure) = row.on_stack_failure {
            extra.push(member("OnStackFailure", on_stack_failure));
        }
        let (status_code, created) = create_change_set(
            &router,
            row.stack,
            change_set,
            &acknowledging_iam(&extra),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateChangeSet to be accepted, got {status_code} {created}"
        );
        settle_the_store(&router, &engine).await;

        let mut on_the_call = Vec::new();
        if let Some(disable_rollback) = row.disable_rollback {
            on_the_call.push(member("DisableRollback", disable_rollback));
        }
        let (status_code, body) =
            execute_change_set(&router, change_set, row.stack, &on_the_call).await;
        if row.accepted {
            assert_eq!(
                status_code, 200,
                "case {case:?}: expected the execution to be accepted, got {status_code} {body}"
            );
            let settled = settled_status(&router, &engine, case, row.stack).await;
            assert_eq!(
                settled, "UPDATE_COMPLETE",
                "case {case:?}: expected the execution to complete the stack, got {settled:?}"
            );
            // ...and what it deployed is in force in the service that owns it.
            let function = function_name(row.stack, 0);
            let described = function_description(&router, &function).await;
            assert_eq!(
                described.as_deref(),
                Some(UPDATED_DESCRIPTION),
                "case {case:?}: expected AWS Lambda to hold the executed description for \
                 {function:?}, got {described:?}"
            );
            continue;
        }
        assert_eq!(
            status_code, 400,
            "case {case:?}: expected the execution to be refused, got {status_code} {body}"
        );
        let (code, message) = refusal(&body);
        let expected = non_terminal_refusal("UPDATE_FAILED");
        assert_eq!(
            code, VALIDATION_ERROR,
            "case {case:?}: expected {VALIDATION_ERROR:?}, got {code:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // A refusal leaves the stack where it was and the plan still available.
        let still = stack_status(&router, case, row.stack).await;
        assert_eq!(
            still, "UPDATE_FAILED",
            "case {case:?}: expected the refused execution to leave the stack parked, got \
             {still:?}"
        );
        let execution = execution_status_of(&router, case, change_set, row.stack).await;
        assert_eq!(
            execution, "AVAILABLE",
            "case {case:?}: expected the refused execution to leave the plan available, got \
             {execution:?}"
        );
        // ...and the stack it was refused about is the one the message named.
        assert!(
            message.contains("UPDATE_FAILED"),
            "case {case:?}: expected the refusal to name the status of {parked_id:?}, got \
             {message:?}"
        );
    }
}

/// RULE 2 on `UpdateStack`: the same gate, worded the same way, for the call that deploys
/// onto the parked stack directly.
#[tokio::test]
async fn an_update_of_a_parked_stack_has_to_ask_for_the_failed_state_to_be_kept() {
    let (router, engine) = router_with_engine();
    let stack = "cfnuf-update-gate";
    let case = "the fix-forward update";
    parked_in_update_failed(&router, &engine, case, stack, &["593"], CREATED_DESCRIPTION).await;

    let body = correcting_change_set(stack);
    let unasked = acknowledging_iam(&[
        member("StackName", stack),
        member("TemplateBody", &body),
    ]);
    let (status_code, answered) = call(&router, "UpdateStack", &unasked).await;
    assert_eq!(
        status_code, 400,
        "case {case:?}: expected the un-flagged update to be refused, got {status_code} \
         {answered}"
    );
    let (code, message) = refusal(&answered);
    let expected = non_terminal_refusal("UPDATE_FAILED");
    assert_eq!(
        code, VALIDATION_ERROR,
        "case {case:?}: expected {VALIDATION_ERROR:?}, got {code:?}"
    );
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );

    let mut asked = unasked.clone();
    asked.push(member("DisableRollback", "true"));
    let (status_code, answered) = call(&router, "UpdateStack", &asked).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the fix-forward update to be accepted, got {status_code} \
         {answered}"
    );
    let settled = settled_status(&router, &engine, case, stack).await;
    assert_eq!(
        settled, "UPDATE_COMPLETE",
        "case {case:?}: expected the fix-forward update to complete, got {settled:?}"
    );
}

/// One row of the change-set table: what the failed update broke, and what it carried through
/// beside it.
struct DiffRow {
    case: &'static str,
    stack: &'static str,
    /// The version each alias is pointed at by the failed update — one per function, so the
    /// row's own length is how many resources it breaks.
    missing_versions: &'static [&'static str],
    /// Whether the failed update also changed the functions' `Description`, which the owning
    /// service APPLIED: a change the stack's resources really carry.
    changed_the_description: bool,
}

/// The class rule 3 covers: how many resources the failed operation could not carry through,
/// and whether anything it DID carry through stands beside them.
const DIFF_ROWS: &[DiffRow] = &[
    DiffRow {
        case: "one resource put back",
        stack: "cfnuf-diff-one",
        missing_versions: &["593"],
        changed_the_description: false,
    },
    DiffRow {
        case: "two resources put back",
        stack: "cfnuf-diff-two",
        missing_versions: &["4", "17"],
        changed_the_description: false,
    },
    DiffRow {
        case: "one resource put back beside a change the update carried through",
        stack: "cfnuf-diff-applied",
        missing_versions: &["268"],
        changed_the_description: true,
    },
];

/// One `ResourceChange` a `DescribeChangeSet` response carries, as a row spells it: the
/// logical id, the action, whether the resource is replaced, and the properties the change is
/// scoped to.
type ReportedChange = (String, String, String, Vec<String>);

/// Every `ResourceChange` of a response, in response order.
fn reported_changes(xml: &str) -> Vec<ReportedChange> {
    xml_blocks(xml, "ResourceChange")
        .iter()
        .map(|change| {
            let details = xml_blocks(change, "Details")
                .first()
                .map(|details| {
                    xml_blocks(details, "member")
                        .iter()
                        .filter_map(|entry| xml_text(entry, "Name"))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            (
                xml_text(change, "LogicalResourceId").unwrap_or_default(),
                xml_text(change, "Action").unwrap_or_default(),
                xml_text(change, "Replacement").unwrap_or_default(),
                details,
            )
        })
        .collect()
}

/// The `Scope` entries a response carries, across every change in it.
fn scope_entries(xml: &str) -> Vec<String> {
    xml_blocks(xml, "Scope")
        .iter()
        .flat_map(|scope| xml_texts(scope, "member"))
        .collect()
}

/// RULE 3: a change set is computed against what the stack's resources CARRY — so restating
/// the configuration a put-back resource already holds proposes nothing and cannot be
/// executed, while restating the template in force proposes putting the failed value back.
#[tokio::test]
async fn a_change_set_of_a_parked_stack_is_computed_against_the_live_configuration() {
    for row in DIFF_ROWS {
        let case = row.case;
        let (router, engine) = router_with_engine();
        let broken_description = if row.changed_the_description {
            UPDATED_DESCRIPTION
        } else {
            CREATED_DESCRIPTION
        };
        parked_in_update_failed(
            &router,
            &engine,
            case,
            row.stack,
            row.missing_versions,
            broken_description,
        )
        .await;

        // ---- The change set of the LAST STABLE revision. ----
        let healthy: Vec<&str> = row
            .missing_versions
            .iter()
            .map(|_| HEALTHY_VERSION)
            .collect();
        let stable = template(row.stack, &healthy, CREATED_DESCRIPTION);
        let (status_code, created) = create_change_set(
            &router,
            row.stack,
            "of-the-stable-revision",
            &acknowledging_iam(&[
                member("ChangeSetType", "UPDATE"),
                member("TemplateBody", &stable),
            ]),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateChangeSet to be accepted, got {status_code} {created}"
        );
        settle_the_store(&router, &engine).await;
        let (status_code, described) =
            describe_change_set(&router, "of-the-stable-revision", Some(row.stack)).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeChangeSet to answer, got {status_code} {described}"
        );
        let reported = reported_changes(&described);
        // The only change such a revision can propose is one the failed update CARRIED
        // THROUGH, which this revision would put back; the resources it could not change
        // already hold what this revision declares.
        let expected: Vec<ReportedChange> = if row.changed_the_description {
            (0..row.missing_versions.len())
                .map(|index| {
                    (
                        format!("Fn{index}"),
                        "Modify".to_string(),
                        "False".to_string(),
                        vec!["Description".to_string()],
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stable revision to propose {expected:?}, got \
             {reported:?}"
        );
        let status = xml_text(&described, "Status").unwrap_or_default();
        let execution = xml_text(&described, "ExecutionStatus").unwrap_or_default();
        let change_set_id = xml_text(&described, "ChangeSetId").unwrap_or_default();
        if expected.is_empty() {
            // Nothing to apply: the change set settles FAILED with CloudFormation's own
            // reason, is not offered for execution, and an execution of it is refused on its
            // status.
            assert_eq!(
                status, "FAILED",
                "case {case:?}: expected a change set with no changes to fail, got {status:?} \
                 {described}"
            );
            let reason = xml_text(&described, "StatusReason").unwrap_or_default();
            assert_eq!(
                reason, NO_CHANGES_REASON,
                "case {case:?}: expected {NO_CHANGES_REASON:?}, got {reason:?}"
            );
            assert_eq!(
                execution, "UNAVAILABLE",
                "case {case:?}: expected the plan not to be offered for execution, got \
                 {execution:?}"
            );
            let (status_code, refused) = execute_change_set(
                &router,
                "of-the-stable-revision",
                row.stack,
                &[member("DisableRollback", "true")],
            )
            .await;
            assert_eq!(
                status_code, 400,
                "case {case:?}: expected the execution to be refused, got {status_code} \
                 {refused}"
            );
            let (code, message) = refusal(&refused);
            let expected_message = format!(
                "ChangeSet [{change_set_id}] cannot be executed in its current status of \
                 [FAILED]"
            );
            assert_eq!(
                code, INVALID_CHANGE_SET_STATUS,
                "case {case:?}: expected {INVALID_CHANGE_SET_STATUS:?}, got {code:?}"
            );
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected {expected_message:?}, got {message:?}"
            );
        } else {
            assert_eq!(
                status, "CREATE_COMPLETE",
                "case {case:?}: expected a change set with changes to complete, got \
                 {status:?} {described}"
            );
            assert_eq!(
                execution, "AVAILABLE",
                "case {case:?}: expected the plan to be offered for execution, got {execution:?}"
            );
        }

        // ---- The change set of the revision IN FORCE: the one the failed update left. ----
        let in_force = template(row.stack, row.missing_versions, broken_description);
        let (status_code, created) = create_change_set(
            &router,
            row.stack,
            "of-the-revision-in-force",
            &acknowledging_iam(&[
                member("ChangeSetType", "UPDATE"),
                member("TemplateBody", &in_force),
            ]),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateChangeSet to be accepted, got {status_code} {created}"
        );
        settle_the_store(&router, &engine).await;
        let (status_code, described) =
            describe_change_set(&router, "of-the-revision-in-force", Some(row.stack)).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeChangeSet to answer, got {status_code} {described}"
        );
        // One in-place `Modify` per resource the update could not change, of the one property
        // whose value it could not apply — and nothing about the resources that carry what
        // the revision declares.
        let expected: Vec<ReportedChange> = (0..row.missing_versions.len())
            .map(|index| {
                (
                    alias_logical_id(index),
                    "Modify".to_string(),
                    "False".to_string(),
                    vec!["FunctionVersion".to_string()],
                )
            })
            .collect();
        let reported = reported_changes(&described);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the revision in force to propose {expected:?}, got \
             {reported:?} {described}"
        );
        let status = xml_text(&described, "Status").unwrap_or_default();
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the change set to complete, got {status:?} {described}"
        );
        let scope = scope_entries(&described);
        let expected_scope: Vec<String> = (0..row.missing_versions.len())
            .map(|_| "Properties".to_string())
            .collect();
        assert_eq!(
            scope, expected_scope,
            "case {case:?}: expected every change to be scoped to {expected_scope:?}, got \
             {scope:?}"
        );
    }
}

/// The refusal CloudFormation words for a deploy onto a stack whose own failed operation was
/// left standing: fixed text apart from the status the stack is really in, which the sentence
/// brackets, and no full stop after the last clause.
///
/// Spelled here rather than read off the service, so what the rows assert is the WORDING and
/// not whatever the service happens to produce.
fn non_terminal_refusal(status: &str) -> String {
    format!(
        "This stack is currently in a non-terminal [{status}] state. To update the stack from \
         this state, please use the disable-rollback parameter with update-stack API. To \
         rollback to the last known good state, use the rollback-stack API"
    )
}

/// Which operation's failure PARKED the stack a row updates, and so which status the refusal
/// brackets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ParkedBy {
    /// A CREATE whose alias named a version the function never published, submitted with
    /// `DisableRollback=true`: the stack rests in `CREATE_FAILED` still holding the role and
    /// the function the walk did make.
    AFailedCreate,
    /// The same refusal on an UPDATE of a stack whose create completed: the stack rests in
    /// `UPDATE_FAILED` with every resource the update could not change put back.
    AFailedUpdate,
}

impl ParkedBy {
    /// The `StackStatus` such a stack reports, and so the status its refusal names.
    fn status(self) -> &'static str {
        match self {
            Self::AFailedCreate => "CREATE_FAILED",
            Self::AFailedUpdate => "UPDATE_FAILED",
        }
    }
}

/// One row of the update gate: how the stack was parked, what the update states about keeping
/// that state, and whether it is carried out.
struct ParkedUpdateRow {
    case: &'static str,
    stack: &'static str,
    parked_by: ParkedBy,
    /// The version the broken revision points this row's alias at — a version the function
    /// never published, so the owning service refuses it. Its own per row.
    missing_version: &'static str,
    /// `DisableRollback` the update states, where the row states it at all.
    disable_rollback: Option<&'static str>,
    /// Whether the update asks for the failed state to be kept, and so is carried out.
    accepted: bool,
}

/// The class rule 2 covers on `UpdateStack`: both operations whose failure can park a stack,
/// and each way a request can state — or fail to state — that the failed state is to be kept.
const PARKED_UPDATE_ROWS: &[ParkedUpdateRow] = &[
    ParkedUpdateRow {
        case: "a create that failed, with nothing asked about keeping it",
        stack: "cfnuf-create-gate-unasked",
        parked_by: ParkedBy::AFailedCreate,
        missing_version: "7",
        disable_rollback: None,
        accepted: false,
    },
    ParkedUpdateRow {
        case: "a create that failed, with DisableRollback=false",
        stack: "cfnuf-create-gate-unflagged",
        parked_by: ParkedBy::AFailedCreate,
        missing_version: "31",
        disable_rollback: Some("false"),
        accepted: false,
    },
    ParkedUpdateRow {
        case: "a create that failed, with DisableRollback=true",
        stack: "cfnuf-create-gate-flagged",
        parked_by: ParkedBy::AFailedCreate,
        missing_version: "412",
        disable_rollback: Some("true"),
        accepted: true,
    },
    ParkedUpdateRow {
        case: "an update that failed, with DisableRollback=false",
        stack: "cfnuf-update-gate-unflagged",
        parked_by: ParkedBy::AFailedUpdate,
        missing_version: "86",
        disable_rollback: Some("false"),
        accepted: false,
    },
];

/// Drive a CREATE that the owning service refuses part way, with the failed state kept: the
/// stack rests in `CREATE_FAILED` holding the role and the function the walk did make, and
/// nothing holds the alias its create was refused for.
async fn parked_in_create_failed(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    missing_version: &str,
) {
    let broken = template(stack, &[missing_version], CREATED_DESCRIPTION);
    let mut members = acknowledging_iam(&[]);
    members.push(member("DisableRollback", "true"));
    let (status_code, body) = create_stack(router, stack, &broken, &members).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected CreateStack to be accepted, got {status_code} {body}"
    );
    let settled = settled_status(router, engine, case, stack).await;
    assert_eq!(
        settled, "CREATE_FAILED",
        "case {case:?}: expected the kept create to park the stack, got {settled:?}"
    );
    // The partially created estate the caller kept it for, reported resource by resource.
    for (logical_id, expected) in [
        ("Role", "CREATE_COMPLETE"),
        ("Fn0", "CREATE_COMPLETE"),
        ("Fn0Alias", "CREATE_FAILED"),
    ] {
        let reported = reported_resource_status(router, case, stack, logical_id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {logical_id:?} to report {expected:?}, got {reported:?}"
        );
    }
    // ...and the function it did make is still there in the service that owns it.
    let function = function_name(stack, 0);
    let described = function_description(router, &function).await;
    assert_eq!(
        described.as_deref(),
        Some(CREATED_DESCRIPTION),
        "case {case:?}: expected AWS Lambda to still hold {function:?}, got {described:?}"
    );
}

/// The `StackStatusReason` the stack reports right now, which is what a failed deployment
/// names its cause in — carried into a row's assertion so a failure says WHY.
async fn reported_status_reason(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status_code, body) = describe_stack(router, stack).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected DescribeStacks to answer, got {status_code} {body}"
    );
    xml_text(&body, "StackStatusReason").unwrap_or_default()
}

/// The `ResourceStatus` the stack's own per-resource view reports for `logical_id`.
async fn reported_resource_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status_code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected DescribeStackResources to answer, got {status_code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .find(|entry| xml_text(entry, "LogicalResourceId").as_deref() == Some(logical_id))
        .and_then(|entry| xml_text(entry, "ResourceStatus"))
        .unwrap_or_default()
}

/// RULE 2 on `UpdateStack`, over BOTH statuses an operation that preserved its failure leaves:
/// the deploy is carried out only while the request asks for that state to go on being kept,
/// and otherwise the call is refused with the status the stack is really in and the two APIs
/// that move it on — leaving the stack, and the estate the failure kept, exactly as they were.
#[tokio::test]
async fn an_update_of_a_stack_parked_by_any_failure_has_to_ask_for_that_state() {
    for row in PARKED_UPDATE_ROWS {
        let case = row.case;
        let (router, engine) = router_with_engine();
        match row.parked_by {
            ParkedBy::AFailedCreate => {
                parked_in_create_failed(&router, &engine, case, row.stack, row.missing_version)
                    .await;
            }
            ParkedBy::AFailedUpdate => {
                parked_in_update_failed(
                    &router,
                    &engine,
                    case,
                    row.stack,
                    &[row.missing_version],
                    CREATED_DESCRIPTION,
                )
                .await;
            }
        }
        let parked = row.parked_by.status();
        let parked_id = stack_id(&router, case, row.stack).await;

        let correcting = correcting_change_set(row.stack);
        let mut members = acknowledging_iam(&[
            member("StackName", row.stack),
            member("TemplateBody", &correcting),
        ]);
        if let Some(disable_rollback) = row.disable_rollback {
            members.push(member("DisableRollback", disable_rollback));
        }
        let (status_code, answered) = call(&router, "UpdateStack", &members).await;

        if row.accepted {
            assert_eq!(
                status_code, 200,
                "case {case:?}: expected the fix-forward update of a stack in {parked} to be \
                 accepted, got {status_code} {answered}"
            );
            let settled = settled_status(&router, &engine, case, row.stack).await;
            let reason = reported_status_reason(&router, case, row.stack).await;
            assert_eq!(
                settled, "UPDATE_COMPLETE",
                "case {case:?}: expected the fix-forward update to complete, got {settled:?} \
                 ({reason:?})"
            );
            // ...and what it deployed is in force in the service that owns it: the alias the
            // parked create could not make now invokes the version the correction names.
            let function = function_name(row.stack, 0);
            let invoked = alias_version(&router, &function).await;
            assert_eq!(
                invoked.as_deref(),
                Some(HEALTHY_VERSION),
                "case {case:?}: expected the alias of {function:?} to invoke \
                 {HEALTHY_VERSION:?}, got {invoked:?}"
            );
            let described = function_description(&router, &function).await;
            assert_eq!(
                described.as_deref(),
                Some(UPDATED_DESCRIPTION),
                "case {case:?}: expected AWS Lambda to hold the deployed description for \
                 {function:?}, got {described:?}"
            );
            continue;
        }

        assert_eq!(
            status_code, 400,
            "case {case:?}: expected the update of a stack in {parked} to be refused, got \
             {status_code} {answered}"
        );
        let (code, message) = refusal(&answered);
        let expected = non_terminal_refusal(parked);
        assert_eq!(
            code, VALIDATION_ERROR,
            "case {case:?}: expected {VALIDATION_ERROR:?}, got {code:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // A refused update changes nothing: the stack is the one it named, still parked, and
        // the estate the failure kept is still there.
        let still = stack_status(&router, case, row.stack).await;
        assert_eq!(
            still, parked,
            "case {case:?}: expected the refused update to leave {parked_id:?} in {parked}, \
             got {still:?}"
        );
        let function = function_name(row.stack, 0);
        let described = function_description(&router, &function).await;
        assert_eq!(
            described.as_deref(),
            Some(CREATED_DESCRIPTION),
            "case {case:?}: expected the refused update to leave {function:?} as it was, got \
             {described:?}"
        );
    }
}

/// RULE 2's OTHER way out of a parked state, and where it leads: `RollbackStack` puts the
/// stack back to its last stable state, which for a create that never had one is a stack
/// holding no configuration at all — so from there an update is refused by the status
/// sentence, however it asks for a failed state to be kept.
#[tokio::test]
async fn a_rolled_back_stack_takes_no_update_however_the_request_asks() {
    let case = "the update after the rollback the refusal pointed at";
    let stack = "cfnuf-create-rolled-back";
    let (router, engine) = router_with_engine();
    parked_in_create_failed(&router, &engine, case, stack, "7").await;
    let stack_id = stack_id(&router, case, stack).await;

    let (status_code, body) = call(&router, "RollbackStack", &[member("StackName", stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected RollbackStack to be accepted, got {status_code} {body}"
    );
    let settled = settled_status(&router, &engine, case, stack).await;
    let rolled_back = "ROLLBACK_COMPLETE";
    assert_eq!(
        settled, rolled_back,
        "case {case:?}: expected the rollback of a create with no stable state to reach \
         {rolled_back}, got {settled:?}"
    );
    // The estate the parked create kept is gone from the service that owns it: the rollback
    // really was carried out.
    let function = function_name(stack, 0);
    let described = function_description(&router, &function).await;
    assert_eq!(
        described, None,
        "case {case:?}: expected AWS Lambda to no longer hold {function:?}, got {described:?}"
    );

    // Asking for the failed state to be kept no longer helps: there is no failed state to
    // keep, and the stack holds no configuration an update could change.
    let correcting = correcting_change_set(stack);
    for disable_rollback in [None, Some("true")] {
        let mut members = acknowledging_iam(&[
            member("StackName", stack),
            member("TemplateBody", &correcting),
        ]);
        if let Some(stated) = disable_rollback {
            members.push(member("DisableRollback", stated));
        }
        let (status_code, refused) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            status_code, 400,
            "case {case:?} (DisableRollback {disable_rollback:?}): expected the update to be \
             refused, got {status_code} {refused}"
        );
        let (code, message) = refusal(&refused);
        let expected = format!("Stack:{stack_id} is in {rolled_back} state and can not be updated.");
        assert_eq!(
            code, VALIDATION_ERROR,
            "case {case:?} (DisableRollback {disable_rollback:?}): expected \
             {VALIDATION_ERROR:?}, got {code:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?} (DisableRollback {disable_rollback:?}): expected {expected:?}, got \
             {message:?}"
        );
        let still = stack_status(&router, case, stack).await;
        assert_eq!(
            still, rolled_back,
            "case {case:?} (DisableRollback {disable_rollback:?}): expected the refused update \
             to leave the stack in {rolled_back}, got {still:?}"
        );
    }
}

/// The type whose create the owning service refuses in one row of rule 4.
///
/// Both of them are types whose create and update arms live in their OWN module rather than in
/// the per-type catalogue the deployment's declared-only question used to be put to — which is
/// exactly the distinction the rule must not turn on.
#[derive(Clone, Copy, Debug)]
enum RefusedResource {
    /// `AWS::Lambda::Alias` pointed at a version the function never published: AWS Lambda
    /// refuses the `CreateAlias`, so no alias exists.
    AnAlias,
    /// `AWS::Logs::LogGroup` declaring a retention that is not one of the values its owning
    /// service takes: the setting cannot be applied, so the group the create had made is taken
    /// away again and none exists.
    ALogGroup,
}

/// One row of rule 4: the type under test and the stack that drives it.
struct RefusedRow {
    case: &'static str,
    stack: &'static str,
    resource: RefusedResource,
    /// The logical id of the resource whose create is refused, as the row's template declares
    /// it.
    logical_id: &'static str,
}

/// The class rule 4 covers: one refused resource per type, each in its own stack.
const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "an alias AWS Lambda refused to create",
        stack: "cfnuf-recreate-alias",
        resource: RefusedResource::AnAlias,
        logical_id: "Fn0Alias",
    },
    RefusedRow {
        case: "a log group Amazon CloudWatch Logs refused to configure",
        stack: "cfnuf-recreate-group",
        resource: RefusedResource::ALogGroup,
        logical_id: "Group",
    },
];

/// The log group the `ALogGroup` row declares, one per stack.
fn group_name(stack: &str) -> String {
    format!("/aws/{stack}")
}

/// A retention the owning service TAKES, and one it refuses.
///
/// Source: `PutRetentionPolicy`'s own `retentionInDays` catalogue
/// (`smithy_query(query="operation",operation="PutRetentionPolicy")`), which lists 7 and does
/// not list 9.
const TAKEN_RETENTION: u32 = 7;
const REFUSED_RETENTION: u32 = 9;

/// The log group template, with whatever retention the row deploys.
fn log_group_template(stack: &str, retention: u32) -> String {
    format!(
        r#"{{"Resources":{{"Group":{{"Type":"AWS::Logs::LogGroup","Properties":{{"LogGroupName":"{}","RetentionInDays":{retention}}}}}}}}}"#,
        group_name(stack)
    )
}

/// The template whose resource the owning service refuses, and the one that corrects it.
fn refused_and_corrected(resource: RefusedResource, stack: &str) -> (String, String) {
    match resource {
        RefusedResource::AnAlias => (
            template(stack, &["911"], CREATED_DESCRIPTION),
            template(stack, &[HEALTHY_VERSION], CREATED_DESCRIPTION),
        ),
        RefusedResource::ALogGroup => (
            log_group_template(stack, REFUSED_RETENTION),
            log_group_template(stack, TAKEN_RETENTION),
        ),
    }
}

/// Whether the service that OWNS the row's resource holds it, read through that service's own
/// wire.
async fn held_by_the_owning_service(
    router: &MultiServiceRouter,
    resource: RefusedResource,
    stack: &str,
) -> bool {
    match resource {
        RefusedResource::AnAlias => alias_version(router, &function_name(stack, 0))
            .await
            .is_some(),
        RefusedResource::ALogGroup => group_exists(router, &group_name(stack)).await,
    }
}

/// RULE 4: a resource whose create the owning service refused does not exist there, so the
/// fix-forward deployment that declares it again CREATES it — and the resource then exists in
/// that service, whichever module holds its type's arms.
#[tokio::test]
async fn a_refused_resource_is_created_by_the_next_deployment() {
    for row in REFUSED_ROWS {
        let case = row.case;
        let (router, engine) = router_with_engine();
        let (refused, corrected) = refused_and_corrected(row.resource, row.stack);

        let mut members = acknowledging_iam(&[]);
        members.push(member("DisableRollback", "true"));
        let (status_code, body) = create_stack(&router, row.stack, &refused, &members).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {status_code} {body}"
        );
        let settled = settled_status(&router, &engine, case, row.stack).await;
        assert_eq!(
            settled, "CREATE_FAILED",
            "case {case:?}: expected the refused create to park the stack, got {settled:?}"
        );
        let reported = reported_resource_status(&router, case, row.stack, row.logical_id).await;
        assert_eq!(
            reported, "CREATE_FAILED",
            "case {case:?}: expected {:?} to report its refused create, got {reported:?}",
            row.logical_id
        );
        let held = held_by_the_owning_service(&router, row.resource, row.stack).await;
        assert!(
            !held,
            "case {case:?}: expected the owning service to hold no {:?}, got one",
            row.logical_id
        );

        // The fix-forward update rule 2 admits: what it has to carry out is the CREATE of the
        // resource the refusal left unmade.
        let members = acknowledging_iam(&[
            member("StackName", row.stack),
            member("TemplateBody", &corrected),
            member("DisableRollback", "true"),
        ]);
        let (status_code, answered) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the fix-forward update to be accepted, got {status_code} \
             {answered}"
        );
        let settled = settled_status(&router, &engine, case, row.stack).await;
        let reason = reported_status_reason(&router, case, row.stack).await;
        assert_eq!(
            settled, "UPDATE_COMPLETE",
            "case {case:?}: expected the fix-forward update to complete, got {settled:?} \
             ({reason:?})"
        );
        let reported = reported_resource_status(&router, case, row.stack, row.logical_id).await;
        assert_eq!(
            reported, "CREATE_COMPLETE",
            "case {case:?}: expected {:?} to report the create this deployment carried out, got \
             {reported:?}",
            row.logical_id
        );
        // ...and so does the resource's own event stream, which is where a client watches the
        // work WHILE it is under way: the deployment opened a CREATE on it, not a change to a
        // resource nothing held.
        let mut sequence = resource_event_statuses(&router, case, row.stack, row.logical_id).await;
        sequence.reverse();
        let after_the_refusal: Vec<String> = sequence
            .into_iter()
            .skip_while(|status| status != "CREATE_FAILED")
            .skip(1)
            .collect();
        assert!(
            after_the_refusal.last().map(String::as_str) == Some("CREATE_COMPLETE")
                && after_the_refusal
                    .iter()
                    .all(|status| status.starts_with("CREATE_")),
            "case {case:?}: expected the fix-forward deployment to open and finish a CREATE of \
             {:?} and report no change to it, got {after_the_refusal:?}",
            row.logical_id
        );
        let held = held_by_the_owning_service(&router, row.resource, row.stack).await;
        assert!(
            held,
            "case {case:?}: expected the owning service to hold {:?} now, got none",
            row.logical_id
        );
    }
}
