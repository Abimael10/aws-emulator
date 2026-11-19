// SPDX-License-Identifier: CC0-1.0

//! WHAT the PERMISSIONS BOUNDARY attached to a Lambda execution role does to the S3 calls a
//! running `python3.12` handler makes with the SDK its runtime supplies.
//!
//! RULE: **a permissions boundary attached to an IAM identity is the CEILING on what that
//! identity's own policies grant: the permissions in force are the INTERSECTION of the two, so
//! a request an identity-based statement allows and the boundary does not is refused 403
//! `AccessDenied` whose sentence names the boundary as what withheld it — `because no
//! permissions boundary allows the <action> action` where the boundary merely states nothing
//! about the request, and `with an explicit deny in a permissions boundary` where it denies it
//! — while a request both allow is served; and the ceiling a CloudFormation
//! `AWS::IAM::Role` declares is in force on the role the stack created, whichever managed
//! policy a later stack update points it at.**
//!
//! "A permissions boundary sets the maximum permissions that an identity-based policy can
//! grant to an IAM entity" (IAM User Guide, *Permissions boundaries for IAM entities*), and
//! `AttachedPermissionsBoundary$PermissionsBoundaryArn` is "the ARN of the policy used to set
//! the permissions boundary" — a MANAGED policy, so the document in force is that policy's
//! default version. The refusal's own wording is IAM's published denial sentence: the
//! principal that signed, the action, the resource and the clause naming which document
//! withheld the grant (IAM User Guide, *Troubleshoot access denied error messages*). A
//! boundary caps only what the identity's OWN documents grant, so it is the clause a client
//! reads to tell "this role was never granted the action" from "this role is capped".
//!
//! Five crates have to agree for that to hold — CloudFormation deploys the ceiling, the role
//! and the function, IAM holds the boundary and the role's own document, the token service
//! vends the session the handler's calls are signed as, Lambda RUNS the handler and supplies
//! it the SDK, and Amazon S3 takes the decision — so the suite lives here, where a crate-local
//! one could only see one side of it. `lambda_in_function_s3_wildcard_grant_scope` pins what a
//! role's own statement authorizes with NO ceiling over it, which is the half this suite adds
//! the ceiling to; `cfn_role_widening_update_decides_a_functions_listings` pins the same
//! deploy-then-update arm for the role's own statements.
//!
//! The rows differ along what the rule generalises over: WHAT the ceiling says about the
//! request (nothing at all, every action, only another service's actions, the same action on
//! another bucket, an explicit deny of one action, one action of the two asked for) crossed
//! with WHICH call the handler makes (an object write, an object read, a HEAD of the bucket
//! itself, the ACCOUNT's own bucket listing — which addresses no resource of its own, so its
//! refusal names none) — so no verdict rests on one boundary shape or one operation. Each
//! refusal is checked for the CLAUSE it carries and for the 403 it is carried at, and each
//! served call is checked against what the emulator holds: the bytes the read handed back, the
//! Region of the bucket the HEAD resolved, the buckets the listing named, and what the bucket
//! holds afterwards read back by the account's own credential — a refusal that still wrote,
//! and a grant that answered without writing, both fail here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the buckets, the roles and
/// the functions belong to. `us-east-1`, so a bucket create names no location constraint.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes: the account's own credential, which no
/// identity-based document bounds, so every fixture call is made with it and no row's own
/// ceiling decides the setup.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// Where Lambda binds the function collection and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below runs, and the handler its inline source is addressed by.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The API versions the two awsQuery services here are called at.
const IAM_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// The code Amazon S3 refuses a request no document in force allows with, at the status it
/// carries it at (that service's "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const FORBIDDEN: i64 = 403;

/// What an SDK reports as the CODE of a refused `HeadBucket`: a HEAD answer carries no body —
/// "a message body isn't included, so you can't determine the exception beyond these HTTP
/// response codes" (`HeadBucket`'s own documentation) — so the status itself is the code.
const FORBIDDEN_CODE: &str = "403";

/// The clause IAM's denial sentence carries for a request the principal's permissions boundary
/// merely states nothing about, and the one it carries where the boundary DENIES it.
fn no_boundary_allows(action: &str) -> String {
    format!("because no permissions boundary allows the {action} action")
}
const BOUNDARY_EXPLICIT_DENY: &str = "with an explicit deny in a permissions boundary";

