// SPDX-License-Identifier: CC0-1.0

//! A CloudFront distribution over an APPLICATION LOAD BALANCER named as a CUSTOM ORIGIN, end
//! to end on the assembled emulator.
//!
//! RULE: a custom origin is dialled over the protocol its `OriginProtocolPolicy` names —
//! `http-only` "always uses HTTP to connect to the origin", `https-only` always HTTPS, and
//! `match-viewer` "the same protocol that the viewer used to connect to CloudFront" — and the
//! edge serves the origin's own answer only where the origin's endpoint accepts a connection
//! of that kind. An endpoint that accepts none of it is an origin connection that cannot be
//! established, which CloudFront answers `504` for, carrying none of the origin's content.
//!
//! Beside it, the second rule the pair of services makes observable: `Origin$CustomHeaders` is
//! "a list of HTTP header names and values that CloudFront adds to the requests that it sends
//! to the origin", so the load balancer's own `http-header` rule — and not CloudFront's
//! network position — is what admits the edge and refuses a viewer that bypassed it.
//!
//! The rows span what the two rules generalise over: each of the three protocol policies, both
//! viewer schemes (which is what `match-viewer` follows), an origin endpoint that accepts
//! connections in the clear alone (a load balancer with an `HTTP` listener) and one that
//! accepts only encrypted ones (a load balancer with an `HTTPS` listener), whether the origin
//! adds the secret header to every origin request, and — for the direct caller — whether it
//! carries that header at all.
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

/// The API version the Amazon EC2 query protocol carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The API version the Elastic Load Balancing v2 query protocol carries.
const ELB_VERSION: &str = "2015-12-01";

/// CloudFront's control-plane host, and the API version its routes are under.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";
const API_VERSION: &str = "2020-05-31";

/// The header the listener's rule matches on, and the value that matches it: the standard way
/// an origin behind a distribution admits the edge and refuses a viewer that bypassed it.
const SECRET_HEADER: &str = "x-origin-secret";
const SECRET_VALUE: &str = "alb-origin-secret-value";

/// What the listener's RULE answers — the origin's own document — and what its DEFAULT action
/// answers a request carrying no (or a wrong) secret.
const MARKER_BODY: &str = "alb-origin-marker-served-by-the-listener";
const DENIED_BODY: &str = "request-without-origin-secret-denied";
const MARKER_STATUS: u16 = 200;
const DENIED_STATUS: u16 = 403;

/// What CloudFront answers for an origin connection it could not establish, which carries none
/// of the origin's own content.
const GATEWAY_TIMEOUT: u16 = 504;

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row's answer is the origin's own rather
/// than an entry a previous row left in the edge cache.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The ids the distribution's origins carry, one per protocol policy, and the path pattern each
/// non-default behaviour routes to its own.
const PLAIN_ORIGIN: &str = "alb-origin-http-only";
const TLS_ORIGIN: &str = "alb-origin-https-only";
const MATCH_ORIGIN: &str = "alb-origin-match-viewer";
const TLS_PATTERN: &str = "/tls/*";
const MATCH_PATTERN: &str = "/mv/*";

/// How long a wait for work the CLOCK owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: nothing here measures a wait, only what came back.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

