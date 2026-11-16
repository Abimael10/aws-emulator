// SPDX-License-Identifier: CC0-1.0

//! A Lambda-backed custom resource whose provider function and execution role were created
//! OUT OF BAND — through AWS Lambda's and IAM's own APIs rather than by the stack — and whose
//! role grants exactly the one object write the provider performs.
//!
//! The rule under test: **every lifecycle event of a custom resource reaches the provider its
//! `ServiceToken` names and the stack settles on the answer that provider sends — a `Create`
//! on the deployment that first declares it and an `Update` on each later deployment that
//! changes its declaration — so the side effect the provider performs lands once per event
//! and the stack reports the terminal status of the operation that drove it.**
//!
//! The provider is written the way AWS documents one: it does its work with the AWS SDK its
//! runtime supplies and answers the request at the `ResponseURL` with the HTTP client that SDK
//! is built on, which the managed runtime carries beside it.
//!
//! Three crates have to agree for that to hold — CloudFormation sends the request and holds
//! the resource until the provider answers, AWS Lambda runs the provider under its execution
//! role and supplies it both of those libraries, and Amazon S3 decides the provider's call
//! against the role's own policy — so the suite lives here.
//!
//! The rows are the lifecycle events of ONE deployment, each asserted where a client sees it:
//! the create writing the first object and reaching `CREATE_COMPLETE`, and two updates — one
//! changing the key the provider writes and one changing a property it only echoes — each
//! writing their own object, reaching `UPDATE_COMPLETE` and leaving every earlier object
//! untouched.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// How long a condition the clock owes is waited for, and how often it is looked at. The wait
/// is on the CONDITION: nothing here sleeps a fixed span on a service's behalf, and the
/// deadline is far longer than the spans this emulator gives these transitions, so a loaded
/// host cannot decide the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The stack, the bucket, the role and the provider function this suite deploys.
const STACK: &str = "least-privileged-provider";
const PROVIDER_STACK: &str = "least-privileged-provider-function";
const BUCKET: &str = "least-privileged-provider-bucket";
const ROLE: &str = "least-privileged-provider-role";
const FUNCTION: &str = "least-privileged-provider";

/// The logical id of the custom resource the stack declares.
const CUSTOM: &str = "Writer";

/// The provider's source: it writes the object its properties name with the AWS SDK its
/// runtime supplies — the one call its execution role allows — and PUTs the outcome to the
/// request's `ResponseURL` with the HTTP client that SDK is built on, which is how AWS's own
/// documented provider code answers.
const PROVIDER_SOURCE: &str = r#"
import json

import boto3
import urllib3

s3 = boto3.client("s3")
http = urllib3.PoolManager()


def handler(event, context):
    status = "SUCCESS"
    reason = ""
    data = {}
    try:
        properties = event["ResourceProperties"]
        if event["RequestType"] != "Delete":
            s3.put_object(
                Bucket=properties["Bucket"],
                Key=properties["Key"],
                Body=properties["Body"].encode("utf-8"),
            )
            data = {"Written": properties["Key"], "Echo": properties.get("Echo", "")}
    except Exception as error:
        status = "FAILED"
        reason = "Error: %s" % (error,)
    body = json.dumps(
        {
            "Status": status,
            "Reason": reason,
            "PhysicalResourceId": "written-" + event["LogicalResourceId"],
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": data,
        }
    ).encode("utf-8")
    request = http.request(
        "PUT",
        event["ResponseURL"],
        headers={"content-type": ""},
        body=body,
    )
    return {"answered": request.status}
"#;

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine, with request authentication ON — the
/// configuration it serves traffic in: the provider CloudFormation invokes is the function
/// created here, its own call is dispatched back into this same router, and the answer it PUTs
/// to the request's `ResponseURL` is served by this router too, so whether a router that
/// VERIFIES signatures carries the whole lifecycle is part of what this suite asks.
fn emulator() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(true);
    router
}

/// The credential every call below signs with: the pair this process publishes once request
/// authentication is on.
fn published() -> &'static (String, String) {
    static PUBLISHED: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    PUBLISHED.get_or_init(|| {
        let minted = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning request authentication on publishes this process's keys");
        (
            minted.access_key_id.clone(),
            minted.secret_access_key.clone(),
        )
    })
}