/// The IAM actions the handler's calls are judged under: the write, the read, the bucket
/// listing a HEAD of the bucket needs ("to use this operation, you must have permissions to
/// perform the `s3:ListBucket` action", `HeadBucket`), and the ACCOUNT's own bucket listing,
/// which addresses no bucket of its own ("to grant IAM permission to use this operation, you
/// must add the `s3:ListAllMyBuckets` policy action", `ListBuckets`).
const PUT_ACTION: &str = "s3:PutObject";
const GET_ACTION: &str = "s3:GetObject";
const LIST_BUCKET_ACTION: &str = "s3:ListBucket";
const LIST_ALL_MY_BUCKETS_ACTION: &str = "s3:ListAllMyBuckets";

/// The log actions every Lambda execution role carries whatever a row withholds.
const LOG_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// How long this suite waits for work the clock owes before giving up, and how often it looks.
/// Nothing here tests how FAST a provisioning window closes, so the deadlines are generous: the
/// condition is polled for, never slept through.
const WAIT: Duration = Duration::from_secs(180);
const LOOK: Duration = Duration::from_millis(50);

/// The timeout every function below is created with. Nothing here tests a function's own
/// timeout, so it is given generous room: a run shares this process with every other test.
const FUNCTION_TIMEOUT: u64 = 120;

/// The status Lambda answers an invocation this process did not run at all with
/// (`TooManyRequestsException`, `@httpError(429)`): decided by what else is running at that
/// instant and by nothing a row varies, so a probe looks again rather than reading a verdict
/// out of an invocation that never happened.
const UNSERVED: u16 = 429;

/// The `StackStatus` values a stack operation of this suite must settle on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The suffix of every status CloudFormation reports for an operation still in flight.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The handler every row runs: `import boto3` at module scope, then exactly one S3 call per
/// invocation, reporting what it answered or the refusal the service raised — the code, the
/// sentence and the status, which is what a client branches on.
const PROBE_HANDLER: &str = r#"
import boto3
from botocore.exceptions import ClientError

S3 = boto3.client("s3")


def handler(event, context):
    bucket = event["bucket"]
    key = event.get("key", "")
    try:
        action = event["action"]
        if action == "put":
            S3.put_object(Bucket=bucket, Key=key, Body=event["content"].encode("utf-8"))
            return {"ok": True}
        if action == "get":
            answered = S3.get_object(Bucket=bucket, Key=key)
            return {"ok": True, "body": answered["Body"].read().decode("utf-8")}
        if action == "head-bucket":
            answered = S3.head_bucket(Bucket=bucket)
            return {"ok": True, "region": answered.get("BucketRegion", "")}
        if action == "list-buckets":
            answered = S3.list_buckets()
            return {
                "ok": True,
                "names": sorted(
                    entry["Name"] for entry in answered.get("Buckets", [])
                ),
            }
        return {"ok": False, "error": "UnsupportedAction"}
    except ClientError as refused:
        return {
            "ok": False,
            "error": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"#;

/// Which of the suite's two buckets a probe acts on. Both are created before any document
/// exists, so nothing but a pattern can authorize a call on either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    Named,
    Other,
}

/// The S3 call a probe's handler makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Put,
    Get,
    HeadBucket,
    ListBuckets,
}

impl Op {
    /// The `action` member the handler branches on.
    fn wire(self) -> &'static str {
        match self {
            Op::Put => "put",
            Op::Get => "get",
            Op::HeadBucket => "head-bucket",
            Op::ListBuckets => "list-buckets",
        }
    }

    /// The IAM action the call is judged under, which is the action the refusal names.
    fn iam_action(self) -> &'static str {
        match self {
            Op::Put => PUT_ACTION,
            Op::Get => GET_ACTION,
            Op::HeadBucket => LIST_BUCKET_ACTION,
            Op::ListBuckets => LIST_ALL_MY_BUCKETS_ACTION,
        }
    }
}

/// What a probe must be answered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expected {
    /// Served: the ceiling and the identity's documents both allow it.
    Served,
    /// Refused because the ceiling states nothing about the request.
    NoBoundaryAllows,
    /// Refused because the ceiling DENIES the request.
    BoundaryDenies,
}

/// One probe of a row's ceiling: the bucket and the call the handler is asked for, and what the
/// policies in force answer it with.
struct Probe {
    case: &'static str,
    which: Which,
    op: Op,
    expected: Expected,
}

