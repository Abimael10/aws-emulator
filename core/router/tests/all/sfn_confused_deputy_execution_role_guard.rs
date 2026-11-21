// SPDX-License-Identifier: CC0-1.0

//! The CONFUSED-DEPUTY guard on a Step Functions execution role's trust policy, driven
//! across the three services the question spans — Step Functions, IAM and the token
//! service — mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them, so the
//! trust document the assume is decided against is the one IAM's own handlers wrote and
//! the call the `Task` state makes is served by the token service itself.
//!
//! RULE: Step Functions does an execution's work by assuming the state machine's
//! `roleArn`, presenting the state machine the work is for under the two condition keys a
//! service trust policy is guarded with (`aws:SourceArn`, the state machine's own ARN, and
//! `aws:SourceAccount`, the account that owns it). A `Condition` is part of its statement's
//! grant, so a guard the starting state machine does not satisfy withholds the assume and
//! the `Task` that needs the role fails — with the service's FIXED sentence, whose only
//! variable part is the role reference the state machine was configured with: "The
//! principal states.amazonaws.com is not authorized to assume the provided role. (role:
//! <role ARN>)". Which of this service's principals the assume was asked about, and the
//! Region it was asked in, are how the decision is TAKEN and belong to no refusal. A guard
//! the state machine DOES satisfy leaves the assume intact, so the call is made as a
//! session of that role and answered.
//!
//! The rows differ along what the rule generalises over: the condition OPERATOR
//! (`StringEquals`, `StringNotEquals`, `ArnEquals`, `ArnLike`), WHICH of the two keys the
//! guard is written over, whether the value the assume presents satisfies it, how many
//! clauses the statement carries, and the PATH IAM holds the role under — which the role
//! ARN in the parenthesised suffix carries verbatim, and an STS session ARN does not carry
//! at all.
//!
//! `arn:aws:states:::aws-sdk:sts:getCallerIdentity` is the `Task`'s work throughout,
//! because it is the call that REPORTS the identity it was made under: a satisfied guard is
//! read back as the session of the state machine's own role, which is what shows the role
//! was really assumed rather than the state skipped.
//!
//! Three crates have to agree for that to hold — Step Functions decides the assume and
//! makes the call, IAM holds the trust document, and the token service both resolves the
//! credential and serves the call — so the suite lives here rather than in any of them. The
//! harness it borrows (the IAM caller, the Step Functions caller, the execution reader, the
//! failed task's details and the session spelling) is `crate::sfn_task_target_authorization`'s
//! and the XML reader is `crate::query_wire`'s; `tests/all/main.rs` is one crate root, so
//! this suite mounts the services the question needs and states none of them twice.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::state::integration::TASK_FAILED_ERROR;

use crate::query_wire::element;
use crate::sfn_task_target_authorization::{
    iam, ran_with_input, session_principal, sfn, string, task_failure, ACCOUNT, REGION,
};

/// This service's own global service principal: the identity a service role's trust
/// policy is written for, and the one the refusal blames.
const SERVICE_PRINCIPAL: &str = "states.amazonaws.com";

/// The `Resource` of the AWS SDK integration that reports the identity its call was made
/// under.
const WHO_AM_I: &str = "arn:aws:states:::aws-sdk:sts:getCallerIdentity";

/// An account that is not the one these resources belong to, for a guard the assume
/// cannot satisfy.
const ANOTHER_ACCOUNT: &str = "111122223333";

/// Step Functions, IAM and the token service over ONE engine: the state machine's role
/// lives in IAM, and the credential the `Task`'s call presents is resolved by the token
/// service, which is also the service the call goes to.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// AWS's sentence for an execution role this service may not assume: every word of it
/// fixed, with only `role_arn` — the `roleArn` configured on the state machine, path and
/// all — substituted into the parenthesised suffix.
fn expected_cause(role_arn: &str) -> String {
    format!(
        "The principal {SERVICE_PRINCIPAL} is not authorized to assume the provided role. \
         (role: {role_arn})"
    )
}

/// A trust policy granting this service's principal the assume, guarded by `condition`
/// where the row states one.
fn trust(condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": SERVICE_PRINCIPAL },
        "Action": "sts:AssumeRole",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// The one call the rows' `Task` state makes, which is all a row's role is granted: what
/// the rows decide is whether the role can be ASSUMED, not what it may then do.
fn reports_its_identity() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "sts:GetCallerIdentity",
            "Resource": "*",
        }]
    })
}

