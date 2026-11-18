// SPDX-License-Identifier: CC0-1.0

//! A running function's own Secrets Manager read, made with the SDK ITS OWN managed runtime
//! bundles, decided against the policies in force on the execution role at the instant of the
//! call.
//!
//! RULE: a call an AWS SDK makes from inside a running function is signed as a session of that
//! function's execution role, and the service serving it decides each call afresh against the
//! identity-based documents then in force on that role — whichever managed runtime's bundled
//! SDK makes the call and whichever call form the handler writes it in. So a `GetSecretValue`
//! those documents allow answers that version's own `ARN`, `Name`, `VersionId`, `VersionStages`
//! and the stored `SecretString`; the identical call made once the grant is withdrawn is
//! refused the AWS-common `AccessDeniedException` at this service's client-error status, naming
//! the assumed-role session, the IAM action the read performs and the identifier the request
//! supplied; and the identical call made once the grant is put back is served again. Nothing
//! about the function, its environment or its held session changes between the three calls.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor
//! a resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*), and
//! `secretsmanager:GetSecretValue` is the permission that read requires ("Required permissions:
//! secretsmanager:GetSecretValue", `GetSecretValue`, that service's own model). The runtime a
//! function declares decides only WHICH SDK is on the handler's import path — Lambda's managed
//! Python runtimes supply boto3 and its `nodejs18.x` and later runtimes the AWS SDK for
//! JavaScript v3 — never who the call speaks for or who decides it.
//!
//! Five crates have to agree for that to hold — IAM holds the role and the inline document a
//! `PutRolePolicy` rewrites between calls, STS mints the session the function's calls are
//! signed with, CloudFormation archives the inline handler into a deployment package this
//! emulator can run, Lambda runs that handler and supplies it the SDK of its declared runtime,
//! and Secrets Manager holds the secret and takes the decision — so the suite lives here: a
//! crate-local suite can only ever see one side of it.
//!
//! What only this suite reaches: `lambda_in_function_secretsmanager_grant_revocation` pins the
//! same three phases for ONE runtime, generalising over how the grant is written, and
//! `lambda_in_function_secretsmanager_denied_operations` generalises over the operation called;
//! both hold the runtime, the SDK and the call form fixed, and both hand the handler the secret
//! to read in the INVOCATION EVENT. The rows here differ along exactly those: WHICH runtime the
//! function declares (two Python lines, two Node lines), WHICH SDK and call form the handler
//! reads the secret with (a boto3 client, a client built from a boto3 `Session`, a v3 bare
//! client sending a command object, the aggregated v3 client's own operation method), and the
//! FORM of the value the granted read must answer with — and every row takes the secret it
//! reads from its own `SECRET_ARN` environment variable rather than from the event, so the read
//! is the one a deployed function makes of the secret it was configured with.
//!
//! Every expected value is computed here from what the row declares, from the ARN Secrets
//! Manager minted and from the account every ARN this emulator mints carries; nothing is read
//! back out of the answer being checked. The status each handler reports is reported as TEXT
//! because the two SDK families hand a handler the HTTP status in their own number type, and
//! what this suite is about is the status itself and not which type carried it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the secrets and the functions
/// belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every session's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that declares one function per row.
const STACK: &str = "xsvc-in-fn-secret-sdks";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The IAM action the handler's read performs, which the grant names and a refusal blames
/// ("Actions defined by AWS Secrets Manager", IAM Service Authorization Reference).
const READ_ACTION: &str = "secretsmanager:GetSecretValue";

/// The code Secrets Manager refuses an unauthorized call with, and the status it comes back on:
/// the AWS-common `AccessDeniedException`, at this service's client-error status because
/// awsJson1_1 discriminates a failure by the error type in the body rather than by the
/// transport status.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS_TEXT: &str = "400";

/// The staging label the version a read resolves by default carries: "If you don't specify
/// either a VersionStage or VersionId, Secrets Manager returns the AWSCURRENT version."
/// (`GetSecretValue`, that service's Smithy model.)
const STAGE_CURRENT: &str = "AWSCURRENT";

/// The name of the one inline policy each row's role carries, which every rewrite of that row's
/// grant replaces.
const POLICY_NAME: &str = "inline";

/// The environment variable each function is configured with the secret it reads in, which is
/// how a deployed function is told which secret is its own.
const SECRET_VARIABLE: &str = "SECRET_ARN";

/// How long one invocation is given, as the function's own `Timeout`: well past what reading one
/// secret takes, so nothing here is decided by how fast the host runs.
const FUNCTION_TIMEOUT: u64 = 30;