/// One row: the ceiling put on the role, and the probes asked of it.
struct Row {
    /// The slug the row's boundary policy is named by, and the key its probes write under.
    slug: &'static str,
    /// The boundary document, rendered against the suite's two buckets, or `None` for a role
    /// carrying NO boundary at all.
    boundary: Option<fn(&str, &str) -> Value>,
    probes: &'static [Probe],
}

/// Every statement of the ceiling the row called `slug` puts on the role.
fn rows() -> Vec<Row> {
    vec![
        // No ceiling at all: the identity's own document decides alone, which is the position
        // every other row is a departure from.
        Row {
            slug: "none",
            boundary: None,
            probes: &[
                Probe {
                    case: "with no boundary the role's own grant writes",
                    which: Which::Named,
                    op: Op::Put,
                    expected: Expected::Served,
                },
                Probe {
                    case: "with no boundary the role's own grant reads",
                    which: Which::Named,
                    op: Op::Get,
                    expected: Expected::Served,
                },
            ],
        },
        // A ceiling as wide as the grant: the intersection is the grant itself.
        Row {
            slug: "wide",
            boundary: Some(|_, _| {
                json!([{"Effect": "Allow", "Action": "*", "Resource": "*"}])
            }),
            probes: &[
                Probe {
                    case: "a boundary allowing every action admits the write",
                    which: Which::Named,
                    op: Op::Put,
                    expected: Expected::Served,
                },
                Probe {
                    case: "a boundary allowing every action admits the bucket HEAD",
                    which: Which::Named,
                    op: Op::HeadBucket,
                    expected: Expected::Served,
                },
                Probe {
                    case: "a boundary allowing every action admits the account's listing",
                    which: Which::Named,
                    op: Op::ListBuckets,
                    expected: Expected::Served,
                },
            ],
        },
        // A ceiling that names only ANOTHER service's actions: the intersection over S3 is
        // empty, so every S3 call the role is granted is capped away.
        Row {
            slug: "logs-only",
            boundary: Some(|_, _| {
                json!([{"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"}])
            }),
            probes: &[
                Probe {
                    case: "a boundary excluding the object store caps the write",
                    which: Which::Named,
                    op: Op::Put,
                    expected: Expected::NoBoundaryAllows,
                },
                Probe {
                    case: "a boundary excluding the object store caps the bucket HEAD",
                    which: Which::Named,
                    op: Op::HeadBucket,
                    expected: Expected::NoBoundaryAllows,
                },
                Probe {
                    case: "a boundary excluding the object store caps the account's listing",
                    which: Which::Named,
                    op: Op::ListBuckets,
                    expected: Expected::NoBoundaryAllows,
                },
            ],
        },
        // The same actions, on another bucket's ARNs: a ceiling is a pattern over RESOURCES
        // too, so it caps the bucket it does not name and leaves the one it does.
        Row {
            slug: "other-bucket",
            boundary: Some(|_, other| {
                json!([{
                    "Effect": "Allow",
                    "Action": "s3:*",
                    "Resource": [
                        format!("arn:aws:s3:::{other}"),
                        format!("arn:aws:s3:::{other}/*"),
                    ],
                }])
            }),
            probes: &[
                Probe {
                    case: "a boundary scoped to another bucket caps this bucket's write",
                    which: Which::Named,
                    op: Op::Put,
                    expected: Expected::NoBoundaryAllows,
                },
                Probe {
                    case: "the bucket the same boundary names is written",
                    which: Which::Other,
                    op: Op::Put,
                    expected: Expected::Served,
                },
            ],
        },
        // A ceiling that allows everything and DENIES one action: the deny is a fact about the
        // boundary, so the refusal names it rather than the absence of a grant.
        Row {
            slug: "deny-write",
            boundary: Some(|_, _| {
                json!([
                    {"Effect": "Allow", "Action": "*", "Resource": "*"},
                    {"Effect": "Deny", "Action": PUT_ACTION, "Resource": "*"},
                ])
            }),
            probes: &[
                Probe {
                    case: "a boundary denying the write refuses it",
                    which: Which::Named,
                    op: Op::Put,
                    expected: Expected::BoundaryDenies,
                },
                Probe {
                    case: "the same boundary leaves the read alone",
                    which: Which::Named,
                    op: Op::Get,
                    expected: Expected::Served,
                },
            ],
        },
        // A ceiling that admits ONE of the two actions asked for: the intersection is taken per
        // action, not per service.
        Row {
            slug: "read-only",
            boundary: Some(|_, _| {
                json!([{"Effect": "Allow", "Action": GET_ACTION, "Resource": "*"}])
            }),
            probes: &[
                Probe {
                    case: "a read-only boundary admits the read",
                    which: Which::Named,
                    op: Op::Get,
                    expected: Expected::Served,
                },
                Probe {
                    case: "a read-only boundary caps the write",
                    which: Which::Named,
                    op: Op::Put,
                    expected: Expected::NoBoundaryAllows,
                },
            ],
        },
    ]
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the bucket it writes is the bucket this suite created and the
/// documents deciding it are the ones this suite put on the role.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite signs nothing, as in every other suite of this crate; the credential scope is
    // what routes a request and the access key id is what names its caller.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name` and whose credential is
/// the account's own published key.
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
    body: &[u8],
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_vec()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.body().to_vec())
}

