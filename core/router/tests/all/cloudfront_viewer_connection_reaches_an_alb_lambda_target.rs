// SPDX-License-Identifier: CC0-1.0

//! What the CloudFront edge tells an ORIGIN about the viewer's connection, read at the far end
//! of the hop a distribution over a VPC origin takes: an internal Application Load Balancer
//! whose `lambda` target group runs a function that reports the request it was handed.
//!
//! RULE: **`CloudFront-Viewer-Address` describes the CONNECTION one viewer request arrived on
//! — the address the request came from AND the port it came from — so the edge generates it as
//! `<address>:<port>` (an IPv6 literal bracketed, which is where its own colons live), and adds
//! it to an origin request exactly where the matched behaviour's origin request policy NAMES
//! it.** The address half is the one the edge itself observes for the viewer, the same value its
//! signed-URL source-address condition and its log records are read against; the port half is
//! the viewer's own end of that connection, which lies in the range RFC 6335 reserves for the
//! end a client opens from. A value the VIEWER states for that header is the viewer's claim
//! about a connection it did not decide, so it is dropped rather than relayed, and a behaviour
//! whose policy does not name the header sends the origin none of it.
//!
//! An origin that recovers the client's address and source port out of that header — rate
//! limiting per client, abuse logging, an access decision, any `rsplit(":", 1)` of the value —
//! reads both halves or neither, which is why the FORM is the requirement rather than any
//! particular address or port.
//!
//! Why this suite lives here rather than in the cloudfront crate: the hop the value has to
//! survive crosses four services on one engine — CloudFormation archives the inline handler
//! into a package this emulator runs, IAM holds its execution role, Lambda runs it, and Elastic
//! Load Balancing opens the address the VPC origin names, evaluates the listener and invokes
//! the function — so the origin that REPORTS what reached it is only reachable with all of them
//! mounted together. The cloudfront crate's own `edge_viewer_connection_address` suite pins the
//! same rule over a Lambda function URL origin; this one pins it over the other origin kind a
//! distribution can carry, where the request also traverses a load balancer before an origin
//! reads it.
//!
//! The rows differ along what the rule generalises over: what the request says about where it
//! came from (nothing at all, one address, a proxy chain whose first entry is the viewer, a
//! value that is no address, an IPv6 viewer), whether the viewer also states a connection of
//! its own for the generated header, and HOW the behaviour's origin request policy names that
//! header (a policy of the account's own listing it, the AWS-managed policy listing every
//! CloudFront-generated header, and no origin request policy at all, where the origin must be
//! told nothing).
//!
//! Every clock-owed move is POLLED for until it holds, never slept for, and each deadline is
//! generous: a row asserts that the move HAPPENS, not how fast this host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for the wire suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode, opt_element};
// The one reading of a stack settling, from the sibling that owns it.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;

/// The Region every setup call is signed for.
const REGION: &str = "us-east-1";

/// The API versions the query protocols carry, and CloudFront's own route prefix.
const CFN_VERSION: &str = "2010-05-15";
const EC2_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";
const API_VERSION: &str = "2020-05-31";

/// CloudFront's control-plane host.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";

/// The stack the execution role and the function are deployed as, and the names it declares
/// them under.
const STACK: &str = "cf-viewer-connection-alb-target";
const ROLE: &str = "cf-viewer-connection-target-exec";
const FUNCTION: &str = "cf-viewer-connection-target";

/// The load balancer, its target group, and the statement the invoke grant is written under.
const LOAD_BALANCER: &str = "cf-viewer-connection-alb";
const TARGET_GROUP: &str = "cf-viewer-conn-tg";
const STATEMENT: &str = "elbv2-invoke";

/// The id the distribution's one origin carries, which its default behaviour targets.
const ORIGIN_ID: &str = "alb-vpc-origin";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row reads what the edge sent for THAT
/// request rather than bytes an earlier row left in the cache.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The AWS-managed origin request policy `Managed-AllViewerAndCloudFrontHeaders-2022-06`,
/// whose list is every CloudFront-generated header beside all viewer parameters. The id is the
/// one this service's own managed catalogue publishes for it
/// (`smithy_query` models no generated-header value; the catalogue is
/// `state::MANAGED_ORIGIN_REQUEST_POLICIES`).
const MANAGED_ALL_VIEWER_AND_CLOUDFRONT: &str = "33f36d7e-f396-46d9-90e0-52428a34d9dc";

