// SPDX-License-Identifier: CC0-1.0

//! WHAT an execution-role statement whose `Action` or `Resource` is a WILDCARD authorizes for the
//! S3 calls a running `python3.12` handler makes with the SDK its runtime supplies.
//!
//! RULE: an identity-based statement authorizes a request when the request's action matches one of
//! the statement's `Action` patterns and the request's resource matches one of its `Resource`
//! patterns, where `*` in a pattern stands for any sequence of characters — so a statement is
//! evaluated against the PATTERN it carries and not against the resources that happened to exist
//! when it was written: `{"Action":"s3:*","Resource":"*"}` on a function's execution role
//! authorizes every S3 action the handler calls on every bucket of the account, including buckets
//! created independently of the role and never named by it, while a statement naming one bucket's
//! ARNs authorizes only that bucket's requests and leaves another bucket's refused. Either way the
//! handler's own return value is the invocation's payload: HTTP 200 with no `X-Amz-Function-Error`
//! and the report the handler built, so a refusal arrives as the handler's own `{"ok": false, …}`
//! document rather than as a failed invocation a poll-until-success loop can never leave.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and `InvocationResponse$FunctionError` is bound to the response
//! header `X-Amz-Function-Error` with `StatusCode` bound to the HTTP status
//! (`smithy_query(query="operation",operation="Invoke")` on the lambda model), so an invocation
//! whose handler ran to completion carries neither signal. A request of the resource owner's own
//! account is allowed when an identity-based policy allows it, and denied with neither an
//! identity-based nor a resource-based allow (IAM User Guide, *Policy evaluation logic*); `*`
//! matching any sequence of characters in an `Action` or `Resource` element is that guide's
//! *Wildcards* rule.
//!
//! Five crates have to agree for that to hold — CloudFormation archives each inline handler into a
//! deployment package and deploys the functions, IAM holds each role and its inline document, the
//! token service vends the session the handler's calls are signed as, Lambda runs the handler and
//! supplies it the SDK, and Amazon S3 takes the decision and holds what arrived — so the suite
//! lives here, where a crate-local one could only see one side of it. What the neighbours pin and
//! this one does not repeat: `lambda_python_runtime_bundles_the_aws_sdk` pins that every managed
//! Python runtime supplies the SDK at all; the neighbouring suite on objects and events tables the
//! import FORM crossed with per-action grants over ONE bucket; and the Lambda crate's own
//! `handler_s3_wildcard_grant` owns the WORDING of the refusal and the explicit-deny override for
//! the write and the read of a role whose document is rewritten between invocations. What only
//! this suite reaches is the whole deploy-then-invoke arm of the same rule: a function reached
//! through the ASSEMBLED router after a stack deployed it, asked for the object operations the
//! neighbours do not call (the delete and the listing) as well as the write and the read, and
//! under a `Resource` spelled as the object-ARN wildcard — so a grant honoured only for the two
//! operations a crate-local table drives, or only on a role written before the buckets existed,
//! fails here.
//!
//! The rows differ along what the rule generalises over: the breadth of the statement's `Action`
//! (every action of the object store, every action of every service, a prefix wildcard over a
//! family of actions) crossed with the breadth of its `Resource` (every resource, every resource
//! of the object store, the ARNs of ONE bucket) — and, within each row, which of the row's two
//! independently created buckets the handler acts on and which object operation it calls, so a
//! grant is observed per action and per bucket rather than per role. Each probe asserts both
//! halves: what the handler reported, and what the bucket holds afterwards read back by the
//! account's own credential — so a refusal that still wrote, and a grant that answered without
//! writing, both fail here.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request here is signed for, and so the Region the buckets, the roles and the
/// functions belong to. `us-east-1`, so a bucket create names no location constraint.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes: the account's own credential, which no
/// identity-based document bounds, so every fixture call is made with it and no row's own grant
/// decides the setup.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// Where Lambda binds the function collection and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below is created with, and the handler CloudFormation's inline
/// source is addressed by.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The one stack that deploys every row's function.
const STACK: &str = "xsvc-in-fn-s3-wildcard-grants";

/// The `StackStatus` that stack must reach, and the `State` a function serves invocations in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const FUNCTION_ACTIVE: &str = "Active";

/// The error code Amazon S3 refuses a request no document in force allows with.
const ACCESS_DENIED: &str = "AccessDenied";

/// How long this suite waits for work the clock owes before giving up, and how often it looks.
/// Nothing here tests how FAST a provisioning window closes, so the deadline is generous: the
/// condition is polled for, never slept through.
const WAIT: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The timeout every function below is created with. Nothing here tests a function's own timeout,
/// so it is given generous room: a run shares this process with every other test of this target.
const FUNCTION_TIMEOUT: u64 = 120;