/// A role of this account held at `path` under `name`, carrying `trust` as its trust
/// policy and the one grant its `Task`'s call needs, by the ARN IAM minted for it.
async fn guarded_role(
    router: &MultiServiceRouter,
    name: &str,
    path: &str,
    trust: &Value,
) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("Path", path.to_string()),
            ("AssumeRolePolicyDocument", trust.to_string()),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", reports_its_identity().to_string()),
        ],
    )
    .await;
    arn
}

/// The ARN a state machine of this account named `name` carries, which is the value the
/// assume presents under `aws:SourceArn`.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// A definition whose single `Task` reports the identity its own call was made under,
/// keeping the account and the principal the token service answered.
fn who_am_i_definition() -> String {
    json!({
        "StartAt": "WhoAmI",
        "States": {
            "WhoAmI": {
                "Type": "Task",
                "Resource": WHO_AM_I,
                "Parameters": {},
                "ResultSelector": { "Account.$": "$.Account", "Arn.$": "$.Arn" },
                "End": true,
            }
        }
    })
    .to_string()
}

/// A state machine named `name` running [`who_am_i_definition`] as `role_arn`, by the ARN
/// the create answered — which must be the one the row's guard was written over.
async fn machine(router: &MultiServiceRouter, name: &str, role_arn: &str, case: &str) -> String {
    let created = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": who_am_i_definition(),
            "roleArn": role_arn,
        }),
    )
    .await;
    let created_arn = string(&created, "stateMachineArn");
    assert_eq!(
        created_arn,
        machine_arn(name),
        "case {case:?}: expected the guard to be written over the machine that was created, got \
         {created_arn:?}"
    );
    created_arn
}

/// What the cause must NOT carry: the Region this emulator asked IAM in, and the Region's
/// own service principal, neither of which belongs to the refusal.
fn names_no_region(case: &str, cause: &str) {
    for leaked in [
        REGION.to_string(),
        format!("states.{REGION}.amazonaws.com"),
    ] {
        assert!(
            !cause.contains(&leaked),
            "case {case:?}: expected no {leaked:?} in the cause, got {cause:?}"
        );
    }
}

/// One row: the guard the trust statement carries, the path IAM holds the role under, and
/// whether the state machine the row starts satisfies that guard.
struct Row {
    case: &'static str,
    /// The `Condition` element, built from the ARN of the state machine the row starts.
    condition: fn(&str) -> Option<Value>,
    /// Where IAM holds the role, which the role ARN the refusal names carries verbatim.
    path: &'static str,
}

/// The guards a starting state machine does NOT satisfy, so the assume is withheld.
fn refused_rows() -> Vec<Row> {
    vec![
        Row {
            // The guard pointed at another account's state machines: the assume presents
            // THIS one under `aws:SourceArn`, which no pattern over another account
            // matches.
            case: "a guard over another account's state machines under ArnLike aws:SourceArn",
            condition: |_machine| {
                Some(json!({
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:states:{REGION}:{ANOTHER_ACCOUNT}:stateMachine:*"),
                    }
                }))
            },
            path: "/service-role/states/",
        },
        Row {
            case: "a guard naming another state machine of this account under ArnEquals \
                   aws:SourceArn",
            condition: |_machine| {
                Some(json!({ "ArnEquals": { "aws:SourceArn": machine_arn("somewhere-else") } }))
            },
            path: "/service-role/",
        },
        Row {
            case: "a guard naming another account under StringEquals aws:SourceAccount",
            condition: |_machine| {
                Some(json!({ "StringEquals": { "aws:SourceAccount": ANOTHER_ACCOUNT } }))
            },
            path: "/",
        },
        Row {
            // A negated operator over the value the assume DOES present excludes exactly
            // this account's state machines.
            case: "a guard excluding the owning account under StringNotEquals \
                   aws:SourceAccount",
            condition: |_machine| {
                Some(json!({ "StringNotEquals": { "aws:SourceAccount": ACCOUNT } }))
            },
            path: "/deputy/guards/",
        },
        Row {
            // The boundary of the rule: one clause that cannot be satisfied withholds the
            // grant whatever the clause beside it says.
            case: "a satisfied clause beside one naming another account",
            condition: |machine| {
                Some(json!({
                    "ArnEquals": { "aws:SourceArn": machine },
                    "StringEquals": { "aws:SourceAccount": ANOTHER_ACCOUNT },
                }))
            },
            path: "/deputy/",
        },
    ]
}

