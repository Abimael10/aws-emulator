// SPDX-License-Identifier: CC0-1.0

//! The CONFUSED-DEPUTY clauses of a state machine's execution-role trust, over the two ways
//! an execution of that machine is run, driven over both services' own wire protocols
//! through the production `MultiServiceRouter` with Step Functions and IAM mounted on ONE
//! engine — the shape `core/router/src/lib.rs` mounts them in — so the trust document the
//! assume is decided against is the one IAM's own handlers wrote.
//!
//! RULE: Step Functions takes a state machine's `roleArn` on through `sts:AssumeRole` to do
//! an execution's work, presenting the state machine the work is for under the two global
//! condition keys a service trust policy is written against (`aws:SourceArn`, that state
//! machine's own ARN, and `aws:SourceAccount`, the account that owns it). A statement's
//! `Condition` is part of its grant, so a trust narrowed to ANOTHER account's state machines
//! grants nothing for this account's: every `Task` of such an execution fails with
//! `States.TaskFailed` and the service's fixed sentence for a role it cannot act as, and the
//! work the task named never happens — identically for a STANDARD workflow's
//! `DescribeExecution` and for the answer an EXPRESS workflow's `StartSyncExecution` carries,
//! because it is one assume and one refusal. The same clauses pointed at THIS account are
//! satisfied by that same assume, so the twin execution runs.
//!
//! The rows differ along what the rule generalises over: which key the clause is written over
//! and with which operator (`ArnLike aws:SourceArn`, `ArnEquals aws:SourceArn`,
//! `StringEquals aws:SourceAccount`), whether the value it names is this account's or
//! another's, and how many clauses the statement carries — and each row is run through BOTH
//! workflow types.
//!
//! The harness (`Chain`) is `tests/all/task_execution_role.rs`'s and the parent/child
//! definitions and the inline policy are `tests/all/execution_role_assumption.rs`'s;
//! `tests/all/main.rs` is this binary's crate root, so each is reached through
//! `crate::<suite>`. The WORDING of the refusal over every way an assume fails is
//! `tests/all/unassumable_role_cause.rs`'s rule, and the conditions over a start that has to
//! acquire a data key first are `tests/all/execution_role_trust_conditions.rs`'s.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::integration::TASK_FAILED_ERROR;

use crate::execution_data_encryption::xml_element;
use crate::execution_role_assumption::{allow_everything, parent_definition, CHILD};
use crate::state_machine_lifecycle::{string, ACCOUNT, REGION};
use crate::task_execution_role::Chain;

/// This service's own global service principal, the identity a service role's trust policy
/// is written for and the one the refusal blames.
const SERVICE_PRINCIPAL: &str = "states.amazonaws.com";

/// An account that is not the one these resources belong to, for a clause the assume cannot
/// satisfy.
const ANOTHER_ACCOUNT: &str = "111122223333";

/// AWS's sentence for an execution role this service may not assume: every word of it fixed,
/// with only the `roleArn` the state machine was configured with substituted into the
/// parenthesised suffix.
fn unassumable_cause(role_arn: &str) -> String {
    format!(
        "The principal {SERVICE_PRINCIPAL} is not authorized to assume the provided role. \
         (role: {role_arn})"
    )
}

/// A trust policy granting this service's principal the assume, narrowed by `condition`.
fn trust(condition: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SERVICE_PRINCIPAL },
            "Action": "sts:AssumeRole",
            "Condition": condition,
        }],
    })
}

/// A role of this account carrying `trust` as its trust policy and `allow_everything` as its
/// inline policy, by the ARN IAM minted for it: a row that gets past the assume must not then
/// be refused on what the role may do.
async fn role_trusting(chain: &Chain, name: &str, trust: &Value) -> String {
    let created = chain
        .iam(
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust.to_string()),
            ],
        )
        .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateRole answers the role Arn, got {created}"));
    chain
        .iam(
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", allow_everything().to_string()),
            ],
        )
        .await;
    arn
}