/// `request`, signed for `signing_name` in [`REGION`] the way an SDK signs one.
fn signed(mut request: http::Request<Vec<u8>>, signing_name: &str) -> http::Request<Bytes> {
    let (key_id, secret) = published();
    let identity = Credentials::new(key_id, secret, None, None, "test").into();
    let params = SigningParams::builder()
        .identity(&identity)
        .region(REGION)
        .name(signing_name)
        .time(std::time::SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .expect("the signing parameters are complete")
        .into();
    let (method, uri) = (request.method().to_string(), request.uri().to_string());
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().expect("a text header").to_string(),
            )
        })
        .collect();
    let signable = SignableRequest::new(
        &method,
        &uri,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        SignableBody::Bytes(request.body()),
    )
    .expect("the request is signable");
    let (instructions, _) = sign(signable, &params)
        .expect("the request signs")
        .into_parts();
    instructions.apply_to_request_http1x(&mut request);
    let (parts, payload) = request.into_parts();
    http::Request::from_parts(parts, Bytes::from(payload))
}

/// Dispatch one signed request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = signed(
        builder.body(body).expect("a well-formed request"),
        signing_name,
    );
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// One awsQuery call of the service `signing_name`.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        signing_name,
        &format!("{signing_name}.{REGION}.amazonaws.com"),
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
    .await
}

/// One CloudFormation call (API version 2010-05-15).
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One IAM call (API version 2010-05-08), which must be answered.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let answered = query(router, "iam", "2010-05-08", action, members).await;
    assert_eq!(
        answered.status,
        200,
        "setup: {action} must be answered by IAM, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Answered {
    dispatch(
        router,
        method,
        "lambda",
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[("content-type", "application/json")],
        body.map(|document| document.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await
}

/// One request against Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answered {
    dispatch(
        router,
        method,
        "s3",
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[("content-type", "application/octet-stream")],
        body.as_bytes().to_vec(),
    )
    .await
}

/// The execution role the provider runs as: trusted by AWS Lambda, and allowed exactly the
/// one object write the provider performs on the one bucket it writes to.
async fn given_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({ROLE:?}) must answer an ARN, got {created}"));
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "write-the-object"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
    arn
}

/// The bucket the provider writes into.
async fn given_bucket(router: &MultiServiceRouter) {
    let answered = s3(router, "PUT", &format!("/{BUCKET}"), "").await;
    assert_eq!(
        answered.status,
        200,
        "setup: CreateBucket({BUCKET:?}) must answer 200, got {} with {}",
        answered.status,
        answered.text()
    );
}

/// The provider function, deployed BEFORE the stack that declares the custom resource and
/// outside it — the shape a provider shared by several stacks has — and waited for until
/// Lambda reports it `Active`; answers its ARN.
///
/// The function's own source is declared inline, which is what archives it into a deployment
/// package; its execution role is the one created above, so the provider runs as a role
/// granted the single object write it performs and nothing else.
async fn given_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Provider": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Code": { "ZipFile": PROVIDER_SOURCE },
                },
            },
        },
    })
    .to_string();
    let created = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", PROVIDER_STACK),
            ("TemplateBody", &template),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateStack({PROVIDER_STACK:?}) must be accepted, got {} with {}",
        created.status,
        created.text()
    );
    let (settled, described) = settled_status(router, PROVIDER_STACK).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "setup: the provider's stack must complete, got {settled:?} with {described}"
    );
    let mut arn = String::new();
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if let Some(reported) = reported.json()["FunctionArn"].as_str() {
            arn = reported.to_string();
        }
        let state = reported.json()["State"].as_str().unwrap_or_default().to_string();
        if state == "Active" {
            assert!(
                !arn.is_empty(),
                "setup: the function must report its ARN, got {}",
                reported.text()
            );
            return arn;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda must report {FUNCTION:?} Active, last reported {state:?} with {}",
            reported.text()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The template one deployment declares: the custom resource alone, backed by the provider
/// created out of band, naming the key it writes and the body it writes there.
fn template(service_token: &str, key: &str, body: &str, echo: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            CUSTOM: {
                "Type": "Custom::ObjectWriter",
                "Properties": {
                    "ServiceToken": service_token,
                    "Bucket": BUCKET,
                    "Key": key,
                    "Body": body,
                    "Echo": echo,
                },
            },
        },
        "Outputs": {
            "WriterRef": { "Value": { "Ref": CUSTOM } },
            "WrittenKey": { "Value": { "Fn::GetAtt": [CUSTOM, "Written"] } },
            "WrittenEcho": { "Value": { "Fn::GetAtt": [CUSTOM, "Echo"] } },
        },
    })
    .to_string()
}