/// The generated headers a policy of the account's own names here, spelled as CloudFront
/// documents them — the connection the origin is told about, and the viewer's country beside
/// it, which is the pair a distribution behind an abuse-logging origin lists.
const VIEWER_ADDRESS_HEADER: &str = "CloudFront-Viewer-Address";
const VIEWER_COUNTRY_HEADER: &str = "CloudFront-Viewer-Country";

/// The header name as an ORIGIN request carries it (lower case, as HTTP names are compared),
/// and the header a viewer that traversed a proxy states its own address in.
const VIEWER_ADDRESS: &str = "cloudfront-viewer-address";
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The range RFC 6335 reserves for the end of a connection a CLIENT opens from — "the Dynamic
/// and/or Private Ports" — which is where the port of a viewer's own end of a connection lies.
const FIRST_DYNAMIC_PORT: u16 = 49_152;
const LAST_DYNAMIC_PORT: u16 = 65_535;

/// The marker the handler puts in every report of its own, so an answer that carries it came
/// from the origin and one that does not was produced short of it.
const ORIGIN_MARKER: &str = "ALB-LAMBDA-TARGET-OK";

/// `X-Cache` for an answer the origin produced for this very request.
const MISS: &str = "Miss from cloudfront";

/// The network the load balancer is placed in, created through EC2 itself: a load balancer is
/// placed in subnets of a VPC, which the service it is placed from resolves. An INTERNAL load
/// balancer is the scheme a VPC origin fronts, and such a one needs no internet gateway.
const VPC_CIDR: &str = "10.90.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.90.1.0/24", "us-east-1a"),
    ("10.90.2.0/24", "us-east-1b"),
];

/// How long a wait for work the CLOCK owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: everything waited for happens in this process, so a
/// loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the target runs: it reports the headers of the request it was handed, by
/// lower-cased name, so what the origin was told is what this suite reads. It imports only the
/// standard library, so the report is the handler's own work.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    seen = {}
    for name, value in (event.get("headers") or {}).items():
        seen[str(name).lower()] = value
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({"marker": "ALB-LAMBDA-TARGET-OK", "headers": seen}),
    }
"#;

/// What one call answered.
struct Answer {
    status: u16,
    headers: ::http::HeaderMap,
    body: String,
}

impl Answer {
    /// A response header's value, empty when the answer carries none.
    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the function the load balancer forwards to
/// is run by the Lambda of this same router, and the distribution the viewer dials is served by
/// the CloudFront of it.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request. A VIEWER request carries no credential at all,
    // as a client of a distribution sends none.
    router.set_request_authentication(false);
    // The address the emulator publishes its data port at, recorded as the core records it on
    // boot, so every URL a service hands a caller takes the form it takes in service.
    ::emulator_server::EmulatorService::serving_at(&router, "https://127.0.0.1:9297");
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes a
/// setup call.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire, addressed at `uri` with `host`.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// One form-encoded (query-protocol) call of `service`, as its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        &host,
        &format!("https://{host}/"),
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(service)),
        ],
        Bytes::from(body),
    )
    .await;
    (answered.status, answered.body)
}

/// One EC2 call that must be served.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "ec2", EC2_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// The subnets of a VPC of this account that a load balancer is placed in.
async fn placement_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (cidr, zone) in SUBNET_CIDRS {
        let created = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&created, "subnetId"));
    }
    subnets
}

/// One Elastic Load Balancing v2 call that must be served. The credential scope names THIS
/// model's own routing key, which is how an ELBv2 request is addressed at the model rather than
/// at the Classic one it shares an endpoint with.
async fn elbv2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(
        router,
        "elasticloadbalancingv2",
        ELB_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One CloudFront control-plane call.
async fn cloudfront(router: &MultiServiceRouter, method: &str, path: &str, body: Bytes) -> Answer {
    dispatch(
        router,
        method,
        CLOUDFRONT_HOST,
        &format!("https://{CLOUDFRONT_HOST}{path}"),
        &[
            ("content-type", "application/xml"),
            ("authorization", &authorization("cloudfront")),
        ],
        body,
    )
    .await
}

/// One Lambda `restJson1` call, as its status and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the request body serializes")
    };
    let answered = dispatch(
        router,
        method,
        &host,
        &format!("https://{host}{path}"),
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    let parsed = if answered.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&answered.body).unwrap_or(Value::Null)
    };
    (answered.status, parsed)
}

