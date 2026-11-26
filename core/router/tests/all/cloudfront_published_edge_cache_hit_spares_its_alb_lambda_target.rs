// SPDX-License-Identifier: CC0-1.0

//! The whole flow a client of a distribution follows — create it, wait for `Deployed`, read
//! `DomainName`, fetch `https://<DomainName>/<object>` — driven to the far end of an origin that
//! is an Application Load Balancer with a `lambda` target group, twice per object.
//!
//! RULE: **an address a service PUBLISHES for a client to dial is one this emulator answers at,
//! so `Distribution$DomainName` — "the domain name that CloudFront assigns when you create a
//! distribution", the value a viewer composes `https://<it>/<object>` from — is a host carrying
//! the port its edge answers on and never a value with a URI path inside it; and the edge
//! reached there answers from its own cache every request whose cache key the matched
//! behaviour's cache policy still holds an entry for, so the ORIGIN behind it runs once per
//! entry FILLED rather than once per viewer request.** The request that does reach the origin
//! carries what that behaviour's origin request policy names — the viewer's connection and its
//! country among the CloudFront-generated headers, and the forwarded-for chain every origin
//! request states — and an origin counting its own runs sees no run at all for a request the
//! cache answered.
//!
//! Why this suite lives here rather than in the cloudfront crate: the leg crosses five services
//! on one engine — CloudFormation archives the inline handler into a package this emulator runs,
//! IAM holds its execution role, Lambda runs it and mints the invocation id each run reports,
//! Elastic Load Balancing opens the address the VPC origin names, evaluates the listener rule
//! that admits only the edge's own origin header and invokes the function, and CloudFront mints
//! the address, holds the distribution and performs the origin fetch — so only the assembled
//! emulator shows whether a viewer request reached the function at all.
//!
//! WHICH run answered a request is read off the origin's own report of the invocation id it ran
//! under, so a second answer carrying the first answer's id came from the cache and not from the
//! function. The siblings [`crate::cloudfront_viewer_connection_reaches_an_alb_lambda_target`]
//! and [`crate::cloudfront_vpc_origin_alb_header_rule_at_the_published_edge`] pin what the edge
//! TELLS such an origin and which callers the listener rule admits; this one pins the published
//! address a client dials and the runs the cache spares the origin.
//!
//! The COUNTRY a row expects is the one value this emulator's edge resolves for a viewer
//! request: no geolocation database is consulted off this machine, so the edge reads the country
//! out of the one header CloudFront itself states it in where the viewer states a code there, and
//! reports the country of its own endpoint otherwise. That is this emulator's reading rather than
//! a modelled member, which is why the rows vary the country by what the viewer states rather
//! than by moving its address between continents, and why the unstated rows are held to the one
//! value every such request gets.
//!
//! The rows differ along what the rule generalises over: the cache key a request falls under
//! (one object, then another), how many times that key is asked for (the request that fills the
//! entry, and the ones answered from it), what the viewer states about itself (nothing, an
//! address, an address and a country — none of which this behaviour's cache key names), and
//! whether the behaviour's cache policy holds an entry at all (a policy that caches, and the
//! managed policy whose TTLs are all zero, under which every request is the origin's).
//!
//! Every clock-owed move is POLLED for until it holds, never slept for, and each deadline is
//! generous: a row asserts that the move HAPPENS, not how fast this host runs.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one reading of a stack settling, from the sibling that owns it.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
// The one reading of a published edge address every suite that reads one holds it to.
use crate::cloudfront_oac_s3_origin_edge::is_published_edge_address;
// The ONE percent-encoder and element readers this test crate keeps for the wire suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode, opt_element};

/// The Region every setup call is signed for.
const REGION: &str = "us-east-1";

/// The API versions the query protocols carry, and CloudFront's own route prefix.
const CFN_VERSION: &str = "2010-05-15";
const EC2_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";
const API_VERSION: &str = "2020-05-31";

/// CloudFront's control-plane host.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";

/// The endpoint this emulator is served at, recorded as the core records it on boot.
const SERVED_AT: &str = "https://127.0.0.1:9297";

/// The stack the execution role and the function are deployed as, and the names it declares
/// them under.
const STACK: &str = "cf-cache-hit-alb-target";
const ROLE: &str = "cf-cache-hit-target-exec";
const FUNCTION: &str = "cf-cache-hit-target";

/// The load balancer, the VPC origin, the `lambda` target group, and the statement the invoke
/// grant is written under.
const LOAD_BALANCER: &str = "cf-cache-hit-alb";
const VPC_ORIGIN: &str = "cf-cache-hit-vpc-origin";
const TARGET_GROUP: &str = "cf-cache-hit-tg";
const STATEMENT: &str = "elbv2-invoke";

