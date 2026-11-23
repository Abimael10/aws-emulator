// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH EVIDENCE refuses the CHILD WORKFLOW EXECUTIONS a
//! Distributed Map state starts, driven over both services' own wire protocols through
//! the production `MultiServiceRouter` with Step Functions and IAM mounted on ONE engine
//! — the shape `core/router/src/lib.rs` mounts them in — so the documents the start is
//! judged against are the ones IAM's own handlers wrote.
//!
//! The rule under test: Step Functions starts a Map Run's child workflow executions
//! ITSELF, on the state machine the Map Run belongs to, so no invoked service answers
//! that start and it fails the WHOLE run before its first item — and it is therefore
//! decided on what this account HOLDS about the execution role. A document the account
//! holds that withholds `states:StartExecution` on the state machine fails the run
//! having started nothing, and one that grants it runs every item; but an account
//! holding NO identity-based document for the role has nothing to read a decision off,
//! exactly as it has nothing to read off an attachment whose document it does not hold,
//! so the children are started. A role's permissions are provisioned outside the client
//! that creates the state machine, which is why `CreateStateMachine` serves a `roleArn`
//! the account does not hold at all and why no session is minted for a role this
//! emulator knows nothing about rather than its calls being refused.
//!
//! The rows differ along what the account holds for the role: no identity-based document,
//! a document granting the action on the resource, a document granting every action of
//! the service, a document that is silent about the action, a document granting it over
//! another resource, and a document that denies it over its own grant.
//!
//! The second test is the BOUNDARY of that reading: a `Task` state's integration call is
//! a call to another service, which decides the same request itself when the call reaches
//! its own front door, so there the documents are read as that service reads them —
//! implicit deny included — and a role the account holds no document for is refused.
//!
//! Whether the role can be ASSUMED at all is the question before this one
//! (`tests/all/execution_role_assumption.rs`), and what a refusal SAYS is
//! `tests/all/task_refusal_session_principal.rs`'s; the harness (`Chain`,
//! `trust_document`) is `tests/all/task_execution_role.rs`'s and the parent/child
//! definitions are `tests/all/execution_role_assumption.rs`'s, so this suite states none
//! of them twice: `tests/all/main.rs` is this binary's crate root, so each is reached
//! through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::RUNTIME_ERROR;

use crate::execution_data_encryption::xml_element;
use crate::execution_role_assumption::{parent_definition, CHILD, NOT_HELD};
use crate::state_machine_lifecycle::{ACCOUNT, REGION};
use crate::task_execution_role::{trust_document, Chain};

/// The IAM action starting a child workflow execution of a Map Run is subject to, which
/// is also the action the `Task` state's own integration call asks for.
const START_EXECUTION: &str = "states:StartExecution";

/// How many items every Distributed Map below fans out over, each one its own child
/// workflow execution.
const ITEMS: i64 = 2;

/// What the account holds for the execution role, as the document `PutRolePolicy` wrote
/// — built over the ARN the state machine will carry, which is the resource the calls
/// are taken on.
type Document = fn(&str) -> Value;

/// One row: what the account holds for the role, and what each of the two decisions
/// then makes of it.
struct Row {
    case: &'static str,
    /// `None` for a role the account holds no identity-based document for.
    document: Option<Document>,
    /// Whether the Map Run's child workflow executions are started — the decision no
    /// invoked service answers, which rests on what the account HOLDS.
    children_run: bool,
    /// Whether a `Task` state's own call to another service is made — the decision that
    /// service answers itself, implicit deny included. The two differ on exactly one
    /// row, which is the rule's boundary.
    task_call_made: bool,
}

/// One statement allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// The table both tests below run: what the account holds for the role decides the
/// call, and holding nothing decides only the call another service answers.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no identity-based document at all",
            document: None,
            children_run: true,
            task_call_made: false,
        },
        Row {
            case: "a document granting the action on the state machine",
            document: Some(|machine| json!([allow(START_EXECUTION, machine)])),
            children_run: true,
            task_call_made: true,
        },
        Row {
            case: "a document granting every action of the service",
            document: Some(|_machine| json!([allow("states:*", "*")])),
            children_run: true,
            task_call_made: true,
        },
        Row {
            case: "a document that is silent about the action",
            document: Some(|machine| json!([allow("states:DescribeExecution", machine)])),
            children_run: false,
            task_call_made: false,
        },
        Row {
            case: "a document granting the action over another state machine",
            document: Some(|_machine| {
                json!([allow(
                    START_EXECUTION,
                    &format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:elsewhere")
                )])
            }),
            children_run: false,
            task_call_made: false,
        },
        Row {
            case: "a document denying the action over its own grant",
            document: Some(|machine| {
                json!([
                    allow(START_EXECUTION, machine),
                    { "Effect": "Deny", "Action": START_EXECUTION, "Resource": machine },
                ])
            }),
            children_run: false,
            task_call_made: false,
        },
    ]
}