/// How often to look while waiting for work the clock owes — a created function reaching
/// `Active` — and how many looks to take. The span is that service's own, so no fixed sleep
/// names the instant it ends, and the deadline is generous so a loaded host cannot decide the
/// outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_200;

/// The Python handler: the SDK a managed Python runtime bundles is boto3, and the plain way to
/// build a client of it is `boto3.client`.
const PYTHON_MODULE_CLIENT: &str = r##"
import os

import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    client = boto3.client("secretsmanager")
    try:
        answered = client.get_secret_value(SecretId=os.environ["SECRET_ARN"])
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": str(refused.response["ResponseMetadata"]["HTTPStatusCode"]),
        }
    return {
        "ok": True,
        "arn": answered["ARN"],
        "name": answered["Name"],
        "secret": answered["SecretString"],
        "version_id": answered["VersionId"],
        "stages": answered["VersionStages"],
    }
"##;

/// The same read, through a client built from a boto3 `Session` — the other form boto3 publishes
/// for building one, which a handler that wants a session of its own uses.
const PYTHON_SESSION_CLIENT: &str = r##"
import os

import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    client = boto3.Session().client("secretsmanager")
    try:
        answered = client.get_secret_value(SecretId=os.environ["SECRET_ARN"])
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": str(refused.response["ResponseMetadata"]["HTTPStatusCode"]),
        }
    return {
        "ok": True,
        "arn": answered["ARN"],
        "name": answered["Name"],
        "secret": answered["SecretString"],
        "version_id": answered["VersionId"],
        "stages": answered["VersionStages"],
    }
"##;

/// The Node handler: the SDK a managed `nodejs18.x`-and-later runtime bundles is the AWS SDK for
/// JavaScript v3, whose bare client sends one COMMAND object per call and throws a refusal as an
/// error carrying the service's own error name and the answer's `$metadata`.
const NODE_COMMAND_CLIENT: &str = r#"
const {
    SecretsManagerClient,
    GetSecretValueCommand,
} = require('@aws-sdk/client-secrets-manager');

exports.handler = async (event) => {
    const client = new SecretsManagerClient({});
    try {
        const answered = await client.send(new GetSecretValueCommand({
            SecretId: process.env.SECRET_ARN,
        }));
        return {
            ok: true,
            arn: answered.ARN,
            name: answered.Name,
            secret: answered.SecretString,
            version_id: answered.VersionId,
            stages: answered.VersionStages,
        };
    } catch (refused) {
        return {
            ok: false,
            code: refused.name,
            message: refused.message,
            status: String(refused.$metadata.httpStatusCode),
        };
    }
};
"#;

/// The same read through the AGGREGATED client the same v3 package publishes beside the bare one,
/// which carries one method per operation instead of taking a command object.
const NODE_AGGREGATED_CLIENT: &str = r#"
const { SecretsManager } = require('@aws-sdk/client-secrets-manager');

exports.handler = async (event) => {
    const client = new SecretsManager({});
    try {
        const answered = await client.getSecretValue({
            SecretId: process.env.SECRET_ARN,
        });
        return {
            ok: true,
            arn: answered.ARN,
            name: answered.Name,
            secret: answered.SecretString,
            version_id: answered.VersionId,
            stages: answered.VersionStages,
        };
    } catch (refused) {
        return {
            ok: false,
            code: refused.name,
            message: refused.message,
            status: String(refused.$metadata.httpStatusCode),
        };
    }
};
"#;

/// One row: the runtime the function declares, the handler it runs — written against the SDK that
/// runtime bundles — and the value its own secret holds.
struct Row {
    case: &'static str,
    /// A `Runtime` value this emulator runs a package for (source:
    /// `smithy_query(query="shape", shape="Runtime")`).
    runtime: &'static str,
    source: &'static str,
    /// The `SecretString` the row's secret is created with, which a served read must answer
    /// verbatim. `SecretStringType`'s only constraint is a length of 1..65536
    /// (`smithy_query(query="shape", shape="SecretStringType")`), so its text is free.
    value: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "python3.12, a boto3 client, over a plain string",
        runtime: "python3.12",
        source: PYTHON_MODULE_CLIENT,
        value: "a-plain-secret-string",
    },
    Row {
        case: "python3.9, a client built from a boto3 Session, over a JSON document",
        runtime: "python3.9",
        source: PYTHON_SESSION_CLIENT,
        value: r#"{"endpoint":"db.internal","port":5432,"username":"app"}"#,
    },
    Row {
        case: "nodejs20.x, the v3 bare client sending a command, over text with newlines and \
               non-ASCII",
        runtime: "nodejs20.x",
        source: NODE_COMMAND_CLIENT,
        value: "line one\nline two with £ and a space\n",
    },
    Row {
        case: "nodejs22.x, the aggregated v3 client's own method, over text carrying base64 \
               punctuation",
        runtime: "nodejs22.x",
        source: NODE_AGGREGATED_CLIENT,
        value: "YSBzZWNyZXQ/IHdpdGggPSBhbmQgKyBpbiBpdA==",
    },
];

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the secret it reads is the secret this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls are
/// the account's own and not any row's role's.
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

