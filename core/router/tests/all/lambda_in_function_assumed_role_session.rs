// SPDX-License-Identifier: CC0-1.0

//! WHICH IDENTITY a running function's own AWS SDK call is signed as, with the whole emulator
//! assembled: the credentials the client was built with.
//!
//! RULE: the AWS SDK a managed runtime supplies signs each call with the credentials the
//! CLIENT holds — the ones the handler built that client with, and otherwise the session of
//! the function's own execution role — so the service serving the call decides it against the
//! policies in force on the identity those credentials name. A handler that assumes a second
//! role through the security token service and builds a client with the session it was vended
//! therefore calls as that session: an object the execution role may not read is read with it,
//! and the token service reports the assumed-role session as the caller. An assume the role's
//! own trust policy does not admit is the token service's `AccessDenied` (HTTP 403), which the
//! handler catches like any other refusal — so both the allowed and the refused path answer
//! `Invoke` HTTP 200 with the handler's own payload and NO `X-Amz-Function-Error`, because
//! neither is a fault of the run.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and role chaining is how a function reaches a resource its
//! own role is not granted: `AssumeRole` is decided by "a role trust policy that specifies who
//! can assume the role" (`AssumeRole`), and the credentials it vends are what the next call is
//! signed with.
//!
//! Six crates have to agree for that to hold — CloudFormation deploys the roles and the probe
//! function together, IAM holds each role, its trust policy and its inline document, STS
//! decides the assume and mints the session, Lambda runs the handler and supplies it the SDK,
//! and Amazon S3 takes the decision on the read — so the suite lives here: a crate-local suite
//! can only ever see one side of it. What the neighbours pin and this one does not repeat:
//! `lambda_in_function_sqs_authorization` drives the decision taken on a call signed with the
//! EXECUTION ROLE's own session, and the Lambda crate's own `handler_assumed_role_session`
//! drives the same rule over services it mounts itself. What only this suite reaches is the
//! identity such a call speaks for when the whole router is assembled and the roles are the
//! ones a TEMPLATE deployed.
//!
//! The rows differ along what the rule generalises over: WHICH credentials the client holds
//! (none, the session one role vended, a second session under another session name, the
//! session a role that does not trust the function would vend) and what the identity behind
//! them may do to the one object every row reads.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every role ARN and every session ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The bucket holding the one object every row reads, and that object's key and bytes.
const BUCKET: &str = "xsvc-in-fn-sts-restricted";
const KEY: &str = "restricted/report.txt";
const BODY: &str = "only the assumed session may read this";

/// The execution role the probe function runs as: it may assume the two roles below and may
/// read NOTHING of the bucket.
const EXEC_ROLE: &str = "xsvc-in-fn-sts-exec";

/// The role whose trust policy names the execution role, and whose own document allows the
/// read: the session it vends is what a row's client is built with.
const READER_ROLE: &str = "xsvc-in-fn-sts-reader";

/// The role whose trust policy names ANOTHER role of this account and not the execution role,
/// so the execution role's assume of it is refused.
const CLOSED_ROLE: &str = "xsvc-in-fn-sts-closed";

/// That other role, which exists so the closed role's trust policy names a principal of this
/// account that is simply not the function's.
const OTHER_ROLE: &str = "xsvc-in-fn-sts-other";

/// The function every row invokes, and the stack that deploys it with the roles.
const FUNCTION: &str = "xsvc-in-fn-sts-probe";
const STACK: &str = "xsvc-in-fn-sts-probes";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The code and status a service refuses a call the policies in force do not permit with.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u64 = 403;

/// The IAM action an assume performs, which the refusal of one names.
const ASSUME_ROLE_ACTION: &str = "sts:AssumeRole";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is which identity a call is signed as and not
/// how fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack reaching
/// `CREATE_COMPLETE`, a created function reaching `Active` — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler every row runs. It builds its clients with the credentials the row names —
/// none, or the session it assumed — reports the identity the token service says those calls
/// are made as, and reports the object's bytes or the code the read was refused with.
const PROBE_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError


def identity(**credentials):
    return boto3.client("sts", **credentials).get_caller_identity()["Arn"]


