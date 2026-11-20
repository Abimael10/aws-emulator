// SPDX-License-Identifier: CC0-1.0

//! WHICH resource an identity-based refusal of a call to this service NAMES, for the calls
//! that carry no resource ARN of their own.
//!
//! Driven end to end over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and STS mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — reusing the deployment the neighbouring
//! `identity_policy_authorization` suite builds, so the documents a decision reads are the
//! ones IAM's own handlers wrote and the tokens a call carries are the ones this service
//! minted for work it really handed over.
//!
//! RULE: the `on resource:` part of the refusal AWS words an authorization failure with is
//! the resource THE REQUEST ACTED ON — for a `SendTask*` call, which names no ARN at all,
//! the resource the task token it carries belongs to (the activity a worker was handed the
//! task by, and for a callback (`.waitForTaskToken`) task the state machine whose execution
//! waits at it) — never the `Resource` element of the statement that decided the call, and
//! the same resource whichever of IAM's rules refused it.
//!
//! The rows differ along what the rule generalises over: the OPERATION (all three calls a
//! worker settles or renews a task with, plus one addressed by an ARN of its own, so the
//! rule is not a special case of the token-addressed calls), WHICH resource the call acts on
//! (an activity, the state machine of a waiting execution, one execution), and — for each of
//! them — WHICH of IAM's rules refused the call and WHAT the documents in force name: a
//! document granting nothing of this service on every resource, a grant of the action on
//! ANOTHER resource of this service, a wildcard grant overturned by an explicit deny on
//! every resource, a wildcard grant a permissions boundary states nothing about, and one a
//! boundary explicitly denies on every resource. Every refusing document names `*` or an ARN
//! the call did not act on, so a message that echoed the statement rather than the request
//! fails here.
//!
//! What the neighbours pin and this one does not repeat: `identity_policy_authorization`
//! takes the decision itself — served where the documents allow the action on the resource
//! the token belongs to, refused where they do not — over every operation of the API, and
//! `in_function_identity_authorization` takes it for a call a RUNNING LAMBDA makes as the
//! role it runs as. What only this suite reaches is the wording of the refusal under each of
//! IAM's rules, the permissions-boundary ones included, with the documents in force naming
//! resources the request did not act on.

use serde_json::{json, Value};

use crate::execution_data_encryption::xml_element;
use crate::identity_policy_authorization::{
    activity_handoff, activity_task_token, allowing, callback_machine, callback_task_token, message,
    no_grant, not_authorized, Emulator, ACCESS_DENIED, ACCESS_DENIED_STATUS, IAM_VERSION,
};
use crate::state_machine_lifecycle::error_code;

/// The clause a call an explicit `Deny` in a readable identity-based document refuses is
/// stated with, in place of the one [`no_grant`] words a default deny with.
const EXPLICIT_IDENTITY_DENY: &str = "with an explicit deny in an identity-based policy";

/// The clause a call whose permissions boundary explicitly denies it is stated with: "a
/// permissions boundary sets the maximum permissions that an identity-based policy can
/// grant", so a grant the boundary refuses is not a permission the principal has.
const EXPLICIT_BOUNDARY_DENY: &str = "with an explicit deny in a permissions boundary";

/// The clause a call its permissions boundary states nothing about is refused with.
fn no_boundary_grant(action: &str) -> String {
    format!("because no permissions boundary allows the {action} action")
}

/// A document that ALLOWS `action` on every resource and then explicitly DENIES it there:
/// the shape an explicit deny decides, with a `Resource` element naming nothing in
/// particular — so a refusal that echoed the statement would say `*`.
fn denied_on_every_resource(action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": action, "Resource": "*" },
            { "Effect": "Deny", "Action": action, "Resource": "*" },
        ],
    })
}

