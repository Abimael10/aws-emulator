// SPDX-License-Identifier: CC0-1.0

//! A distribution over a VPC ORIGIN is reached at the address it PUBLISHED, and the origin
//! behind it is an internal Application Load Balancer whose listener rule requires the origin
//! header CloudFront was told to send.
//!
//! RULE: **`Distribution$DomainName` is "the domain name that CloudFront assigns when you
//! create a distribution", so it names an address THIS emulator answers at for the client that
//! read it — a host, optionally carrying the port it answers on, and never a value with a URI
//! path inside it — and a viewer composing `https://<DomainName>/<object>` from that value
//! reaches THAT distribution's edge.** What the edge then does is the
//! distribution's own configuration: `Origin$CustomHeaders` is "a header that you want
//! CloudFront to send to your origin", so every origin request carries it, and an internal load
//! balancer whose listener rule matches that header answers the edge with the rule's own fixed
//! response while a caller that reaches the load balancer directly without it gets the
//! listener default's refusal instead. That difference is the whole point of the arrangement:
//! only the distribution gets through.
//!
//! Why this suite lives here: the hop crosses three services on one engine — EC2 holds the
//! network the load balancer is placed in, Elastic Load Balancing opens the address the VPC
//! origin names and evaluates the listener's rules, and CloudFront mints the address, holds the
//! distribution and performs the origin fetch — so nothing but the assembled emulator can show
//! it. The sibling [`crate::cloudfront_viewer_connection_reaches_an_alb_lambda_target`] drives
//! the same topology over a `lambda` target group; this one drives the listener's own
//! fixed-response rules, where the ORIGIN HEADER is what decides the answer.
//!
//! The rows differ along what the rule generalises over: the caller (the viewer through the
//! edge, and a client that reaches the load balancer's own published address directly) crossed
//! with what the request says about the secret header (nothing, a wrong value, the value the
//! rule requires).
//!
//! Every clock-owed move is POLLED for until it holds, never slept for, and each deadline is
//! generous: a row asserts that the move HAPPENS, not how fast this host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for the wire suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::cloudfront_oac_s3_origin_edge::is_published_edge_address;
use crate::query_wire::{element, form_encode, opt_element};

/// The Region every setup call is signed for.
const REGION: &str = "us-east-1";

/// The API versions the query protocols carry, and CloudFront's own route prefix.
const EC2_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";
const API_VERSION: &str = "2020-05-31";

/// CloudFront's control-plane host.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";

/// The load balancer, the VPC origin and the distribution's one origin id.
const LOAD_BALANCER: &str = "cf-vpc-origin-header-alb";
const VPC_ORIGIN: &str = "cf-vpc-origin-header";
const ORIGIN_ID: &str = "alb-vpc-origin";

/// The header the listener's rule requires and the value only the distribution is told to
/// send: the standard way an origin admits the edge and nobody else.
const SECRET_HEADER: &str = "x-origin-verify";
const SECRET_VALUE: &str = "s3cr3t-origin-token";

/// The body the rule answers with, and the one the listener's DEFAULT action answers a caller
/// the rule does not match.
const RULE_BODY: &str = "ALB-RULE-MARKER";
const DENIAL_BODY: &str = "ALB-DEFAULT-FORBIDDEN";

/// The statuses each of those actions answers: `FixedResponseActionConfig$StatusCode` takes
/// "the HTTP response code (2XX, 4XX, or 5XX)".
const RULE_STATUS: u16 = 200;
const DENIAL_STATUS: u16 = 403;

/// The object a viewer asks the edge for. The path is the viewer's own and is appended to the
/// origin's own address, so the load balancer sees it too.
const VIEWER_PATH: &str = "/sim2real/resource";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row reads what the edge fetched for THAT
/// request rather than bytes an earlier row left in the cache.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// `X-Cache` for an answer the origin produced for this very request.
const MISS: &str = "Miss from cloudfront";

/// The network the load balancer is placed in, created through EC2 itself: an INTERNAL load
/// balancer is the scheme a VPC origin fronts, and such a one needs no internet gateway.
const VPC_CIDR: &str = "10.91.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.91.1.0/24", "us-east-1a"),
    ("10.91.2.0/24", "us-east-1b"),
];

/// How long a wait for work the CLOCK owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: everything waited for happens in this process, so a
/// loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

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

/// The endpoint this emulator is served at, recorded as the core records it on boot.
const SERVED_AT: &str = "https://127.0.0.1:9297";

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
    let (status, body) = query(router, "elasticloadbalancingv2", ELB_VERSION, action, members).await;
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