def read(event, **credentials):
    client = boto3.client("s3", **credentials)
    try:
        answered = client.get_object(Bucket=event["bucket"], Key=event["key"])
    except ClientError as refused:
        return {
            "read": False,
            "code": refused.response["Error"]["Code"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {"read": True, "body": answered["Body"].read().decode("utf-8")}


def vended(event):
    answered = boto3.client("sts").assume_role(
        RoleArn=event["role_arn"], RoleSessionName=event["session"]
    )
    held = answered["Credentials"]
    credentials = {
        "aws_access_key_id": held["AccessKeyId"],
        "aws_secret_access_key": held["SecretAccessKey"],
        "aws_session_token": held["SessionToken"],
    }
    return credentials, answered["AssumedRoleUser"]["Arn"]


def handler(event, context):
    mode = event["mode"]
    if mode == "own":
        report = read(event)
        report["identity"] = identity()
        return report
    if mode == "assumed":
        credentials, assumed = vended(event)
        report = read(event, **credentials)
        report["identity"] = identity(**credentials)
        report["assumed"] = assumed
        return report
    try:
        vended(event)
    except ClientError as refused:
        return {
            "assumed": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {"assumed": True}
"##;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the roles it assumes and the object it reads are the ones this
/// suite planted.
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
/// are the account's own and not any role's.
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
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One request against the `s3` signing name, as the account's own credential.
async fn s3_wire(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
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

/// The ARN a role of this account carries, which a trust policy and a refusal name.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The session ARN the token service reports for a session of `role` opened under `session`,
/// which is the identity a call signed with that session's credentials speaks for.
fn session_arn(role: &str, session: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{session}")
}

/// WHICH credentials one row's clients are built with.
#[derive(Debug, Clone, Copy)]
enum Built {
    /// With no credentials: the call is signed as the identity the invocation was given.
    NoCredentials,
    /// With the session the named role vends, under the session name the row chose.
    Session(&'static str, &'static str),
}

/// What one row expects of the handler's report.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The read was served, as a session of the role the row assumed.
    ReadsTheObject,
    /// The call was made as the execution role's own session, which may not read the object.
    RefusedTheRead,
    /// The token service refused the assume itself.
    RefusedTheAssume,
}

/// One row: which credentials the handler's clients hold, and what that decides.
struct Row {
    case: &'static str,
    built: Built,
    expect: Expect,
}

const ROWS: &[Row] = &[
    // The case the rule is stated about from the function's own side: a client built with no
    // credentials is the execution role's, and that role is granted nothing of the bucket.
    Row {
        case: "a client built with no credentials calls as the execution role's own session",
        built: Built::NoCredentials,
        expect: Expect::RefusedTheRead,
    },
    // The same handler, the same object, the same function: only the credentials the client
    // holds differ, and the read is served.
    Row {
        case: "a client built with the session the reader role vended reads the object",
        built: Built::Session(READER_ROLE, "first-pass"),
        expect: Expect::ReadsTheObject,
    },
    // The session NAME is the variable part of the identity a session speaks for, so a second
    // pass under another name is another session of the same role.
    Row {
        case: "a second session of that role is named by the session name the handler chose",
        built: Built::Session(READER_ROLE, "second-pass"),
        expect: Expect::ReadsTheObject,
    },
    // A role whose trust policy names another principal of the account vends nothing to this
    // function: the token service refuses the assume, and the handler catches it.
    Row {
        case: "a role whose trust policy omits the execution role vends no session",
        built: Built::Session(CLOSED_ROLE, "third-pass"),
        expect: Expect::RefusedTheAssume,
    },
];

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn lambda_trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": ASSUME_ROLE_ACTION,
        }],
    })
}

/// The trust policy of a role that trusts ONE role of this account by ARN.
fn role_trust_policy(trusted: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": role_arn(trusted)},
            "Action": ASSUME_ROLE_ACTION,
        }],
    })
}

/// The execution role's own document: the log actions every execution role carries and the
/// assume of the two roles this suite points it at — and NOTHING of Amazon S3, so the object is
/// out of reach of the function's own identity.
fn exec_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": ASSUME_ROLE_ACTION,
                "Resource": [role_arn(READER_ROLE), role_arn(CLOSED_ROLE)],
            },
        ],
    })
}

/// The document the assumable roles carry: the read of the one object, which is what makes a
/// session of such a role able to do what the execution role cannot.
fn reader_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
        }],
    })
}

/// The template the suite deploys: the four roles and the one probe function. The inline source
/// is archived into the deployment package by CloudFormation, which is how a function whose
/// handler is written here comes to exist at all.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    for (logical, name, trust, document) in [
        ("ExecRole", EXEC_ROLE, lambda_trust_policy(), exec_document()),
        (
            "OtherRole",
            OTHER_ROLE,
            lambda_trust_policy(),
            reader_document(),
        ),
        (
            "ReaderRole",
            READER_ROLE,
            role_trust_policy(EXEC_ROLE),
            reader_document(),
        ),
        (
            "ClosedRole",
            CLOSED_ROLE,
            role_trust_policy(OTHER_ROLE),
            reader_document(),
        ),
    ] {
        resources.insert(
            logical.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": name,
                    "AssumeRolePolicyDocument": trust,
                    "Policies": [{
                        "PolicyName": "probe-grants",
                        "PolicyDocument": document,
                    }],
                },
            }),
        );
    }
    resources.insert(
        "Probe".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": FUNCTION,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": ["ExecRole", "Arn"]},
                "Timeout": FUNCTION_TIMEOUT,
                "Code": {"ZipFile": PROBE_HANDLER},
            },
        }),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the stack, wait for `CREATE_COMPLETE` and answer the physical id of each declared