/// A Distributed Map over [`ITEMS`] literal items whose processor is one `Pass`: the
/// items are literal, so an execution of it needs no input of its own.
fn distributed_map() -> String {
    json!({
        "StartAt": "Fanout",
        "States": {
            "Fanout": {
                "Type": "Map",
                "Items": (1..=ITEMS).collect::<Vec<i64>>(),
                "ItemProcessor": {
                    "ProcessorConfig": { "Mode": "DISTRIBUTED", "ExecutionType": "EXPRESS" },
                    "StartAt": "Pass",
                    "States": { "Pass": { "Type": "Pass", "End": true } },
                },
                "End": true,
            },
        },
    })
    .to_string()
}

/// The role this row runs as, held under `name` with a trust policy this service can
/// assume and the document the row names (and nothing at all where it names none), by
/// the ARN IAM minted for it.
async fn role_for(chain: &Chain, name: &str, document: Option<Value>) -> String {
    let created = chain
        .iam(
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust_document().to_string()),
            ],
        )
        .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateRole answers the role Arn, got {created}"));
    if let Some(document) = document {
        chain
            .iam(
                "PutRolePolicy",
                &[
                    ("RoleName", name.to_string()),
                    ("PolicyName", "inline".to_string()),
                    ("PolicyDocument", document.to_string()),
                ],
            )
            .await;
    }
    arn
}

/// The ARN `CreateStateMachine` will mint for `name`, so a row may write its policy over
/// the resource the calls are taken on before the machine exists.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// The Map Run of `execution_arn`, or `None` where the execution started none.
async fn map_run(chain: &Chain, execution_arn: &str) -> Option<Value> {
    let listed = chain
        .sfn("ListMapRuns", &json!({ "executionArn": execution_arn }))
        .await;
    let arn = listed
        .get("mapRuns")
        .and_then(Value::as_array)?
        .first()?
        .get("mapRunArn")?
        .as_str()?
        .to_string();
    Some(
        chain
            .sfn("DescribeMapRun", &json!({ "mapRunArn": arn }))
            .await,
    )
}

/// The `status` of a described record, as a string.
fn status(described: &Value) -> Option<&str> {
    described.get("status").and_then(Value::as_str)
}

/// The `status` the described Map Run reports, and `None` where the execution started
/// no Map Run at all.
fn run_status(run: &Option<Value>) -> Option<&str> {
    run.as_ref().and_then(|run| status(run))
}