/// Create the INTERNAL load balancer, the HTTP:80 listener whose DEFAULT action refuses every
/// caller, and the priority-1 rule that answers the one carrying the secret header. Answers the
/// load balancer's `(ARN, DNSName)` once the clock has made it active.
async fn load_balancer(router: &MultiServiceRouter) -> (String, String) {
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
            ("Actions.member.1.Type", "fixed-response"),
            (
                "Actions.member.1.FixedResponseConfig.StatusCode",
                &RULE_STATUS.to_string(),
            ),
            ("Actions.member.1.FixedResponseConfig.ContentType", "text/plain"),
            ("Actions.member.1.FixedResponseConfig.MessageBody", RULE_BODY),
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

/// `cloudfront:CreateDistribution` over the VPC origin `vpc_origin_id`, whose one origin names
/// the load balancer's published address and carries the secret header CloudFront is to send,
/// polled until the distribution reports `Deployed`. Answers its published `DomainName`.
async fn deployed_distribution(
    router: &MultiServiceRouter,
    published: &str,
    vpc_origin_id: &str,
) -> String {
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>cf-vpc-origin-header-rule</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>{ORIGIN_ID}</Id><DomainName>{published}</DomainName>\
         <CustomHeaders><Quantity>1</Quantity><Items><OriginCustomHeader>\
         <HeaderName>{SECRET_HEADER}</HeaderName><HeaderValue>{SECRET_VALUE}</HeaderValue>\
         </OriginCustomHeader></Items></CustomHeaders>\
         <VpcOriginConfig><VpcOriginId>{vpc_origin_id}</VpcOriginId></VpcOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>a vpc origin over an internal load balancer</Comment>\
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
        "setup: CreateDistribution must be accepted, got {} with {:?}",
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

/// The request a VIEWER handed the published `domain` sends: the host it states and the path it
/// asks for.
///
/// A domain name states no port, so the port the viewer dials is the one its client reaches this
/// emulator on — this suite's requests are dispatched in process, so the host is the published
/// name itself and the path is the object the viewer asked for.
fn viewer_request(domain: &str, object: &str) -> (String, String) {
    (domain.to_string(), object.to_string())
}

/// One row: who makes the request, and what it says about the secret header.
struct Row {
    case: &'static str,
    /// Whether the request goes through the distribution's published address (the edge adds the
    /// header itself) or straight to the load balancer's own.
    through_the_edge: bool,
    /// The value the DIRECT caller states for the secret header, where it states one.
    stated: Option<&'static str>,
    status: u16,
    body: &'static str,
}

/// The rows: the viewer through the edge, and a direct caller stating nothing, a wrong value and
/// the value the rule requires.
const ROWS: &[Row] = &[
    Row {
        case: "a viewer through the distribution's published address",
        through_the_edge: true,
        stated: None,
        status: RULE_STATUS,
        body: RULE_BODY,
    },
    Row {
        case: "a direct caller stating no origin header",
        through_the_edge: false,
        stated: None,
        status: DENIAL_STATUS,
        body: DENIAL_BODY,
    },
    Row {
        case: "a direct caller stating the wrong value",
        through_the_edge: false,
        stated: Some("not-the-token"),
        status: DENIAL_STATUS,
        body: DENIAL_BODY,
    },
    Row {
        case: "a direct caller stating the value the rule requires",
        through_the_edge: false,
        stated: Some(SECRET_VALUE),
        status: RULE_STATUS,
        body: RULE_BODY,
    },
];

/// The whole arrangement: the address the distribution publishes is one this emulator serves, a
/// viewer that composes a URL from it reaches the edge, and the edge's own origin request
/// carries the header the load balancer's rule requires — while a caller reaching that load
/// balancer directly is answered the listener default's refusal unless it states the header
/// itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_published_edge_reaches_a_vpc_origin_whose_rule_requires_the_origin_header() {
    let router = build_router();
    let (load_balancer_arn, published) = load_balancer(&router).await;
    let vpc_origin = deployed_vpc_origin(&router, &load_balancer_arn).await;
    let domain = deployed_distribution(&router, &published, &vpc_origin).await;

    // The published value is an address this emulator answers on — a host carrying the port
    // its edge answers on, whatever address the create was reached at — and a viewer composes
    // its URL from it.
    assert!(
        is_published_edge_address(&domain),
        "expected an address this emulator answers on (host:port), got {domain:?}"
    );

    for row in ROWS {
        let case = row.case;
        let (host, path, headers) = if row.through_the_edge {
            let (host, path) = viewer_request(&domain, VIEWER_PATH);
            (host, path, Vec::new())
        } else {
            let stated: Vec<(&str, &str)> = row
                .stated
                .map(|value| vec![(SECRET_HEADER, value)])
                .unwrap_or_default();
            (published.clone(), VIEWER_PATH.to_string(), stated)
        };
        let answered = dispatch(
            &router,
            "GET",
            &host,
            &format!("http://{host}{path}"),
            &headers,
            Bytes::new(),
        )
        .await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} from {host}{path}, got {} with {:?}",
            row.status, answered.status, answered.body
        );
        assert!(
            answered.body.contains(row.body),
            "case {case:?}: expected the answer to carry {:?}, got {:?}",
            row.body,
            answered.body
        );
        if row.through_the_edge {
            // The answer came through the EDGE for this very request, which is what says the
            // published address resolved to the distribution rather than to anything else.
            assert_eq!(
                answered.header("x-cache"),
                MISS,
                "case {case:?}: expected an answer the origin produced for this request \
                 ({MISS:?}), got X-Cache {:?} with {:?}",
                answered.header("x-cache"),
                answered.body
            );
            continue;
        }
        // A direct caller reaches the load balancer, which answers no `X-Cache` of CloudFront's.
        assert!(
            answered.header("x-cache").is_empty(),
            "case {case:?}: a direct caller is answered by the load balancer, not by an edge, \
             got X-Cache {:?}",
            answered.header("x-cache")
        );
    }
}