/// One awsQuery call of `signing_name` that must answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = dispatch(
        router,
        signing_name,
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        body.as_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from {signing_name}, got {status} with {text}"
    );
    text
}

/// One S3 call on the path-style address, made as the account's own credential — the buckets'
/// owner, whose own calls no document here bounds.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
    dispatch(router, "s3", method, path, "application/octet-stream", body).await
}

/// Create one bucket of this account.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{bucket}"), b"").await;
    assert_eq!(
        status,
        200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
}

/// Write one object as the ACCOUNT's own credential, which a probe's own write cannot be
/// confused with.
async fn plant_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str, body: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{bucket}/{key}"), body.as_bytes()).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: planting {key:?} in {bucket:?} must succeed, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
}

/// The bytes the bucket holds under `key`, read back through S3's own `GetObject` as the
/// account's own credential, or `None` where it holds no object there.
async fn held_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{bucket}/{key}"), b"").await;
    let text = String::from_utf8_lossy(&body).into_owned();
    match status {
        200 => Some(text),
        403 | 404 => None,
        other => panic!(
            "case {case:?}: reading {bucket:?}/{key:?} back must answer 200, 403 or 404, got \
             {other} with {text}"
        ),
    }
}

/// The trust policy every execution role here carries: AWS Lambda assumes it to run the
/// function.
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

/// The document every role here carries as its OWN grant: the log writes, plus every action of
/// the object store on every resource — so the only thing that can refuse a probe is the
/// ceiling over it.
fn broad_grant() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"},
            {"Effect": "Allow", "Action": "s3:*", "Resource": "*"},
        ],
    })
    .to_string()
}

/// One customer managed policy, created to be used as a boundary; answers its ARN.
async fn given_managed_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    statements: Value,
) -> String {
    let document = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
    let created = query(
        router,
        case,
        "iam",
        IAM_VERSION,
        "CreatePolicy",
        &[("PolicyName", name), ("PolicyDocument", &document)],
    )
    .await;
    element(&created, "Arn")
}