/// Put `document` in force on `role` as its permissions boundary: a customer-managed policy
/// carrying it (`iam:CreatePolicy`) attached as the boundary
/// (`iam:PutRolePermissionsBoundary`), which is how a ceiling is set on an entity.
async fn given_boundary(emulator: &Emulator, role: &str, policy_name: &str, document: &Value) {
    let created = emulator
        .query(
            "iam",
            IAM_VERSION,
            "CreatePolicy",
            &[
                ("PolicyName", policy_name.to_string()),
                ("PolicyDocument", document.to_string()),
            ],
        )
        .await;
    let policy_arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreatePolicy answers the policy's Arn, got {created}"));
    emulator
        .query(
            "iam",
            IAM_VERSION,
            "PutRolePermissionsBoundary",
            &[
                ("RoleName", role.to_string()),
                ("PermissionsBoundary", policy_arn),
            ],
        )
        .await;
}

/// Take the boundary off `role` again (`iam:DeleteRolePermissionsBoundary`), leaving its
/// identity-based documents to decide on their own.
async fn without_boundary(emulator: &Emulator, role: &str) {
    emulator
        .query(
            "iam",
            IAM_VERSION,
            "DeleteRolePermissionsBoundary",
            &[("RoleName", role.to_string())],
        )
        .await;
}

/// `request` with the members `extra` carries added to it.
fn with_members(mut request: Value, extra: &Value) -> Value {
    let members = request.as_object_mut().expect("the request is an object");
    for (member, value) in extra.as_object().expect("the extra members are an object") {
        members.insert(member.clone(), value.clone());
    }
    request
}

/// HOW one row's call names the resource it acts on.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// Through the token of an outstanding ACTIVITY task: the resource is that activity.
    ActivityToken,
    /// Through the token of an outstanding CALLBACK (`.waitForTaskToken`) task: the resource
    /// is the state machine whose execution waits at it.
    CallbackToken,
    /// Through an execution ARN the request carries in a member of its own.
    ExecutionArn,
}

/// One call the rule is taken over: the operation, how it names its resource, and the
/// members it carries beside the one that names it.
struct Row {
    case: &'static str,
    operation: &'static str,
    addressing: Addressing,
    extra: Value,
}

