// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING of the failure an execution reports when Step
//! Functions may not assume the state machine's execution role, driven over both
//! services' own wire protocols through the production `MultiServiceRouter` with Step
//! Functions and IAM mounted on ONE engine — the shape `core/router/src/lib.rs` mounts
//! them in — so the trust document the assume is decided against is the one IAM's own
//! handlers wrote.
//!
//! The rule under test: when the assume of a state machine's execution role is refused,
//! the failure carries the service's FIXED sentence blaming its own service principal
//! and naming the configured role in a parenthesised suffix — "The principal
//! states.amazonaws.com is not authorized to assume the provided role. (role: <role
//! ARN>)" — whose only variable part is that role ARN, so a catcher or a log alarm
//! matching the sentence, or parsing the role out of the parenthesis, reads the same
//! text whatever made the assume fail. Which of the service's principals this emulator
//! asked IAM about, and the Region it asked in, are how the decision is TAKEN and
//! belong to no refusal.
//!
//! The rows differ along what the rule generalises over: WHY the assume fails (a trust
//! policy naming another service principal, one explicitly denying the assume, one
//! narrowed by `aws:SourceArn` to another account's state machines, one narrowed to a
//! condition key the assume carries no value for, and an ARN this account's IAM holds
//! no role under) and the FORM of the role ARN substituted into the sentence (a role at
//! the root path, under a path, and under a nested path, so the suffix carries the ARN
//! IAM minted verbatim). The same sentence is asserted everywhere the failure surfaces:
//! `DescribeExecution`, the `TaskFailed` history event, and the synchronous answer an
//! EXPRESS workflow's `StartSyncExecution` carries.
//!
//! Whether a role can be assumed AT ALL is `tests/all/execution_role_assumption.rs`'s
//! rule and the conditions a trust policy narrows the assume to are
//! `tests/all/execution_role_trust_conditions.rs`'s; the harness (`Chain`) is
//! `tests/all/task_execution_role.rs`'s and the parent/child definitions, the unheld
//! role and the inline policy are `execution_role_assumption`'s, so this suite states
//! none of them twice: `tests/all.rs` is this binary's crate root, so each is reached
//! through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::integration::TASK_FAILED_ERROR;
use service_com_amazonaws_sfn_awsstepfunctions::state::EVENT_TASK_FAILED;

use crate::execution_data_encryption::xml_element;
use crate::execution_role_assumption::{allow_everything, parent_definition, CHILD, NOT_HELD};
use crate::state_machine_lifecycle::{string, ACCOUNT, REGION};
use crate::task_execution_role::Chain;

/// This service's own global service principal, the identity a service role's trust
/// policy is written for and the one the refusal blames.
const SERVICE_PRINCIPAL: &str = "states.amazonaws.com";

/// An account that is not the one these resources belong to, for a trust condition the
/// assume cannot satisfy.
const ANOTHER_ACCOUNT: &str = "111122223333";

/// AWS's sentence for an execution role this service may not assume: every word of it
/// fixed, with only `role_arn` — the `roleArn` configured on the state machine —
/// substituted into the parenthesised suffix.
fn expected_cause(role_arn: &str) -> String {
    format!(
        "The principal {SERVICE_PRINCIPAL} is not authorized to assume the provided role. \
         (role: {role_arn})"
    )
}

