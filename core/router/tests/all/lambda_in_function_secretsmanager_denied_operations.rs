// SPDX-License-Identifier: CC0-1.0

//! What a running function's Secrets Manager call reports when the policies in force on its
//! execution role allow NOTHING of that service — whichever of the service's operations the
//! handler called.
//!
//! RULE: the AWS SDK a managed runtime supplies builds a client for every service the runtime
//! can call, so the client itself is CONSTRUCTIBLE and the call is made as a session of the
//! function's execution role; the service serving it then decides it against the documents in
//! force on that role and, where none allows it, answers the AWS-common
//! `AccessDeniedException` — HTTP 400 — naming the assumed-role session, the IAM action the
//! CALLED OPERATION performs (`<service action prefix>:<OperationName>`), the resource the
//! request addressed as the request itself spelled it, and the rule that withheld the grant.
//! An action that addresses no resource of its own is named with no resource clause at all.
//! Because that decision is taken ahead of the service's own reading of the store, a call
//! about a resource the account does not hold is refused as unauthorized rather than answered
//! as absent — and once the grant is in force the same call is served, or reports the absence,
//! instead.
//!
//! Why it matters: a handler (or the check that deploys one) distinguishes "my role is missing
//! a grant" from "the SDK could not even build a client for this service" by the error it is
//! given. A client that cannot be constructed raises botocore's own `UnknownServiceError`
//! before any request is sent, so no authorization decision happens at all and a stack deployed
//! with an insufficient role looks denied for the wrong reason. Every row below therefore
//! asserts the exception CLASS the SDK raised as well as the code, the status and the sentence.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`); the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor
//! a resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*); and
//! the action an operation of this service is performed under is
//! `secretsmanager:<OperationName>` ("Actions defined by AWS Secrets Manager", IAM Service
//! Authorization Reference).
//!
//! Five crates have to agree for that to hold — IAM holds the role and the inline document the
//! decision reads, STS mints the session the handler's calls are signed with, CloudFormation
//! archives the inline handler into a deployment package this emulator can run, Lambda runs it
//! and supplies the SDK, and Secrets Manager takes the decision — so the suite lives here. The
//! Secrets Manager crate's own suites pin the decision for a credential a client signs with
//! directly; `lambda_in_function_secretsmanager_grant_revocation` pins one operation's read
//! being served, withdrawn and served again as the role's document is rewritten. What only this
//! suite reaches is the refusal generalised over the OPERATION called: that the action a
//! refusal blames is the called operation's own, and that an operation addressing no secret
//! carries no resource clause.
//!
//! The rows differ along what the rule generalises over: WHICH operation of the service the
//! handler calls (a read of a version, a read of the metadata, a list of one secret's versions,
//! a WRITE of a new version, two account-wide actions that address no secret), HOW the request
//! spells the secret it addresses (the ARN the service minted, the name it was created under, a
//! name the account holds no secret under, or nothing at all), and therefore whether the
//! sentence carries a resource clause. Every row is probed twice over — with the role granted
//! nothing of the service, and with it granted the service's actions — so no row's verdict can
//! come from the call failing for some reason other than authorization.
//!
//! Every expected value is computed here from what the row declares and from the account every
//! ARN this emulator mints carries; nothing is read back out of the answer being checked.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the secret and the function
/// belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every session ARN and every secret ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that declares the one function every row runs in.
const STACK: &str = "xsvc-in-fn-secret-denials";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The role the function runs as, the function itself, and the one inline policy the role
/// carries — rewritten between the two phases. The refusal's own sentence names the first two:
/// the identity a function's call is signed as is
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
const ROLE_NAME: &str = "xsvc-in-fn-secret-denial-role";
const FUNCTION_NAME: &str = "xsvc-in-fn-secret-denial-probe";
const POLICY_NAME: &str = "inline";