/// Create the execution role `name`, trusted by Lambda and carrying [`broad_grant`]; answers
/// its ARN.
async fn given_execution_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let trust = trust_policy();
    let created = query(
        router,
        case,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let document = broad_grant();
    query(
        router,
        case,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "in-force"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Put `wanted` on the role as its permissions boundary, through the IAM call that moves the
/// ceiling of an existing role: `PutRolePermissionsBoundary` for a row that declares one, and
/// the call that removes it for a row that declares none over a role still carrying one.
/// `in_force` is the ceiling the role carries now, so nothing is called where the two agree.
async fn set_boundary(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    in_force: Option<&str>,
    wanted: Option<&str>,
) {
    if in_force == wanted {
        return;
    }
    match wanted {
        Some(arn) => {
            query(
                router,
                case,
                "iam",
                IAM_VERSION,
                "PutRolePermissionsBoundary",
                &[("RoleName", role), ("PermissionsBoundary", arn)],
            )
            .await;
        }
        None => {
            query(
                router,
                case,
                "iam",
                IAM_VERSION,
                "DeleteRolePermissionsBoundary",
                &[("RoleName", role)],
            )
            .await;
        }
    }
}

/// The boundary ARN IAM reports for `role`, which is what `GetRole` answers under
/// `PermissionsBoundary.PermissionsBoundaryArn`, or `None` for a role carrying none.
async fn reported_boundary(router: &MultiServiceRouter, case: &str, role: &str) -> Option<String> {
    let read = query(
        router,
        case,
        "iam",
        IAM_VERSION,
        "GetRole",
        &[("RoleName", role)],
    )
    .await;
    elements(&read, "PermissionsBoundaryArn").into_iter().next()
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    let parsed = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answered).into_owned()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// Deploy one function running [`PROBE_HANDLER`] as the role `role_arn` names, through a stack
/// of its own — CloudFormation archives the inline source into the deployment package, which
/// is how a function whose handler is written out in a suite comes to exist — and wait until
/// Lambda reports it ready to be invoked.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    name: &str,
    role_arn: &str,
) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            STACK_FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
    })
    .to_string();
    query(
        router,
        case,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    settled_on(router, case, stack, CREATE_COMPLETE).await;
    await_active(router, case, name).await;
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function ready to be invoked.
/// The window a function spends coming up is that service's own, so no fixed sleep names the
/// instant it ends.
async fn await_active(router: &MultiServiceRouter, case: &str, function: &str) {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let (status, _, reported) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetFunctionConfiguration({function:?}) must answer 200, got \
             {status} with {reported}"
        );
        if reported["State"].as_str() == Some("Active")
            && reported["LastUpdateStatus"].as_str() != Some("InProgress")
        {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: Lambda must report {function:?} ready to invoke, last saw {reported}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Invoke `function` with `event` and answer the payload the handler returned: the invocation
/// must be served and the run must complete, whatever the handler's own report says, because an
/// authorization refusal is the handler's report and not a failed invocation.
async fn invoke(router: &MultiServiceRouter, case: &str, function: &str, event: &Value) -> Value {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let (status, headers, payload) = lambda(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            event.to_string().as_bytes(),
        )
        .await;
        if status == UNSERVED {
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: an invocation of {function:?} must be served, last answered \
                 {status} with {payload}"
            );
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation must answer HTTP 200, got {status} with {payload}"
        );
        let faulted = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            faulted, None,
            "case {case:?}: the handler must run to completion, got the function-error signal \
             {faulted:?} with {payload}"
        );
        return payload;
    }
}

/// Assert `payload` is what the policies in force answer `op` with. `resource` is the ARN the
/// request addressed, and `None` for an action that addresses no resource of its own — the
/// account's own bucket listing, whose refusal therefore names none either.
fn assert_probe(
    case: &str,
    op: Op,
    resource: Option<&str>,
    expected: Expected,
    payload: &Value,
) {
    match expected {
        Expected::Served => {
            assert_eq!(
                payload["ok"],
                json!(true),
                "case {case:?}: expected the call to be served, got {payload}"
            );
        }
        Expected::NoBoundaryAllows | Expected::BoundaryDenies => {
            assert_eq!(
                payload["ok"],
                json!(false),
                "case {case:?}: expected the ceiling to refuse the call, got {payload}"
            );
            assert_eq!(
                payload["status"],
                json!(FORBIDDEN),
                "case {case:?}: expected the refusal at HTTP {FORBIDDEN}, got {payload}"
            );
            // A HEAD carries no body, so an SDK has no code or sentence to read out of one and
            // reports the status as the code; every other refusal carries S3's own code and
            // IAM's sentence, whose clause is what names the boundary as the document that
            // withheld the grant.
            if op == Op::HeadBucket {
                assert_eq!(
                    payload["error"],
                    json!(FORBIDDEN_CODE),
                    "case {case:?}: expected a HEAD refusal reported by its status, got {payload}"
                );
                return;
            }
            assert_eq!(
                payload["error"],
                json!(ACCESS_DENIED),
                "case {case:?}: expected the service's own refusal code, got {payload}"
            );
            let clause = match expected {
                Expected::BoundaryDenies => BOUNDARY_EXPLICIT_DENY.to_string(),
                _ => no_boundary_allows(op.iam_action()),
            };
            let message = payload["message"].as_str().unwrap_or_default();
            assert!(
                message.ends_with(&clause),
                "case {case:?}: expected the refusal to end with {clause:?}, got {message:?}"
            );
            assert!(
                message.contains(op.iam_action()),
                "case {case:?}: expected the refusal to name {:?}, got {message:?}",
                op.iam_action()
            );
            match resource {
                Some(resource) => assert!(
                    message.contains(resource),
                    "case {case:?}: expected the refusal to name {resource:?}, got {message:?}"
                ),
                // An action that addresses no resource of its own has nothing for the `on
                // resource:` clause to carry, so the sentence carries no such clause at all.
                None => assert!(
                    !message.contains("on resource:"),
                    "case {case:?}: expected the refusal of an account-wide action to name no \
                     resource, got {message:?}"
                ),
            }
        }
    }
}

