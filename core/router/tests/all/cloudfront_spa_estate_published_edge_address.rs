// SPDX-License-Identifier: CC0-1.0

//! The whole SPA ESTATE behind one CloudFront distribution — an Amazon S3 shell origin and an
//! API Gateway REST stage whose `AWS_PROXY` methods write Amazon DynamoDB — reached by
//! DIALLING the `DomainName` the create published, on a real socket, with no credential of any
//! kind.
//!
//! RULE: **an address a service publishes for a client to dial is an address this emulator
//! itself answers on, and what answers there is the data plane of the resource that published
//! it, applying that resource's whole stored configuration.**
//! `Distribution$DomainName` is "the domain name that corresponds to the distribution … such
//! as `d111111abcdef8.cloudfront.net`", and the only way a viewer uses a distribution is to
//! issue HTTP(S) to that name — so the value a create reports has to be a `host:port` a client
//! on the machine this emulator runs on can open a connection to, and every cache behaviour
//! the distribution carries is served there: the one the viewer path matches decides which
//! origin answers, and whatever that origin answers — the object's bytes, the function's own
//! result, or the gateway's refusal — is what the viewer is served.
//!
//! Five crates have to agree for that to hold (CloudFormation provisions the functions and the
//! table, CloudFront mints the address and serves the edge, Amazon S3 holds the shell objects,
//! API Gateway resolves the deployed stage and performs the proxy integration, AWS Lambda runs
//! the handler that writes DynamoDB), so the suite lives here rather than in any one of them.
//! The viewer is a real client on a real connection, which is the one thing an in-process
//! dispatch addressed by a `Host` header cannot show: the sibling suites
//! (`cloudfront_apigateway_origin_edge`, `cloudfront_oac_s3_origin_edge`) address the edge that
//! way, and `cloudfront_dialled_edge_address_over_a_private_s3_origin` dials the address over
//! an S3 origin alone.
//!
//! The rows differ along what the rule generalises over: which cache behaviour the viewer path
//! matches (the default one and the path-pattern one), the kind of origin behind it (an S3
//! bucket and a deployed REST stage), the viewer's method (a read and a write), the path form
//! (the distribution root under its `DefaultRootObject`, a nested key, a resource under the
//! api), and whether the integration behind the api path may be performed at all — so the one
//! published address is shown serving the shell, carrying a write all the way into DynamoDB,
//! and surfacing the gateway's own refusal for a function that grants it nothing.
//!
//! Every expected value is computed here from what the row declares. The distribution's id,
//! ARN and published address are CloudFront's to mint, the function and table names are
//! CloudFormation's, and the api's ids are API Gateway's: what is asserted about the address is
//! its FORM (a `host:port` a client can dial), that `GetDistribution` reports the same one, and
//! what it answers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
// The viewer's own connection, and the one reading of "is this published value an address a
// client can dial": both written by the sibling dialled-address suite, which this suite dials
// the same way with a method and an entity of its own.
use crate::cloudfront_dialled_edge_address_over_a_private_s3_origin::{dial_with, is_dialable};
use crate::query_wire::{form_encode, physical_ids_of};

/// The Region every call is signed for, and the Region the origin hosts name.
const REGION: &str = "us-east-1";

/// CloudFront's control-plane host, and the API version its routes are under.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";
const API_VERSION: &str = "2020-05-31";

/// The `StackStatus` a completed create reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `Status` CloudFront reports a distribution in once its configuration has reached the
/// edge locations, which is the state a viewer request is served in.
const DEPLOYED: &str = "Deployed";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row is served the origin's own answer
/// rather than one the edge held from an earlier row.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The AWS-managed origin request policy `Managed-AllViewerExceptHostHeader`, "to forward all
/// parameters in viewer requests except for the Host header" — the policy AWS publishes for an
/// API Gateway origin, whose own host has to stay the origin's.
const ALL_VIEWER_EXCEPT_HOST: &str = "b689b0a8-53d0-40ab-baf2-68738e2966ac";

/// The origin ids the distribution's two origins carry, and the ones its behaviours target.
const SHELL_ORIGIN: &str = "shell-origin";
const API_ORIGIN: &str = "api-origin";