/// What the stack reports now: its `StackStatus`, the answer it was read from, and whether
/// CloudFormation still holds a stack under that name at all.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String, bool) {
    let answered = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    let body = answered.text();
    (
        opt_element(&body, "StackStatus").unwrap_or_default(),
        body,
        answered.status == 200,
    )
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS`, and answer the last
/// status it reported with the answer that status was read from.
async fn settled_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (reported, body, held) = stack_status(router, stack).await;
        let settled = !held || (!reported.is_empty() && !reported.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= giving_up_at {
            return (reported, body);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every resource failure the stack reported, with the reason it gave — what this suite quotes
/// when a deployment did not reach the status it must.
async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let answered =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    crate::query_wire::members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let status = elements(member, "ResourceStatus").into_iter().next()?;
            if !status.contains("FAILED") {
                return None;
            }
            let logical = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .unwrap_or_default();
            let reason = elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some(format!("{logical} {status}: {reason}"))
        })
        .collect()
}

/// The body Amazon S3 holds at `key`, or `None` when it holds no object there.
async fn object_body(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let answered = s3(router, "GET", &format!("/{BUCKET}/{key}"), "").await;
    (answered.status == 200).then(|| answered.text())
}

/// One lifecycle event: the key the provider writes, the body it writes there, the property it
/// only echoes, and the status the operation that drove it must reach.
struct Row {
    case: &'static str,
    key: &'static str,
    body: &'static str,
    echo: &'static str,
    status: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the create, which first declares the resource",
        key: "v1",
        body: "the first version",
        echo: "first",
        status: "CREATE_COMPLETE",
    },
    Row {
        case: "an update changing the key the provider writes",
        key: "v2",
        body: "the second version",
        echo: "second",
        status: "UPDATE_COMPLETE",
    },
    Row {
        case: "an update changing only a property the provider echoes",
        key: "v2",
        body: "the second version",
        echo: "third",
        status: "UPDATE_COMPLETE",
    },
];

#[tokio::test]
async fn each_lifecycle_event_reaches_the_provider_and_settles_the_stack() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;
    let service_token = given_function(&router, &role_arn).await;

    let mut written: BTreeMap<&str, &str> = BTreeMap::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let body = template(&service_token, row.key, row.body, row.echo);
        let (action, answered) = if index == 0 {
            (
                "CreateStack",
                cloudformation(
                    &router,
                    "CreateStack",
                    &[("StackName", STACK), ("TemplateBody", &body)],
                )
                .await,
            )
        } else {
            (
                "UpdateStack",
                cloudformation(
                    &router,
                    "UpdateStack",
                    &[("StackName", STACK), ("TemplateBody", &body)],
                )
                .await,
            )
        };
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: {action} must be accepted, got {} with {}",
            answered.status,
            answered.text()
        );

        let (settled, described) = settled_status(&router, STACK).await;
        if settled != row.status {
            let failures = reported_failures(&router, STACK).await;
            panic!(
                "case {case:?}: the stack must settle at {}, got {settled:?}; the failures it \
                 reported: {failures:?}; last answer {described}",
                row.status
            );
        }

        // The provider's answer IS the resource: the id it reported is what `Ref` resolves to
        // and the `Data` it published is what `Fn::GetAtt` resolves against.
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("WriterRef").map(String::as_str),
            Some(format!("written-{CUSTOM}").as_str()),
            "case {case:?}: expected Ref to resolve to the answered id, got {outputs:?}"
        );
        assert_eq!(
            outputs.get("WrittenKey").map(String::as_str),
            Some(row.key),
            "case {case:?}: expected Fn::GetAtt to resolve to this event's own answer, got \
             {outputs:?}"
        );
        assert_eq!(
            outputs.get("WrittenEcho").map(String::as_str),
            Some(row.echo),
            "case {case:?}: expected Fn::GetAtt to resolve to this event's own answer, got \
             {outputs:?}"
        );

        // The side effect this event's request asked for landed, and every object an earlier
        // event wrote is as that event left it.
        written.insert(row.key, row.body);
        for (key, expected) in &written {
            let held = object_body(&router, key).await;
            assert_eq!(
                held.as_deref(),
                Some(*expected),
                "case {case:?}: expected the object {key:?} the provider wrote to hold \
                 {expected:?}, got {held:?}"
            );
        }
    }

    // The stack's own record of the resource is the provider's answer too.
    let resources = cloudformation(
        &router,
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        resources.status,
        200,
        "DescribeStackResources must be answered, got {} with {}",
        resources.status,
        resources.text()
    );
    let described = resources.text();
    assert_eq!(
        element(&described, "PhysicalResourceId"),
        format!("written-{CUSTOM}"),
        "expected the stack to hold the physical id the provider answered with, got {described}"
    );
}