/// resource.
async fn deploy(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares named IAM roles, which a create acknowledges before
            // CloudFormation provisions them.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant the owning services report its resources
    // ready, which no fixed sleep can name: this looks until it reports it, on the shared wait
    // whose deadline is generous enough that a loaded host does not decide the outcome.
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

/// Plant the bucket and the one object every row reads, as the account's own credential.
async fn plant_object(router: &MultiServiceRouter) {
    let (status, answered) = s3_wire(router, "PUT", &format!("/{BUCKET}"), "").await;
    assert_eq!(
        status, 200,
        "setup: CreateBucket({BUCKET:?}) must succeed, got {status} with {answered}"
    );
    let (status, answered) = s3_wire(router, "PUT", &format!("/{BUCKET}/{KEY}"), BODY).await;
    assert_eq!(
        status, 200,
        "setup: PutObject({BUCKET:?}, {KEY:?}) must succeed, got {status} with {answered}"
    );
}

/// Wait until Lambda reports the function `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}");
}

/// The `X-Amz-Function-Error` an invocation carries, which is set exactly when the run failed.
fn function_error(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// The event one row hands the handler.
fn event(row: &Row) -> Value {
    let mut held = json!({"bucket": BUCKET, "key": KEY});
    let members = held.as_object_mut().expect("the event is an object");
    match row.built {
        Built::NoCredentials => {
            members.insert("mode".to_string(), json!("own"));
        }
        Built::Session(role, session) => {
            let mode = match row.expect {
                Expect::RefusedTheAssume => "refused",
                _ => "assumed",
            };
            members.insert("mode".to_string(), json!(mode));
            members.insert("role_arn".to_string(), json!(role_arn(role)));
            members.insert("session".to_string(), json!(session));
        }
    }
    held
}

/// What one row's report must be.
fn expected(row: &Row) -> Value {
    match (row.expect, row.built) {
        (Expect::ReadsTheObject, Built::Session(role, session)) => json!({
            "read": true,
            "body": BODY,
            "identity": session_arn(role, session),
            "assumed": session_arn(role, session),
        }),
        (Expect::RefusedTheRead, _) => json!({
            "read": false,
            "code": ACCESS_DENIED,
            "status": ACCESS_DENIED_STATUS,
            "identity": session_arn(EXEC_ROLE, FUNCTION),
        }),
        (Expect::RefusedTheAssume, Built::Session(role, _session)) => json!({
            "assumed": false,
            "code": ACCESS_DENIED,
            // How AWS words an authorization refusal: the principal the call was signed as,
            // the action it asked to perform and the resource it named.
            "message": format!(
                "User: {} is not authorized to perform: {ASSUME_ROLE_ACTION} on resource: {}",
                session_arn(EXEC_ROLE, FUNCTION),
                role_arn(role),
            ),
            "status": ACCESS_DENIED_STATUS,
        }),
        (expect, built) => {
            panic!("case {:?}: {expect:?} is not a row built {built:?}", row.case)
        }
    }
}

/// Every row: which identity a call a running function makes speaks for is decided by the
/// credentials the SDK client was built with, and what that identity may do decides the answer.
#[tokio::test]
async fn an_in_function_call_is_signed_as_the_credentials_its_client_holds() {
    let router = build_router();
    plant_object(&router).await;
    let physical = deploy(&router).await;
    let reported = physical.get("Probe").cloned().unwrap_or_default();
    assert_eq!(
        reported, FUNCTION,
        "setup: the stack must report the probe function as {FUNCTION:?}, got {reported:?}"
    );
    wait_until_active(&router).await;

    for row in ROWS {
        let case = row.case;
        let (status, headers, payload) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
            event(row).to_string().as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation must be served, got {status} with {payload}"
        );
        // Whatever the row's credentials decide, the RUN itself completes: the handler's own
        // payload is the answer, and no path here is a fault of the function.
        assert_eq!(
            function_error(&headers),
            None,
            "case {case:?}: the handler must run to completion, got {:?} with {payload}",
            function_error(&headers)
        );
        let wanted = expected(row);
        assert_eq!(
            payload, wanted,
            "case {case:?}: expected the handler to report {wanted}, got {payload}"
        );
    }

    // The object is exactly as it was planted: every row only ever read it, and the row whose
    // identity was granted nothing read nothing at all.
    let (status, held) = s3_wire(&router, "GET", &format!("/{BUCKET}/{KEY}"), "").await;
    assert_eq!(
        (status, held.as_str()),
        (200, BODY),
        "the object must be exactly as it was planted, got {status} with {held:?}"
    );
}
