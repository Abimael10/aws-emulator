// SPDX-License-Identifier: CC0-1.0

//! WHOSE identity a Step Functions `Task` state's AWS SDK call is made under, driven
//! across the three services the question spans — Step Functions, IAM and the token
//! service — mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: Step Functions does an execution's work AS the state machine's `roleArn`: it
//! assumes that role and makes each integration call with the temporary credentials the
//! assume vends, so the service serving the call resolves a SESSION of that role as its
//! caller — `arn:<partition>:sts::<account>:assumed-role/<role name>/<session name>` —
//! and never a principal of whoever runs the emulator. That is what every
//! least-privilege assumption about a state machine rests on: the identity doing the
//! work is the role, so anything keyed on the caller sees the role, and two state
//! machines whose roles carry different grants are refused different things.
//!
//! `arn:aws:states:::aws-sdk:sts:getCallerIdentity` is the call that REPORTS that
//! identity, so what the task puts out is read back as the answer the invoked service
//! gave: "returns details about the IAM user or role whose credentials are used to call
//! the operation" (`sts:GetCallerIdentity`).
//!
//! The rows differ along what the rule generalises over: WHICH role the state machine
//! carries (so the session names that role and not another), and the form IAM holds it
//! in — at the account root and under a path, whose session ARN carries the role's NAME
//! alone. A second test pins the invariant that the credentials belong to the RUN: two
//! calls of one execution are made as the same session.
//!
//! Three crates have to agree for that to hold — Step Functions assumes the role and
//! makes the call, IAM holds the role, and the token service resolves the credential's
//! principal — so the suite lives here rather than in any of them. The harness it
//! drives (the IAM caller, the role, the Step Functions caller and the execution
//! reader) is `crate::sfn_task_target_authorization`'s; `tests/all.rs` is one crate
//! root, so this suite mounts the services the question needs and borrows the rest.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::element;
use crate::sfn_task_target_authorization::{
    allow, iam, ran_with_input, role, session_principal, sfn, string, trust_document, ACCOUNT,
};

/// The `Resource` of the AWS SDK integration that reports the identity a call is made
/// under.
const WHO_AM_I: &str = "arn:aws:states:::aws-sdk:sts:getCallerIdentity";

/// Step Functions, IAM and the token service over ONE engine: the state machine's role
/// lives in IAM, and the credential the task's call presents is resolved by the token
/// service, which is also the service the call itself goes to.
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

/// A policy that allows every action on every resource: the rows are about WHO the call
/// is made as, so nothing any of them does is refused for what the role may do.
fn every_action() -> Value {
    json!({ "Version": "2012-10-17", "Statement": [allow("*", "*")] })
}

/// An IAM role under `path`, carrying the trust document a Step Functions execution
/// role needs and a policy that refuses nothing; answers its own ARN.
///
/// `role` (the harness's) creates one at the account root, which is the usual form; a
/// path is what makes the role's ARN carry a segment its SESSION's ARN must not.
async fn role_under_path(router: &MultiServiceRouter, name: &str, path: &str) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("Path", path.to_string()),
            ("AssumeRolePolicyDocument", trust_document().to_string()),
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
            ("PolicyDocument", every_action().to_string()),
        ],
    )
    .await;
    arn
}

/// A definition whose `Task` states each report the identity their own call was made
/// under, one per name in `states`, each put out under its own member.
///
/// Every state sends an EMPTY request: `sts:GetCallerIdentity` models no input member,
/// and the document the execution carries is the identities already reported, which is
/// not a request.
fn definition(states: &[&str]) -> String {
    let mut declared = serde_json::Map::new();
    for (index, name) in states.iter().enumerate() {
        let mut state = json!({
            "Type": "Task",
            "Resource": WHO_AM_I,
            "Parameters": {},
            "ResultSelector": { "Account.$": "$.Account", "Arn.$": "$.Arn" },
            "ResultPath": format!("$.{name}"),
        });
        let members = state.as_object_mut().expect("the state is an object");
        match states.get(index + 1) {
            Some(next) => members.insert("Next".to_string(), json!(next)),
            None => members.insert("End".to_string(), json!(true)),
        };
        declared.insert((*name).to_string(), state);
    }
    json!({
        "StartAt": states.first().copied().unwrap_or_default(),
        "States": Value::Object(declared),
    })
    .to_string()
}

/// Create a state machine running `definition` under `role_arn`; answers its ARN.
async fn machine(
    router: &MultiServiceRouter,
    name: &str,
    written: &str,
    role_arn: &str,
) -> String {
    let created = sfn(
        router,
        "CreateStateMachine",
        &json!({ "name": name, "definition": written, "roleArn": role_arn }),
    )
    .await;
    string(&created, "stateMachineArn")
}