/// The secret the rows address, the value it is created with, and the token that create names
/// its first version by. "You must provide a ClientRequestToken value"
/// (`CreateSecretRequest$ClientRequestToken`) for a call no SDK client filled one in for, and
/// this suite's own setup calls are made over the wire rather than through one.
const SECRET_NAME: &str = "xsvc-in-fn-secret-denial-target";
const SECRET_VALUE: &str = "the-stored-secret-string";
const CREATE_TOKEN: &str = "5f2a1c4e-0b7d-4a19-9c3e-8d6b2f70a145";

/// The value the writing row stores, which differs from the created one so a served write is
/// not indistinguishable from no write at all.
const WRITTEN_VALUE: &str = "the-rewritten-secret-string";

/// A secret name this account holds no secret under, which the row about an absent resource
/// addresses.
const ABSENT_SECRET: &str = "xsvc-in-fn-secret-denial-nothing-holds-this";

/// This service's IAM action prefix: an action is `secretsmanager:<OperationName>` ("Actions
/// defined by AWS Secrets Manager", IAM Service Authorization Reference). Written out here
/// rather than taken from the service crate, so the sentence this suite asserts has something
/// to be wrong about.
const ACTION_PREFIX: &str = "secretsmanager";

/// The code and status an unauthorized call is refused with: the AWS-common
/// `AccessDeniedException`, carried at this service's client-error status because `awsJson1_1`
/// discriminates a failure by the error type in the body rather than by the transport status.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 400;

/// The code, status and sentence this service answers for a secret the account does not hold,
/// which is what the absent row's call reports once its role IS granted the action
/// (`smithy_query(query="operation",operation="GetSecretValue")`: `ResourceNotFoundException`,
/// HTTP 400). The sentence carries no part of the request.
const NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u64 = 400;
const NOT_FOUND_MESSAGE: &str = "Secrets Manager can't find the specified secret.";

/// The exception class an AWS SDK for Python raises for a refusal the called service's model
/// declares no error shape for — which the AWS-common refusals are: the base
/// `botocore.exceptions.ClientError`. A refusal the called operation DOES declare a shape for
/// is raised as the class named after that shape, which is why the absent row expects its own.
const CLIENT_ERROR: &str = "ClientError";

/// The class botocore raises when it cannot build a client for the token at all, which is the
/// failure this suite exists to keep distinguishable from a refusal.
const UNKNOWN_SERVICE_ERROR: &str = "UnknownServiceError";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — the created function reaching
/// `Active` — and how many looks to take. The span is that service's own, so no fixed sleep
/// names the instant it ends, and the deadline is generous so a loaded host cannot decide the
/// outcome.
const LOOK: std::time::Duration = std::time::Duration::from_millis(100);
const LOOKS: usize = 1_200;

/// The handler every row runs: for each probe the event names it builds a Secrets Manager
/// client with the SDK the runtime supplies and calls the method the probe names with the
/// members it carries, reporting either that the call was served or the exception it raised —
/// the exception's own class, and for a refusal the code, the sentence and the HTTP status the
/// service answered.
///
/// The client is built INSIDE the guard, so a runtime whose SDK cannot build one for this
/// service is reported as its own class rather than taking the whole invocation down.
const PROBE_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    probed = {}
    for probe in event["probes"]:
        try:
            client = boto3.client("secretsmanager")
            getattr(client, probe["method"])(**probe["kwargs"])
        except ClientError as refused:
            probed[probe["case"]] = {
                "ok": False,
                "raised": type(refused).__name__,
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            }
        except Exception as error:
            probed[probe["case"]] = {
                "ok": False,
                "raised": type(error).__name__,
                "sentence": str(error),
            }
        else:
            probed[probe["case"]] = {"ok": True, "raised": None}
    return {"probed": probed}