/// The emulator as it is ASSEMBLED (`aws_emulator_router::build_router`), on one engine: the
/// distribution, the load balancer, the network it is placed in and the certificate its TLS
/// listener presents are four services' records, and every one of them is read where it lives.
/// Request authentication is off: a setup call carries a credential scope (which is what names
/// the Region and routes it) without a computed signature, and a VIEWER request carries no
/// credential at all, as a client of a distribution — or of a load balancer — sends none.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
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
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// One member of a form body.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One Elastic Load Balancing v2 call that must be served, as its XML body.
async fn elbv2_ok(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> String {
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
    assert_eq!(
        answered.status, 200,
        "setup: ELBv2 {action} must answer 200, got {} with body {}",
        answered.status, answered.body
    );
    answered.body
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

/// One Amazon EC2 call that must be served, as its XML body.
async fn ec2_ok(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let host = format!("ec2.{REGION}.amazonaws.com");
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
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
            ("authorization", &authorization("ec2")),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "setup: EC2 {action} must answer 200, got {} with body {}",
        answered.status, answered.body
    );
    answered.body
}

/// One Certificate Manager call that must be served, answered with its parsed body.
async fn acm_ok(
    router: &MultiServiceRouter,
    operation: &str,
    body: &::serde_json::Value,
) -> ::serde_json::Value {
    let host = format!("acm.{REGION}.amazonaws.com");
    let answered = dispatch(
        router,
        "POST",
        &host,
        &format!("https://{host}/"),
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("CertificateManager.{operation}")),
            ("authorization", &authorization("acm")),
        ],
        Bytes::from(::serde_json::to_vec(body).expect("the request body serializes")),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "setup: ACM {operation} must answer 200, got {} with {}",
        answered.status, answered.body
    );
    ::serde_json::from_str(&answered.body).unwrap_or_else(|error| {
        panic!(
            "setup: ACM {operation} must answer a JSON document, got {} ({error})",
            answered.body
        )
    })
}