/// The two buckets every row probes.
fn buckets() -> (String, String) {
    (
        "xsvc-fn-s3-boundary-named".to_string(),
        "xsvc-fn-s3-boundary-other".to_string(),
    )
}

/// A permissions boundary is the CEILING on what the execution role's own document grants: the
/// S3 call a handler makes is served only where both allow it, and the refusal of one the
/// ceiling withholds names the boundary.
#[tokio::test]
async fn a_permissions_boundary_caps_the_execution_roles_own_grant() {
    let router = build_router();
    let setup = "the role, the ceilings and the function every row probes";
    let (named, other) = buckets();
    // Both buckets exist before any document does, so no document was written around them.
    given_bucket(&router, setup, &named).await;
    given_bucket(&router, setup, &other).await;

    let role = "xsvc-fn-s3-boundary-role";
    let function = "xsvc-fn-s3-boundary-probe";
    let role_arn = given_execution_role(&router, setup, role).await;
    given_function(
        &router,
        setup,
        "xsvc-fn-s3-boundary-probe-stack",
        function,
        &role_arn,
    )
    .await;

    let rows = rows();
    // One managed policy per row that declares a ceiling, created up front: the ceiling in
    // force is swapped on the ROLE between invocations, which is where a boundary lives.
    let mut ceilings = Vec::new();
    for row in &rows {
        let arn = match row.boundary {
            Some(statements) => Some(
                given_managed_policy(
                    &router,
                    setup,
                    &format!("xsvc-fn-s3-ceiling-{}", row.slug),
                    statements(&named, &other),
                )
                .await,
            ),
            None => None,
        };
        ceilings.push(arn);
    }

    let mut in_force: Option<String> = None;
    for (index, row) in rows.iter().enumerate() {
        let ceiling = ceilings[index].clone();
        set_boundary(
            &router,
            row.slug,
            role,
            in_force.as_deref(),
            ceiling.as_deref(),
        )
        .await;
        in_force = ceiling.clone();
        let reported = reported_boundary(&router, row.slug, role).await;
        assert_eq!(
            reported, ceiling,
            "case {:?}: expected IAM to report the ceiling {ceiling:?} on {role:?}, got \
             {reported:?}",
            row.slug
        );

        for (probe_index, probe) in row.probes.iter().enumerate() {
            let case = probe.case;
            let bucket = match probe.which {
                Which::Named => &named,
                Which::Other => &other,
            };
            let key = format!("{}-{probe_index}.txt", row.slug);
            let written = format!("{}-{probe_index}", row.slug);
            // What the request ADDRESSES, which is what its refusal names: the object for the
            // object calls, the bucket for the HEAD of it, and no resource at all for the
            // account's own listing.
            let resource = match probe.op {
                Op::HeadBucket => Some(format!("arn:aws:s3:::{bucket}")),
                Op::ListBuckets => None,
                _ => Some(format!("arn:aws:s3:::{bucket}/{key}")),
            };

            // A read needs something to read; a write must not find one already there.
            if probe.op == Op::Get {
                plant_object(&router, case, bucket, &key, &written).await;
            }

            let mut event = json!({"action": probe.op.wire(), "bucket": bucket, "key": key});
            if probe.op == Op::Put {
                event["content"] = json!(written);
            }
            let payload = invoke(&router, case, function, &event).await;
            assert_probe(case, probe.op, resource.as_deref(), probe.expected, &payload);

            // What a SERVED call answered, beside the report: the bytes the read handed back,
            // the Region the HEAD reported for the bucket it resolved, and the buckets the
            // account's listing named — held against what this account actually holds.
            if probe.expected == Expected::Served {
                match probe.op {
                    Op::Get => assert_eq!(
                        payload["body"],
                        json!(written),
                        "case {case:?}: expected the planted bytes, got {payload}"
                    ),
                    Op::HeadBucket => assert_eq!(
                        payload["region"],
                        json!(REGION),
                        "case {case:?}: expected the bucket's own Region, got {payload}"
                    ),
                    Op::ListBuckets => {
                        let named_by_the_listing =
                            payload["names"].as_array().cloned().unwrap_or_default();
                        for held in [&named, &other] {
                            assert!(
                                named_by_the_listing.contains(&json!(held)),
                                "case {case:?}: expected the listing to name {held:?}, got \
                                 {payload}"
                            );
                        }
                    }
                    Op::Put => {}
                }
            }

            // And what the BUCKET holds, read back by the account's own credential: a served
            // write is there and a capped one left nothing behind.
            if probe.op == Op::Put {
                let held = held_object(&router, case, bucket, &key).await;
                let expected = (probe.expected == Expected::Served).then(|| written.clone());
                assert_eq!(
                    held, expected,
                    "case {case:?}: expected {bucket:?}/{key:?} to hold {expected:?} \
                     afterwards, got {held:?}"
                );
            }
        }
    }
}

