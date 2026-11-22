// SPDX-License-Identifier: CC0-1.0

//! A CloudFront distribution over a VPC ORIGIN whose endpoint is an INTERNAL Application Load
//! Balancer, end to end across the two services on one engine.
//!
//! Two rules, each observable only where both services are mounted together — the VPC origin,
//! the distribution and its origin are CloudFront's records, and the thing that answers the
//! origin fetch is Elastic Load Balancing's listener:
//!
//! 1. **the address a load balancer publishes as `LoadBalancer$DNSName` — "the public DNS name
//!    of the load balancer", the value a client is handed and sends its traffic to — is an
//!    address this emulator ANSWERS on, whatever the load balancer's `Scheme`.** `Scheme`
//!    decides which clients may reach it in a real network ("the nodes of an internal load
//!    balancer have only private IP addresses … internal load balancers can route requests
//!    only from clients with access to the VPC for the load balancer"), not whether the
//!    published value is the address it is served at: in this emulator every such client — a
//!    CloudFront VPC origin among them — reaches the load balancer at the one address the
//!    emulator serves it on, so an internal load balancer publishes it exactly as a public one
//!    does. A client that honours the published value is answered by the listener it reached.
//! 2. **a VPC-origin fetch is addressed by the VPC ORIGIN's own endpoint configuration.** An
//!    origin that names a VPC origin carries no `CustomOriginConfig`: `Origin$VpcOriginConfig`
//!    carries only "the VPC origin ID", and the `VpcOriginEndpointConfig` the VPC origin
//!    resource holds states the `OriginProtocolPolicy` and the two ports the endpoint is
//!    reached on — `http-only` being "CloudFront uses only HTTP to access the origin". So the
//!    edge fetches such an origin over the protocol that configuration names (rather than over
//!    HTTPS, which is how an origin that states no configuration at all — an Amazon S3 one —
//!    is reached), and the answer the viewer is served is the load balancer's own: the
//!    listener's rule answers the origin request that carries the origin's `CustomHeaders`
//!    ("a list of HTTP header names and values that CloudFront adds to the requests that it
//!    sends to the origin"), and its default action answers the one that does not.
//!
//! The rows span what the two rules generalise over: WHICH client addresses the load balancer
//! (the published authority itself, and the reserved path namespace naming the load balancer),
//! whether the request carries the secret header its rule matches on, the viewer path the edge
//! is asked for (the distribution's root and a path deeper inside it), and — for the VPC
//! origin — the two protocol policies an endpoint on these ports can be reached under.
//!
//! Every clock-owed move is POLLED for until it holds, never slept for, and each deadline is
//! generous: a row asserts that the move HAPPENS, not how fast this host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for the wire suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode, opt_element};

/// The Region every setup call is signed for.
const REGION: &str = "us-west-2";

/// The API version the Elastic Load Balancing v2 query protocol carries.
const ELB_VERSION: &str = "2015-12-01";

/// CloudFront's control-plane host, and the API version its routes are under.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";
const API_VERSION: &str = "2020-05-31";

/// The header the listener's rule matches on, and the value that matches it: the standard way
/// an origin behind a distribution admits the edge and refuses a viewer that bypassed it.
const SECRET_HEADER: &str = "x-origin-secret";
const SECRET_VALUE: &str = "vpc-origin-secret-value";

/// What the listener's RULE answers — the origin's own document — and what its DEFAULT action
/// answers a request carrying no (or a wrong) secret.
const MARKER_BODY: &str = "vpc-origin-marker-served-by-the-internal-alb";
const DENIED_BODY: &str = "request-without-origin-secret-denied";
const MARKER_STATUS: u16 = 200;
const DENIED_STATUS: u16 = 403;

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row's answer is the origin's own rather
/// than an entry a previous row left in the edge cache.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The id the distribution's one origin carries, which its default behaviour targets.
const ORIGIN_ID: &str = "alb-vpc-origin";

/// The reserved path namespace and the segment that name the load balancer data plane, which a
/// client addresses on any host: `/_aws/elasticloadbalancing/{name}/{path}`.
const DATA_PLANE_PREFIX: &str = "/_aws/elasticloadbalancing";