/// A trust policy with one statement: `effect` for `principal` over `sts:AssumeRole`,
/// narrowed by `condition` where the row states one.
///
/// `tests/all/execution_role_assumption.rs`'s own `trust` varies the ACTION instead, and
/// `tests/all/execution_role_trust_conditions.rs`'s is over the KMS-mounted chain, so
/// the one this suite's rows need — a condition over this service's own principal,
/// decided on the data plane — is stated here.
fn trust(effect: &str, principal: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Principal": { "Service": principal },
        "Action": "sts:AssumeRole",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// A role of this account held at `path` under `name`, carrying `trust` as its trust
/// policy and `allow_everything` inline, by the ARN IAM minted for it.
///
/// It differs from `execution_role_assumption`'s `role_trusting` in taking a `Path`,
/// which is what makes the role ARN the refusal has to carry vary by form.
async fn role_at(chain: &Chain, name: &str, path: &str, trust: &Value) -> String {
    let created = chain
        .iam(
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("Path", path.to_string()),
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

/// One row: why the assume fails, and the role ARN the refusal has to name.
struct Row {
    case: &'static str,
    role_arn: String,
}

/// Every way an assume of an execution role is refused, each paired with a different
/// form of role ARN.
async fn rows(chain: &Chain) -> Vec<Row> {
    vec![
        Row {
            case: "a trust policy naming another service principal, role at the root path",
            role_arn: role_at(
                chain,
                "cause-other-principal",
                "/",
                &trust("Allow", "lambda.amazonaws.com", None),
            )
            .await,
        },
        Row {
            case: "a trust policy explicitly denying the assume, role under a path",
            role_arn: role_at(
                chain,
                "cause-explicit-deny",
                "/service-role/",
                &trust("Deny", SERVICE_PRINCIPAL, None),
            )
            .await,
        },
        Row {
            // The confused-deputy guard, pointed at the wrong account: the assume
            // presents THIS state machine under `aws:SourceArn`, which no state machine
            // of another account matches.
            case: "a trust narrowed by aws:SourceArn to another account, role under a nested path",
            role_arn: role_at(
                chain,
                "cause-foreign-source-arn",
                "/service-role/states/",
                &trust(
                    "Allow",
                    SERVICE_PRINCIPAL,
                    Some(json!({
                        "ArnLike": {
                            "aws:SourceArn":
                                format!("arn:aws:states:{REGION}:{ANOTHER_ACCOUNT}:stateMachine:*"),
                        }
                    })),
                ),
            )
            .await,
        },
        Row {
            // Nothing is guessed for a key the assume does not carry, so the clause
            // cannot be shown to hold and the grant does not cover the assume.
            case: "a trust narrowed to a condition key the assume carries no value for",
            role_arn: role_at(
                chain,
                "cause-undecidable-condition",
                "/",
                &trust(
                    "Allow",
                    SERVICE_PRINCIPAL,
                    Some(json!({ "StringEquals": { "sts:ExternalId": "a-shared-secret" } })),
                ),
            )
            .await,
        },
        Row {
            // No trust document at all to go by: the sentence still names the role
            // reference the state machine was configured with.
            case: "an ARN this account's IAM holds no role under",
            role_arn: NOT_HELD.to_string(),
        },
    ]
}

/// What the cause must NOT carry: the Region this emulator asked IAM in, and the
/// Region's own service principal, neither of which belongs to the refusal.
fn names_no_region(case: &str, cause: &str) {
    for leaked in [REGION, &format!("states.{REGION}.amazonaws.com")] {
        assert!(
            !cause.contains(leaked),
            "case {case:?}: expected no {leaked:?} in the cause, got {cause:?}"
        );
    }
}

/// Every refusal of an execution role's assume fails the `Task` with the one fixed
/// sentence, naming the configured role and nothing else — in `DescribeExecution` and in
/// the `TaskFailed` history event alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unassumable_role_fails_the_task_with_the_fixed_sentence() {
    let chain = Chain::new();
    let child_arn = chain.machine("cause-child", CHILD, NOT_HELD).await;

    for (index, row) in rows(&chain).await.into_iter().enumerate() {
        let Row { case, role_arn } = row;
        let parent = chain
            .machine(
                &format!("cause-parent-{index}"),
                &parent_definition(&child_arn),
                &role_arn,
            )
            .await;
        let (execution_arn, described) = chain.ran(&parent, "only", case).await;

        let expected = expected_cause(&role_arn);
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
                described.get("cause").and_then(Value::as_str),
            ),
            (
                Some("FAILED"),
                Some(TASK_FAILED_ERROR),
                Some(expected.as_str()),
            ),
            "case {case:?}: expected FAILED/{TASK_FAILED_ERROR} with {expected:?}, got {described:?}"
        );
        names_no_region(case, &string(&described, "cause"));
        // The role ARN is the one IAM minted, path and all, so a reader can parse the
        // role out of the parenthesis and act on exactly that reference.
        let cause = string(&described, "cause");
        assert!(
            cause.ends_with(&format!("(role: {role_arn})")),
            "case {case:?}: expected the cause to end naming {role_arn:?}, got {cause:?}"
        );

        // The TASK reported that failure as its own, with the same sentence.
        let events = chain.history(&execution_arn).await;
        let details: Vec<&Value> = events
            .iter()
            .filter(|event| event.get("type").and_then(Value::as_str) == Some(EVENT_TASK_FAILED))
            .filter_map(|event| event.get("taskFailedEventDetails"))
            .collect();
        assert_eq!(
            details.len(),
            1,
            "case {case:?}: expected exactly one {EVENT_TASK_FAILED} detail, got {events:?}"
        );
        assert_eq!(
            (
                details[0].get("error").and_then(Value::as_str),
                details[0].get("cause").and_then(Value::as_str),
            ),
            (Some(TASK_FAILED_ERROR), Some(expected.as_str())),
            "case {case:?}: expected the event to carry {expected:?}, got {:?}",
            details[0]
        );
    }
}

/// The synchronous answer an EXPRESS workflow's `StartSyncExecution` carries is the same
/// sentence: one wording for the refusal, wherever the execution reports it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_synchronous_express_run_answers_the_same_sentence() {
    let chain = Chain::new();
    let child_arn = chain.machine("sync-cause-child", CHILD, NOT_HELD).await;
    let role_arn = role_at(
        &chain,
        "sync-cause-role",
        "/service-role/",
        &trust("Allow", "lambda.amazonaws.com", None),
    )
    .await;
    let case = "an EXPRESS workflow whose role trusts another service principal";

    let created = chain
        .sfn(
            "CreateStateMachine",
            &json!({
                "name": "sync-cause-parent",
                "definition": parent_definition(&child_arn),
                "roleArn": role_arn,
                "type": "EXPRESS",
            }),
        )
        .await;
    let machine = string(&created, "stateMachineArn");
    let answered = chain
        .sfn(
            "StartSyncExecution",
            &json!({ "stateMachineArn": machine, "name": "only" }),
        )
        .await;

    let expected = expected_cause(&role_arn);
    assert_eq!(
        (
            answered.get("status").and_then(Value::as_str),
            answered.get("error").and_then(Value::as_str),
            answered.get("cause").and_then(Value::as_str),
        ),
        (
            Some("FAILED"),
            Some(TASK_FAILED_ERROR),
            Some(expected.as_str()),
        ),
        "case {case:?}: expected FAILED/{TASK_FAILED_ERROR} with {expected:?}, got {answered:?}"
    );
    names_no_region(case, &string(&answered, "cause"));
    // The account the role belongs to is this one, read out of the ARN the sentence
    // carries rather than written into the sentence.
    assert!(
        role_arn.starts_with(&format!("arn:aws:iam::{ACCOUNT}:role/")),
        "case {case:?}: expected a role of this account, got {role_arn:?}"
    );
}
