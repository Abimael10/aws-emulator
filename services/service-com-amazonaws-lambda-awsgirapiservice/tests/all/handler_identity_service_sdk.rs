// SPDX-License-Identifier: CC0-1.0

//! The IDENTITY SERVICE a managed `python3.x` handler reaches with the SDK its runtime
//! bundles, driven end to end over the wire: Lambda, IAM and the token service on ONE engine,
//! a function whose module-scope code builds `boto3.client("iam")`, and the verdict IAM gave
//! each of its calls.
//!
//! RULE: every service this runtime's SDK seam binds is reachable from a running handler, and
//! the call it makes is signed as the session of the function's EXECUTION ROLE — so the
//! service that serves it judges it against the policies in force on that role and answers
//! its own verdict. For the identity service that makes two things observable from inside a
//! function that were not: the identity the function runs as
//! (`arn:aws:sts::<account>:assumed-role/<role>/<function>`), and IAM's own evaluation of the
//! execution role's documents — an explicit `Deny` about the request refuses it whatever else
//! allows it, an `Allow` with no `Deny` serves it, and a role with neither is refused the
//! implicit deny.
//!
//! The first suite's rows differ along the DOCUMENT in force on the role the function runs as,
//! which is what the rule generalises over: a grant for the very role the call addresses, that
//! same grant beside an explicit `Deny` of the same action, no grant at all, and a grant for
//! another role's ARN. Every row makes the same call, so the only thing that moves between
//! them is the policy.
//!
//! The second suite's rows differ along the OPERATION: reads of a role, of a document embedded
//! in it and of the managed policies attached to it, a listing of the account's roles, a WRITE
//! read back through the listing of the role's tags, and an operation the seam binds no request
//! for — so the surface is not one call's.
//!
//! The suites RUN handlers, so they take this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, element, given_role, iam, logs_only};
use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound, and the prefix every function request below is built on.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute every package below carries its handler as.
const HANDLER: &str = "index.handler";

/// The runtime every function below runs under.
const RUNTIME: &str = "python3.12";

/// The name of the one inline policy each role below carries, which is also the document the
/// `GetRolePolicy` row reads back.
const INLINE_POLICY: &str = "handler-identity";

/// The code and status IAM refuses an unauthorized request with (its `AccessDenied`,
/// HTTP 403), as `crate::state::authorization` words it.
const ACCESS_DENIED: &str = "AccessDenied";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u64 = 403;

/// The action of the call every row of the first suite makes.
const GET_ROLE_ACTION: &str = "iam:GetRole";

/// Lambda, IAM and the token service over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the handler's own SDK call is dispatched back into this same router, so the
/// role it reads is the record IAM holds and the session it is signed as is the one the token
/// service minted.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
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

/// A statement DENYING `actions` of `namespace` on `resource`, which is what IAM's deny
/// precedence is read through.
fn deny(namespace: &str, actions: &[&str], resource: &str) -> Value {
    json!({
        "Effect": "Deny",
        "Action": actions
            .iter()
            .map(|action| format!("{namespace}:{action}"))
            .collect::<Vec<_>>(),
        "Resource": resource,
    })
}

/// Put `statements` on the role `role` as its one inline policy, through IAM's own API.
async fn given_role_policy(router: &MultiServiceRouter, case: &str, role: &str, statements: Value) {
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutRolePolicy on {role:?} to answer 200, got {status}: {written}"
    );
}