"##;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched
/// back into this same router, so the secret it addresses is the secret this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls
/// are the account's own and never the role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One Secrets Manager operation over its `awsJson1_1` wire, made as the account's own
/// credential.
async fn secretsmanager(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("authorization", authorization("secretsmanager"))
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The secret every row addresses: the ARN the service minted for it, and the name it was
/// created under — both of which a `SecretId` accepts ("the ARN or name of the secret").
struct Secret {
    arn: String,
    name: String,
}

/// What the SAME call answers once the role IS granted this service's actions, which is the
/// control that shows each refusal above was an authorization decision.
#[derive(Debug, Clone, Copy)]
enum Granted {
    /// The call is served.
    Served,
    /// The call reaches the store and reports the secret it names as absent.
    NotFound,
}

/// One row: the operation the handler calls, how it addresses the secret, and what the same
/// call answers once the grant is in force.
struct Probe {
    case: &'static str,
    /// The method the handler calls on the client, which names the operation it performs.
    method: &'static str,
    /// The operation that method names — the `<OperationName>` of the action a refusal blames.
    operation: &'static str,
    /// The members the handler passes, built from the secret this suite created.
    members: fn(&Secret) -> Value,
    /// What the refusal must name as the resource — the identifier the request supplied,
    /// verbatim — or `None` for an account-wide action, which addresses no secret.
    resource: fn(&Secret) -> Option<String>,
    granted: Granted,
}

/// The members and the resource clause of a row addressing the secret by its ARN.
fn by_arn(secret: &Secret) -> Value {
    json!({ "SecretId": secret.arn })
}
fn arn_resource(secret: &Secret) -> Option<String> {
    Some(secret.arn.clone())
}

/// The same, addressing it by the name it was created under.
fn by_name(secret: &Secret) -> Value {
    json!({ "SecretId": secret.name })
}
fn name_resource(secret: &Secret) -> Option<String> {
    Some(secret.name.clone())
}

/// A write of a new version, which addresses the secret by name and carries the value to store.
fn write_by_name(secret: &Secret) -> Value {
    json!({ "SecretId": secret.name, "SecretString": WRITTEN_VALUE })
}

/// A read addressing a name this account holds no secret under.
fn by_absent_name(_: &Secret) -> Value {
    json!({ "SecretId": ABSENT_SECRET })
}
fn absent_resource(_: &Secret) -> Option<String> {
    Some(ABSENT_SECRET.to_string())
}

/// No members at all: an operation that addresses no secret.
fn no_members(_: &Secret) -> Value {
    json!({})
}

/// No resource: an account-wide action names none.
fn no_resource(_: &Secret) -> Option<String> {
    None
}

const PROBES: &[Probe] = &[
    Probe {
        case: "a read of the current version, addressed by the secret's own ARN",
        method: "get_secret_value",
        operation: "GetSecretValue",
        members: by_arn,
        resource: arn_resource,
        granted: Granted::Served,
    },
    Probe {
        case: "a read of the metadata, addressed by the secret's name",
        method: "describe_secret",
        operation: "DescribeSecret",
        members: by_name,
        resource: name_resource,
        granted: Granted::Served,
    },
    Probe {
        case: "a list of one secret's versions, addressed by its ARN",
        method: "list_secret_version_ids",
        operation: "ListSecretVersionIds",
        members: by_arn,
        resource: arn_resource,
        granted: Granted::Served,
    },
    Probe {
        case: "a write of a new version, addressed by the secret's name",
        method: "put_secret_value",
        operation: "PutSecretValue",
        members: write_by_name,
        resource: name_resource,
        granted: Granted::Served,
    },
    Probe {
        case: "a read addressed by a name this account holds no secret under",
        method: "get_secret_value",
        operation: "GetSecretValue",
        members: by_absent_name,
        resource: absent_resource,
        granted: Granted::NotFound,
    },
    Probe {
        case: "an account-wide list, which addresses no secret",
        method: "list_secrets",
        operation: "ListSecrets",
        members: no_members,
        resource: no_resource,
        granted: Granted::Served,
    },
    Probe {
        case: "an account-wide generation, which addresses no secret",
        method: "get_random_password",
        operation: "GetRandomPassword",
        members: no_members,
        resource: no_resource,
        granted: Granted::Served,
    },
];

/// The sentence Secrets Manager refuses a call nothing in force allows with. Written out here
/// rather than built from the service's own helper, so the wording has something to be wrong
/// about: the principal is the session the function's call is signed as, the action is the one
/// the CALLED OPERATION performs, and the resource clause is the identifier the request
/// supplied — absent for an action that addresses no secret.
fn refusal_message(probe: &Probe, secret: &Secret) -> String {
    let action = format!("{ACTION_PREFIX}:{}", probe.operation);
    let clause = match (probe.resource)(secret) {
        Some(named) => format!(" on resource: {named}"),
        None => String::new(),
    };
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE_NAME}/{FUNCTION_NAME} is not \
         authorized to perform: {action}{clause} because no identity-based policy allows the \
         {action} action"
    )
}