/// RULE: an identity-based refusal names the resource the REQUEST acted on — for a
/// `SendTask*` call the resource the task token it carries belongs to — and never the
/// `Resource` of the statement that decided the call, under every one of IAM's rules: a
/// default deny, an explicit deny, a boundary that states nothing and a boundary that denies.
/// The call is refused HTTP 400 `AccessDeniedException` carrying that one sentence, and it
/// settles nothing — which the grant put in force at the end of each row proves by settling
/// the task that is still outstanding.
#[tokio::test]
async fn a_refusal_names_the_resource_the_request_acted_on_whatever_the_documents_name() {
    let rows = [
        Row {
            case: "a success on a callback task",
            operation: "SendTaskSuccess",
            addressing: Addressing::CallbackToken,
            extra: json!({ "output": r#"{"approved":true}"# }),
        },
        Row {
            case: "a failure on an activity task",
            operation: "SendTaskFailure",
            addressing: Addressing::ActivityToken,
            extra: json!({ "error": "WorkerSaidNo", "cause": "the worker refused" }),
        },
        Row {
            case: "a heartbeat on a callback task",
            operation: "SendTaskHeartbeat",
            addressing: Addressing::CallbackToken,
            extra: json!({}),
        },
        Row {
            case: "a success on an activity task",
            operation: "SendTaskSuccess",
            addressing: Addressing::ActivityToken,
            extra: json!({ "output": r#"{"approved":true}"# }),
        },
        Row {
            case: "a read addressed by an execution ARN of its own",
            operation: "DescribeExecution",
            addressing: Addressing::ExecutionArn,
            extra: json!({}),
        },
    ];

    let emulator = Emulator::new();
    let machine_role = emulator.state_machine_role("sfn-denial-machines").await;
    let (activity, activity_machine) = activity_handoff(&emulator, "denial-named").await;
    let callback_parent = callback_machine(&emulator, "denial-named", &machine_role).await;

    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            operation,
            addressing,
            extra,
        } = row;
        let run = format!("denial-run-{index}");
        // The resource the call ACTS ON, and the request that addresses it.
        let (acted_on, request) = match addressing {
            Addressing::ActivityToken => {
                let token =
                    activity_task_token(&emulator, case, &activity, &activity_machine, &run).await;
                (
                    activity.clone(),
                    with_members(json!({ "taskToken": token }), extra),
                )
            }
            Addressing::CallbackToken => {
                let (token, _waiting) =
                    callback_task_token(&emulator, case, &callback_parent, &run).await;
                (
                    callback_parent.clone(),
                    with_members(json!({ "taskToken": token }), extra),
                )
            }
            Addressing::ExecutionArn => {
                let (_token, waiting) =
                    callback_task_token(&emulator, case, &callback_parent, &run).await;
                (waiting.clone(), json!({ "executionArn": waiting }))
            }
        };

        let action = format!("states:{operation}");
        let role = format!("sfn-denial-{index}");
        // Another resource of this service, which a statement below names and a refusal must
        // not: the ARN the request acted on with a suffix no resource of it carries.
        let elsewhere = format!("{acted_on}-elsewhere");
        let principal = emulator
            .assumed_role(&role, &allowing(&["s3:GetObject"], &["*"]))
            .await;

        // (the case's own wording, the identity document in force, the boundary in force,
        // the clause that refusal is stated with). The boundary rows come last, so each
        // row's phases only ever ADD the ceiling.
        let phases: Vec<(&str, Value, Option<Value>, String)> = vec![
            (
                "a document granting nothing of this service on every resource",
                allowing(&["s3:GetObject"], &["*"]),
                None,
                no_grant(&action),
            ),
            (
                "a grant of the action on another resource of this service",
                allowing(&[action.as_str()], &[elsewhere.as_str()]),
                None,
                no_grant(&action),
            ),
            (
                "a wildcard grant overturned by an explicit deny on every resource",
                denied_on_every_resource(&action),
                None,
                EXPLICIT_IDENTITY_DENY.to_string(),
            ),
            (
                "a wildcard grant a permissions boundary states nothing about",
                allowing(&[action.as_str()], &["*"]),
                Some(allowing(&["s3:GetObject"], &["*"])),
                no_boundary_grant(&action),
            ),
            (
                "a wildcard grant a permissions boundary denies on every resource",
                allowing(&[action.as_str()], &["*"]),
                Some(denied_on_every_resource(&action)),
                EXPLICIT_BOUNDARY_DENY.to_string(),
            ),
        ];

        for (phase_index, (phase, document, boundary, reason)) in phases.iter().enumerate() {
            emulator.put_role_policy(&role, document).await;
            if let Some(boundary) = boundary {
                // One managed policy per ceiling, since a boundary is a policy ARN: the row
                // and the phase it is put in force for name it.
                let policy_name = format!("sfn-denial-boundary-{index}-{phase_index}");
                given_boundary(&emulator, &role, &policy_name, boundary).await;
            }
            let decided = format!("{case:?} with {phase:?}");
            let (status, answer) = emulator
                .call_as(&principal.access_key_id, operation, &request)
                .await;
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                "case {decided}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {status} \
                 with {answer:?}"
            );
            let expected = not_authorized(&principal.arn, &action, &acted_on, reason);
            let stated = message(&answer);
            assert_eq!(
                stated, expected,
                "case {decided}: expected {expected:?}, got {stated:?}"
            );
            // The refusal names the resource the request acted on, so it names neither the
            // wildcard nor the other ARN the documents in force carry.
            assert!(
                !stated.contains("on resource: *") && !stated.contains(&elsewhere),
                "case {decided}: expected the refusal to name {acted_on:?} alone, got {stated:?}"
            );
        }

        // The identical call, with the ceiling lifted and the action granted on the very
        // resource the refusals named, is served — which is also what proves every refused
        // call above settled nothing: a redeemed token would answer anything but 200.
        without_boundary(&emulator, &role).await;
        emulator
            .put_role_policy(&role, &allowing(&[action.as_str()], &[acted_on.as_str()]))
            .await;
        let (status, answer) = emulator
            .call_as(&principal.access_key_id, operation, &request)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the call granted on {acted_on} to be served, got {status} \
             with {answer:?}"
        );
    }
}