/// The handler every function below runs: it imports the SDK at MODULE scope, builds its
/// token-service and identity-service clients there, reports the identity the run is signed
/// as, and makes the ONE identity-service call the event names — reporting what it read, the
/// refusal that service answered, or the call this runtime's SDK did not make.
fn handler_source() -> String {
    r#"
import boto3
import os
from botocore.exceptions import ClientError

STS = boto3.client("sts")
IAM = boto3.client("iam")


def _refused(error):
    return {
        "refused": {
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    }


def _made(event):
    action = event["action"]
    role = event["role"]
    if action == "GetRole":
        return {"role": IAM.get_role(RoleName=role)["Role"]["Arn"]}
    if action == "GetRolePolicy":
        read = IAM.get_role_policy(RoleName=role, PolicyName=event["policy"])
        return {"policy": read["PolicyName"], "document": read["PolicyDocument"]}
    if action == "ListAttachedRolePolicies":
        listed = IAM.list_attached_role_policies(RoleName=role)
        return {"attached": sorted(p["PolicyArn"] for p in listed["AttachedPolicies"])}
    if action == "ListRoles":
        return {"roles": sorted(r["RoleName"] for r in IAM.list_roles()["Roles"])}
    if action == "TagRole":
        IAM.tag_role(RoleName=role, Tags=[{"Key": event["key"], "Value": event["value"]}])
        listed = IAM.list_role_tags(RoleName=role)
        return {"tags": sorted([t["Key"], t["Value"]] for t in listed["Tags"])}
    return {"unbound": IAM.simulate_principal_policy(PolicySourceArn=role)}


def handler(event, context):
    answered = {
        "identity": STS.get_caller_identity()["Arn"],
        "function": os.environ["AWS_LAMBDA_FUNCTION_NAME"],
    }
    try:
        answered.update(_made(event))
    except ClientError as error:
        answered.update(_refused(error))
    except Exception as error:
        answered["not_made"] = str(error)
    return answered
"#
    .to_string()
}

/// Create the function `name`, running the handler above as `role_arn`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, role_arn: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("index.py", &handler_source())])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
}

/// Invoke `name` with `event`, answering what the handler reported — asserting on the way that
/// the run reported no function error, which is what a runtime whose SDK could not be imported
/// reports instead of a result.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event,
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    let payload = answered.json();
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: expected the handler's own result, got a function error with {payload}"
    );
    payload
}

/// The assumed-role session a call made by an invocation of `function` running as `role`
/// speaks for: the session is named for the FUNCTION, and it is the principal IAM's refusal
/// names.
fn session(role: &str, function: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{function}",
        engine::emulator_account_id()
    )
}

/// The ARN a role of `name` carries at the default path, which is the resource a statement
/// about that role names.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// One row of the first suite: the policy in force on the role the function runs as, and the
/// verdict IAM must give the one call it makes.
struct Row {
    case: &'static str,
    /// The role and the function of this row, which is also the session the call is signed as.
    name: &'static str,
    /// The statements of the role's one inline policy, given the ARN of the role itself and
    /// the ARN of a BYSTANDER role the call does not address.
    statements: fn(&str, &str) -> Value,
    /// The clause IAM's refusal closes on, or `None` where the call must be served. The two
    /// clauses are the ones `crate::state::authorization` of the identity service words: the
    /// explicit-deny clause, and the implicit deny naming the action no document allowed.
    refusal: Option<&'static str>,
}

/// The rows: the four positions an execution role's own documents can put one call in.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a grant for the very role the call addresses",
            name: "handler-identity-granted",
            statements: |own, _bystander| {
                json!([allow("iam", &["GetRole"], own), logs_only()])
            },
            refusal: None,
        },
        Row {
            case: "that same grant beside an explicit deny of the same action",
            name: "handler-identity-denied",
            statements: |own, _bystander| {
                json!([
                    allow("iam", &["GetRole"], own),
                    deny("iam", &["GetRole"], own),
                    logs_only(),
                ])
            },
            refusal: Some("with an explicit deny in an identity-based policy"),
        },
        Row {
            case: "no grant for the action at all",
            name: "handler-identity-ungranted",
            statements: |_own, _bystander| json!([logs_only()]),
            refusal: Some("because no identity-based policy allows the iam:GetRole action"),
        },
        Row {
            case: "a grant for another role's ARN",
            name: "handler-identity-elsewhere",
            statements: |_own, bystander| {
                json!([allow("iam", &["GetRole"], bystander), logs_only()])
            },
            refusal: Some("because no identity-based policy allows the iam:GetRole action"),
        },
    ]
}

/// The one call every row of the first suite makes: the function reads the role it runs as.
fn read_own_role(role: &str) -> Value {
    json!({ "action": "GetRole", "role": role })
}