/// The guards the starting state machine DOES satisfy, so the assume stands.
fn satisfied_rows() -> Vec<Row> {
    vec![
        Row {
            case: "no guard at all",
            condition: |_machine| None,
            path: "/",
        },
        Row {
            case: "this state machine's own ARN under ArnEquals aws:SourceArn",
            condition: |machine| Some(json!({ "ArnEquals": { "aws:SourceArn": machine } })),
            path: "/service-role/",
        },
        Row {
            case: "every state machine of this account and Region under ArnLike aws:SourceArn",
            condition: |_machine| {
                Some(json!({
                    "ArnLike": {
                        "aws:SourceArn": format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:*"),
                    }
                }))
            },
            path: "/service-role/states/",
        },
        Row {
            case: "the owning account under StringEquals aws:SourceAccount",
            condition: |_machine| Some(json!({ "StringEquals": { "aws:SourceAccount": ACCOUNT } })),
            path: "/deputy/",
        },
        Row {
            case: "both keys together, each satisfied",
            condition: |machine| {
                Some(json!({
                    "StringEquals": { "aws:SourceAccount": ACCOUNT },
                    "ArnEquals": { "aws:SourceArn": machine },
                }))
            },
            path: "/deputy/guards/",
        },
    ]
}

/// A guard the starting state machine does not satisfy fails the `Task` with the service's
/// one fixed sentence, naming the configured role in its parenthesised suffix and nothing
/// else — in `DescribeExecution` and in the `TaskFailed` history event alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unsatisfied_guard_fails_the_task_with_the_fixed_sentence() {
    let router = emulator();

    for (index, row) in refused_rows().into_iter().enumerate() {
        let Row {
            case,
            condition,
            path,
        } = row;
        let name = format!("guard-refused-{index}");
        let role_arn = guarded_role(
            &router,
            &format!("GuardRefused{index}"),
            path,
            &trust(condition(&machine_arn(&name))),
        )
        .await;
        let guarded = machine(&router, &name, &role_arn, case).await;
        // The role ARN the refusal has to name is the one IAM minted, path and all.
        assert!(
            role_arn.starts_with(&format!("arn:aws:iam::{ACCOUNT}:role{path}")),
            "case {case:?}: expected a role of this account held under {path:?}, got {role_arn:?}"
        );

        let (execution_arn, described) =
            ran_with_input(&router, &guarded, Some("{}"), case).await;
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
            "case {case:?}: expected FAILED/{TASK_FAILED_ERROR} with {expected:?}, got {described}"
        );
        let cause = string(&described, "cause");
        names_no_region(case, &cause);
        // A reader can parse the refused role out of the parenthesis and act on exactly
        // that reference.
        assert!(
            cause.ends_with(&format!("(role: {role_arn})")),
            "case {case:?}: expected the cause to close naming {role_arn:?}, got {cause:?}"
        );
        // The work the `Task` named never happened, so nothing of the identity it would
        // have reported is in the execution's data.
        assert_eq!(
            described.get("output").and_then(Value::as_str),
            None,
            "case {case:?}: expected no output for a task that was never made, got {described}"
        );

        // The TASK reported that failure as its own, with the same sentence.
        let details = task_failure(&router, &execution_arn, case).await;
        assert_eq!(
            (
                details.get("error").and_then(Value::as_str),
                details.get("cause").and_then(Value::as_str),
            ),
            (Some(TASK_FAILED_ERROR), Some(expected.as_str())),
            "case {case:?}: expected the event to carry {expected:?}, got {details}"
        );
    }
}

/// A guard the starting state machine satisfies leaves the assume intact: the `Task`'s call
/// is made as a SESSION of that role and answered, whatever path IAM holds the role under.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_satisfied_guard_leaves_the_assume_intact() {
    let router = emulator();

    for (index, row) in satisfied_rows().into_iter().enumerate() {
        let Row {
            case,
            condition,
            path,
        } = row;
        let name = format!("guard-allowed-{index}");
        let role_arn = guarded_role(
            &router,
            &format!("GuardAllowed{index}"),
            path,
            &trust(condition(&machine_arn(&name))),
        )
        .await;
        let guarded = machine(&router, &name, &role_arn, case).await;

        let (_execution_arn, described) =
            ran_with_input(&router, &guarded, Some("{}"), case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the guarded assume to stand and the call to be answered, \
             got {described}"
        );
        let produced: Value = serde_json::from_str(&string(&described, "output"))
            .unwrap_or_else(|error| panic!("case {case:?}: expected JSON output, got {error}"));
        // The identity the call reported is a session of THIS role — which is what shows
        // the role was really assumed for the run — in the role's own account.
        let reported = string(&produced, "Arn");
        assert_eq!(
            reported,
            session_principal(&role_arn, &reported),
            "case {case:?}: expected a session of {role_arn}, got {reported:?}"
        );
        assert_eq!(
            string(&produced, "Account"),
            ACCOUNT,
            "case {case:?}: expected the session to belong to the role's account, got {produced}"
        );
    }
}
