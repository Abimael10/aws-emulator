// SPDX-License-Identifier: CC0-1.0

//! What a bucket policy's `Principal.AWS` is resolved against when the grantee is a RUNNING
//! function's execution identity, and what a document naming a principal that resolves to
//! nothing does to the policy already in force.
//!
//! RULE: a resource-based policy states WHO a permission is granted to, so `PutBucketPolicy`
//! resolves every identity its document names against the account's own IAM records as they
//! stand when the call is made: a document whose principal the account holds is stored (and is
//! then in force, so a caller signing as that identity — a function's execution-role session
//! included — is served exactly the actions it grants), while a document naming an IAM user or
//! role of this account that the account does NOT hold is refused HTTP 400 `MalformedPolicy`
//! with the fixed sentence `Invalid principal in policy` and leaves the document the bucket
//! already carried exactly as it was, so the grant already in force keeps serving. The refusal
//! is attributable to the principal's nonexistence alone: once an identity exists under that
//! name — created for the first time, or created again after a delete, which carries the
//! identical ARN — the byte-identical call succeeds and that document is the one in force. Each
//! of those calls is answered within the one request: nothing here is a state a caller has to
//! poll for.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>` — named by that spelling or by the
//! ARN of the role behind it. A request of a principal of the bucket's own account is allowed
//! when an identity-based policy allows it OR the bucket's own policy names the caller and
//! allows it (IAM User Guide, *Policy evaluation logic*); no execution role here carries a grant
//! of the read, so the bucket's document is the only thing that can authorize it.
//!
//! Five crates have to agree for that to hold — CloudFormation archives each inline handler into
//! a deployment package this emulator can run and deploys the roles beside it, IAM holds the
//! roles and the users every principal is resolved against, STS mints the session the handler's
//! own SDK call is signed as, Lambda runs the handler and supplies it that SDK, and Amazon S3
//! both resolves the document's principals and takes the read's decision — so the suite lives
//! here, where a crate-local one could only ever see one side of it. What the neighbours pin and
//! this one does not repeat: the Amazon S3 crate's `bucket_policy_principal_lifecycle` tables
//! the same resolution for a client presenting a credential of its own, and
//! `s3_object_read_identity_or_bucket_policy` drives the union of the two documents for an IAM
//! user. What only this suite reaches is the pair of decisions a FUNCTION's read depends on —
//! the document naming its session being accepted, and a later refused write not disturbing it.
//!
//! The rows differ along what the rule generalises over: WHICH spelling the live document names
//! the function's execution identity by (the role's ARN, the assumed-role session's ARN, the
//! role's ARN as one entry of a list beside another identity that also resolves), and WHICH kind
//! of unresolvable principal the refused document names (a role of this account that EXISTED and
//! was deleted, a role under a path of its own that the account has never held, and an IAM user
//! the account has never held — the two IAM entity types a resource policy grants to by name).
//! Every row works on a bucket of its own against ONE execution role and ONE function, because
//! what a row varies is how a document NAMES that identity and not the identity itself. Every row
//! asserts the resolution — what is stored, what is refused and what the bucket reports — and ONE
//! row reads through the RUNNING function, twice: the two reads differ in nothing but the
//! document in force, which is what says a stored document decides a function's own call.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the bucket, the roles and the
/// functions belong to. `us-east-1`, so a bucket create names no location constraint.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The long-term key every emulator publishes: the account's own credential, which no
/// identity-based document bounds, so every fixture call is made with it and no row's own
/// identity decides the setup.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The stack the execution roles and the reader functions are deployed by.
const STACK: &str = "xsvc-bucket-policy-principal-read";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The API version an awsQuery call to IAM names.
const IAM_API_VERSION: &str = "2010-05-08";

/// The API version an awsQuery call to CloudFormation names.
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";

/// The object every function reads, and the bytes it holds: what a served read answers, exactly.
const OBJECT_KEY: &str = "reports/data.txt";
const OBJECT_BODY: &str = "region,amount\nus-east-1,17\n";

/// The action a read of the current version of an object is authorized under ("only the
/// `s3:GetObject` permission is required", `GetObject`'s own documentation), which is what every
/// document here grants.
const GET_OBJECT: &str = "s3:GetObject";

/// The code and the message `PutBucketPolicy` refuses a document naming an identity it cannot
/// resolve as a grantee with. Written out here rather than imported from the service, so the
/// wording has something to be wrong about; the sentence is fixed text in full — the value that
/// did not resolve is not quoted back.
const MALFORMED_POLICY: &str = "MalformedPolicy";
const INVALID_PRINCIPAL: &str = "Invalid principal in policy";
/// The status such a refusal carries.
const MALFORMED_POLICY_STATUS: u16 = 400;