/// The template the suite deploys: the execution role the function runs as, and the function
/// itself. The inline source is archived into the deployment package by CloudFormation, which
/// is how a function whose handler is written here comes to exist at all.
fn stack_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "Target": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Timeout": 30,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string()
}

/// Deploy that stack, wait for `CREATE_COMPLETE`, and answer the ARN Lambda itself reports for
/// the function — the value a `lambda` target group registers.
async fn deployed_function(router: &MultiServiceRouter) -> String {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        CFN_VERSION,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateStack: expected 200, got {status} with {answered}"
    );
    let (reported, detail) = wait_for_stack_status(router, STACK, "CREATE_COMPLETE").await;
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "setup: the stack must reach CREATE_COMPLETE, got {reported} with {detail}"
    );
    let (status, configuration) = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup GetFunctionConfiguration: expected 200, got {status} {configuration}"
    );
    configuration["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no FunctionArn in {configuration}"))
        .to_string()
}

/// Create the INTERNAL Application Load Balancer, its `lambda` target group over
/// `function_arn`, the grant that admits the invocation and the listener that forwards to the
/// group, and answer the load balancer's ARN and the address it publishes as its `DNSName`.
async fn load_balancer_over(router: &MultiServiceRouter, function_arn: &str) -> (String, String) {
    let subnets = placement_subnets(router).await;
    let created = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", LOAD_BALANCER),
            ("Type", "application"),
            ("Scheme", "internal"),
            ("Subnets.member.1", subnets[0].as_str()),
            ("Subnets.member.2", subnets[1].as_str()),
        ],
    )
    .await;
    let arn = element(&created, "LoadBalancerArn");
    let published = element(&created, "DNSName");
    assert!(
        !arn.is_empty() && !published.is_empty(),
        "setup: the created load balancer reports no ARN or DNSName: {created}"
    );

    let target_group = element(
        &elbv2(
            router,
            "CreateTargetGroup",
            &[("Name", TARGET_GROUP), ("TargetType", "lambda")],
        )
        .await,
        "TargetGroupArn",
    );
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        &json!({
            "StatementId": STATEMENT,
            "Action": "lambda:InvokeFunction",
            "Principal": "elasticloadbalancing.amazonaws.com",
            "SourceArn": target_group,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission: expected 201, got {status} {granted}"
    );
    elbv2(
        router,
        "RegisterTargets",
        &[
            ("TargetGroupArn", target_group.as_str()),
            ("Targets.member.1.Id", function_arn),
        ],
    )
    .await;
    elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", arn.as_str()),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", target_group.as_str()),
        ],
    )
    .await;

    // The state the clock owes the load balancer, which a client waits for before sending
    // traffic: looked at until it holds.
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = elbv2(
            router,
            "DescribeLoadBalancers",
            &[("LoadBalancerArns.member.1", arn.as_str())],
        )
        .await;
        if described.contains("<Code>active</Code>") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the load balancer never became active, last answer {described}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
    (arn, published)
}