/// A host that names no service of the emulator, which is what the prefixed addressing is read
/// on — its point being that it is read whatever the request's host is.
const NO_SERVICE_HOST: &str = "127.0.0.1";

/// How long a wait for work the CLOCK owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: nothing here measures a wait, only what came back.
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

/// The two services this suite exercises, mounted on one engine. Request authentication is
/// off: a setup call carries a credential scope (which is what names the Region and routes it)
/// without a computed signature, and a VIEWER request carries no credential at all, as a
/// client of a distribution — or of a load balancer — sends none.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes a
/// setup call. The signature itself is never checked here.
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

/// One member of a form body.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One Elastic Load Balancing v2 call, as its status and its XML body.
async fn elbv2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let host = format!("elasticloadbalancingv2.{REGION}.amazonaws.com");
    let mut body = format!("Action={action}&Version={ELB_VERSION}");
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
            ("authorization", &authorization("elasticloadbalancingv2")),
        ],
        Bytes::from(body),
    )
    .await;
    (answered.status, answered.body)
}

/// One Elastic Load Balancing v2 call that must be served.
async fn elbv2_ok(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> String {
    let (status, body) = elbv2(router, action, members).await;
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

/// Create one INTERNAL Application Load Balancer, and answer its ARN and the value it
/// publishes as its `DNSName` — asserting the create and a later `DescribeLoadBalancers` report
/// the same one, because the published address is the load balancer's own and not one call's
/// answer.
async fn internal_load_balancer(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = elbv2_ok(
        router,
        "CreateLoadBalancer",
        &[
            p("Name", name),
            p("Type", "application"),
            p("Scheme", "internal"),
            p("Subnets.member.1", "subnet-11111111"),
            p("Subnets.member.2", "subnet-22222222"),
        ],
    )
    .await;
    let arn = element(&created, "LoadBalancerArn");
    let published = element(&created, "DNSName");
    let described = elbv2_ok(
        router,
        "DescribeLoadBalancers",
        &[p("LoadBalancerArns.member.1", &arn)],
    )
    .await;
    assert_eq!(
        element(&described, "DNSName"),
        published,
        "setup {name}: a read must report the address the create published, got {described}"
    );
    (arn, published)
}

/// Create the HTTP:80 listener of `lb_arn`: its DEFAULT action is the denial, and one rule at
/// priority 1 answers the marker for a request carrying the secret header.
async fn secret_gated_listener(router: &MultiServiceRouter, lb_arn: &str) {
    let created = elbv2_ok(
        router,
        "CreateListener",
        &[
            p("LoadBalancerArn", lb_arn),
            p("Protocol", "HTTP"),
            p("Port", "80"),
            p("DefaultActions.member.1.Type", "fixed-response"),
            p(
                "DefaultActions.member.1.FixedResponseConfig.StatusCode",
                &DENIED_STATUS.to_string(),
            ),
            p(
                "DefaultActions.member.1.FixedResponseConfig.MessageBody",
                DENIED_BODY,
            ),
            p(
                "DefaultActions.member.1.FixedResponseConfig.ContentType",
                "text/plain",
            ),
        ],
    )
    .await;
    let listener_arn = element(&created, "ListenerArn");
    elbv2_ok(
        router,
        "CreateRule",
        &[
            p("ListenerArn", &listener_arn),
            p("Priority", "1"),
            p("Conditions.member.1.Field", "http-header"),
            p(
                "Conditions.member.1.HttpHeaderConfig.HttpHeaderName",
                SECRET_HEADER,
            ),
            p(
                "Conditions.member.1.HttpHeaderConfig.Values.member.1",
                SECRET_VALUE,
            ),
            p("Actions.member.1.Type", "fixed-response"),
            p(
                "Actions.member.1.FixedResponseConfig.StatusCode",
                &MARKER_STATUS.to_string(),
            ),
            p(
                "Actions.member.1.FixedResponseConfig.MessageBody",
                MARKER_BODY,
            ),
            p(
                "Actions.member.1.FixedResponseConfig.ContentType",
                "text/plain",
            ),
        ],
    )
    .await;
}

/// Look at `DescribeLoadBalancers` until the load balancer reports the state the clock owes it,
/// which is what a client waits for before sending traffic.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, arn: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = elbv2_ok(
            router,
            "DescribeLoadBalancers",
            &[p("LoadBalancerArns.member.1", arn)],
        )
        .await;
        if body.contains("<Code>active</Code>") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the load balancer never became active, last answer {body}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `cloudfront:CreateVpcOrigin` over the load balancer `endpoint_arn`, with the protocol policy
/// `policy` on the standard ports, polled until the VPC origin reports the terminal status the
/// clock owes it. Answers its `Id`.
async fn deployed_vpc_origin(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    endpoint_arn: &str,
    policy: &str,
) -> String {
    let body = format!(
        "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <VpcOriginEndpointConfig>\
         <Name>{name}</Name><Arn>{endpoint_arn}</Arn>\
         <HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>{policy}</OriginProtocolPolicy>\
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
        "case {case:?}: CreateVpcOrigin must be accepted, got {} with {:?}",
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
            "case {case:?}: GetVpcOrigin must be served, got {} with {:?}",
            read.status, read.body
        );
        if opt_element(&read.body, "Status").as_deref() == Some("Deployed") {
            // The endpoint configuration is read back from the VPC origin's own record, so
            // what the edge is addressed by below is what this service stored.
            assert_eq!(
                element(&read.body, "OriginProtocolPolicy"),
                policy,
                "case {case:?}: the stored VPC origin must hold the policy it was created \
                 with, got {:?}",
                read.body
            );
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the VPC origin never reported Deployed, last answer {:?}",
            read.body
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `cloudfront:CreateDistribution` over the VPC origin `vpc_origin_id`, whose one origin names
/// the load balancer's published address as its `DomainName`, and carries the secret header
/// among its `CustomHeaders` when `with_secret`. Answers the distribution's `(Id, DomainName)`.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    published: &str,
    vpc_origin_id: &str,
    with_secret: bool,
) -> (String, String) {
    let custom_headers = if with_secret {
        format!(
            "<CustomHeaders><Quantity>1</Quantity><Items><OriginCustomHeader>\
             <HeaderName>{SECRET_HEADER}</HeaderName><HeaderValue>{SECRET_VALUE}</HeaderValue>\
             </OriginCustomHeader></Items></CustomHeaders>"
        )
    } else {
        "<CustomHeaders><Quantity>0</Quantity></CustomHeaders>".to_string()
    };
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>{ORIGIN_ID}</Id><DomainName>{published}</DomainName>\
         {custom_headers}\
         <VpcOriginConfig><VpcOriginId>{vpc_origin_id}</VpcOriginId></VpcOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>an internal load balancer behind a vpc origin</Comment>\
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
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status, created.body
    );
    let (id, domain) = (
        element(&created.body, "Id"),
        element(&created.body, "DomainName"),
    );
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "case {case:?}: the created distribution reports no Id or DomainName: {:?}",
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
            return (id, domain);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the distribution never reported Deployed, last answer {:?}",
            read.body
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// How a client addresses the LOAD BALANCER directly, which is one dimension the first test's
/// rows differ along.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// The authority the control plane published, sent as the request's `Host` — which is what
    /// a client that honours the published value sends.
    Published,
    /// The reserved path namespace naming this load balancer, on a host that names no service.
    PrefixedPath,
}

/// One row of the direct-to-the-load-balancer table: how the client addresses it, whether the
/// request carries the secret header, and what the listener must answer.
struct DirectRow {
    case: &'static str,
    addressing: Addressing,
    path: &'static str,
    secret: Option<&'static str>,
    status: u16,
    body: &'static str,
}

const DIRECT_ROWS: &[DirectRow] = &[
    DirectRow {
        case: "the published authority, carrying the secret the rule matches",
        addressing: Addressing::Published,
        path: "/",
        secret: Some(SECRET_VALUE),
        status: MARKER_STATUS,
        body: MARKER_BODY,
    },
    DirectRow {
        case: "the published authority, carrying no secret at all",
        addressing: Addressing::Published,
        path: "/",
        secret: None,
        status: DENIED_STATUS,
        body: DENIED_BODY,
    },
    DirectRow {
        case: "the published authority, carrying a secret the rule does not match",
        addressing: Addressing::Published,
        path: "/deep/page",
        secret: Some("not-the-secret"),
        status: DENIED_STATUS,
        body: DENIED_BODY,
    },
    DirectRow {
        case: "the published authority deeper in the path, carrying the secret",
        addressing: Addressing::Published,
        path: "/deep/page",
        secret: Some(SECRET_VALUE),
        status: MARKER_STATUS,
        body: MARKER_BODY,
    },
    DirectRow {
        case: "the reserved path namespace naming the load balancer, carrying the secret",
        addressing: Addressing::PrefixedPath,
        path: "/",
        secret: Some(SECRET_VALUE),
        status: MARKER_STATUS,
        body: MARKER_BODY,
    },
    DirectRow {
        case: "the reserved path namespace naming the load balancer, carrying no secret",
        addressing: Addressing::PrefixedPath,
        path: "/deep/page",
        secret: None,
        status: DENIED_STATUS,
        body: DENIED_BODY,
    },
];

/// Send one request to the load balancer the way `row` addresses it.
async fn direct(
    router: &MultiServiceRouter,
    row: &DirectRow,
    name: &str,
    published: &str,
) -> Answer {
    let headers: Vec<(&str, &str)> = row
        .secret
        .map(|value| vec![(SECRET_HEADER, value)])
        .unwrap_or_default();
    match row.addressing {
        Addressing::Published => {
            dispatch(
                router,
                "GET",
                published,
                &format!("http://{published}{path}", path = row.path),
                &headers,
                Bytes::new(),
            )
            .await
        }
        Addressing::PrefixedPath => {
            dispatch(
                router,
                "GET",
                NO_SERVICE_HOST,
                &format!(
                    "http://{NO_SERVICE_HOST}{DATA_PLANE_PREFIX}/{name}{path}",
                    path = row.path
                ),
                &headers,
                Bytes::new(),
            )
            .await
        }
    }
}

/// RULE 1: the address an INTERNAL load balancer publishes is an address this emulator answers
/// on, so a client that honours it reaches the listener — which answers from its own rules.
#[tokio::test]
async fn an_internal_load_balancer_publishes_the_address_it_is_served_at() {
    let router = emulator();
    let name = "vpc-origin-direct-alb";
    let (arn, published) = internal_load_balancer(&router, name).await;
    assert!(
        published.parse::<std::net::SocketAddr>().is_ok(),
        "case {name:?}: the published DNSName must be an address a client can dial, got \
         {published:?}"
    );
    secret_gated_listener(&router, &arn).await;
    wait_until_active(&router, name, &arn).await;

    for row in DIRECT_ROWS {
        let answered = direct(&router, row, name, &published).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected the listener's HTTP {}, got {} with body {:?}",
            row.case, row.status, answered.status, answered.body
        );
        assert_eq!(
            answered.body, row.body,
            "case {:?}: expected the listener's body {:?}, got {:?}",
            row.case, row.body, answered.body
        );
    }
}