/// The id the distribution's one origin carries, which its default behaviour targets.
const ORIGIN_ID: &str = "alb-vpc-origin";

/// The header the listener's rule requires and the value only the distribution is told to send:
/// the standard way an origin admits the edge and nobody else.
const SECRET_HEADER: &str = "x-origin-secret";
const SECRET_VALUE: &str = "s3cr3t-origin-token";

/// The body the listener's DEFAULT action answers a caller the rule does not match, and its
/// status: `FixedResponseActionConfig$StatusCode` takes "the HTTP response code (2XX, 4XX, or
/// 5XX)".
const DENIAL_BODY: &str = "ALB-DEFAULT-FORBIDDEN";
const DENIAL_STATUS: u16 = 403;

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day and
/// whose cache key names no header, cookie or query string: a second request for one object
/// falls under the entry the first one filled.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: the
/// behaviour that names it holds nothing, so every request under it is the origin's own.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The generated headers the distribution's origin request policy NAMES, spelled as CloudFront
/// documents them: the connection the origin is told about and the viewer's country beside it.
const VIEWER_ADDRESS_HEADER: &str = "CloudFront-Viewer-Address";
const VIEWER_COUNTRY_HEADER: &str = "CloudFront-Viewer-Country";

/// Those header names as an ORIGIN request carries them (lower case, as HTTP names are
/// compared), and the header a viewer that traversed a proxy states its own address in.
const VIEWER_ADDRESS: &str = "cloudfront-viewer-address";
const VIEWER_COUNTRY: &str = "cloudfront-viewer-country";
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The range RFC 6335 reserves for the end of a connection a CLIENT opens from — "the Dynamic
/// and/or Private Ports" — which is where the port of a viewer's own end of a connection lies.
const FIRST_DYNAMIC_PORT: u16 = 49_152;
const LAST_DYNAMIC_PORT: u16 = 65_535;

/// The two objects the viewers ask for, which are two cache keys of the caching behaviour.
const WARMUP: &str = "/sim2real/warmup";
const OTHER: &str = "/sim2real/report";

/// The marker the handler puts in every report of its own, so an answer that carries it came
/// from the origin and one that does not was produced short of it.
const ORIGIN_MARKER: &str = "ALB-LAMBDA-CACHE-TARGET-OK";

/// What `X-Cache` reports for an answer the origin produced for this very request, and for one
/// the edge cache held.
const MISS: &str = "Miss from cloudfront";
const HIT: &str = "Hit from cloudfront";

/// The network the load balancer is placed in, created through EC2 itself: an INTERNAL load
/// balancer is the scheme a VPC origin fronts, and such a one needs no internet gateway.
const VPC_CIDR: &str = "10.92.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.92.1.0/24", "us-east-1a"),
    ("10.92.2.0/24", "us-east-1b"),
];

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because nothing here asserts how fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a wait for work the CLOCK owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: everything waited for happens in this process, so a
/// loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the target runs: it reports the id of the invocation it is running under and the
/// headers of the request it was handed, by lower-cased name. The invocation id is what tells one
/// RUN from another, so an answer carrying an id a viewer already saw was not produced by a new
/// run. It imports only the standard library, so the report is the handler's own work.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    seen = {}
    for name, value in (event.get("headers") or {}).items():
        seen[str(name).lower()] = value
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({
            "marker": "ALB-LAMBDA-CACHE-TARGET-OK",
            "run": context.aws_request_id,
            "headers": seen,
        }),
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

/// The emulator as it is ASSEMBLED, on one engine, and SERVED at an endpoint as a started one
/// is: every address a service publishes then takes the form it takes in service.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request. A VIEWER request carries no credential at all,
    // as a client of a distribution sends none.
    router.set_request_authentication(false);
    ::emulator_server::EmulatorService::serving_at(&router, SERVED_AT);
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
/// itself. The inline source is archived into the deployment package by CloudFormation, which is
/// how a function whose handler is written here comes to exist at all.
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
                    "Timeout": FUNCTION_TIMEOUT,
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

/// The subnets of a VPC of this account that the load balancer is placed in.
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