/// The logical ids and outputs the stack of the deploy-then-update arm declares.
const CEILING: &str = "NarrowCeiling";
const WIDE_CEILING: &str = "WideCeiling";
const STACK_ROLE: &str = "ExecRole";
const STACK_FUNCTION: &str = "Probe";
const FUNCTION_OUTPUT: &str = "FunctionName";
const ROLE_OUTPUT: &str = "RoleName";
const NARROW_OUTPUT: &str = "NarrowArn";
const WIDE_OUTPUT: &str = "WideArn";

/// The template of that arm: two managed policies to use as ceilings, a role carrying one of
/// them and the broad grant, and the function that runs as it. `boundary` names which of the
/// two the role declares, or none at all.
fn boundary_template(stack: &str, boundary: Option<&str>) -> String {
    let mut role = json!({
        "RoleName": format!("{stack}-role"),
        "AssumeRolePolicyDocument": serde_json::from_str::<Value>(&trust_policy())
            .expect("the trust policy is a document"),
        "Policies": [{
            "PolicyName": "in-force",
            "PolicyDocument": serde_json::from_str::<Value>(&broad_grant())
                .expect("the grant is a document"),
        }],
    });
    if let Some(logical) = boundary {
        role["PermissionsBoundary"] = json!({"Ref": logical});
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            CEILING: {
                "Type": "AWS::IAM::ManagedPolicy",
                "Properties": {
                    "ManagedPolicyName": format!("{stack}-narrow"),
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [
                            {"Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*"},
                        ],
                    },
                },
            },
            WIDE_CEILING: {
                "Type": "AWS::IAM::ManagedPolicy",
                "Properties": {
                    "ManagedPolicyName": format!("{stack}-wide"),
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{"Effect": "Allow", "Action": "*", "Resource": "*"}],
                    },
                },
            },
            STACK_ROLE: {"Type": "AWS::IAM::Role", "Properties": role},
            STACK_FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{stack}-probe"),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [STACK_ROLE, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
        "Outputs": {
            FUNCTION_OUTPUT: {"Value": {"Ref": STACK_FUNCTION}},
            ROLE_OUTPUT: {"Value": {"Ref": STACK_ROLE}},
            NARROW_OUTPUT: {"Value": {"Ref": CEILING}},
            WIDE_OUTPUT: {"Value": {"Ref": WIDE_CEILING}},
        },
    })
    .to_string()
}

/// The members every stack operation of that arm sends: the body and the two capabilities a
/// template declaring NAMED IAM resources needs acknowledged.
fn stack_members<'a>(stack: &'a str, body: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("StackName", stack),
        ("TemplateBody", body),
        ("Capabilities.member.1", "CAPABILITY_IAM"),
        ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
    ]
}