/// One row of the edge table: the protocol policy the VPC ORIGIN's endpoint configuration
/// states, whether the distribution's origin adds the secret header to every origin request,
/// the viewer path, and what the viewer must be served.
struct EdgeRow {
    case: &'static str,
    policy: &'static str,
    with_secret: bool,
    path: &'static str,
    status: u16,
    body: &'static str,
}

const EDGE_ROWS: &[EdgeRow] = &[
    EdgeRow {
        case: "an http-only vpc origin whose origin requests carry the secret",
        policy: "http-only",
        with_secret: true,
        path: "/",
        status: MARKER_STATUS,
        body: MARKER_BODY,
    },
    EdgeRow {
        case: "the same origin, for a path deeper inside the distribution",
        policy: "http-only",
        with_secret: true,
        path: "/deep/page",
        status: MARKER_STATUS,
        body: MARKER_BODY,
    },
    EdgeRow {
        case: "an http-only vpc origin whose origin requests carry no secret",
        policy: "http-only",
        with_secret: false,
        path: "/",
        status: DENIED_STATUS,
        body: DENIED_BODY,
    },
    EdgeRow {
        case: "a match-viewer vpc origin whose origin requests carry the secret",
        policy: "match-viewer",
        with_secret: true,
        path: "/",
        status: MARKER_STATUS,
        body: MARKER_BODY,
    },
    EdgeRow {
        case: "a match-viewer vpc origin whose origin requests carry no secret",
        policy: "match-viewer",
        with_secret: false,
        path: "/deep/page",
        status: DENIED_STATUS,
        body: DENIED_BODY,
    },
];