/// The bucket the SPA shell is served from, the object the distribution names as its
/// `DefaultRootObject`, and the nested asset beside it.
const SHELL_BUCKET: &str = "spa-edge-shell";
const ROOT_OBJECT: &str = "index.html";
const SHELL_BODY: &str = "<!doctype html><title>spa shell</title>";
const ASSET_KEY: &str = "static/app/main.js";
const ASSET_BODY: &str = "export const mounted = true;\n";

/// The stage the api is deployed to, which is also the api origin's `OriginPath`, and the
/// resource paths of its two methods under the distribution's api behaviour.
const STAGE: &str = "live";
const API_PREFIX: &str = "api";
const GRANTED_PATH: &str = "items";
const UNGRANTED_PATH: &str = "ungranted";

/// The stack the estate's functions and table are provisioned by.
const STACK: &str = "spa-edge-estate";

/// How long one invocation of the handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// The status and body API Gateway answers a request whose integration it cannot perform with
/// — a proxy integration at a function that grants it no `lambda:InvokeFunction`.
const CONFIGURATION_ERROR_STATUS: u16 = 500;
const CONFIGURATION_ERROR_BODY: &str = r#"{"message":"Internal server error"}"#;

/// What the edge reports as the outcome of an answer the ORIGIN produced for this request.
const MISS: &str = "Miss from cloudfront";

/// How long a clock-owed transition is given to settle, and how often it is read while
/// waiting. GENEROUS: what is waited for is the CONDITION each service reports, polled until it
/// holds, so this bound only decides when something that never settles is reported as a
/// failure.
const SETTLES_WITHIN: Duration = Duration::from_secs(90);
const POLL_EVERY: Duration = Duration::from_millis(50);

/// The handler both functions run: it writes the posted item to the table it was configured
/// with and answers the proxy envelope the client is served.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3


def handler(event, context):
    posted = json.loads(event.get("body") or "{}")
    boto3.client("dynamodb").put_item(
        TableName=os.environ["TABLE"],
        Item={"id": {"S": posted["id"]}, "data": {"S": posted["data"]}},
    )
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"stored": posted["id"]}),
    }
"#;

// ---------------------------------------------------------------------------
// The emulator and the setup plane
// ---------------------------------------------------------------------------

/// The emulator as it is ASSEMBLED, on one engine: the listener the distribution's edge is
/// served on hands the requests it accepts back to this same emulator, so the origins it
/// fetches are the ones this suite created.
///
/// Request authentication is off: a setup call carries a credential scope (which is what routes
/// it and what names the Region) without a computed signature, and the VIEWER carries no
/// credential at all, as a client of a distribution sends none.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// What one in-process call answered.
struct Answer {
    status: u16,
    body: Bytes,
}

impl Answer {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The text of `<name>…</name>` in the body, or the empty string.
    fn element(&self, name: &str) -> String {
        let body = self.text();
        body.split_once(&format!("<{name}>"))
            .and_then(|(_before, rest)| rest.split_once(&format!("</{name}>")))
            .map_or(String::new(), |(text, _after)| text.to_string())
    }
}

/// Dispatch one setup request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed setup request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call that has to be accepted with `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    dispatch(
        router,
        "POST",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One Amazon S3 call, addressed PATH-STYLE at the Region's endpoint so the bucket travels in
/// the path.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        headers,
        body,
    )
    .await
}

/// One CloudFront control-plane call.
async fn cloudfront(router: &MultiServiceRouter, method: &str, path: &str, body: Bytes) -> Answer {
    dispatch(
        router,
        method,
        CLOUDFRONT_HOST,
        path,
        &[
            ("content-type", "application/xml"),
            ("authorization", &authorization("cloudfront")),
        ],
        body,
    )
    .await
}

/// A required string member of a JSON object.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// The estate
// ---------------------------------------------------------------------------