/// One row: how the trust statement's `Condition` is narrowed, and whether the assume it
/// governs covers an execution of a state machine of THIS account.
struct Row {
    case: &'static str,
    condition: fn() -> Value,
    assumable: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "another account's state machines under ArnLike aws:SourceArn",
            condition: || {
                json!({
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:states:{REGION}:{ANOTHER_ACCOUNT}:stateMachine:*"),
                    }
                })
            },
            assumable: false,
        },
        Row {
            case: "another account's state machine under ArnEquals aws:SourceArn",
            condition: || {
                json!({
                    "ArnEquals": {
                        "aws:SourceArn":
                            format!(
                                "arn:aws:states:{REGION}:{ANOTHER_ACCOUNT}:stateMachine:elsewhere"
                            ),
                    }
                })
            },
            assumable: false,
        },
        Row {
            case: "this account under StringEquals aws:SourceAccount beside another \
                   account's machines under ArnLike aws:SourceArn",
            condition: || {
                json!({
                    "StringEquals": { "aws:SourceAccount": ACCOUNT },
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:states:{REGION}:{ANOTHER_ACCOUNT}:stateMachine:*"),
                    },
                })
            },
            assumable: false,
        },
        Row {
            case: "this account's state machines under ArnLike aws:SourceArn beside its own \
                   account under StringEquals aws:SourceAccount",
            condition: || {
                json!({
                    "StringEquals": { "aws:SourceAccount": ACCOUNT },
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:*"),
                    },
                })
            },
            assumable: true,
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trust_narrowed_to_another_accounts_machines_blocks_every_task_of_the_execution() {
    let chain = Chain::new();
    // The work the parent's `Task` names: a child this account holds, started under a role
    // whose trust this service's principal satisfies, so nothing but the parent's own role
    // decides a row.
    let child_role = role_trusting(
        &chain,
        "source-arn-child-role",
        &trust(json!({ "StringEquals": { "aws:SourceAccount": ACCOUNT } })),
    )
    .await;
    let child = chain
        .machine("source-arn-child", CHILD, &child_role)
        .await;

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            condition,
            assumable,
        } = row;
        let role_arn = role_trusting(
            &chain,
            &format!("source-arn-role-{index}"),
            &trust(condition()),
        )
        .await;
        let definition = parent_definition(&child);

        // The STANDARD workflow, read back through `DescribeExecution` once it has left
        // `RUNNING`.
        let standard = chain
            .machine(&format!("source-arn-standard-{index}"), &definition, &role_arn)
            .await;
        let (_, described) = chain.ran(&standard, "only", case).await;

        // ...and the EXPRESS workflow, whose synchronous start answers the same outcome in
        // the call itself.
        let created = chain
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": format!("source-arn-express-{index}"),
                    "definition": definition,
                    "roleArn": role_arn,
                    "type": "EXPRESS",
                }),
            )
            .await;
        let answered = chain
            .sfn(
                "StartSyncExecution",
                &json!({
                    "stateMachineArn": string(&created, "stateMachineArn"),
                    "name": "only",
                }),
            )
            .await;

        for (surface, reported) in [("DescribeExecution", &described), ("StartSyncExecution", &answered)] {
            let outcome = (
                reported.get("status").and_then(Value::as_str),
                reported.get("error").and_then(Value::as_str),
                reported.get("cause").and_then(Value::as_str),
            );
            match assumable {
                false => {
                    let expected = unassumable_cause(&role_arn);
                    assert_eq!(
                        outcome,
                        (
                            Some("FAILED"),
                            Some(TASK_FAILED_ERROR),
                            Some(expected.as_str())
                        ),
                        "case {case:?} on {surface}: expected FAILED/{TASK_FAILED_ERROR} with \
                         {expected:?}, got {reported:?}"
                    );
                }
                true => {
                    assert_eq!(
                        (outcome.0, outcome.1),
                        (Some("SUCCEEDED"), None),
                        "case {case:?} on {surface}: expected the execution to run, got \
                         {reported:?}"
                    );
                }
            }
        }
    }
}