/// RULE 2: the edge fetches a VPC origin at the address its endpoint names, over the protocol
/// that endpoint's configuration states — so what the viewer is served through the
/// distribution is the internal load balancer's own answer to the origin request.
#[tokio::test]
async fn the_edge_serves_what_the_internal_load_balancer_answers_the_vpc_origin_fetch() {
    let router = emulator();
    let name = "vpc-origin-edge-alb";
    let (arn, published) = internal_load_balancer(&router, name).await;
    secret_gated_listener(&router, &arn).await;
    wait_until_active(&router, name, &arn).await;

    // One VPC origin per protocol policy the rows name, and one distribution per
    // (policy, custom headers) pairing: the policy is the VPC ORIGIN's own and the headers are
    // the distribution ORIGIN's, so those two are what a pairing is, and a row that differs
    // only in the viewer path reads the pairing it belongs to rather than a second copy of it.
    let mut pairings: Vec<(&str, bool, String)> = Vec::new();
    for row in EDGE_ROWS {
        if pairings
            .iter()
            .any(|(policy, with_secret, _)| *policy == row.policy && *with_secret == row.with_secret)
        {
            continue;
        }
        let index = pairings.len();
        let vpc_origin = deployed_vpc_origin(
            &router,
            row.case,
            &format!("vpc-origin-{index}"),
            &arn,
            row.policy,
        )
        .await;
        let (_id, domain) = create_distribution(
            &router,
            row.case,
            &format!("vpc-origin-edge-{index}"),
            &published,
            &vpc_origin,
            row.with_secret,
        )
        .await;
        pairings.push((row.policy, row.with_secret, domain));
    }

    for row in EDGE_ROWS {
        let case = row.case;
        let domain = pairings
            .iter()
            .find(|(policy, with_secret, _)| {
                *policy == row.policy && *with_secret == row.with_secret
            })
            .map(|(_policy, _with_secret, domain)| domain.as_str())
            .unwrap_or_else(|| panic!("case {case:?}: the pairing this row reads was created"));
        let viewer = dispatch(
            &router,
            "GET",
            domain,
            &format!("http://{domain}{path}", path = row.path),
            &[],
            Bytes::new(),
        )
        .await;
        assert_eq!(
            viewer.status, row.status,
            "case {case:?}: expected the origin's HTTP {} through the edge, got {} with body \
             {:?}",
            row.status, viewer.status, viewer.body
        );
        assert_eq!(
            viewer.body, row.body,
            "case {case:?}: expected the load balancer's own body {:?} through the edge, got \
             {:?}",
            row.body, viewer.body
        );
        // For an answer the origin produced that is NOT an error, the edge reports a miss:
        // the origin was reached and this request is what went to it. An origin answer that
        // is itself an error is reported as one CloudFront did not serve from its cache
        // either ("Error from cloudfront", the same outcome a `502` the edge produced itself
        // carries), so there the discriminator is the body asserted above — the load
        // balancer's own denial, which only a fetch that reached it can carry.
        if row.status == MARKER_STATUS {
            assert_eq!(
                viewer.header("x-cache"),
                "Miss from cloudfront",
                "case {case:?}: expected the answer to be reported as the origin's, got X-Cache \
                 {:?} with body {:?}",
                viewer.header("x-cache"),
                viewer.body
            );
        }
    }
}