/// The status a stored bucket policy is acknowledged with.
const POLICY_STORED_STATUS: u16 = 204;

/// The code and the status Amazon S3 refuses a read no document in force authorizes with (Amazon
/// S3 "Error responses": `AccessDenied`, HTTP 403) — what the handler reports of such a read.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the authorization decision and not how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created function reaching
/// `Active` — and how many looks to take. The span is Lambda's own, so no fixed sleep names the
/// instant it ends, and the deadline is generous enough that a loaded host cannot decide the
/// outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler every row runs: it imports the SDK its managed runtime supplies at MODULE scope
/// and builds its clients there — so a runtime that made no SDK importable could not run it at
/// all — and reads the one object the event names, reporting either its bytes or the refusal
/// Amazon S3 answered. Every answer carries the identity its own calls are signed as, read from
/// STS rather than assumed, which is the identity the bucket's document decides.
const READER_HANDLER: &str = r#"
import boto3
from botocore.exceptions import ClientError

OBJECTS = boto3.client("s3")
IDENTITY = boto3.client("sts").get_caller_identity()["Arn"]


def handler(event, context):
    answered = {"identity": IDENTITY}
    try:
        read = OBJECTS.get_object(Bucket=event["bucket"], Key=event["key"])
    except ClientError as refused:
        answered["read"] = False
        answered["code"] = refused.response["Error"]["Code"]
        answered["status"] = refused.response["ResponseMetadata"]["HTTPStatusCode"]
        return answered
    answered["read"] = True
    answered["body"] = read["Body"].read().decode("utf-8")
    return answered
"#;

/// WHICH spelling a row's live document names the function's execution identity by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// The ARN of the execution ROLE behind the session — the spelling a policy names the role
    /// itself by.
    RoleArn,
    /// The ARN of the assumed-role SESSION the function's own calls are signed as.
    SessionArn,
    /// The role's ARN as one entry of a list whose other entry also resolves, so the document
    /// stands or falls on neither entry alone.
    RoleArnBesideAnother,
}

/// WHICH kind of principal a row's refused document names: one the account cannot resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Absent {
    /// A role of this account that EXISTED and was then deleted.
    DeletedRole,
    /// A role under a path of its own that the account has never held — the path is part of a
    /// role's ARN, so this is a different spelling as well as a different record.
    RoleNeverHeld { path: &'static str },
    /// An IAM user the account has never held: the other IAM entity type a resource policy
    /// grants to by name, resolved under its own type.
    UserNeverHeld,
}

/// One row: the spelling its live document names the function by, the unresolvable principal its
/// refused document names, and whether the row reads through the RUNNING function.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What to call the case in a failure.
    case: &'static str,
    names: Named,
    absent: Absent,
    /// Whether this row reads through the function, which ONE row does: the two reads it takes
    /// differ in nothing but the document in force (the grant that survived the refused write,
    /// then the once-refused document accepted in its place), so they are what says the stored
    /// document decides a running function's own call. Every row asserts the resolution itself —
    /// what is stored, what is refused and what the bucket reports — because that is the half the
    /// spellings vary, and a handler run says nothing more about it than the policy reads do.
    reads_through_the_function: bool,
}

/// The table: every spelling of the live grantee against every kind of unresolvable principal
/// this account can be asked about.
const ROWS: &[Row] = &[
    Row {
        case: "the execution role's own ARN, against a role that was deleted",
        names: Named::RoleArn,
        absent: Absent::DeletedRole,
        reads_through_the_function: true,
    },
    Row {
        case: "the assumed-role session's ARN, against a role under a path the account never held",
        names: Named::SessionArn,
        absent: Absent::RoleNeverHeld { path: "/sim2real/" },
        reads_through_the_function: false,
    },
    Row {
        case: "the role's ARN in a list beside another identity, against a user never held",
        names: Named::RoleArnBesideAnother,
        absent: Absent::UserNeverHeld,
        reads_through_the_function: false,
    },
];

/// The execution role every row's function runs as, the further role that resolves but grants
/// that function nothing (the grantee of each baseline document, and the other entry of the list
/// form), and the function itself: ONE of each, because what the rows vary is the SPELLING a
/// document names that identity by and the kind of principal it cannot resolve — not the
/// identity. Each row works on a bucket of its own, so nothing one row stores decides another's
/// outcome.
const ROLE_NAME: &str = "xsvc-principal-read-role";
const BYSTANDER_NAME: &str = "xsvc-principal-read-other";
const FUNCTION_NAME: &str = "xsvc-principal-read-fn";