/// How many child workflow executions `ListExecutions` reports for the Map Run
/// `run` is, and `0` where there is no Map Run to ask about.
async fn children_count(chain: &Chain, run: &Option<Value>) -> usize {
    let Some(arn) = run
        .as_ref()
        .and_then(|run| run.get("mapRunArn"))
        .and_then(Value::as_str)
    else {
        return 0;
    };
    let listed = chain
        .sfn("ListExecutions", &json!({ "mapRunArn": arn }))
        .await;
    listed
        .get("executions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// A Distributed Map's child workflow executions are refused only on a document the
/// account HOLDS for the execution role: a role it holds none for runs the Map Run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_map_runs_children_are_refused_only_on_a_document_the_account_holds() {
    let chain = Chain::new();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("documented-map-{index}");
        let expected_arn = machine_arn(&name);
        let document = row.document.map(|build| {
            json!({ "Version": "2012-10-17", "Statement": build(&expected_arn) })
        });
        let role_arn = role_for(&chain, &format!("DocumentedMapRole{index}"), document).await;
        let created = chain.machine(&name, &distributed_map(), &role_arn).await;
        assert_eq!(
            created, expected_arn,
            "case {case:?}: the document is written over the machine's own ARN, got {created}"
        );
        let (execution, described) = chain.ran(&created, &format!("run-{index}"), case).await;
        let run = map_run(&chain, &execution).await;
        let counted = children_count(&chain, &run).await;

        if row.children_run {
            assert_eq!(
                status(&described),
                Some("SUCCEEDED"),
                "case {case:?}: the Map Run ran, so the execution succeeded, got {described:?}"
            );
            assert_eq!(
                run_status(&run),
                Some("SUCCEEDED"),
                "case {case:?}: expected a SUCCEEDED Map Run, got {run:?}"
            );
            let counts = run
                .as_ref()
                .and_then(|run| run.get("itemCounts").cloned())
                .unwrap_or(Value::Null);
            assert_eq!(
                (
                    counts.get("total").and_then(Value::as_i64),
                    counts.get("succeeded").and_then(Value::as_i64),
                    counts.get("failed").and_then(Value::as_i64),
                ),
                (Some(ITEMS), Some(ITEMS), Some(0)),
                "case {case:?}: every item ran, got {counts:?}"
            );
            assert_eq!(
                counted,
                usize::try_from(ITEMS).unwrap_or_default(),
                "case {case:?}: one child workflow execution per item, got {counted}"
            );
            continue;
        }
        assert_eq!(
            (status(&described), described.get("error").and_then(Value::as_str)),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {case:?}: a refused child start fails the execution, got {described:?}"
        );
        let cause = described.get("cause").and_then(Value::as_str).unwrap_or("");
        assert!(
            cause.contains(&format!("is not authorized to perform: {START_EXECUTION}")),
            "case {case:?}: the cause is the refusal the start came back with, got {cause:?}"
        );
        assert_eq!(
            run_status(&run),
            Some("FAILED"),
            "case {case:?}: expected a FAILED Map Run, got {run:?}"
        );
        assert_eq!(
            counted, 0,
            "case {case:?}: a refused Map Run starts no child, got {counted}"
        );
    }
}

/// How many executions `ListExecutions` reports for `machine_arn`.
async fn started_count(chain: &Chain, machine_arn: &str) -> usize {
    let listed = chain
        .sfn(
            "ListExecutions",
            &json!({ "stateMachineArn": machine_arn, "maxResults": 10 }),
        )
        .await;
    listed
        .get("executions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// The BOUNDARY of that reading: a `Task` state's integration call is a call to another
/// service, which decides the same request itself, so there the documents are read as
/// that service reads them — a role the account holds no document for is refused by the
/// implicit deny every identity starts from, and only a document that grants the call
/// makes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tasks_own_call_is_refused_by_the_implicit_deny_of_a_documentless_role() {
    let chain = Chain::new();
    // What the account holds decides the call either way; holding nothing is the implicit
    // deny here rather than an absence of evidence, which is the one row the two tables
    // answer differently.
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let made = row.task_call_made;
        // The child the parent's `Task` starts. Its own role decides nothing: it runs one
        // `Pass` and makes no call.
        let child = chain
            .machine(&format!("documented-task-child-{index}"), CHILD, NOT_HELD)
            .await;
        let document = row
            .document
            .map(|build| json!({ "Version": "2012-10-17", "Statement": build(&child) }));
        let role_arn = role_for(&chain, &format!("DocumentedTaskRole{index}"), document).await;
        let parent = chain
            .machine(
                &format!("documented-task-{index}"),
                &parent_definition(&child),
                &role_arn,
            )
            .await;
        let (_execution, described) = chain.ran(&parent, &format!("run-{index}"), case).await;
        let started = started_count(&chain, &child).await;

        if made {
            assert_eq!(
                status(&described),
                Some("SUCCEEDED"),
                "case {case:?}: the task's call was made, got {described:?}"
            );
            assert_eq!(
                started, 1,
                "case {case:?}: the child execution the task started, got {started}"
            );
            continue;
        }
        assert_eq!(
            status(&described),
            Some("FAILED"),
            "case {case:?}: a refused call fails the task, got {described:?}"
        );
        let cause = described.get("cause").and_then(Value::as_str).unwrap_or("");
        assert!(
            cause.contains(&format!("is not authorized to perform: {START_EXECUTION}")),
            "case {case:?}: the cause is the refusal the call came back with, got {cause:?}"
        );
        assert_eq!(
            started, 0,
            "case {case:?}: a refused call starts no child execution, got {started}"
        );
    }
}