/// A handler's own identity-service call is signed as the session of the function's execution
/// role, so IAM's evaluation of that role's own documents is what decides it — and the
/// identity the run reports is the session named for the function.
#[tokio::test]
async fn the_execution_roles_own_documents_decide_the_identity_calls_a_handler_makes() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    let rows = rows();

    // The role no row's call addresses, which the fourth row's grant names instead of its own.
    let bystander = given_role(&router, "the bystander role", "handler-identity-bystander", None)
        .await;

    for row in &rows {
        let own = given_role(&router, row.case, row.name, None).await;
        assert_eq!(
            own,
            role_arn(row.name),
            "case {:?}: expected IAM to hold the role at the default path",
            row.case
        );
        given_role_policy(&router, row.case, row.name, (row.statements)(&own, &bystander)).await;
        given_function(&router, row.case, row.name, &own).await;
    }
    // Every function first, so the provisioning spans the clock owes run down together rather
    // than one after another.
    for row in &rows {
        await_function_active(&router, row.case, row.name).await;
    }

    for row in &rows {
        let reported = invoked(&router, row.case, row.name, &read_own_role(row.name)).await;
        // The function runs as a session of its execution role, named for the function.
        assert_eq!(
            reported["identity"].as_str(),
            Some(session(row.name, row.name).as_str()),
            "case {:?}: expected the run to be signed as the session named for the function, \
             got {reported}",
            row.case
        );
        match row.refusal {
            None => assert_eq!(
                reported["role"].as_str(),
                Some(role_arn(row.name).as_str()),
                "case {:?}: expected the allowed read to answer the role's own ARN, got \
                 {reported}",
                row.case
            ),
            Some(reason) => {
                let refused = &reported["refused"];
                assert_eq!(
                    (refused["code"].as_str(), refused["status"].as_u64()),
                    (Some(ACCESS_DENIED), Some(ACCESS_DENIED_STATUS)),
                    "case {:?}: expected IAM's own AccessDenied, got {reported}",
                    row.case
                );
                // IAM words the refusal about the three things the decision was made on —
                // the principal, the action and the resource — and closes on why; the
                // authorization id the sentence ends with is minted per request, so the
                // sentence is read up to it.
                let expected = format!(
                    "User: {} is not authorized to perform: {GET_ROLE_ACTION} on resource: role \
                     {} {reason}",
                    session(row.name, row.name),
                    row.name
                );
                let message = refused["message"].as_str().unwrap_or_default();
                assert!(
                    message.starts_with(&expected),
                    "case {:?}: expected the refusal to be worded {expected:?}, got {message:?}",
                    row.case
                );
            }
        }
    }
}

/// The role of the second suite, and the function that runs as it.
const SURFACE: &str = "handler-identity-surface";

/// The managed policy the second suite attaches to that role, which its
/// `ListAttachedRolePolicies` row reads back.
const ATTACHED_POLICY: &str = "handler-identity-attached";

/// One row of the second suite: the call the handler makes, and what the answer must carry.
struct Call {
    case: &'static str,
    /// The event naming the call, given the ARN of the managed policy attached to the role.
    event: fn(&str) -> Value,
    /// What the answer must carry, given that same ARN and the document IAM ITSELF reports
    /// for the role's inline policy — so a row reading a document is compared against the
    /// identity service's own answer rather than against anything written out here.
    expected: fn(&str, &str) -> Value,
}

/// The calls: a role read, a read of the document embedded in it, a read of the managed
/// policies attached to it, a listing of the account's roles, a WRITE read back, and an
/// operation this runtime's SDK binds no request for.
fn calls() -> Vec<Call> {
    vec![
        Call {
            case: "a read of the role itself",
            event: |_attached| json!({ "action": "GetRole", "role": SURFACE }),
            expected: |_attached, _document| json!({ "role": role_arn(SURFACE) }),
        },
        Call {
            case: "a read of the document embedded in the role",
            event: |_attached| {
                json!({ "action": "GetRolePolicy", "role": SURFACE, "policy": INLINE_POLICY })
            },
            expected: |_attached, document| {
                json!({ "policy": INLINE_POLICY, "document": document })
            },
        },
        Call {
            case: "a read of the managed policies attached to the role",
            event: |_attached| {
                json!({ "action": "ListAttachedRolePolicies", "role": SURFACE })
            },
            expected: |attached, _document| json!({ "attached": [attached] }),
        },
        Call {
            case: "a listing of the account's roles",
            event: |_attached| json!({ "action": "ListRoles", "role": SURFACE }),
            expected: |_attached, _document| json!({ "roles": [SURFACE] }),
        },
        Call {
            case: "a write, read back through the role's own tags",
            event: |_attached| {
                json!({
                    "action": "TagRole",
                    "role": SURFACE,
                    "key": "owner",
                    "value": "platform",
                })
            },
            expected: |_attached, _document| json!({ "tags": [["owner", "platform"]] }),
        },
    ]
}