/// The bucket one row works on, and the name its unresolvable principal is spelled with.
fn bucket_name(index: usize) -> String {
    format!("xsvc-principal-read-{index}")
}
fn absent_name(index: usize) -> String {
    format!("xsvc-principal-read-absent-{index}")
}

impl Absent {
    /// The ARN a row's refused document names, in the spelling IAM reports for such an entity.
    /// Confirmed against IAM itself by [`Self::make_it_exist`], which asserts the record created
    /// under that name carries this very ARN.
    fn arn(self, index: usize) -> String {
        let name = absent_name(index);
        match self {
            Self::DeletedRole => format!("arn:aws:iam::{ACCOUNT}:role/{name}"),
            Self::RoleNeverHeld { path } => format!("arn:aws:iam::{ACCOUNT}:role{path}{name}"),
            Self::UserNeverHeld => format!("arn:aws:iam::{ACCOUNT}:user/{name}"),
        }
    }

    /// Bring the account to the state the row's refusal is about: for the deleted-role row, the
    /// role is created and then deleted — so the ARN its document names is one the account HELD
    /// — and for the others there is nothing to do, because the account has never held them.
    async fn prepare(self, router: &MultiServiceRouter, case: &str, index: usize) {
        if self != Self::DeletedRole {
            return;
        }
        let name = absent_name(index);
        let created = create_role(router, case, &name, None).await;
        assert_eq!(
            created,
            self.arn(index),
            "setup for {case:?}: the role {name:?} must carry the ARN its document names, got \
             {created:?}"
        );
        let (status, answered) = query(
            router,
            "iam",
            IAM_API_VERSION,
            "DeleteRole",
            &[("RoleName", &name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup for {case:?}: DeleteRole({name:?}) must succeed, got {status} with {answered}"
        );
    }

    /// Create an identity under the name the refused document named, and answer the ARN IAM
    /// reports for it: for the deleted-role row, a role created AGAIN under the identical name.
    async fn make_it_exist(self, router: &MultiServiceRouter, case: &str, index: usize) -> String {
        let name = absent_name(index);
        match self {
            Self::DeletedRole => create_role(router, case, &name, None).await,
            Self::RoleNeverHeld { path } => create_role(router, case, &name, Some(path)).await,
            Self::UserNeverHeld => {
                let (status, answered) = query(
                    router,
                    "iam",
                    IAM_API_VERSION,
                    "CreateUser",
                    &[("UserName", &name)],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: CreateUser({name:?}) must succeed, got {status} with \
                     {answered}"
                );
                element(&answered, "Arn")
            }
        }
    }
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the object it reads is the object this suite stored and the document
/// deciding it is the one this suite put on the bucket.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request, and the access key id is what names its caller.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is the account's own published key.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One request of `signing_name`, answered as `(status, body)`.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: &str,
) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_owned()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One awsQuery call of `service`, every component percent-encoded so a policy document travels
/// verbatim.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        service,
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        &body,
    )
    .await
}

/// One S3 call on the path-style address, made as the account's own credential — the bucket's
/// owner, whose own calls no document here refuses.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    dispatch(router, "s3", method, path, "application/xml", body).await
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
        .expect("the test request builds");
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

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The inline document every execution role here carries: the log actions an execution role
/// needs and NOTHING of Amazon S3, so the bucket's own document is the only thing that can
/// authorize a read.
fn execution_role_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
}

/// A one-statement bucket policy granting [`GET_OBJECT`] on `bucket`'s objects to `principal`,
/// under the statement id `sid`.
fn document(sid: &str, principal: &Value, bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": sid,
            "Effect": "Allow",
            "Principal": principal,
            "Action": GET_OBJECT,
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
        }],
    })
    .to_string()
}

/// The template the suite deploys: the execution role the function runs as, one further role
/// that resolves but grants that function nothing (the grantee of each baseline document, and
/// the other entry of the list form), and the reader function itself. The inline source is
/// archived into the deployment package by CloudFormation, which is how a function whose handler
/// is written here comes to exist at all.
fn stack_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE_NAME,
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "reader-grants",
                        "PolicyDocument": execution_role_document(),
                    }],
                },
            },
            "Bystander": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": BYSTANDER_NAME,
                    "AssumeRolePolicyDocument": trust_policy(),
                },
            },
            "Reader": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION_NAME,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": READER_HANDLER},
                },
            },
        },
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
        CLOUDFORMATION_API_VERSION,
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
            CLOUDFORMATION_API_VERSION,
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
        CLOUDFORMATION_API_VERSION,
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