/// Create the INTERNAL Application Load Balancer, its `lambda` target group over `function_arn`,
/// the grant that admits the invocation, and the HTTP:80 listener whose DEFAULT action refuses
/// every caller and whose priority-1 rule forwards the one carrying the origin header to the
/// group. Answers the load balancer's ARN and the address it publishes as its `DNSName`.
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
    let listener = elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", arn.as_str()),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "fixed-response"),
            (
                "DefaultActions.member.1.FixedResponseConfig.StatusCode",
                &DENIAL_STATUS.to_string(),
            ),
            (
                "DefaultActions.member.1.FixedResponseConfig.ContentType",
                "text/plain",
            ),
            (
                "DefaultActions.member.1.FixedResponseConfig.MessageBody",
                DENIAL_BODY,
            ),
        ],
    )
    .await;
    let listener_arn = element(&listener, "ListenerArn");
    assert!(
        !listener_arn.is_empty(),
        "setup: the created listener reports no ARN: {listener}"
    );
    elbv2(
        router,
        "CreateRule",
        &[
            ("ListenerArn", listener_arn.as_str()),
            ("Priority", "1"),
            ("Conditions.member.1.Field", "http-header"),
            (
                "Conditions.member.1.HttpHeaderConfig.HttpHeaderName",
                SECRET_HEADER,
            ),
            (
                "Conditions.member.1.HttpHeaderConfig.Values.member.1",
                SECRET_VALUE,
            ),
            ("Actions.member.1.Type", "forward"),
            ("Actions.member.1.TargetGroupArn", target_group.as_str()),
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
async fn deployed_vpc_origin(router: &MultiServiceRouter, endpoint_arn: &str) -> String {
    let body = format!(
        "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <VpcOriginEndpointConfig>\
         <Name>{VPC_ORIGIN}</Name><Arn>{endpoint_arn}</Arn>\
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
/// distribution asks to be told the viewer's connection and country. Answers its minted id.
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

/// What one distribution of this suite is made with: the caller reference it is created under,
/// the cache policy its default behaviour names, and the origin request policy it carries.
struct Configured<'a> {
    caller_reference: &'a str,
    cache_policy: &'a str,
    origin_request_policy: &'a str,
}

/// `cloudfront:CreateDistribution` over the VPC origin `vpc_origin_id`, whose one origin names
/// the load balancer's published address and carries the origin header the listener rule
/// requires, polled until the distribution reports `Deployed`. Answers its published
/// `DomainName`, which a viewer composes its URL from.
async fn deployed_distribution(
    router: &MultiServiceRouter,
    published: &str,
    vpc_origin_id: &str,
    configured: &Configured<'_>,
) -> String {
    let Configured {
        caller_reference,
        cache_policy,
        origin_request_policy,
    } = *configured;
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>{ORIGIN_ID}</Id><DomainName>{published}</DomainName>\
         <CustomHeaders><Quantity>1</Quantity><Items><OriginCustomHeader>\
         <HeaderName>{SECRET_HEADER}</HeaderName><HeaderValue>{SECRET_VALUE}</HeaderValue>\
         </OriginCustomHeader></Items></CustomHeaders>\
         <VpcOriginConfig><VpcOriginId>{vpc_origin_id}</VpcOriginId></VpcOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{cache_policy}</CachePolicyId>\
         <OriginRequestPolicyId>{origin_request_policy}</OriginRequestPolicyId>\
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
            // The address a client dials is the one BOTH payloads of this record publish:
            // the create's and the read a client waiting for `Deployed` made.
            assert_eq!(
                opt_element(&read.body, "DomainName").as_deref(),
                Some(domain.as_str()),
                "setup: GetDistribution must report the published address {domain:?}, got {:?}",
                read.body
            );
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

/// One row: which distribution the viewer dials, the object it asks for, what it states about
/// itself, and whether the edge must answer it from the cache.
struct Row {
    case: &'static str,
    /// Whether the distribution dialled is the one whose behaviour CACHES.
    caching: bool,
    /// The object asked for, which is this behaviour's cache key.
    object: &'static str,
    /// The headers the viewer states of its own accord.
    stated: &'static [(&'static str, &'static str)],
    /// Whether this request must be answered out of the edge cache — and so whether the origin
    /// runs for it at all.
    from_cache: bool,
    /// The address the origin must be told the viewer came from, for a request that reaches it.
    address: &'static str,
    /// The country the origin must be told the viewer is in, for a request that reaches it.
    country: &'static str,
}

/// The country the edge reports for a viewer that states none: a two-letter, uppercase code,
/// which is the form `CloudFront-Viewer-Country` carries whatever the viewer said.
const UNSTATED_COUNTRY: &str = "US";

/// The rows, in the order one client drives them. They differ along the cache key asked for, how
/// many times it is asked for, what the viewer states about itself, and whether the behaviour's
/// cache policy holds anything at all.
const ROWS: &[Row] = &[
    Row {
        case: "the first viewer request for an object, which fills the entry",
        caching: true,
        object: WARMUP,
        stated: &[(FORWARDED_FOR, "203.0.113.7")],
        from_cache: false,
        address: "203.0.113.7",
        country: UNSTATED_COUNTRY,
    },
    Row {
        case: "the same viewer request again, which the entry answers",
        caching: true,
        object: WARMUP,
        stated: &[(FORWARDED_FOR, "203.0.113.7")],
        from_cache: true,
        address: "203.0.113.7",
        country: UNSTATED_COUNTRY,
    },
    Row {
        case: "that object asked for from another country, which this cache key does not name",
        caching: true,
        object: WARMUP,
        stated: &[
            (FORWARDED_FOR, "198.51.100.9"),
            (VIEWER_COUNTRY, "DE"),
        ],
        from_cache: true,
        address: "198.51.100.9",
        country: "DE",
    },
    Row {
        case: "another object, whose cache key nothing has filled",
        caching: true,
        object: OTHER,
        stated: &[(FORWARDED_FOR, "203.0.113.7")],
        from_cache: false,
        address: "203.0.113.7",
        country: UNSTATED_COUNTRY,
    },
    Row {
        case: "that other object again",
        caching: true,
        object: OTHER,
        stated: &[(FORWARDED_FOR, "203.0.113.7")],
        from_cache: true,
        address: "203.0.113.7",
        country: UNSTATED_COUNTRY,
    },
    Row {
        case: "an object under a behaviour whose cache policy holds nothing",
        caching: false,
        object: WARMUP,
        stated: &[(FORWARDED_FOR, "192.0.2.33"), (VIEWER_COUNTRY, "DE")],
        from_cache: false,
        address: "192.0.2.33",
        country: "DE",
    },
    Row {
        case: "the same object again under that behaviour, which holds nothing either time",
        caching: false,
        object: WARMUP,
        stated: &[(FORWARDED_FOR, "192.0.2.33"), (VIEWER_COUNTRY, "DE")],
        from_cache: false,
        address: "192.0.2.33",
        country: "DE",
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

/// Assert that the origin was told this viewer's whole connection: the address `expected` and a
/// port of the viewer's own end of a connection.
fn assert_connection(case: &str, reported: &str, expected: &str) {
    let (address, port) = halves(case, reported);
    assert_eq!(
        address, expected,
        "case {case:?}: expected the origin to be told the address the edge observes \
         ({expected:?}), got {address:?} in {reported:?}"
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

/// Assert that `chain` is the forwarded-for list an origin behind a load balancer reads: every
/// entry an address, the viewer's own first, and the hop the load balancer itself saw after it.
fn assert_forwarded_chain(case: &str, chain: &str, viewer: &str) {
    let entries: Vec<&str> = chain.split(',').map(str::trim).collect();
    assert!(
        entries.len() >= 2,
        "case {case:?}: expected the viewer's address and the hop the load balancer saw in the \
         forwarded-for chain, got {chain:?}"
    );
    for entry in &entries {
        assert!(
            entry.parse::<::std::net::IpAddr>().is_ok(),
            "case {case:?}: expected every forwarded-for entry to be an address, got {entry:?} \
             in {chain:?}"
        );
    }
    assert_eq!(
        entries.first().copied().unwrap_or_default(),
        viewer,
        "case {case:?}: expected the viewer's own address first in the forwarded-for chain, got \
         {chain:?}"
    );
}

/// The report the origin produced for one answer — its invocation id and the headers it was
/// handed — or the panic that says the answer never came from the origin at all.
fn origin_report(case: &str, body: &str) -> (String, serde_json::Map<String, Value>) {
    let report: Value = serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: expected the target's own report, got {body:?} ({error})")
    });
    assert_eq!(
        report["marker"].as_str().unwrap_or_default(),
        ORIGIN_MARKER,
        "case {case:?}: expected the target's own marker in {report}"
    );
    let run = report["run"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the target reported no invocation id ({report})"))
        .to_string();
    let headers = report["headers"]
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the target reported no headers at all ({report})"));
    (run, headers)
}

/// One header of the request the origin was handed, empty where it was handed none.
fn seen(headers: &serde_json::Map<String, Value>, name: &str) -> String {
    headers
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// RULE: the published `DomainName` is an address this emulator answers at, and the edge behind
/// it answers from its own cache every request whose cache key it still holds an entry for — so
/// the Lambda target behind the load balancer runs once per entry filled and not once per
/// viewer request, while every request that does reach it carries the generated headers the
/// behaviour's origin request policy names.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_published_edge_answers_a_cache_hit_without_running_its_alb_lambda_target() {
    let router = build_router();
    let function_arn = deployed_function(&router).await;
    let (load_balancer_arn, published) = load_balancer_over(&router, &function_arn).await;
    let vpc_origin = deployed_vpc_origin(&router, &load_balancer_arn).await;
    let policy = created_origin_request_policy(&router, "cf-cache-hit-viewer-headers").await;

    // One distribution per cache policy under test: the policy is what a pairing is here, so
    // rows differing only in the object they ask for read the distribution they belong to.
    let caching = deployed_distribution(
        &router,
        &published,
        &vpc_origin,
        &Configured {
            caller_reference: "cf-cache-hit-caching",
            cache_policy: CACHING_OPTIMIZED,
            origin_request_policy: &policy,
        },
    )
    .await;
    let uncached = deployed_distribution(
        &router,
        &published,
        &vpc_origin,
        &Configured {
            caller_reference: "cf-cache-hit-uncached",
            cache_policy: CACHING_DISABLED,
            origin_request_policy: &policy,
        },
    )
    .await;

    // The locality rule, for every address this flow published: a client that read one dials it
    // as given, so it is a host carrying the port its edge answers on and carries no URI path.
    for (which, domain) in [("the caching distribution", &caching), ("the uncached one", &uncached)]
    {
        assert!(
            is_published_edge_address(domain),
            "case {which:?}: expected an address this emulator answers on (host:port), got \
             {domain:?}"
        );
    }

    // What filled each cache key: the invocation the entry's bytes were produced by, and those
    // bytes — so a later row answered from the entry is held to both.
    let mut filled: BTreeMap<(bool, &str), (String, String)> = BTreeMap::new();
    // Every invocation any row saw, which is what counts the runs this traffic cost the origin.
    let mut runs: BTreeSet<String> = BTreeSet::new();

    for row in ROWS {
        let case = row.case;
        let domain = if row.caching { &caching } else { &uncached };
        let answered = dispatch(
            &router,
            "GET",
            domain,
            &format!("http://{domain}{object}", object = row.object),
            row.stated,
            Bytes::new(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the object through the edge, got {} with {:?}",
            answered.status, answered.body
        );
        let expected_cache = if row.from_cache { HIT } else { MISS };
        assert_eq!(
            answered.header("x-cache"),
            expected_cache,
            "case {case:?}: expected X-Cache {expected_cache:?}, got {:?} with body {:?}",
            answered.header("x-cache"),
            answered.body
        );
        let (run, headers) = origin_report(case, &answered.body);
        let key = (row.caching, row.object);
        if row.from_cache {
            // The cache answered: the bytes are the ones the entry holds, down to the
            // invocation the origin ran under when it produced them — nothing ran for THIS
            // request.
            let (filled_by, held) = filled.get(&key).unwrap_or_else(|| {
                panic!("case {case:?}: no earlier row filled the entry this row reads")
            });
            assert_eq!(
                &run, filled_by,
                "case {case:?}: expected the entry's own answer, produced by invocation \
                 {filled_by:?}, got one produced by {run:?}"
            );
            assert_eq!(
                &answered.body, held,
                "case {case:?}: expected the bytes the entry holds, got {:?}",
                answered.body
            );
            continue;
        }
        // The origin ran for this request: a run no row has seen before, and the request it was
        // handed carries what the behaviour's policies name.
        assert!(
            runs.insert(run.clone()),
            "case {case:?}: expected a run of the origin's own, got the invocation {run:?} an \
             earlier row already saw"
        );
        assert_eq!(
            seen(&headers, SECRET_HEADER),
            SECRET_VALUE,
            "case {case:?}: expected the origin header the listener rule requires, got {:?}",
            seen(&headers, SECRET_HEADER)
        );
        assert_connection(case, &seen(&headers, VIEWER_ADDRESS), row.address);
        assert_eq!(
            seen(&headers, VIEWER_COUNTRY),
            row.country,
            "case {case:?}: expected the origin to be told the country {:?}, got {:?}",
            row.country,
            seen(&headers, VIEWER_COUNTRY)
        );
        assert_forwarded_chain(case, &seen(&headers, FORWARDED_FOR), row.address);
        filled.insert(key, (run, answered.body));
    }

    // The whole point, counted: the origin ran once per row that reached it and for no row the
    // cache answered — four runs for seven viewer requests.
    let reached = ROWS.iter().filter(|row| !row.from_cache).count();
    assert_eq!(
        runs.len(),
        reached,
        "expected the origin to run once per request the cache could not answer ({reached} of \
         {} viewer requests), got {} runs: {runs:?}",
        ROWS.len(),
        runs.len()
    );
}