/// Request one PRIVATE certificate for `domain` and answer the ARN Certificate Manager minted,
/// once it reports the certificate `ISSUED` — the one status a listener can present a
/// certificate in. A private request needs no domain validation ("if you are requesting a
/// private certificate, domain validation is not required", `RequestCertificate`), so it is the
/// request that reaches that status.
async fn issued_certificate(router: &MultiServiceRouter, domain: &str) -> String {
    let authority = format!(
        "arn:aws:acm-pca:{REGION}:000000000000:certificate-authority\
         /12345678-1234-1234-1234-123456789012"
    );
    let requested = acm_ok(
        router,
        "RequestCertificate",
        &::serde_json::json!({ "DomainName": domain, "CertificateAuthorityArn": authority }),
    )
    .await;
    let arn = requested["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: RequestCertificate must mint an ARN, got {requested}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = acm_ok(
            router,
            "DescribeCertificate",
            &::serde_json::json!({ "CertificateArn": arn }),
        )
        .await;
        let reported = described["Certificate"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == "ISSUED" {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "setup: expected Certificate Manager to report ISSUED for {arn}, got {reported:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The network an internet-facing load balancer is placed in, built through Amazon EC2 itself
/// so every id the placement carries is one EC2 handed out: a VPC with an internet gateway
/// attached (which is what a public placement needs), two subnets in two zones, and a security
/// group admitting `tcp` on `port` from the CloudFront origin-facing managed prefix list — the
/// standard way an origin behind a distribution is reached by the edge and by nothing else.
///
/// Answers `(subnet ids, security group id)`.
async fn public_network(
    router: &MultiServiceRouter,
    second_octet: u8,
    port: u16,
) -> (Vec<String>, String) {
    let vpc = element(
        &ec2_ok(
            router,
            "CreateVpc",
            &[("CidrBlock", &format!("10.{second_octet}.0.0/16"))],
        )
        .await,
        "vpcId",
    );
    let gateway = element(
        &ec2_ok(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2_ok(
        router,
        "AttachInternetGateway",
        &[("InternetGatewayId", &gateway), ("VpcId", &vpc)],
    )
    .await;
    let mut subnets = Vec::new();
    for (index, zone) in ["us-west-2a", "us-west-2b"].iter().enumerate() {
        let created = ec2_ok(
            router,
            "CreateSubnet",
            &[
                ("VpcId", &vpc),
                ("CidrBlock", &format!("10.{second_octet}.{}.0/24", index + 1)),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&created, "subnetId"));
    }
    let security_group = element(
        &ec2_ok(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", &format!("cf-alb-origin-{second_octet}")),
                ("GroupDescription", "admits the CloudFront origin-facing list"),
                ("VpcId", &vpc),
            ],
        )
        .await,
        "groupId",
    );
    // The AWS-managed prefix list of the addresses CloudFront reaches an origin from, which is
    // the only source this group admits: EC2 publishes it, so the id is one EC2 reported.
    let listed = ec2_ok(
        router,
        "DescribeManagedPrefixLists",
        &[
            ("Filter.1.Name", "prefix-list-name"),
            ("Filter.1.Value.1", "com.amazonaws.global.cloudfront.origin-facing"),
        ],
    )
    .await;
    let prefix_list = element(&listed, "prefixListId");
    assert!(
        !prefix_list.is_empty(),
        "setup: EC2 must publish the CloudFront origin-facing prefix list, got {listed}"
    );
    ec2_ok(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", &security_group),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", &port.to_string()),
            ("IpPermissions.1.ToPort", &port.to_string()),
            ("IpPermissions.1.PrefixListIds.1.PrefixListId", &prefix_list),
        ],
    )
    .await;
    (subnets, security_group)
}

/// Create one internet-facing Application Load Balancer whose single listener speaks
/// `protocol` on `port`, its DEFAULT action answering the denial and one rule at priority 1
/// answering the marker for a request carrying the secret header. Answers the `DNSName` it
/// publishes, once the clock owes it `active`.
async fn load_balancer(
    router: &MultiServiceRouter,
    name: &str,
    protocol: &str,
    port: u16,
    placement: &(Vec<String>, String),
) -> String {
    let (subnets, security_group) = placement;
    let mut create = vec![
        p("Name", name),
        p("Type", "application"),
        p("Scheme", "internet-facing"),
        p("IpAddressType", "ipv4"),
        p("SecurityGroups.member.1", security_group),
    ];
    for (index, subnet) in subnets.iter().enumerate() {
        create.push(p(&format!("Subnets.member.{}", index + 1), subnet));
    }
    let created = elbv2_ok(router, "CreateLoadBalancer", &create).await;
    let arn = element(&created, "LoadBalancerArn");
    let published = element(&created, "DNSName");
    let mut listener = vec![
        p("LoadBalancerArn", &arn),
        p("Protocol", protocol),
        p("Port", &port.to_string()),
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
    ];
    if protocol == "HTTPS" {
        let certificate = issued_certificate(router, "origin.example.com").await;
        listener.push(p("Certificates.member.1.CertificateArn", &certificate));
    }
    let created = elbv2_ok(router, "CreateListener", &listener).await;
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
    // Look at `DescribeLoadBalancers` until the load balancer reports the state the clock owes
    // it, which is what a client waits for before sending traffic.
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = elbv2_ok(
            router,
            "DescribeLoadBalancers",
            &[p("LoadBalancerArns.member.1", &arn)],
        )
        .await;
        if described.contains("<Code>active</Code>") {
            return published;
        }
        assert!(
            Instant::now() < deadline,
            "case {name:?}: the load balancer never became active, last answer {described}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One origin of the distribution below: the id it carries, the protocol policy it is dialled
/// under, and whether CloudFront adds the secret header to the requests it sends to it.
struct OriginSpec<'a> {
    id: &'a str,
    policy: &'a str,
    with_secret: bool,
}

/// The `<Origin>` element for one origin over the load balancer `published`, a CUSTOM origin on
/// the standard ports with the connection bounds the recorded configuration carries.
fn origin_element(published: &str, spec: &OriginSpec<'_>) -> String {
    let custom_headers = if spec.with_secret {
        format!(
            "<CustomHeaders><Quantity>1</Quantity><Items><OriginCustomHeader>\
             <HeaderName>{SECRET_HEADER}</HeaderName><HeaderValue>{SECRET_VALUE}</HeaderValue>\
             </OriginCustomHeader></Items></CustomHeaders>"
        )
    } else {
        "<CustomHeaders><Quantity>0</Quantity></CustomHeaders>".to_string()
    };
    format!(
        "<Origin><Id>{id}</Id><DomainName>{published}</DomainName>\
         {custom_headers}\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>{policy}</OriginProtocolPolicy>\
         </CustomOriginConfig>\
         <ConnectionAttempts>1</ConnectionAttempts><ConnectionTimeout>2</ConnectionTimeout>\
         </Origin>",
        id = spec.id,
        policy = spec.policy,
    )
}

/// `cloudfront:CreateDistribution` over the load balancer `published` with one origin per
/// entry of `origins`, the first being the default behaviour's target and the two path
/// patterns routing to the others. Answers the distribution's `DomainName`, once the clock owes
/// it `Deployed`.
async fn deployed_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    published: &str,
    origins: &[OriginSpec<'_>],
) -> String {
    let items: String = origins
        .iter()
        .map(|spec| origin_element(published, spec))
        .collect();
    let behaviors: String = origins
        .iter()
        .skip(1)
        .zip([TLS_PATTERN, MATCH_PATTERN])
        .map(|(spec, pattern)| {
            format!(
                "<CacheBehavior><PathPattern>{pattern}</PathPattern>\
                 <TargetOriginId>{id}</TargetOriginId>\
                 <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
                 <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
                 </CacheBehavior>",
                id = spec.id,
            )
        })
        .collect();
    let cache_behaviors = if behaviors.is_empty() {
        "<CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>".to_string()
    } else {
        format!(
            "<CacheBehaviors><Quantity>{count}</Quantity><Items>{behaviors}</Items>\
             </CacheBehaviors>",
            count = origins.len() - 1,
        )
    };
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>{count}</Quantity><Items>{items}</Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{default_origin}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         {cache_behaviors}\
         <CustomErrorResponses><Quantity>1</Quantity><Items><CustomErrorResponse>\
         <ErrorCode>504</ErrorCode><ErrorCachingMinTTL>0</ErrorCachingMinTTL>\
         </CustomErrorResponse></Items></CustomErrorResponses>\
         <Comment>an application load balancer behind a custom origin</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        count = origins.len(),
        default_origin = origins[0].id,
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
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetDistribution must be served, got {} with {:?}",
            read.status, read.body
        );
        if opt_element(&read.body, "Status").as_deref() == Some("Deployed") {
            return domain;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the distribution never reported Deployed, last answer {:?}",
            read.body
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One viewer request through the edge of the distribution `domain`, over `scheme`.
async fn viewer(router: &MultiServiceRouter, domain: &str, scheme: &str, path: &str) -> Answer {
    dispatch(
        router,
        "GET",
        domain,
        &format!("{scheme}://{domain}{path}"),
        &[],
        Bytes::new(),
    )
    .await
}

/// One row of the edge table: the protocol policy the behaviour's origin is dialled under, the
/// scheme the VIEWER dialled, the viewer path, and what the viewer must be served.
struct EdgeRow {
    case: &'static str,
    scheme: &'static str,
    path: &'static str,
    status: u16,
    body: Option<&'static str>,
}

/// The rows for the load balancer whose only listener speaks HTTP: a connection in the clear
/// reaches it, and an encrypted one reaches nothing.
const CLEARTEXT_ROWS: &[EdgeRow] = &[
    EdgeRow {
        case: "an http-only origin, viewer over HTTP",
        scheme: "http",
        path: "/plain/a",
        status: MARKER_STATUS,
        body: Some(MARKER_BODY),
    },
    EdgeRow {
        case: "the same http-only origin, viewer over HTTPS",
        scheme: "https",
        path: "/plain/a",
        status: MARKER_STATUS,
        body: Some(MARKER_BODY),
    },
    EdgeRow {
        case: "an https-only origin whose endpoint accepts no encrypted connection",
        scheme: "http",
        path: "/tls/a",
        status: GATEWAY_TIMEOUT,
        body: None,
    },
    EdgeRow {
        case: "the same https-only origin, viewer over HTTPS",
        scheme: "https",
        path: "/tls/a",
        status: GATEWAY_TIMEOUT,
        body: None,
    },
    EdgeRow {
        case: "a match-viewer origin, viewer over HTTP",
        scheme: "http",
        path: "/mv/a",
        status: MARKER_STATUS,
        body: Some(MARKER_BODY),
    },
    EdgeRow {
        case: "the same match-viewer origin, viewer over HTTPS",
        scheme: "https",
        path: "/mv/a",
        status: GATEWAY_TIMEOUT,
        body: None,
    },
];

/// The rows for the load balancer whose only listener terminates TLS: an encrypted connection
/// reaches it, and one in the clear reaches nothing.
const ENCRYPTED_ROWS: &[EdgeRow] = &[
    EdgeRow {
        case: "an http-only origin whose endpoint accepts no connection in the clear",
        scheme: "http",
        path: "/plain/a",
        status: GATEWAY_TIMEOUT,
        body: None,
    },
    EdgeRow {
        case: "an https-only origin whose endpoint terminates TLS",
        scheme: "http",
        path: "/tls/a",
        status: MARKER_STATUS,
        body: Some(MARKER_BODY),
    },
    EdgeRow {
        case: "a match-viewer origin, viewer over HTTPS",
        scheme: "https",
        path: "/mv/a",
        status: MARKER_STATUS,
        body: Some(MARKER_BODY),
    },
    EdgeRow {
        case: "the same match-viewer origin, viewer over HTTP",
        scheme: "http",
        path: "/mv/a",
        status: GATEWAY_TIMEOUT,
        body: None,
    },
];

/// Every origin of the distribution the tables above read, one per protocol policy, each adding
/// the secret header the listener's rule matches to its origin requests.
const SECRET_CARRYING_ORIGINS: &[OriginSpec<'static>] = &[
    OriginSpec {
        id: PLAIN_ORIGIN,
        policy: "http-only",
        with_secret: true,
    },
    OriginSpec {
        id: TLS_ORIGIN,
        policy: "https-only",
        with_secret: true,
    },
    OriginSpec {
        id: MATCH_ORIGIN,
        policy: "match-viewer",
        with_secret: true,
    },
];

/// Assert one table of rows against the distribution `domain`.
async fn assert_rows(router: &MultiServiceRouter, domain: &str, rows: &[EdgeRow]) {
    for row in rows {
        let case = row.case;
        let answered = viewer(router, domain, row.scheme, row.path).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} through the edge, got {} with body {:?}",
            row.status, answered.status, answered.body
        );
        match row.body {
            Some(expected) => assert_eq!(
                answered.body, expected,
                "case {case:?}: expected the load balancer's own body {expected:?} through the \
                 edge, got {:?}",
                answered.body
            ),
            None => assert!(
                !answered.body.contains(MARKER_BODY) && !answered.body.contains(DENIED_BODY),
                "case {case:?}: an answer for an origin connection that was never established \
                 must carry none of the origin's content, got {:?}",
                answered.body
            ),
        }
    }
}

/// RULE: the protocol policy decides which kind of connection the origin is dialled over, and
/// an endpoint accepting none of that kind is an origin connection CloudFront could not
/// establish — the `504` it answers for one, carrying none of the origin's content.
#[tokio::test]
async fn the_protocol_policy_decides_whether_the_origin_connection_can_be_established() {
    let router = emulator();
    let admits_plaintext = public_network(&router, 10, 80).await;
    let cleartext =
        load_balancer(&router, "cf-alb-origin-http", "HTTP", 80, &admits_plaintext).await;
    let domain = deployed_distribution(
        &router,
        "a load balancer listening in the clear",
        "alb-custom-origin-cleartext",
        &cleartext,
        SECRET_CARRYING_ORIGINS,
    )
    .await;
    assert_rows(&router, &domain, CLEARTEXT_ROWS).await;

    let admits_tls = public_network(&router, 20, 443).await;
    let encrypted =
        load_balancer(&router, "cf-alb-origin-https", "HTTPS", 443, &admits_tls).await;
    let domain = deployed_distribution(
        &router,
        "a load balancer terminating TLS",
        "alb-custom-origin-encrypted",
        &encrypted,
        SECRET_CARRYING_ORIGINS,
    )
    .await;
    assert_rows(&router, &domain, ENCRYPTED_ROWS).await;
}

/// RULE: `Origin$CustomHeaders` are added to every origin request, so the load balancer's own
/// header rule is what admits the edge — and refuses a caller that goes round it.
#[tokio::test]
async fn the_origins_custom_headers_are_what_the_listener_rule_admits() {
    let router = emulator();
    // The load balancer is placed in a network EC2 holds, reached by the edge alone: the whole
    // combination a client that fronts an ALB with CloudFront builds.
    let placement = public_network(&router, 80, 80).await;
    let published =
        load_balancer(&router, "cf-alb-origin-secret", "HTTP", 80, &placement).await;
    let with_secret = deployed_distribution(
        &router,
        "an origin carrying the secret header",
        "alb-custom-origin-with-secret",
        &published,
        &[OriginSpec {
            id: PLAIN_ORIGIN,
            policy: "http-only",
            with_secret: true,
        }],
    )
    .await;
    let without_secret = deployed_distribution(
        &router,
        "an origin carrying no custom header",
        "alb-custom-origin-without-secret",
        &published,
        &[OriginSpec {
            id: PLAIN_ORIGIN,
            policy: "http-only",
            with_secret: false,
        }],
    )
    .await;
    let rows: &[(&str, &str, u16, &str)] = &[
        (
            "through the edge of the distribution whose origin adds the secret",
            with_secret.as_str(),
            MARKER_STATUS,
            MARKER_BODY,
        ),
        (
            "through the edge of the distribution whose origin adds none",
            without_secret.as_str(),
            DENIED_STATUS,
            DENIED_BODY,
        ),
    ];
    for (case, domain, status, body) in rows {
        let answered = viewer(&router, domain, "http", "/plain/a").await;
        assert_eq!(
            answered.status, *status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.body, *body,
            "case {case:?}: expected the listener's own body {body:?}, got {:?}",
            answered.body
        );
    }

    // And a caller that goes round the distribution is decided by the same rule: the listener
    // refuses it for want of the header the edge adds, and serves it when it carries the value
    // the rule matches.
    let direct: &[(&str, Option<&str>, u16, &str)] = &[
        (
            "a direct caller carrying no secret header",
            None,
            DENIED_STATUS,
            DENIED_BODY,
        ),
        (
            "a direct caller carrying a value the rule does not match",
            Some("not-the-secret"),
            DENIED_STATUS,
            DENIED_BODY,
        ),
        (
            "a direct caller carrying the value the rule matches",
            Some(SECRET_VALUE),
            MARKER_STATUS,
            MARKER_BODY,
        ),
    ];
    for (case, secret, status, body) in direct {
        let headers: Vec<(&str, &str)> = secret
            .map(|value| vec![(SECRET_HEADER, value)])
            .unwrap_or_default();
        let answered = dispatch(
            &router,
            "GET",
            &published,
            &format!("http://{published}/plain/a"),
            &headers,
            Bytes::new(),
        )
        .await;
        assert_eq!(
            answered.status, *status,
            "case {case:?}: expected the listener's HTTP {status}, got {} with body {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.body, *body,
            "case {case:?}: expected the listener's own body {body:?}, got {:?}",
            answered.body
        );
    }
}