/// `iam:CreateRole`, answering the ARN IAM reports for the role — the spelling a document names
/// it by, read from the service rather than built here.
async fn create_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    path: Option<&str>,
) -> String {
    let trust = trust_policy().to_string();
    let mut members = vec![("RoleName", name), ("AssumeRolePolicyDocument", &trust)];
    if let Some(path) = path {
        members.push(("Path", path));
    }
    let (status, answered) = query(router, "iam", IAM_API_VERSION, "CreateRole", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({name:?}) must succeed, got {status} with {answered}"
    );
    element(&answered, "Arn")
}

/// The ARN IAM holds for the role `name`.
async fn role_arn(router: &MultiServiceRouter, name: &str) -> String {
    let (status, answered) = query(
        router,
        "iam",
        IAM_API_VERSION,
        "GetRole",
        &[("RoleName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetRole({name:?}) must succeed, got {status} with {answered}"
    );
    element(&answered, "Arn")
}

/// Create `bucket` and store the one object every read of this row addresses.
async fn seed_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        status, 200,
        "setup for {case:?}: the bucket {bucket:?} must be created, got {status} with {answered}"
    );
    let path = format!("/{bucket}/{OBJECT_KEY}");
    let (status, answered) = s3(router, "PUT", &path, OBJECT_BODY).await;
    assert_eq!(
        status, 200,
        "setup for {case:?}: the object must be stored, got {status} with {answered}"
    );
}

/// `PUT /<bucket>?policy` with `document` as the payload.
async fn put_bucket_policy(
    router: &MultiServiceRouter,
    bucket: &str,
    document: &str,
) -> (u16, String) {
    s3(router, "PUT", &format!("/{bucket}?policy"), document).await
}

/// `PUT /<bucket>?policy`, asserted stored.
async fn store_bucket_policy(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    bucket: &str,
    document: &str,
) {
    let (status, answered) = put_bucket_policy(router, bucket, document).await;
    assert_eq!(
        status, POLICY_STORED_STATUS,
        "case {case:?}: {leg} must be stored with {POLICY_STORED_STATUS}, got {status} with \
         {answered}"
    );
}