/// `cloudfront:CreateVpcOrigin` over the load balancer `endpoint_arn`, reached over HTTP alone,
/// polled until the VPC origin reports the terminal status the clock owes it. Answers its `Id`.
async fn deployed_vpc_origin(router: &MultiServiceRouter, name: &str, endpoint_arn: &str) -> String {
    let body = format!(
        "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <VpcOriginEndpointConfig>\
         <Name>{name}</Name><Arn>{endpoint_arn}</Arn>\
         <HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>http-only</OriginProtocolPolicy>\
         </VpcOriginEndpointConfig>\
         </CreateVpcOriginRequest>"
    );
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/vpc-origin"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup: CreateVpcOrigin must be accepted, got {} with {:?}",
        created.status, created.body
    );
    let id = element(&created.body, "Id");
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = cloudfront(
            router,
            "GET",
            &format!("/{API_VERSION}/vpc-origin/{id}"),
            Bytes::new(),
        )
        .await;
        assert_eq!(
            read.status, 200,
            "setup: GetVpcOrigin must be served, got {} with {:?}",
            read.status, read.body
        );
        if opt_element(&read.body, "Status").as_deref() == Some("Deployed") {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the VPC origin never reported Deployed, last answer {:?}",
            read.body
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create an origin request policy of the ACCOUNT's own whose header behaviour forwards every
/// viewer header and, beside them, the CloudFront-generated headers it LISTS — which is how a
/// distribution asks to be told the viewer's connection. Answers its minted id.
async fn created_origin_request_policy(router: &MultiServiceRouter, name: &str) -> String {
    let body = format!(
        "<OriginRequestPolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name>\
         <HeadersConfig>\
         <HeaderBehavior>allViewerAndWhitelistCloudFront</HeaderBehavior>\
         <Headers><Quantity>2</Quantity><Items>\
         <Name>{VIEWER_ADDRESS_HEADER}</Name><Name>{VIEWER_COUNTRY_HEADER}</Name>\
         </Items></Headers>\
         </HeadersConfig>\
         <CookiesConfig><CookieBehavior>all</CookieBehavior></CookiesConfig>\
         <QueryStringsConfig><QueryStringBehavior>all</QueryStringBehavior></QueryStringsConfig>\
         </OriginRequestPolicyConfig>"
    );
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/origin-request-policy"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateOriginRequestPolicy({name:?}) must succeed, got {} with {:?}",
        created.status, created.body
    );
    let id = element(&created.body, "Id");
    assert!(
        !id.is_empty(),
        "setup: the created origin request policy {name:?} reports no Id: {:?}",
        created.body
    );
    id
}

/// `cloudfront:CreateDistribution` over the VPC origin `vpc_origin_id`, whose one origin names
/// the load balancer's published address as its `DomainName`, with `origin_request_policy` on
/// its default behaviour (none where the behaviour names no policy at all). Polled until the
/// distribution reports `Deployed`; answers its `DomainName`.
async fn deployed_distribution(
    router: &MultiServiceRouter,
    caller_reference: &str,
    published: &str,
    vpc_origin_id: &str,
    origin_request_policy: Option<&str>,
) -> String {
    let policy = origin_request_policy
        .map(|id| format!("<OriginRequestPolicyId>{id}</OriginRequestPolicyId>"))
        .unwrap_or_default();
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>{ORIGIN_ID}</Id><DomainName>{published}</DomainName>\
         <CustomHeaders><Quantity>0</Quantity></CustomHeaders>\
         <VpcOriginConfig><VpcOriginId>{vpc_origin_id}</VpcOriginId></VpcOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>{policy}\
         </DefaultCacheBehavior>\
         <Comment>a lambda target behind an internal load balancer</Comment>\
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
        "setup: CreateDistribution({caller_reference:?}) must succeed, got {} with {:?}",
        created.status, created.body
    );
    let (id, domain) = (
        element(&created.body, "Id"),
        element(&created.body, "DomainName"),
    );
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "setup: the created distribution reports no Id or DomainName: {:?}",
        created.body
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = cloudfront(
            router,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            Bytes::new(),
        )
        .await;
        if opt_element(&read.body, "Status").as_deref() == Some("Deployed") {
            return domain;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the distribution never reported Deployed, last answer {:?}",
            read.body
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// How the matched behaviour's origin request policy NAMES the generated header, which is what
/// decides whether the origin is told the viewer's connection at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Naming {
    /// A policy of the account's own, listing the generated headers beside every viewer header.
    Listed,
    /// The AWS-managed policy that lists every CloudFront-generated header.
    Managed,
    /// No origin request policy at all.
    Unnamed,
}