/// The role one row runs as, the function it runs in, and the secret it reads — named here
/// because the refusal's own sentence names the first two: the identity a function's call is
/// signed as is the session `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-in-fn-sdk-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-sdk-probe-{index}")
}
fn secret_name(index: usize) -> String {
    format!("xsvc-in-fn-sdk-secret-{index}")
}

/// How many characters a token below carries: inside the published 32..64 bound of
/// `ClientRequestTokenType` (`smithy_query(query="shape", shape="ClientRequestTokenType")`).
const VERSION_TOKEN_LEN: usize = 40;

/// The `ClientRequestToken` one secret is created with, which becomes that version's own id —
/// "this parameter specifies a unique identifier for the new version"
/// (`CreateSecretRequest$ClientRequestToken`), so a served read's `VersionId` is this value and
/// not one the suite has to be told.
fn version_token(name: &str) -> String {
    // Every name here is ASCII and shorter than the token, so the token is that name followed by
    // filler out to the fixed length.
    let mut token = format!("{name}-version-");
    token.truncate(VERSION_TOKEN_LEN);
    while token.len() < VERSION_TOKEN_LEN {
        token.push('0');
    }
    token
}

/// The sentence Secrets Manager refuses a read nothing in force allows with. Written out here
/// rather than built from the service's own helper, so the wording has something to be wrong
/// about: the principal is the session the function's call is signed as, the action is the one
/// the read performs, and the resource is the identifier the request supplied for the secret.
fn refusal_message(index: usize, secret_id: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{}/{} is not authorized to perform: \
         {READ_ACTION} on resource: {secret_id} because no identity-based policy allows the \
         {READ_ACTION} action",
        role_name(index),
        function_name(index),
    )
}

/// The trust policy a Lambda execution role needs.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
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

/// The document a row's role carries while its grant is IN FORCE: the log actions, and the read
/// action on the row's own secret and nothing else — the least privilege the read needs.
fn granting_document(own_secret_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            log_statement(),
            {"Effect": "Allow", "Action": READ_ACTION, "Resource": own_secret_arn},
        ],
    })
    .to_string()
}

/// The document that replaces it when the grant is WITHDRAWN: the same least-privilege baseline
/// every execution role needs, and nothing of Secrets Manager at all.
fn logs_only_document() -> String {
    json!({"Version": "2012-10-17", "Statement": [log_statement()]}).to_string()
}

/// Put `document` in force as the one inline policy of the row's role.
async fn put_role_policy(router: &MultiServiceRouter, case: &str, index: usize, document: &str) {
    let name = role_name(index);
    let (status, answered) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", &name),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy({name:?}) must succeed, got {status} with {answered}"
    );
}

/// Create the role one row runs as, and answer the ARN IAM minted for it. The grant itself is put
/// in force later, so one role is rewritten by `PutRolePolicy` between the probes.
async fn create_role(router: &MultiServiceRouter, index: usize) -> String {
    let name = role_name(index);
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", &name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    })
}