/// The handler every row runs: `import boto3` at MODULE SCOPE and the client built there too —
/// which is where a runtime without the SDK fails to import at all — then exactly one object call
/// per invocation, reporting what it answered or the refusal the service raised. The report shape
/// is the handler's own: a served call is `{"ok": true}` (with the bytes read for a read), and a
/// refused one is `{"ok": false, "error": <the service's code>}`.
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
            S3.put_object(
                Bucket=bucket, Key=key, Body=event["content"].encode("utf-8")
            )
            return {"ok": True}
        if action == "get":
            answered = S3.get_object(Bucket=bucket, Key=key)
            return {"ok": True, "body": answered["Body"].read().decode("utf-8")}
        if action == "delete":
            S3.delete_object(Bucket=bucket, Key=key)
            return {"ok": True}
        if action == "list":
            answered = S3.list_objects_v2(Bucket=bucket)
            return {
                "ok": True,
                "keys": sorted(item["Key"] for item in answered.get("Contents", [])),
            }
        return {"ok": False, "error": "UnsupportedAction"}
    except ClientError as refused:
        return {"ok": False, "error": refused.response["Error"]["Code"]}
"#;

/// Which of a row's two independently created buckets a probe acts on. Neither is named by any
/// row's document except the one row whose statement deliberately names the FIRST bucket's ARNs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    First,
    Second,
}

/// The object operation a probe's handler calls, and the action a statement names it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Put,
    Get,
    Delete,
    List,
}

impl Op {
    /// The `action` member the handler branches on.
    fn wire(self) -> &'static str {
        match self {
            Op::Put => "put",
            Op::Get => "get",
            Op::Delete => "delete",
            Op::List => "list",
        }
    }
}

/// The breadth of the statement's `Resource` element, rendered against the row's own buckets.
#[derive(Clone, Copy, Debug)]
enum Scope {
    /// `"*"` — every resource of every service.
    Every,
    /// Every resource of the object store, bucket and object alike, by wildcard ARN.
    EveryOfTheService,
    /// Only the FIRST bucket of the row, named by its own two ARNs.
    OnlyTheFirstBucket,
}

impl Scope {
    fn rendered(self, first: &str) -> Value {
        match self {
            Scope::Every => json!("*"),
            Scope::EveryOfTheService => json!(["arn:aws:s3:::*", "arn:aws:s3:::*/*"]),
            Scope::OnlyTheFirstBucket => json!([
                format!("arn:aws:s3:::{first}"),
                format!("arn:aws:s3:::{first}/*"),
            ]),
        }
    }
}

/// One probe of a row's role: the bucket and operation the handler is asked for, and whether the
/// document in force lets it through.
struct Probe {
    case: &'static str,
    which: Which,
    op: Op,
    allowed: bool,
}

/// One row: the statement the execution role carries, and the probes asked of it.
struct Row {
    /// The slug the row's role, function and buckets are named by.
    slug: &'static str,
    /// The `Action` element of the row's one statement, as the document carries it.
    action: fn() -> Value,
    /// The `Resource` element of that statement.
    scope: Scope,
    probes: &'static [Probe],
}