/// What one execution put out, which must be the document its `Task` states built.
fn output(described: &Value, case: &str) -> Value {
    let text = described
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described:?}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case:?}: expected JSON output, got {error}"))
}

/// The identity one `Task` state reported under `member`.
fn identity(produced: &Value, member: &str, case: &str) -> (String, String) {
    let reported = produced
        .get(member)
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} in {produced:?}"));
    (
        string(reported, "Account"),
        string(reported, "Arn"),
    )
}

/// The `Arn` the token service reports for the credential THIS suite's own requests are
/// signed with, read exactly as a client of that service reads it: the identity a task's
/// call must not be made under.
async fn own_identity(router: &MultiServiceRouter) -> String {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "sts.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-west-2/sts/aws4_request, \
             SignedHeaders=host, Signature=0000",
        )
        .body(::bytes::Bytes::from(
            "Action=GetCallerIdentity&Version=2011-06-15",
        ))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: GetCallerIdentity must answer this suite's own caller, got {status} with {body}"
    );
    element(&body, "Arn")
}

/// One row: the role the state machine carries, and the path IAM holds it under.
struct IdentityCase {
    case: &'static str,
    role_name: &'static str,
    /// `None` for a role at the account root, which is the usual form.
    path: Option<&'static str>,
}

/// A `Task` state's AWS SDK call is made as a SESSION of the state machine's execution
/// role: the identity the invoked service reports names that role, in that account, and
/// is not the identity the caller that started the execution signs as.
#[tokio::test]
async fn an_sdk_task_is_called_as_a_session_of_the_execution_role() {
    let rows: &[IdentityCase] = &[
        IdentityCase {
            case: "a role at the account root",
            role_name: "SfnIdentityRoleOne",
            path: None,
        },
        IdentityCase {
            case: "another role, whose own name its session carries",
            role_name: "SfnIdentityRoleTwo",
            path: None,
        },
        IdentityCase {
            case: "a role IAM holds under a path",
            role_name: "SfnIdentityRolePathed",
            path: Some("/service-role/"),
        },
    ];

    let router = emulator();
    // What the token service reports for the caller THIS suite signs as: the identity no
    // task's call may be made under, whatever it is.
    let own = own_identity(&router).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let role_arn = match row.path {
            Some(path) => role_under_path(&router, row.role_name, path).await,
            None => role(&router, row.role_name, &every_action()).await,
        };
        let machine_arn = machine(
            &router,
            &format!("sdk-identity-{index}"),
            &definition(&["Who"]),
            &role_arn,
        )
        .await;
        let (_, described) = ran_with_input(&router, &machine_arn, Some("{}"), case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the call to be made and answered, got {described:?}"
        );
        let (account, arn) = identity(&output(&described, case), "Who", case);
        // The identity is a session of THIS role: the whole principal, read back as the
        // spelling `sts:AssumeRole` reports one, with the session's own name held to
        // what a `RoleSessionName` may carry.
        assert_eq!(
            arn,
            session_principal(&role_arn, &arn),
            "case {case:?}: expected a session of {role_arn}, got {arn:?}"
        );
        assert_eq!(
            account, ACCOUNT,
            "case {case:?}: the session belongs to the role's own account, got {account:?}"
        );
        // And it is NOT the principal the caller that started the execution signs as:
        // the work is the ROLE's, not the emulator's.
        assert_ne!(
            arn, own,
            "case {case:?}: expected the call to be made as the role rather than as the \
             caller's own identity ({own})"
        );
    }
}

/// The credentials belong to the RUN: every call of one execution is made as the same
/// session of the role, so two `Task` states of one definition report one identity.
#[tokio::test]
async fn two_calls_of_one_execution_are_made_as_the_same_session() {
    let case = "two calls of one execution";
    let router = emulator();
    let role_arn = role(&router, "SfnIdentityHeldRole", &every_action()).await;
    let machine_arn = machine(
        &router,
        "sdk-identity-held",
        &definition(&["First", "Second"]),
        &role_arn,
    )
    .await;

    let (_, described) = ran_with_input(&router, &machine_arn, Some("{}"), case).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected both calls to be made and answered, got {described:?}"
    );
    let produced = output(&described, case);
    let first = identity(&produced, "First", case);
    let second = identity(&produced, "Second", case);
    assert_eq!(
        first, second,
        "case {case:?}: expected one session for the whole run, got {first:?} then {second:?}"
    );
    assert_eq!(
        first.1,
        session_principal(&role_arn, &first.1),
        "case {case:?}: expected a session of {role_arn}, got {:?}",
        first.1
    );
}