/// One row: what the viewer states about its connection, how the behaviour's policy names the
/// generated header, and the ADDRESS half the origin must then be told.
struct Row {
    case: &'static str,
    naming: Naming,
    /// The viewer path asked for, one per row, so no row reads what another sent.
    path: &'static str,
    /// The headers the viewer states of its own accord.
    stated: &'static [(&'static str, &'static str)],
    /// The address the origin must be told, or `None` where the address the edge observes for a
    /// viewer that states nothing is what it reports — one value for every such row, which the
    /// rows hold it to.
    address: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a viewer that states nothing about where it came from",
        naming: Naming::Listed,
        path: "/stated-nothing",
        stated: &[],
        address: None,
    },
    Row {
        case: "a viewer whose proxy states its address",
        naming: Naming::Listed,
        path: "/stated-one",
        stated: &[(FORWARDED_FOR, "203.0.113.7")],
        address: Some("203.0.113.7"),
    },
    Row {
        case: "a proxy chain, whose first entry is the viewer",
        naming: Naming::Listed,
        path: "/stated-chain",
        stated: &[(FORWARDED_FOR, "198.51.100.2, 203.0.113.7")],
        address: Some("198.51.100.2"),
    },
    Row {
        case: "a stated value that is no address at all",
        naming: Naming::Listed,
        path: "/stated-nonsense",
        stated: &[(FORWARDED_FOR, "unknown")],
        address: None,
    },
    Row {
        case: "an IPv6 viewer",
        naming: Naming::Listed,
        path: "/stated-v6",
        stated: &[(FORWARDED_FOR, "2001:db8::1")],
        address: Some("2001:db8::1"),
    },
    Row {
        case: "a viewer that states a connection of its own for the generated header",
        naming: Naming::Listed,
        path: "/stated-spoof",
        stated: &[
            (VIEWER_ADDRESS, "203.0.113.9:1234"),
            (FORWARDED_FOR, "203.0.113.7"),
        ],
        address: Some("203.0.113.7"),
    },
    Row {
        case: "the managed policy that lists every CloudFront-generated header",
        naming: Naming::Managed,
        path: "/managed-policy",
        stated: &[(FORWARDED_FOR, "192.0.2.33")],
        address: Some("192.0.2.33"),
    },
    Row {
        case: "a behaviour whose policy names the generated header nowhere",
        naming: Naming::Unnamed,
        path: "/no-policy",
        stated: &[(FORWARDED_FOR, "192.0.2.33")],
        address: None,
    },
];

/// The two halves of a reported connection, read the way an origin reads them: an IPv6 literal
/// is bracketed, so its address half ends at the closing bracket, and every other address ends
/// at the last colon.
fn halves(case: &str, reported: &str) -> (String, String) {
    let (address, rest) = match reported.strip_prefix('[') {
        Some(tail) => {
            let end = tail.find(']').unwrap_or_else(|| {
                panic!("case {case:?}: expected a bracketed address in {reported:?}")
            });
            (tail[..end].to_string(), &tail[end + 1..])
        }
        None => {
            let at = reported.rfind(':').unwrap_or_else(|| {
                panic!("case {case:?}: expected the viewer's port after its address in {reported:?}")
            });
            (reported[..at].to_string(), &reported[at..])
        }
    };
    let port = rest
        .strip_prefix(':')
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the viewer's port after its address in {reported:?}")
        })
        .to_string();
    (address, port)
}

/// Assert that `reported` is one connection as the rule has the edge report it: the address
/// `expected` and a port of the viewer's own end of a connection.
fn assert_connection(case: &str, reported: &str, expected: &str) {
    let (address, port) = halves(case, reported);
    assert_eq!(
        address, expected,
        "case {case:?}: expected the origin to be told the address the edge observes \
         ({expected:?}), got {address:?} in {reported:?}"
    );
    assert!(
        address.parse::<::std::net::IpAddr>().is_ok(),
        "case {case:?}: expected an IP address in {reported:?}, got {address:?}"
    );
    let port = port.parse::<u16>().unwrap_or_else(|error| {
        panic!("case {case:?}: expected a port number in {reported:?} ({error})")
    });
    assert!(
        (FIRST_DYNAMIC_PORT..=LAST_DYNAMIC_PORT).contains(&port),
        "case {case:?}: expected the viewer's own end of the connection to be a dynamic port \
         ({FIRST_DYNAMIC_PORT}-{LAST_DYNAMIC_PORT}), got {port} in {reported:?}"
    );
}

/// The headers the ORIGIN was told about, off the target's own report of the request it served.
async fn headers_the_origin_saw(
    router: &MultiServiceRouter,
    row: &Row,
    domain: &str,
) -> serde_json::Map<String, Value> {
    let case = row.case;
    let answered = dispatch(
        router,
        "GET",
        domain,
        &format!("http://{domain}{path}", path = row.path),
        row.stated,
        Bytes::new(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the origin's answer through the edge, got {} with {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.header("x-cache"),
        MISS,
        "case {case:?}: expected an answer the origin produced for this request ({MISS:?}), got \
         X-Cache {:?} with body {:?}",
        answered.header("x-cache"),
        answered.body
    );
    let report: Value = serde_json::from_str(&answered.body).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the target's own report, got {:?} ({error})",
            answered.body
        )
    });
    assert_eq!(
        report["marker"].as_str().unwrap_or_default(),
        ORIGIN_MARKER,
        "case {case:?}: expected the target's own marker in {report}"
    );
    report["headers"]
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the target reported no headers at all ({report})"))
}