/// The template the estate is provisioned from: one function per api path — the one the gateway
/// is granted the invocation of and the one it is not — and the table both are configured to
/// write.
fn template() -> String {
    let function = || {
        json!({
            "Type": "AWS::Serverless::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Timeout": FUNCTION_TIMEOUT,
                "InlineCode": HANDLER_SOURCE,
                "Environment": { "Variables": { "TABLE": { "Ref": "Items" } } },
                "Policies": [{ "DynamoDBCrudPolicy": { "TableName": { "Ref": "Items" } } }]
            }
        })
    };
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            "Granted": function(),
            "Ungranted": function(),
            "Items": {
                "Type": "AWS::Serverless::SimpleTable",
                "Properties": { "PrimaryKey": { "Name": "id", "Type": "String" } }
            }
        }
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`, and answer
/// the configuration it reported then. The span is that service's own, so nothing here sleeps a
/// fixed span on its behalf.
async fn awaited_function(router: &MultiServiceRouter, name: &str) -> Value {
    let giving_up_at = Instant::now() + SETTLES_WITHIN;
    loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        let configuration = reported.json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return configuration;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda never reported {name} Active, last read {}",
            reported.text()
        );
        ::tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Poll `DescribeTable` until DynamoDB first reports the table `ACTIVE` — "you can perform read
/// and write operations only on an ACTIVE table".
async fn awaited_table(router: &MultiServiceRouter, table: &str) {
    let giving_up_at = Instant::now() + SETTLES_WITHIN;
    loop {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": table })).await;
        let status = described
            .json()
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status == "ACTIVE" {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: DynamoDB never reported {table} ACTIVE, last read {}",
            described.text()
        );
        ::tokio::time::sleep(POLL_EVERY).await;
    }
}

/// The stack's functions and table: `(granted function arn, ungranted function arn, table)`,
/// each read back from the service that owns it once it reports itself ready.
async fn provisioned_estate(router: &MultiServiceRouter) -> (String, String, String) {
    let body = template();
    let created = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND, and the
            // roles the transform synthesizes for the functions with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the estate's stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let resources = cloudformation(router, "DescribeStackResources", &[("StackName", STACK)]).await;
    let physical = physical_ids_of(&resources.text());
    let named = |logical: &str| {
        physical
            .get(logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: no physical id for {logical} in {physical:?}"))
    };
    let (granted, ungranted, table) = (named("Granted"), named("Ungranted"), named("Items"));
    awaited_table(router, &table).await;
    let mut arns = Vec::new();
    for function in [&granted, &ungranted] {
        let configuration = awaited_function(router, function).await;
        arns.push(member(
            "GetFunctionConfiguration",
            &configuration,
            "FunctionArn",
        ));
    }
    let ungranted_arn = arns.pop().unwrap_or_default();
    let granted_arn = arns.pop().unwrap_or_default();
    // Only ONE of the two functions admits an invocation from API Gateway, which is what the
    // refusal row is about: the other grants the gateway nothing at all.
    let permitted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{granted}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        permitted.status, 201,
        "setup: expected HTTP 201 from AddPermission, got {} with {:?}",
        permitted.status,
        permitted.text()
    );
    (granted_arn, ungranted_arn, table)
}

/// The deployed REST api the distribution's api behaviour fronts: `POST /api/items` on the
/// function the gateway may invoke and `POST /api/ungranted` on the one it may not, deployed to
/// [`STAGE`]. Answers the api's id.
async fn deployed_api(
    router: &MultiServiceRouter,
    granted_arn: &str,
    ungranted_arn: &str,
) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "spa-edge-api" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let prefix = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": API_PREFIX })),
        201,
    )
    .await;
    let prefix_id = member("CreateResource", &prefix, "id");
    for (path_part, function_arn) in [
        (GRANTED_PATH, granted_arn),
        (UNGRANTED_PATH, ungranted_arn),
    ] {
        let resource = apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{prefix_id}"),
            Some(json!({ "pathPart": path_part })),
            201,
        )
        .await;
        let resource_id = member("CreateResource", &resource, "id");
        let method = format!("/restapis/{api}/resources/{resource_id}/methods/POST");
        apigw(
            router,
            "PutMethod",
            "PUT",
            &method,
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("{method}/integration"),
            Some(json!({
                "type": "AWS_PROXY",
                // `integrationHttpMethod` travels under its `jsonName`, which is what
                // `smithy_query(query="operation",operation="PutIntegration")` reports.
                "httpMethod": "POST",
                "uri": format!(
                    "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                     {function_arn}/invocations"
                ),
            })),
            201,
        )
        .await;
    }
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// The bucket the shell is served from, holding the root object and the nested asset.
async fn shell_bucket(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{SHELL_BUCKET}"), &[], Bytes::new()).await;
    assert_eq!(
        created.status, 200,
        "setup: expected HTTP 200 from CreateBucket, got {} with {:?}",
        created.status,
        created.text()
    );
    for (key, body) in [(ROOT_OBJECT, SHELL_BODY), (ASSET_KEY, ASSET_BODY)] {
        let written = s3(
            router,
            "PUT",
            &format!("/{SHELL_BUCKET}/{key}"),
            &[],
            Bytes::from(body),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "setup: expected HTTP 200 from PutObject({key:?}), got {} with {:?}",
            written.status,
            written.text()
        );
    }
}

/// The bucket policy that lets the distribution's own origin request read the shell: the grant
/// to the CloudFront service principal scoped by `AWS:SourceArn` to this distribution, which is
/// why it is written only once the distribution exists.
async fn grant_shell_reads(router: &MultiServiceRouter, distribution_arn: &str) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowCloudFrontServicePrincipal",
            "Effect": "Allow",
            "Principal": { "Service": "cloudfront.amazonaws.com" },
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{SHELL_BUCKET}/*"),
            "Condition": { "StringEquals": { "AWS:SourceArn": distribution_arn } }
        }]
    })
    .to_string();
    let stored = s3(
        router,
        "PUT",
        &format!("/{SHELL_BUCKET}?policy"),
        &[("content-type", "application/json")],
        Bytes::from(document),
    )
    .await;
    // S3 answers a bucket-policy write `204 No Content`.
    assert_eq!(
        stored.status, 204,
        "setup: expected HTTP 204 from PutBucketPolicy, got {} with {:?}",
        stored.status,
        stored.text()
    );
}

/// `cloudfront:CreateOriginAccessControl` for the shell bucket, signing every origin request.
async fn origin_access_control(router: &MultiServiceRouter) -> String {
    let body = format!(
        "<OriginAccessControlConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>spa-edge-shell-access</Name>\
         <SigningProtocol>sigv4</SigningProtocol>\
         <SigningBehavior>always</SigningBehavior>\
         <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
         </OriginAccessControlConfig>"
    );
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/origin-access-control"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: expected HTTP 201 from CreateOriginAccessControl, got {} with {:?}",
        created.status,
        created.text()
    );
    let id = created.element("Id");
    assert!(
        !id.is_empty(),
        "setup: the created origin access control reports no Id: {:?}",
        created.text()
    );
    id
}

/// The distribution of the estate: the shell bucket as the default behaviour's origin and the
/// deployed stage as the origin of the behaviour the api path pattern selects. Answers its
/// `(id, DomainName, ARN)`.
///
/// Member order is the model's own
/// (`smithy_query(query="shape",shape="DistributionConfig,CacheBehavior")`).
async fn create_distribution(
    router: &MultiServiceRouter,
    control: &str,
    api: &str,
) -> (String, String, String) {
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>spa-edge-estate-reference</CallerReference>\
         <DefaultRootObject>{ROOT_OBJECT}</DefaultRootObject>\
         <Origins><Quantity>2</Quantity><Items>\
         <Origin><Id>{SHELL_ORIGIN}</Id>\
         <DomainName>{SHELL_BUCKET}.s3.{REGION}.amazonaws.com</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         <OriginAccessControlId>{control}</OriginAccessControlId>\
         </Origin>\
         <Origin><Id>{API_ORIGIN}</Id>\
         <DomainName>{api}.execute-api.{REGION}.amazonaws.com</DomainName>\
         <OriginPath>/{STAGE}</OriginPath>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         </CustomOriginConfig>\
         </Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{SHELL_ORIGIN}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>1</Quantity><Items><CacheBehavior>\
         <PathPattern>/{API_PREFIX}/*</PathPattern>\
         <TargetOriginId>{API_ORIGIN}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>7</Quantity><Items>\
         <Method>GET</Method><Method>HEAD</Method><Method>OPTIONS</Method>\
         <Method>PUT</Method><Method>POST</Method><Method>PATCH</Method>\
         <Method>DELETE</Method></Items>\
         <CachedMethods><Quantity>2</Quantity><Items>\
         <Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <OriginRequestPolicyId>{ALL_VIEWER_EXCEPT_HOST}</OriginRequestPolicyId>\
         </CacheBehavior></Items></CacheBehaviors>\
         <Comment>a spa shell with an api behaviour</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    );
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: expected HTTP 201 from CreateDistribution, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain, arn) = (
        created.element("Id"),
        created.element("DomainName"),
        created.element("ARN"),
    );
    for (name, value) in [("Id", &id), ("DomainName", &domain), ("ARN", &arn)] {
        assert!(
            !value.is_empty(),
            "setup: the created distribution reports no {name}: {:?}",
            created.text()
        );
    }
    (id, domain, arn)
}

/// Look again at `GetDistribution` until CloudFront first reports the distribution [`DEPLOYED`]
/// — a configuration that has not reached the edge locations serves no viewer — answering the
/// `DomainName` that read reports.
async fn awaited_deployment(router: &MultiServiceRouter, id: &str) -> String {
    let giving_up_at = Instant::now() + SETTLES_WITHIN;
    loop {
        let read = cloudfront(
            router,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            Bytes::new(),
        )
        .await;
        if read.element("Status") == DEPLOYED {
            return read.element("DomainName");
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: CloudFront never reported the distribution {id} {DEPLOYED}, last read {:?}",
            read.text()
        );
        ::tokio::time::sleep(POLL_EVERY).await;
    }
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// What a row expects the viewer dialling the published address to be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The shell origin's object with these exact bytes, under HTTP 200.
    ShellObject(&'static str),
    /// The api's own answer: the proxy function ran and the item it posted is in the table.
    ApiWrite,
    /// The gateway's own configuration-error answer, with nothing written to the table.
    GatewayRefusal,
}

/// One row: the request the viewer makes to the published address, and what that address then
/// answers.
struct Row {
    case: &'static str,
    method: &'static str,
    /// The path the viewer asks the dialled address for, built from the object or resource it
    /// addresses so a row cannot drift from what the setup created.
    path: String,
    /// Whether the row posts an item — the write rows do, and each posts its own id.
    posts: bool,
    expected: Expected,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the distribution root under its default root object",
            method: "GET",
            path: "/".to_string(),
            posts: false,
            expected: Expected::ShellObject(SHELL_BODY),
        },
        Row {
            case: "a nested asset of the shell origin",
            method: "GET",
            path: format!("/{ASSET_KEY}"),
            posts: false,
            expected: Expected::ShellObject(ASSET_BODY),
        },
        Row {
            case: "a write through the api behaviour, whose function the gateway may invoke",
            method: "POST",
            path: format!("/{API_PREFIX}/{GRANTED_PATH}"),
            posts: true,
            expected: Expected::ApiWrite,
        },
        Row {
            case: "the same behaviour over a function that grants the gateway nothing",
            method: "POST",
            path: format!("/{API_PREFIX}/{UNGRANTED_PATH}"),
            posts: true,
            expected: Expected::GatewayRefusal,
        },
    ]
}

/// The item a write row posts: its own id, so one row's write is never another's.
fn posted(index: usize) -> (String, String, String) {
    let id = format!("edge-item-{index}");
    let data = format!("written through the edge {index}");
    let body = json!({ "id": id, "data": data }).to_string();
    (id, data, body)
}

#[tokio::test]
async fn the_published_domain_name_serves_the_spa_shell_and_carries_its_api_writes() {
    let router = emulator();

    // The estate: the two functions and the table they write, the deployed api in front of
    // them, and the bucket the shell is served from.
    let (granted_arn, ungranted_arn, table) = provisioned_estate(&router).await;
    let api = deployed_api(&router, &granted_arn, &ungranted_arn).await;
    shell_bucket(&router).await;
    let control = origin_access_control(&router).await;
    let (id, domain, arn) = create_distribution(&router, &control, &api).await;
    grant_shell_reads(&router, &arn).await;

    // The published name is an address a client on this machine can open a connection to, or
    // there is nothing for a viewer to dial at all.
    assert!(
        is_dialable(&domain),
        "expected a host:port a client on this machine can dial as the published DomainName, \
         got {domain:?}"
    );
    // The address CloudFront reports on a READ is the same one the create published: the name
    // is the distribution's, not the create response's.
    let read_back = awaited_deployment(&router, &id).await;
    assert_eq!(
        read_back, domain,
        "expected GetDistribution to report the published address {domain:?}, got {read_back:?}"
    );

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (item_id, item_data, body) = posted(index);
        let sent = if row.posts { body.as_str() } else { "" };
        let answer = dial_with(case, &domain, row.method, &row.path, sent).await;
        // Whatever the answer is, it is the edge's: CloudFront identifies the request it
        // served, which is what says the dialled address is a distribution's data plane and not
        // some other listener that happened to accept the connection.
        assert!(
            !answer.header("x-amz-cf-id").is_empty(),
            "case {case:?}: expected the edge's own X-Amz-Cf-Id on the answer, got {:?}",
            answer.raw
        );
        match row.expected {
            Expected::ShellObject(bytes) => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the shell origin's object at the published \
                     address, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.body, bytes,
                    "case {case:?}: expected the ORIGIN's own bytes, got {:?}",
                    answer.body
                );
                assert_eq!(
                    answer.header("x-cache"),
                    MISS,
                    "case {case:?}: expected the answer to be the origin's, got X-Cache {:?}",
                    answer.header("x-cache")
                );
            }
            Expected::ApiWrite => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the api origin's own answer at the published \
                     address, got {} with {:?}",
                    answer.status, answer.body
                );
                let served: Value = serde_json::from_str(&answer.body).unwrap_or(Value::Null);
                assert_eq!(
                    served.get("stored").and_then(Value::as_str),
                    Some(item_id.as_str()),
                    "case {case:?}: expected the handler's own result for {item_id:?}, got {:?}",
                    answer.body
                );
                // The work the answer reported is work DynamoDB really did.
                let read = dynamodb(
                    &router,
                    "GetItem",
                    json!({
                        "TableName": table,
                        "Key": { "id": { "S": item_id } },
                        "ConsistentRead": true,
                    }),
                )
                .await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: expected HTTP 200 from GetItem, got {} with {:?}",
                    read.status,
                    read.text()
                );
                assert_eq!(
                    read.json().get("Item").cloned().unwrap_or(Value::Null),
                    json!({ "id": { "S": item_id }, "data": { "S": item_data } }),
                    "case {case:?}: expected the table to hold the posted item under \
                     {item_id:?}, got {:?}",
                    read.text()
                );
            }
            Expected::GatewayRefusal => {
                assert_eq!(
                    answer.status, CONFIGURATION_ERROR_STATUS,
                    "case {case:?}: expected the gateway's configuration-error answer at the \
                     published address, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.body, CONFIGURATION_ERROR_BODY,
                    "case {case:?}: expected {CONFIGURATION_ERROR_BODY:?}, got {:?}",
                    answer.body
                );
                // An integration that was never performed wrote nothing: the table holds no
                // item under the id this row posted.
                let read = dynamodb(
                    &router,
                    "GetItem",
                    json!({
                        "TableName": table,
                        "Key": { "id": { "S": item_id } },
                        "ConsistentRead": true,
                    }),
                )
                .await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: expected HTTP 200 from GetItem, got {} with {:?}",
                    read.status,
                    read.text()
                );
                assert_eq!(
                    read.json().get("Item"),
                    None,
                    "case {case:?}: expected the table to hold nothing under {item_id:?}, got \
                     {:?}",
                    read.text()
                );
            }
        }
    }
}