/// The rows: the breadth of the `Action` pattern crossed with the breadth of the `Resource`
/// pattern, each asked about BOTH of its buckets and about more than one action.
fn rows() -> Vec<Row> {
    vec![
        // Every action of the object store on every resource — the broadest grant there is, and
        // the one a client reaches for first. Both buckets are created independently of the role
        // and named nowhere in it, so nothing but the pattern can authorize these calls.
        Row {
            slug: "service-star",
            action: || json!("s3:*"),
            scope: Scope::Every,
            probes: &[
                Probe {
                    case: "s3:* on * writes into a bucket the document never names",
                    which: Which::First,
                    op: Op::Put,
                    allowed: true,
                },
                Probe {
                    case: "s3:* on * reads from the OTHER bucket of the account",
                    which: Which::Second,
                    op: Op::Get,
                    allowed: true,
                },
                Probe {
                    case: "s3:* on * lists the bucket it has just written",
                    which: Which::First,
                    op: Op::List,
                    allowed: true,
                },
                Probe {
                    case: "s3:* on * deletes from the other bucket as well",
                    which: Which::Second,
                    op: Op::Delete,
                    allowed: true,
                },
            ],
        },
        // Every action of EVERY service on every resource: the same breadth one level wider.
        Row {
            slug: "all-star",
            action: || json!("*"),
            scope: Scope::Every,
            probes: &[
                Probe {
                    case: "* on * writes into the second bucket",
                    which: Which::Second,
                    op: Op::Put,
                    allowed: true,
                },
                Probe {
                    case: "* on * reads from the first bucket",
                    which: Which::First,
                    op: Op::Get,
                    allowed: true,
                },
            ],
        },
        // Every action of the object store on every resource OF THE OBJECT STORE, by wildcard ARN
        // rather than by the bare star.
        Row {
            slug: "service-arns",
            action: || json!("s3:*"),
            scope: Scope::EveryOfTheService,
            probes: &[
                Probe {
                    case: "a wildcard bucket ARN writes into a bucket it never names",
                    which: Which::First,
                    op: Op::Put,
                    allowed: true,
                },
                Probe {
                    case: "a wildcard object ARN reads from the other bucket",
                    which: Which::Second,
                    op: Op::Get,
                    allowed: true,
                },
            ],
        },
        // A prefix wildcard over a FAMILY of actions: the pattern admits the families it spells
        // and no other, so the delete of the same objects is refused under it.
        Row {
            slug: "prefix-star",
            action: || json!(["s3:Get*", "s3:Put*", "s3:List*"]),
            scope: Scope::Every,
            probes: &[
                Probe {
                    case: "s3:Put* admits the write",
                    which: Which::First,
                    op: Op::Put,
                    allowed: true,
                },
                Probe {
                    case: "s3:Get* admits the read of the other bucket",
                    which: Which::Second,
                    op: Op::Get,
                    allowed: true,
                },
                Probe {
                    case: "no named family admits the delete",
                    which: Which::First,
                    op: Op::Delete,
                    allowed: false,
                },
            ],
        },
        // Every action of the object store, but only on ONE bucket's ARNs: the pattern is narrow,
        // so the broad action does not carry over to the account's other bucket.
        Row {
            slug: "one-bucket",
            action: || json!("s3:*"),
            scope: Scope::OnlyTheFirstBucket,
            probes: &[
                Probe {
                    case: "the named bucket's own write is admitted",
                    which: Which::First,
                    op: Op::Put,
                    allowed: true,
                },
                Probe {
                    case: "another bucket's write is not",
                    which: Which::Second,
                    op: Op::Put,
                    allowed: false,
                },
                Probe {
                    case: "another bucket's read is not either",
                    which: Which::Second,
                    op: Op::Get,
                    allowed: false,
                },
            ],
        },
    ]
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back into
/// this same router, so the bucket it writes is the bucket this suite created and the document
/// deciding it is the one this suite put on the role.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite signs nothing, as in every other suite of this crate; the credential scope is what
    // routes a request and the access key id is what names its caller.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name` and whose credential is the
/// account's own published key.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
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

/// Write one object as the ACCOUNT's own credential, which a probe's own write cannot be confused
/// with.
async fn plant_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    body: &str,
) {
    let (status, answered) = s3(router, "PUT", &format!("/{bucket}/{key}"), body.as_bytes()).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: planting {key:?} in {bucket:?} must succeed, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
}

/// The bytes the bucket holds under `key`, read back through S3's own `GetObject` as the account's
/// own credential, or `None` where it holds no object there.
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
        404 => None,
        other => panic!(
            "case {case:?}: reading {bucket:?}/{key:?} back must answer 200 or 404, got {other} \
             with {text}"
        ),
    }
}