/// The trust policy a Lambda execution role needs, naming the principal Lambda assumes a
/// function's role as.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {
                "Service": service_com_amazonaws_lambda_awsgirapiservice::state::EXECUTION_ROLE_PRINCIPAL,
            },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The statement every execution role carries whatever else it is granted: the log actions a
/// function writes its own log stream with, which grant nothing of Secrets Manager.
fn log_statement() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// The document in force for the FIRST phase: the least-privilege baseline every execution role
/// needs, and nothing of Secrets Manager at all.
fn logs_only_document() -> String {
    json!({ "Version": "2012-10-17", "Statement": [log_statement()] }).to_string()
}

/// The document in force for the SECOND phase: the same baseline, plus every action of this
/// service on every resource.
fn granting_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            log_statement(),
            {
                "Effect": "Allow",
                "Action": format!("{ACTION_PREFIX}:*"),
                "Resource": "*",
            },
        ],
    })
    .to_string()
}

/// Put `document` in force as the one inline policy of the function's role.
async fn put_role_policy(router: &MultiServiceRouter, phase: &str, document: &str) {
    let (status, answered) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "phase {phase:?}: PutRolePolicy({ROLE_NAME:?}) must succeed, got {status} with {answered}"
    );
}

/// Create the role the function runs as, and answer the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({ROLE_NAME:?}) must succeed, got {status} with {created}"
    );
    opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({ROLE_NAME:?}) must answer the role's ARN, got {created}")
    })
}

/// Create the secret every row addresses and answer the ARN Secrets Manager minted for it.
async fn create_secret(router: &MultiServiceRouter) -> Secret {
    let (status, created) = secretsmanager(
        router,
        "CreateSecret",
        &json!({
            "Name": SECRET_NAME,
            "SecretString": SECRET_VALUE,
            "ClientRequestToken": CREATE_TOKEN,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSecret({SECRET_NAME:?}) must succeed, got {status} with {created}"
    );
    let arn = created["ARN"].as_str().unwrap_or_else(|| {
        panic!("setup: CreateSecret({SECRET_NAME:?}) must answer the secret's ARN, got {created}")
    });
    Secret {
        arn: arn.to_string(),
        name: SECRET_NAME.to_string(),
    }
}

/// Deploy the one-function stack and wait for it to settle. The inline source is archived into
/// the deployment package by CloudFormation, which is how a function whose handler is written
/// here comes to exist at all.
async fn deploy(router: &MultiServiceRouter, role_arn: &str) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION_NAME,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": PROBE_HANDLER },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports the function created,
    // which no fixed sleep can name: this looks until it reports it, with a deadline generous
    // enough that a loaded host does not decide the outcome.
    let (reached, body) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reached == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {reached:?} with resource failures \
         {failures:?} in {body}"
    );
}

/// Wait until Lambda reports the function `Active` — "the function is ready to be invoked"
/// (`FunctionConfiguration$State`) — which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION_NAME}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION_NAME:?} Active, last reported {last}");
}