/// The identity surface a handler reaches is the bound surface and not one call's: every
/// operation this seam binds is made and answered by IAM, a WRITE one makes is held where a
/// client reads it, and an operation the seam binds no request for is reported as a call that
/// was never made.
#[tokio::test]
async fn the_bound_identity_surface_is_reached_operation_by_operation() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "the identity surface a handler reaches";
    let router = emulator();

    let own = given_role(&router, case, SURFACE, None).await;
    // A managed policy of the account, attached to that role, so the attachment read has
    // something of the account's own to report.
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [allow("logs", &["PutLogEvents"], "*")],
    })
    .to_string();
    let (status, created) = iam(
        &router,
        "CreatePolicy",
        &[
            ("PolicyName", ATTACHED_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreatePolicy to answer 200, got {status}: {created}"
    );
    let attached = element(&created, "Arn");
    let (status, answered) = iam(
        &router,
        "AttachRolePolicy",
        &[("RoleName", SURFACE), ("PolicyArn", &attached)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AttachRolePolicy to answer 200, got {status}: {answered}"
    );

    given_role_policy(
        &router,
        case,
        SURFACE,
        json!([
            allow(
                "iam",
                &[
                    "GetRole",
                    "GetRolePolicy",
                    "ListAttachedRolePolicies",
                    "TagRole",
                    "ListRoleTags",
                ],
                &own,
            ),
            allow("iam", &["ListRoles"], "*"),
            logs_only(),
        ]),
    )
    .await;
    given_function(&router, case, SURFACE, &own).await;
    await_function_active(&router, case, SURFACE).await;

    // The document IAM itself reports for the role's inline policy, read where a client reads
    // it: what the handler reads back must be the identity service's own answer.
    let (status, read) = iam(
        &router,
        "GetRolePolicy",
        &[("RoleName", SURFACE), ("PolicyName", INLINE_POLICY)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetRolePolicy to answer 200, got {status}: {read}"
    );
    let document = element(&read, "PolicyDocument");

    for probe in calls() {
        let reported = invoked(&router, probe.case, SURFACE, &(probe.event)(&attached)).await;
        let expected = (probe.expected)(&attached, &document);
        let expected = expected
            .as_object()
            .expect("every row's expectation is a document");
        for (member, value) in expected {
            assert_eq!(
                &reported[member.as_str()], value,
                "case {:?}: expected {member} to be {value}, got {reported}",
                probe.case
            );
        }
    }

    // The WRITE the tag row made is held by IAM itself, read where a client reads it: a run
    // that reported a tag and wrote none fails here.
    let (status, listed) = iam(&router, "ListRoleTags", &[("RoleName", SURFACE)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListRoleTags to answer 200, got {status}: {listed}"
    );
    assert_eq!(
        (element(&listed, "Key"), element(&listed, "Value")),
        ("owner".to_string(), "platform".to_string()),
        "case {case:?}: expected IAM to hold the tag the handler wrote, got {listed}"
    );

    // An operation this runtime's SDK binds no request for is reported as a call that was
    // never made, naming it and the operations that ARE bound — never sent with a guessed
    // binding and never answered as a service's own refusal.
    let reported = invoked(
        &router,
        "an operation the seam binds no request for",
        SURFACE,
        &json!({ "action": "SimulatePrincipalPolicy", "role": own }),
    )
    .await;
    let not_made = reported["not_made"].as_str().unwrap_or_default();
    for named in [
        "binds no SimulatePrincipalPolicy call on IAM",
        "GetRole",
        "ListRoleTags",
    ] {
        assert!(
            not_made.contains(named),
            "case \"an operation the seam binds no request for\": expected the report to name \
             {named:?}, got {reported}"
        );
    }
}