/// Create the execution role `name`, trusted by Lambda — which `CreateFunction` checks — carrying
/// the log actions every execution role needs plus the row's own statement; answers its ARN.
async fn given_execution_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    action: Value,
    resource: Value,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = query(
        router,
        case,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {"Effect": "Allow", "Action": action, "Resource": resource},
        ],
    })
    .to_string();
    query(
        router,
        case,
        "iam",
        "2010-05-08",
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

/// How many looks fit in [`WAIT`] at [`LOOK`] apart.
fn looks() -> u32 {
    u32::try_from(WAIT.as_millis() / LOOK.as_millis().max(1)).unwrap_or(u32::MAX)
}

/// Deploy one function per `(name, role arn)` pair, each running [`PROBE_HANDLER`], and wait until
/// the stack has completed and Lambda reports every one of them active.
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to exist.
async fn given_functions(router: &MultiServiceRouter, case: &str, functions: &[(String, String)]) {
    let mut resources = serde_json::Map::new();
    for (index, (name, role_arn)) in functions.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            }),
        );
    }
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string();
    query(
        router,
        case,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    // The stack's completion and each function's provisioning window are owed by the clock, which
    // no fixed sleep names: this looks until they hold.
    let mut last = String::new();
    for _ in 0..looks() {
        last = query(
            router,
            case,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        if element(&last, "StackStatus") == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        element(&last, "StackStatus"),
        CREATE_COMPLETE,
        "case {case:?}: the stack {STACK:?} must reach {CREATE_COMPLETE}, last saw {last}"
    );
    for (name, _) in functions {
        let mut configuration = Value::Null;
        let mut active = false;
        for _ in 0..looks() {
            let (_, _, reported) = lambda(
                router,
                "GET",
                &format!("{FUNCTIONS_URI}/{name}/configuration"),
                &[],
            )
            .await;
            if reported["State"].as_str() == Some(FUNCTION_ACTIVE) {
                active = true;
                break;
            }
            configuration = reported;
            tokio::time::sleep(LOOK).await;
        }
        assert!(
            active,
            "case {case:?}: Lambda must report {name:?} {FUNCTION_ACTIVE}, last saw {configuration}"
        );
    }
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

/// Invoke `function` synchronously with `event`, and answer the payload the handler returned: the
/// invocation itself must be served and the run must complete, whatever the handler's own report
/// says, because an authorization refusal is the handler's report and not a failed invocation.
async fn invoke(router: &MultiServiceRouter, case: &str, function: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
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
    payload
}

/// A wildcard statement on a function's execution role is evaluated as a PATTERN: it authorizes the
/// handler's S3 calls on buckets the document never names, a narrower pattern leaves another
/// bucket's calls refused, and either outcome is the handler's own payload.
#[tokio::test]
async fn a_wildcard_execution_role_statement_authorizes_every_bucket_it_matches() {
    let router = build_router();
    let rows = rows();

    // Every row's buckets are created INDEPENDENTLY of its role, before any document exists, so no
    // document could have been written around them.
    let mut functions = Vec::new();
    for row in &rows {
        let setup = row.slug;
        let (first, second) = buckets_of(row.slug);
        given_bucket(&router, setup, &first).await;
        given_bucket(&router, setup, &second).await;
        let role_arn = given_execution_role(
            &router,
            setup,
            &format!("xsvc-in-fn-s3-{}", row.slug),
            (row.action)(),
            row.scope.rendered(&first),
        )
        .await;
        functions.push((function_of(row.slug), role_arn));
    }
    given_functions(&router, "the row functions", &functions).await;

    for (index, row) in rows.iter().enumerate() {
        let (first, second) = buckets_of(row.slug);
        let function = function_of(row.slug);
        for (probe_index, probe) in row.probes.iter().enumerate() {
            let case = probe.case;
            let bucket = match probe.which {
                Which::First => &first,
                Which::Second => &second,
            };
            let key = format!("probe-{index}-{probe_index}.txt");
            let written = format!("{}-{probe_index}", row.slug);

            // A read, a delete and a listing need something there first; a write must not find one.
            let planted = matches!(probe.op, Op::Get | Op::Delete | Op::List);
            if planted {
                plant_object(&router, case, bucket, &key, &written).await;
            }

            let mut event = json!({"action": probe.op.wire(), "bucket": bucket, "key": key});
            if probe.op == Op::Put {
                event["content"] = json!(written);
            }
            let payload = invoke(&router, case, &function, &event).await;

            // What the handler reported.
            if probe.allowed {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the call to be served, got {payload}"
                );
                match probe.op {
                    Op::Get => assert_eq!(
                        payload["body"],
                        json!(written),
                        "case {case:?}: expected the planted bytes, got {payload}"
                    ),
                    Op::List => assert!(
                        payload["keys"]
                            .as_array()
                            .is_some_and(|keys| keys.contains(&json!(key))),
                        "case {case:?}: expected the listing to name {key:?}, got {payload}"
                    ),
                    Op::Put | Op::Delete => {}
                }
            } else {
                assert_eq!(
                    payload,
                    json!({"ok": false, "error": ACCESS_DENIED}),
                    "case {case:?}: expected the handler to report the service's own refusal, got \
                     {payload}"
                );
            }

            // And what the BUCKET holds, read back by the account's own credential: the effect of a
            // served call is there, and a refused one left nothing behind.
            let held = held_object(&router, case, bucket, &key).await;
            let expected = match (probe.op, probe.allowed) {
                (Op::Put, true) => Some(written.clone()),
                (Op::Put, false) => None,
                (Op::Delete, true) => None,
                // A refused delete, and every read or listing, leave the planted object exactly
                // where it was.
                (_, _) => Some(written.clone()),
            };
            assert_eq!(
                held, expected,
                "case {case:?}: expected {bucket:?}/{key:?} to hold {expected:?} afterwards, got \
                 {held:?}"
            );
        }
    }
}

/// The two buckets of the row called `slug`.
fn buckets_of(slug: &str) -> (String, String) {
    (
        format!("xsvc-in-fn-s3-{slug}-one"),
        format!("xsvc-in-fn-s3-{slug}-two"),
    )
}

/// The function of the row called `slug`.
fn function_of(slug: &str) -> String {
    format!("xsvc-in-fn-s3-{slug}-probe")
}