/// RULE: the edge tells an origin the viewer's whole CONNECTION — `<address>:<port>`, the
/// address it observes itself and the port the viewer's end of that connection has — for every
/// viewer request whose behaviour's origin request policy NAMES that header, and nothing of it
/// where no policy does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_edge_tells_an_alb_lambda_target_the_viewers_address_and_port() {
    let router = build_router();
    let function_arn = deployed_function(&router).await;
    let (load_balancer_arn, published) = load_balancer_over(&router, &function_arn).await;
    let vpc_origin = deployed_vpc_origin(&router, "cf-viewer-connection", &load_balancer_arn).await;

    // One distribution per way a behaviour names the generated header: the naming is what a
    // pairing is here, so rows differing only in what the viewer stated read the distribution
    // they belong to rather than a second copy of it.
    let listed = created_origin_request_policy(&router, "cf-viewer-connection-listed").await;
    let distributions: [(Naming, String); 3] = [
        (
            Naming::Listed,
            deployed_distribution(
                &router,
                "cf-viewer-connection-listed",
                &published,
                &vpc_origin,
                Some(&listed),
            )
            .await,
        ),
        (
            Naming::Managed,
            deployed_distribution(
                &router,
                "cf-viewer-connection-managed",
                &published,
                &vpc_origin,
                Some(MANAGED_ALL_VIEWER_AND_CLOUDFRONT),
            )
            .await,
        ),
        (
            Naming::Unnamed,
            deployed_distribution(
                &router,
                "cf-viewer-connection-unnamed",
                &published,
                &vpc_origin,
                None,
            )
            .await,
        ),
    ];

    // The address the edge observes for a viewer that states none: read off the first such row
    // and held to being the same for every other one.
    let mut observed = String::new();
    for row in ROWS {
        let case = row.case;
        let domain = distributions
            .iter()
            .find(|(naming, _)| *naming == row.naming)
            .map(|(_naming, domain)| domain.as_str())
            .unwrap_or_else(|| panic!("case {case:?}: the distribution this row reads was created"));
        let seen = headers_the_origin_saw(&router, row, domain).await;
        let reported = seen
            .get(VIEWER_ADDRESS)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if row.naming == Naming::Unnamed {
            // A behaviour whose policy names the header nowhere sends the origin none of it —
            // not even the value the viewer itself stated, which the edge drops either way.
            assert!(
                reported.is_empty(),
                "case {case:?}: expected the origin to be told no viewer connection at all, got \
                 {reported:?} among {seen:?}"
            );
            continue;
        }
        assert!(
            !reported.is_empty(),
            "case {case:?}: the origin was told no viewer connection at all ({seen:?})"
        );
        match row.address {
            Some(expected) => assert_connection(case, &reported, expected),
            None => {
                if observed.is_empty() {
                    observed = halves(case, &reported).0;
                }
                assert_connection(case, &reported, &observed);
            }
        }
    }

    // One viewer has ONE address: the same viewer asked twice is told the same address, whichever
    // connection each request arrived on, which is what an origin comparing two of its requests
    // reads. The PORT is the other half of a connection rather than of a viewer, so nothing here
    // holds two requests to one port.
    let again = "the same viewer asked a second time";
    let domain = distributions
        .iter()
        .find(|(naming, _)| *naming == Naming::Listed)
        .map(|(_naming, domain)| domain.as_str())
        .expect("the listed-policy distribution was created");
    let mut reported = Vec::new();
    for path in ["/again-first", "/again-second"] {
        let row = Row {
            case: again,
            naming: Naming::Listed,
            path,
            stated: &[(FORWARDED_FOR, "203.0.113.7")],
            address: Some("203.0.113.7"),
        };
        let seen = headers_the_origin_saw(&router, &row, domain).await;
        let value = seen
            .get(VIEWER_ADDRESS)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_connection(again, &value, "203.0.113.7");
        reported.push(value);
    }
    assert_eq!(
        halves(again, &reported[0]).0,
        halves(again, &reported[1]).0,
        "case {again:?}: expected one viewer to be told one address, got {:?} then {:?}",
        reported[0],
        reported[1]
    );
}