/// The document the bucket carries right now, byte for byte.
async fn stored_bucket_policy(router: &MultiServiceRouter, case: &str, bucket: &str) -> String {
    let (status, answered) = s3(router, "GET", &format!("/{bucket}?policy"), "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetBucketPolicy must report the stored document, got {status} with \
         {answered}"
    );
    answered
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
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

/// The `X-Amz-Function-Error` an invocation carries, which is set exactly when the run failed.
fn function_error(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Invoke the reader and answer what its handler reported of the read, less the identity its
/// calls were signed as, which is asserted here against `session`: every probe therefore checks
/// that the session spelling a document names is the one STS reports to the running handler. The
/// invocation is served and the run completes either way, because the handler catches a refusal.
async fn read_through_the_function(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    session: &str,
    bucket: &str,
) -> Value {
    let event = json!({"bucket": bucket, "key": OBJECT_KEY}).to_string();
    let (status, headers, mut payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION_NAME}/invocations"),
        event.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation for {leg} must be served, got {status} with {payload}"
    );
    let failed = function_error(&headers);
    assert_eq!(
        failed, None,
        "case {case:?}: the handler for {leg} catches the refusal, so no run fails, got \
         {failed:?} with {payload}"
    );
    let reported = payload
        .as_object_mut()
        .and_then(|answered| answered.remove("identity"))
        .unwrap_or(Value::Null);
    assert_eq!(
        reported,
        json!(session),
        "case {case:?}: the handler's calls for {leg} must be signed as {session:?}, got \
         {reported:?}"
    );
    payload
}

/// What the handler reports when the document in force authorizes its read.
fn served() -> Value {
    json!({"read": true, "body": OBJECT_BODY})
}

/// What the handler reports when nothing in force authorizes its read.
fn refused() -> Value {
    json!({"read": false, "code": ACCESS_DENIED, "status": ACCESS_DENIED_STATUS})
}

/// RULE: `PutBucketPolicy` resolves every principal its document names against the account's IAM
/// records as they stand when the call is made — the accepted document is in force for the
/// function's own execution-role session, the one naming an unresolvable principal is refused
/// 400 `MalformedPolicy` / `Invalid principal in policy` and leaves the stored document (and so
/// the grant in force) untouched, and the identical call succeeds once an identity exists under
/// that name.
#[tokio::test]
async fn a_bucket_policy_principal_is_resolved_against_iam_and_decides_a_function_read() {
    let router = build_router();
    let physical = deploy(&router).await;
    let reader = physical
        .get("Reader")
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack must report a physical id for the function"));
    assert_eq!(
        reader, FUNCTION_NAME,
        "setup: the stack must name the function {FUNCTION_NAME:?}, got {reader:?}"
    );
    wait_until_active(&router, &reader).await;

    let execution_role = role_arn(&router, ROLE_NAME).await;
    let bystander = role_arn(&router, BYSTANDER_NAME).await;
    // The identity a function's own calls are signed as: the session of its execution role,
    // under the function's own name. Written out here as the spelling a document has to carry,
    // and checked against the one STS reports to the running handler on every probe below.
    let session = format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE_NAME}/{FUNCTION_NAME}");

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let bucket = bucket_name(index);
        seed_bucket(&router, case, &bucket).await;

        let grantee = match row.names {
            Named::RoleArn => json!({"AWS": &execution_role}),
            Named::SessionArn => json!({"AWS": &session}),
            Named::RoleArnBesideAnother => json!({"AWS": [&execution_role, &bystander]}),
        };
        let baseline = document("GrantsAnotherIdentity", &json!({"AWS": &bystander}), &bucket);
        let live = document("GrantsTheFunction", &grantee, &bucket);

        // The document the bucket carries before the row's own grant: it resolves, so it is
        // stored, and it grants the function nothing.
        store_bucket_policy(&router, case, "the baseline document", &bucket, &baseline).await;

        // Leg 1: the document naming the function's own execution identity is accepted within the
        // one call, and is the document the bucket reports.
        store_bucket_policy(&router, case, "the live grant", &bucket, &live).await;
        let stored = stored_bucket_policy(&router, case, &bucket).await;
        assert_eq!(
            stored, live,
            "case {case:?}: the accepted document must be reported byte for byte, got {stored:?}"
        );

        // Leg 2: a document naming a principal the account cannot resolve is refused, and the
        // refusal changes nothing — the stored document, and so the grant in force, is the one
        // from leg 1, which goes on serving the function's read.
        row.absent.prepare(&router, case, index).await;
        let absent_arn = row.absent.arn(index);
        let names_the_absent = document(
            "GrantsTheAbsentIdentity",
            &json!({"AWS": &absent_arn}),
            &bucket,
        );
        let (status, answered) = put_bucket_policy(&router, &bucket, &names_the_absent).await;
        assert_eq!(
            (
                status,
                opt_element(&answered, "Code").unwrap_or_default(),
                opt_element(&answered, "Message").unwrap_or_default()
            ),
            (
                MALFORMED_POLICY_STATUS,
                MALFORMED_POLICY.to_string(),
                INVALID_PRINCIPAL.to_string()
            ),
            "case {case:?}: a document naming the unresolvable {absent_arn:?} must be refused, \
             got {status} with {answered}"
        );
        let stored = stored_bucket_policy(&router, case, &bucket).await;
        assert_eq!(
            stored, live,
            "case {case:?}: the refused write must leave the stored document as it was, got \
             {stored:?}"
        );
        if row.reads_through_the_function {
            let reported = read_through_the_function(
                &router,
                case,
                "the live grant after the refused write",
                &session,
                &bucket,
            )
            .await;
            assert_eq!(
                reported,
                served(),
                "case {case:?}: the grant naming {grantee} must keep serving the function's read \
                 after the refused write, expected {:?}, got {reported:?}",
                served()
            );
        }

        // Leg 3: an identity exists under that name — the identical ARN — so the byte-identical
        // call succeeds, and the document it stored is the one in force: the function's grant is
        // gone with it. The refusal was about the principal's nonexistence alone.
        let now_held = row.absent.make_it_exist(&router, case, index).await;
        assert_eq!(
            now_held, absent_arn,
            "case {case:?}: the identity created under that name must carry the ARN the document \
             named, got {now_held:?}"
        );
        store_bucket_policy(
            &router,
            case,
            "the once-refused document",
            &bucket,
            &names_the_absent,
        )
        .await;
        let stored = stored_bucket_policy(&router, case, &bucket).await;
        assert_eq!(
            stored, names_the_absent,
            "case {case:?}: the document accepted the second time must be the stored one, got \
             {stored:?}"
        );
        if row.reads_through_the_function {
            let reported = read_through_the_function(
                &router,
                case,
                "the once-refused document now in force",
                &session,
                &bucket,
            )
            .await;
            assert_eq!(
                reported,
                refused(),
                "case {case:?}: the document now in force grants the function nothing, expected \
                 {:?}, got {reported:?}",
                refused()
            );
        }
    }
}