/// Invoke the function once with every row's probe, and answer the map its handler reported,
/// keyed by the row's case.
async fn probe_all(router: &MultiServiceRouter, phase: &str, secret: &Secret) -> Value {
    let probes: Vec<Value> = PROBES
        .iter()
        .map(|probe| {
            json!({
                "case": probe.case,
                "method": probe.method,
                "kwargs": (probe.members)(secret),
            })
        })
        .collect();
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION_NAME}/invocations"),
        json!({ "probes": probes }).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "phase {phase:?}: the invocation must be served, got {status} with {payload}"
    );
    let faulted = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        faulted, None,
        "phase {phase:?}: the handler must run to completion, got {faulted:?} with {payload}"
    );
    payload["probed"].clone()
}

/// Every row: with the role granted nothing of this service, the handler's call is refused the
/// AWS-common `AccessDeniedException` naming the called operation's own action — raised as the
/// SDK's base client error rather than as a failure to build a client at all — and with the
/// service's actions granted, the identical call is served or reports the absence it addressed.
#[tokio::test]
async fn an_in_function_secret_call_is_refused_under_the_action_its_operation_performs() {
    let router = build_router();

    let secret = create_secret(&router).await;
    let role_arn = create_role(&router).await;
    put_role_policy(&router, "granted nothing of the service", &logs_only_document()).await;
    deploy(&router, &role_arn).await;
    wait_until_active(&router).await;

    // Phase 1: the role allows nothing of this service, so every row's call is refused — and
    // the refusal is an authorization decision rather than a client that could not be built.
    let phase = "granted nothing of the service";
    let reported = probe_all(&router, phase, &secret).await;
    for probe in PROBES {
        let case = probe.case;
        let answered = reported[case].clone();
        assert!(
            answered.is_object(),
            "case {case:?}: expected the handler to report this probe, got {answered:?} in \
             {reported}"
        );
        assert_ne!(
            answered["raised"].as_str(),
            Some(UNKNOWN_SERVICE_ERROR),
            "case {case:?}: expected the runtime's SDK to build a client for this service, got \
             {answered}"
        );
        assert_eq!(
            answered,
            json!({
                "ok": false,
                "raised": CLIENT_ERROR,
                "code": ACCESS_DENIED,
                "message": refusal_message(probe, &secret),
                "status": ACCESS_DENIED_STATUS,
            }),
            "case {case:?}: expected the refusal IAM words for {}, got {answered}",
            probe.operation
        );
    }

    // Phase 2: the same role, the same function, the same held session — the only thing that
    // changes is the document IAM holds, and now every call gets past the decision.
    let phase = "granted the service's actions";
    put_role_policy(&router, phase, &granting_document()).await;
    let reported = probe_all(&router, phase, &secret).await;
    for probe in PROBES {
        let case = probe.case;
        let answered = reported[case].clone();
        let expected = match probe.granted {
            Granted::Served => json!({ "ok": true, "raised": Value::Null }),
            Granted::NotFound => json!({
                "ok": false,
                "raised": NOT_FOUND,
                "code": NOT_FOUND,
                "message": NOT_FOUND_MESSAGE,
                "status": NOT_FOUND_STATUS,
            }),
        };
        assert_eq!(
            answered, expected,
            "case {case:?}: granted, expected {expected} for {}, got {answered}",
            probe.operation
        );
    }

    // The write the granted phase performed is what a later reader sees, so a served call did
    // the work rather than merely getting past the decision.
    let (status, read) = secretsmanager(
        &router,
        "GetSecretValue",
        &json!({ "SecretId": secret.arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the stored value must be readable back, got {status} with {read}"
    );
    assert_eq!(
        read["SecretString"].as_str(),
        Some(WRITTEN_VALUE),
        "expected the version the granted write stored, got {read}"
    );
}