/// Poll `DescribeStacks` the way a client's waiter does and assert the stack settled on
/// `wanted`; answers the answer it settled in.
async fn settled_on(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let described = query(
            router,
            case,
            "cloudformation",
            CLOUDFORMATION_VERSION,
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with(IN_PROGRESS) {
            if reported != wanted {
                let events = query(
                    router,
                    case,
                    "cloudformation",
                    CLOUDFORMATION_VERSION,
                    "DescribeStackEvents",
                    &[("StackName", stack)],
                )
                .await;
                panic!(
                    "case {case:?}: expected {stack} to settle on {wanted}, got {reported} with \
                     resource failures {:?}",
                    elements(&events, "ResourceStatusReason")
                );
            }
            return described;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack} to reach a terminal status, last saw {reported:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// A ceiling a CloudFormation template declares on an `AWS::IAM::Role` is IN FORCE on the role
/// the stack created — the deployed function's own S3 call is capped by it — and a stack update
/// that points the role at another ceiling, or at none, moves it.
#[tokio::test]
async fn a_stack_declared_ceiling_is_in_force_on_the_role_it_created() {
    let case = "a stack declaring a ceiling on its Lambda execution role";
    let router = build_router();
    let stack = "xsvc-fn-s3-boundary-stack";
    let bucket = "xsvc-fn-s3-boundary-stack-store";
    given_bucket(&router, case, bucket).await;

    let created = boundary_template(stack, Some(CEILING));
    query(
        &router,
        case,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        "CreateStack",
        &stack_members(stack, &created),
    )
    .await;
    let described = settled_on(&router, case, stack, CREATE_COMPLETE).await;
    let outputs = outputs_of(&described);
    let function = outputs
        .get(FUNCTION_OUTPUT)
        .unwrap_or_else(|| panic!("case {case:?}: no {FUNCTION_OUTPUT} output in {outputs:?}"))
        .clone();
    let role = outputs
        .get(ROLE_OUTPUT)
        .unwrap_or_else(|| panic!("case {case:?}: no {ROLE_OUTPUT} output in {outputs:?}"))
        .clone();
    let narrow = outputs
        .get(NARROW_OUTPUT)
        .unwrap_or_else(|| panic!("case {case:?}: no {NARROW_OUTPUT} output in {outputs:?}"))
        .clone();
    let wide = outputs
        .get(WIDE_OUTPUT)
        .unwrap_or_else(|| panic!("case {case:?}: no {WIDE_OUTPUT} output in {outputs:?}"))
        .clone();

    // The ceiling the template declared is the ceiling IAM holds for the role the stack
    // created, read where a client reads it.
    let reported = reported_boundary(&router, case, &role).await;
    assert_eq!(
        reported,
        Some(narrow.clone()),
        "case {case:?}: expected the declared ceiling {narrow:?} in force on {role:?}, got \
         {reported:?}"
    );

    // ... and it decides the deployed function's own S3 call: the role's document allows the
    // write, the ceiling says nothing about it, so it is refused naming the boundary.
    await_active(&router, case, &function).await;
    let key = "stack-declared.txt";
    let event = json!({
        "action": "put",
        "bucket": bucket,
        "key": key,
        "content": "under-the-narrow-ceiling",
    });
    let payload = invoke(&router, case, &function, &event).await;
    assert_probe(
        &format!("{case} (as created)"),
        Op::Put,
        Some(&format!("arn:aws:s3:::{bucket}/{key}")),
        Expected::NoBoundaryAllows,
        &payload,
    );
    let held = held_object(&router, case, bucket, key).await;
    assert_eq!(
        held, None,
        "case {case:?}: a capped write must leave the bucket as it was, got {held:?}"
    );

    // The same template with the role pointed at the WIDER ceiling: the update moves the
    // boundary, and the next call the handler makes is served.
    let widened = boundary_template(stack, Some(WIDE_CEILING));
    query(
        &router,
        case,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        "UpdateStack",
        &stack_members(stack, &widened),
    )
    .await;
    settled_on(&router, case, stack, UPDATE_COMPLETE).await;
    let reported = reported_boundary(&router, case, &role).await;
    assert_eq!(
        reported,
        Some(wide.clone()),
        "case {case:?}: expected the update to put {wide:?} in force on {role:?}, got \
         {reported:?}"
    );

    await_active(&router, case, &function).await;
    let payload = invoke(&router, case, &function, &event).await;
    assert_probe(
        &format!("{case} (after the update)"),
        Op::Put,
        Some(&format!("arn:aws:s3:::{bucket}/{key}")),
        Expected::Served,
        &payload,
    );
    let held = held_object(&router, case, bucket, key).await;
    assert_eq!(
        held,
        Some("under-the-narrow-ceiling".to_string()),
        "case {case:?}: expected the served write in the bucket, got {held:?}"
    );

    // And a template that declares NO boundary at all leaves the role carrying none.
    let uncapped = boundary_template(stack, None);
    query(
        &router,
        case,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        "UpdateStack",
        &stack_members(stack, &uncapped),
    )
    .await;
    settled_on(&router, case, stack, UPDATE_COMPLETE).await;
    let reported = reported_boundary(&router, case, &role).await;
    assert_eq!(
        reported, None,
        "case {case:?}: expected the role to carry no ceiling once the template declares \
         none, got {reported:?}"
    );
}