/// Create one secret holding `value` and answer the ARN Secrets Manager minted for it.
async fn create_secret(router: &MultiServiceRouter, name: &str, value: &str) -> String {
    let (status, created) = secretsmanager(
        router,
        "CreateSecret",
        &json!({
            "Name": name,
            "SecretString": value,
            "ClientRequestToken": version_token(name),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSecret({name:?}) must succeed, got {status} with {created}"
    );
    let arn = created["ARN"].as_str().unwrap_or_else(|| {
        panic!("setup: CreateSecret({name:?}) must answer the secret's ARN, got {created}")
    });
    assert_eq!(
        created["VersionId"].as_str(),
        Some(version_token(name).as_str()),
        "setup: the created version is the one the ClientRequestToken named, got {created}"
    );
    arn.to_string()
}

/// The template the suite deploys: one function per row, in that row's own runtime, running as
/// that row's role and configured with that row's secret. The inline source is archived into the
/// deployment package by CloudFormation, under the file name the declared runtime's handler
/// resolves — which is how a function whose handler is written here comes to exist at all.
fn stack_template(roles: &[String], secrets: &[Secret]) -> String {
    let mut resources = serde_json::Map::new();
    for (index, row) in ROWS.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": row.runtime,
                    "Handler": "index.handler",
                    "Role": roles[index],
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {
                        "Variables": {SECRET_VARIABLE: secrets[index].id},
                    },
                    "Code": {"ZipFile": row.source},
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the stack, wait for `CREATE_COMPLETE` and answer the physical id of each declared
/// function.
async fn deploy(
    router: &MultiServiceRouter,
    roles: &[String],
    secrets: &[Secret],
) -> BTreeMap<String, String> {
    let template = stack_template(roles, secrets);
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
    // The stack's completion is owed from the instant Lambda reports its functions created, which
    // no fixed sleep can name: this looks until it reports it, under a deadline generous enough
    // that a loaded host does not decide the outcome.
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
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
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// Wait until Lambda reports `name` `Active` — "the function is ready to be invoked"
/// (`FunctionConfiguration$State`) — which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// One secret, as the function it belongs to addresses it and as a served read must report it.
struct Secret {
    /// The identifier the handler's request carries — the ARN the function's environment holds —
    /// which is also what a refusal names.
    id: String,
    name: String,
    value: &'static str,
}

/// Invoke one row's function and answer the document its handler returned.
async fn probe(router: &MultiServiceRouter, case: &str, function: &str) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        json!({}).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    let faulted = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        faulted, None,
        "case {case:?}: the handler must run to completion, got {faulted:?} with {payload}"
    );
    payload
}

/// What the handler reports for a read the policies in force ALLOW: the version's own members,
/// read out of the secret this suite created.
fn served(secret: &Secret) -> Value {
    json!({
        "ok": true,
        "arn": secret.id,
        "name": secret.name,
        "secret": secret.value,
        "version_id": version_token(&secret.name),
        "stages": [STAGE_CURRENT],
    })
}

/// What it reports for a read nothing in force allows: the code, the sentence and the HTTP status
/// the service answered.
fn refused(index: usize, secret: &Secret) -> Value {
    json!({
        "ok": false,
        "code": ACCESS_DENIED,
        "message": refusal_message(index, &secret.id),
        "status": ACCESS_DENIED_STATUS_TEXT,
    })
}

/// Every row: the function's own read, made with the SDK its declared runtime bundles, is decided
/// against the document in force on its execution role when the call is made — so withdrawing the
/// grant refuses the identical call with IAM's own sentence and putting it back serves it again.
#[tokio::test]
async fn an_in_function_secret_read_follows_its_roles_grant_on_every_runtime() {
    let router = build_router();

    let mut secrets = Vec::new();
    let mut roles = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let name = secret_name(index);
        secrets.push(Secret {
            id: create_secret(&router, &name, row.value).await,
            name,
            value: row.value,
        });
        roles.push(create_role(&router, index).await);
    }
    let physical = deploy(&router, &roles, &secrets).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let own = &secrets[index];
        let logical = format!("Probe{index}");
        let function = physical
            .get(&logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must report a physical id for {logical}"));
        assert_eq!(
            function,
            function_name(index),
            "setup: the stack must name the function {:?}, got {function:?}",
            function_name(index)
        );
        wait_until_active(&router, &function).await;

        // Phase 1: the grant is in force, so the read is served.
        put_role_policy(&router, case, index, &granting_document(&own.id)).await;
        let answered = probe(&router, case, &function).await;
        let expected = served(own);
        assert_eq!(
            answered, expected,
            "case {case:?}: granted, expected the handler to report {expected}, got {answered}"
        );

        // Phase 2: the same role, carrying the logs-only baseline instead, is granted nothing of
        // this service — so the identical call is refused, with IAM's own sentence.
        put_role_policy(&router, case, index, &logs_only_document()).await;
        let answered = probe(&router, case, &function).await;
        let expected = refused(index, own);
        assert_eq!(
            answered, expected,
            "case {case:?}: withdrawn, expected the handler to report {expected}, got {answered}"
        );

        // Phase 3: the grant is put back, and the identical call is served again.
        put_role_policy(&router, case, index, &granting_document(&own.id)).await;
        let answered = probe(&router, case, &function).await;
        let expected = served(own);
        assert_eq!(
            answered, expected,
            "case {case:?}: restored, expected the handler to report {expected}, got {answered}"
        );
    }
}
