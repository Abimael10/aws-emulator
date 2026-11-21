// SPDX-License-Identifier: CC0-1.0

//! A Route 53 ALIAS record set pointed at an Application Load Balancer, and the HTTPS listener
//! a client reaches through that alias name.
//!
//! The rule under test: **an `A`-type alias record set whose `AliasTarget` names what an
//! Elastic Load Balancing v2 load balancer published as its `DNSName` is answered, once the
//! change batch is applied, with the very ADDRESS that published name resolves to — and a
//! client that takes that answer and addresses the load balancer by the alias name is served
//! by the listener reached there, under its own rules.**
//!
//! `LoadBalancer$DNSName` is "the public DNS name of the load balancer" and
//! `AliasTarget$DNSName` is "the DNS domain name for another resource with which you want to
//! route traffic to … the DNS name that is associated with the load balancer", while
//! `RecordDataEntry` documents the answer for an alias to an Amazon Web Services resource as
//! "an IP address or a domain name for the … resource". So the two names agree on an address:
//! whatever a load balancer publishes for a client to reach it at, an alias to it resolves to
//! that same address. What an `A` record never carries is a PORT, which is why the dial below
//! takes the address out of the Route 53 answer and the port out of what the control plane
//! published — the whole of what a client on this machine needs (`Listeners` binds within the
//! operator's own range, `EMULATOR_LISTENER_PORT_RANGE`, and takes no requested port).
//!
//! Three crates have to agree for the rule to hold — EC2 owns the network, Elastic Load
//! Balancing v2 publishes the address and serves the listener, Route 53 resolves the alias —
//! so the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: how deep the aliased name stands
//! under the zone it lives in (a label under the apex, two labels, the apex itself), and —
//! for the request the listener answers — whether the request carries the header the gating
//! rule names. Every expected value is computed from what Elastic Load Balancing published,
//! never from the answer being checked.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The XML element readers and the form encoder this test crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every call of this suite is signed for, and the Region its zones live in.
const REGION: &str = "us-east-1";

/// The API versions the ec2Query and awsQuery requests carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The network this suite runs its load balancer in.
const VPC_CIDR: &str = "10.80.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.80.1.0/24", "us-east-1a"),
    ("10.80.2.0/24", "us-east-1b"),
];

/// How long a step waits for a transition the clock owes (the load balancer's `active`, the
/// certificate's `ISSUED`, the change batch's own propagation) and how often it looks again.
/// Generous on purpose: the deadline is here so a failure is a failure rather than a hang, and
/// a loaded machine is slower without the rule being any different.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// How long one dial of the published address may take. All of it is in this process.
const DIAL_DEADLINE: Duration = Duration::from_secs(30);

/// The `ChangeInfo$Status` a change batch reaches once every Route 53 name server has it.
const INSYNC: &str = "INSYNC";

/// The response codes a simulated query is answered with: a name the zone answers, and a name
/// it holds no record set for.
const NOERROR: &str = "NOERROR";
const NXDOMAIN: &str = "NXDOMAIN";

/// The header the listener's rule gates its `200` on, and the value it names.
const SECRET_HEADER: &str = "X-Origin-Token";
const SECRET_VALUE: &str = "a-shared-secret";

/// The statuses the listener answers with: its rule's for a request carrying the header, and
/// its default action's for one that does not.
const GATED_STATUS: u16 = 200;
const DEFAULT_STATUS: u16 = 403;

/// The bodies those two actions are created with, so an answer is attributed to the action it
/// came from rather than to its status alone.
const GATED_BODY: &str = "ALIAS-FRONTED-RULE-BODY";
const DEFAULT_BODY: &str = "ALIAS-FRONTED-DEFAULT-BODY";

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One member of a form body.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One form-encoded (query-protocol) call of `service`, as its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One EC2 action that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(String, String)]) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One ELBv2 action that must succeed. The credential scope names THIS model's own routing
/// key, which is how an ELBv2 request is addressed at the model rather than at the Classic one
/// it shares an endpoint with.
async fn elbv2(router: &MultiServiceRouter, action: &str, members: &[(String, String)]) -> String {
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

/// One Certificate Manager call that must succeed, answered with its parsed body.
async fn acm(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header("authorization", authorization("acm"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "setup: ACM {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Route 53 call over its restXml wire, as its status and its XML body.
async fn route53(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("route53.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("route53"))
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The network the load balancer runs in: a VPC with an internet gateway (an internet-facing
/// load balancer needs one) and two subnets in different zones, plus the group the load
/// balancer is placed in.
async fn seed_network(router: &MultiServiceRouter) -> (Vec<String>, String) {
    let vpc = element(
        &ec2(router, "CreateVpc", &[p("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[p("InternetGatewayId", &gateway), p("VpcId", &vpc)],
    )
    .await;
    let mut subnets = Vec::new();
    for (cidr, zone) in SUBNET_CIDRS {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    p("VpcId", &vpc),
                    p("CidrBlock", cidr),
                    p("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                p("GroupName", "alias-fronted-alb"),
                p("GroupDescription", "the group of the aliased load balancer"),
                p("VpcId", &vpc),
            ],
        )
        .await,
        "groupId",
    );
    (subnets, security_group)
}

/// A private certificate authority ARN of the form `CertificateAuthorityArn`'s pattern
/// requires. A request naming one needs no domain validation ("if you are requesting a private
/// certificate, domain validation is not required", `RequestCertificate` doc), so it is the
/// request that reaches `ISSUED` — the only status a listener can present a certificate in.
fn private_ca_arn() -> String {
    format!(
        "arn:aws:acm-pca:{REGION}:000000000000:certificate-authority\
         /12345678-1234-1234-1234-123456789012"
    )
}

/// Request one private certificate for `domain`, look at `DescribeCertificate` until
/// Certificate Manager reports it `ISSUED`, and answer the ARN it minted.
async fn issued_certificate(router: &MultiServiceRouter, domain: &str) -> String {
    let requested = acm(
        router,
        "RequestCertificate",
        &json!({ "DomainName": domain, "CertificateAuthorityArn": private_ca_arn() }),
    )
    .await;
    let arn = requested["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: RequestCertificate must mint an ARN, got {requested}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = acm(
            router,
            "DescribeCertificate",
            &json!({ "CertificateArn": arn }),
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
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The address one internet-facing Application Load Balancer publishes, whose HTTPS listener
/// answers [`DEFAULT_STATUS`] by default and [`GATED_STATUS`] for a request carrying
/// [`SECRET_HEADER`] — reported once the load balancer carries the state the clock owes it,
/// which is what a client waits for before sending traffic.
async fn alias_fronted_load_balancer(
    router: &MultiServiceRouter,
    name: &str,
    domain: &str,
    subnets: &[String],
    security_group: &str,
) -> String {
    let certificate = issued_certificate(router, domain).await;
    let created = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            p("Name", name),
            p("Type", "application"),
            p("Scheme", "internet-facing"),
            p("Subnets.member.1", &subnets[0]),
            p("Subnets.member.2", &subnets[1]),
            p("SecurityGroups.member.1", security_group),
        ],
    )
    .await;
    let arn = element(&created, "LoadBalancerArn");
    let published = element(&created, "DNSName");
    let listener = element(
        &elbv2(
            router,
            "CreateListener",
            &[
                p("LoadBalancerArn", &arn),
                p("Protocol", "HTTPS"),
                p("Port", "443"),
                p("Certificates.member.1.CertificateArn", &certificate),
                p("DefaultActions.member.1.Type", "fixed-response"),
                p(
                    "DefaultActions.member.1.FixedResponseConfig.StatusCode",
                    &DEFAULT_STATUS.to_string(),
                ),
                p(
                    "DefaultActions.member.1.FixedResponseConfig.MessageBody",
                    DEFAULT_BODY,
                ),
            ],
        )
        .await,
        "ListenerArn",
    );
    elbv2(
        router,
        "CreateRule",
        &[
            p("ListenerArn", &listener),
            p("Priority", "10"),
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
                &GATED_STATUS.to_string(),
            ),
            p("Actions.member.1.FixedResponseConfig.MessageBody", GATED_BODY),
        ],
    )
    .await;

    let deadline = Instant::now() + BUDGET;
    loop {
        let described = elbv2(
            router,
            "DescribeLoadBalancers",
            &[p("LoadBalancerArns.member.1", &arn)],
        )
        .await;
        if described.contains("<Code>active</Code>") {
            // The address is the load balancer's own, not one call's answer.
            assert_eq!(
                element(&described, "DNSName"),
                published,
                "setup: DescribeLoadBalancers must report the address the create published, \
                 got {described}"
            );
            return published;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the load balancer never became active, last answer {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `CanonicalHostedZoneId` of the load balancer `name`, read back through
/// `DescribeLoadBalancers` — the zone id an alias to it has to name.
async fn canonical_hosted_zone_id(router: &MultiServiceRouter, name: &str) -> String {
    let described = elbv2(router, "DescribeLoadBalancers", &[p("Names.member.1", name)]).await;
    opt_element(&described, "CanonicalHostedZoneId").unwrap_or_else(|| {
        panic!("setup: the load balancer must report a CanonicalHostedZoneId, got {described}")
    })
}

/// The `host:port` a load balancer published, split into the ADDRESS a resolver answers for it
/// and the port a client dials — which is the reading that makes the published value one of
/// this emulator's own addresses rather than a name in Elastic Load Balancing's own zone.
fn published_address(case: &str, published: &str) -> SocketAddr {
    let (host, port) = published.rsplit_once(':').unwrap_or_else(|| {
        panic!(
            "case {case:?}: the published address {published:?} names no port at all, so no \
             client can dial it"
        )
    });
    let address: IpAddr = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the published address {published:?} does not name an address a \
                 client can reach: {error}"
            )
        });
    let port: u16 = port.parse().unwrap_or_else(|error| {
        panic!("case {case:?}: {published:?} carries no port a client can dial: {error}")
    });
    SocketAddr::new(address, port)
}

/// Create the hosted zone `zone` and answer its id.
async fn hosted_zone(
    router: &MultiServiceRouter,
    case: &str,
    zone: &str,
    reference: &str,
) -> String {
    let (status, created) = route53(
        router,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest \
             xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><Name>{zone}</Name>\
             <CallerReference>{reference}</CallerReference></CreateHostedZoneRequest>"
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateHostedZone, got {status} with {created}"
    );
    element(&created, "Id")
        .trim_start_matches("/hostedzone/")
        .to_string()
}

/// `UPSERT` the `A` alias record set that points `name` at `target` in the zone `target_zone_id`
/// names, and look at `GetChange` until the batch reports the status that means every name
/// server has it.
async fn alias_record(
    router: &MultiServiceRouter,
    case: &str,
    zone_id: &str,
    name: &str,
    target: &str,
    target_zone_id: &str,
) {
    let (status, changed) = route53(
        router,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
        &format!(
            "<ChangeResourceRecordSetsRequest \
             xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><ChangeBatch><Changes>\
             <Change><Action>UPSERT</Action><ResourceRecordSet><Name>{name}</Name><Type>A</Type>\
             <AliasTarget><HostedZoneId>{target_zone_id}</HostedZoneId><DNSName>{target}</DNSName>\
             <EvaluateTargetHealth>false</EvaluateTargetHealth></AliasTarget>\
             </ResourceRecordSet></Change></Changes></ChangeBatch>\
             </ChangeResourceRecordSetsRequest>"
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ChangeResourceRecordSets, got {status} with {changed}"
    );
    let change_id = element(&changed, "Id")
        .trim_start_matches("/change/")
        .to_string();
    let mut reported = element(&changed, "Status");
    let deadline = Instant::now() + BUDGET;
    while reported != INSYNC {
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the change to reach {INSYNC}, got {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
        let (status, read) =
            route53(router, "GET", &format!("/2013-04-01/change/{change_id}"), "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetChange, got {status} with {read}"
        );
        reported = element(&read, "Status");
    }
}

/// What a resolver is answered for `name` in the zone `zone_id`: the response code and the
/// record data of an `A` query.
async fn resolved(
    router: &MultiServiceRouter,
    case: &str,
    zone_id: &str,
    name: &str,
) -> (String, Vec<String>) {
    let (status, answered) = route53(
        router,
        "GET",
        &format!(
            "/2013-04-01/testdnsanswer?hostedzoneid={zone_id}&recordname={}&recordtype=A",
            form_encode(name)
        ),
        "",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from TestDNSAnswer, got {status} with {answered}"
    );
    (
        opt_element(&answered, "ResponseCode").unwrap_or_default(),
        elements(&answered, "RecordDataEntry"),
    )
}

/// What one answer on the alias name carried.
#[derive(Debug)]
struct Answered {
    status: u16,
    body: String,
}

/// Dial the load balancer by the ALIAS NAME, resolving that name to `address` exactly as a
/// client that read the Route 53 answer does, and send one request for `/` carrying `headers`.
///
/// The port is in the URL because an `A` record carries none: a client on this machine takes
/// the address from DNS and the port from the control plane. The client does not verify the
/// certificate the listener presents — what is under test here is what the listener ANSWERS;
/// which certificate it holds is the ELBv2 crate's own suite.
async fn dial(
    case: &str,
    alias: &str,
    address: SocketAddr,
    headers: &[(&'static str, &'static str)],
) -> Answered {
    let url = format!("https://{alias}:{port}/", port = address.port());
    let named = case.to_string();
    let dialled = url.clone();
    let alias = alias.to_string();
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let client = ::reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .resolve(&alias, address)
            .redirect(::reqwest::redirect::Policy::none())
            .timeout(DIAL_DEADLINE)
            .build()
            .expect("a TLS client");
        let mut request = client.get(&dialled);
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        let answered = request.send().unwrap_or_else(|error| {
            panic!("case {named:?}: the alias name {dialled} answered nothing: {error}")
        });
        let status = answered.status().as_u16();
        let body = answered.text().unwrap_or_default();
        Answered { status, body }
    })
    .await
    .unwrap_or_else(|error| panic!("case {case:?}: the dial of {url} did not finish: {error}"))
}

/// One row: the zone the alias record set lives in and the name it stands at inside it.
struct Row {
    case: &'static str,
    /// The hosted zone the alias record set is written in.
    zone: &'static str,
    /// The name the alias record set stands at, which is the name a client addresses.
    alias: &'static str,
    /// The name of the load balancer this row creates, and the name its certificate covers.
    load_balancer: &'static str,
    /// The response code an `A` query for that name is answered with BEFORE the alias record
    /// set is written. A name the zone holds nothing at does not exist, which is
    /// [`NXDOMAIN`]; the zone's own apex exists — it carries the `NS` and `SOA` record sets
    /// Route 53 creates with the zone — so an `A` query there is the "no answer" case, a
    /// [`NOERROR`] response with no record data.
    code_before: &'static str,
}

#[tokio::test]
async fn an_alias_to_a_load_balancer_resolves_to_the_address_it_publishes() {
    let rows: &[Row] = &[
        Row {
            case: "a label under the zone's apex",
            zone: "alias-alb.example.com",
            alias: "app.alias-alb.example.com",
            load_balancer: "alias-alb-one",
            code_before: NXDOMAIN,
        },
        Row {
            case: "two labels under the apex",
            zone: "deep-alias-alb.example.com",
            alias: "api.eu.deep-alias-alb.example.com",
            load_balancer: "alias-alb-two",
            code_before: NXDOMAIN,
        },
        Row {
            case: "the zone's own apex",
            zone: "apex-alias-alb.example.com",
            alias: "apex-alias-alb.example.com",
            load_balancer: "alias-alb-apex",
            code_before: NOERROR,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let (subnets, security_group) = seed_network(&router).await;
        let published = alias_fronted_load_balancer(
            &router,
            row.load_balancer,
            row.alias,
            &subnets,
            &security_group,
        )
        .await;
        // The address the load balancer's OWN published name resolves to, and the port a
        // client dials it on.
        let address = published_address(case, &published);
        let target_zone = canonical_hosted_zone_id(&router, row.load_balancer).await;
        let zone_id = hosted_zone(&router, case, row.zone, row.alias).await;

        // 1. Before the change, the zone answers no address for the name: what makes the name
        //    resolvable is the alias record set, not the zone's existence.
        let (code, before) = resolved(&router, case, &zone_id, row.alias).await;
        assert_eq!(
            (code.as_str(), before.as_slice()),
            (row.code_before, &[][..]),
            "case {case:?}: expected {} and no record data before the alias is written, got \
             {code:?} with {before:?}",
            row.code_before
        );

        // 2. The alias record set a client writes to publish a friendly name for the load
        //    balancer, exactly as AWS documents the association.
        alias_record(&router, case, &zone_id, row.alias, &published, &target_zone).await;

        // 3. THE RULE: the alias answers with the address the load balancer's own published
        //    name resolves to — the two names share an address, and the answer carries no
        //    port.
        let (code, answer) = resolved(&router, case, &zone_id, row.alias).await;
        assert_eq!(
            code, NOERROR,
            "case {case:?}: expected {NOERROR}, got {code:?}"
        );
        assert_eq!(
            answer,
            vec![address.ip().to_string()],
            "case {case:?}: expected the alias to resolve to the address the load balancer \
             published ({published}), got {answer:?}"
        );

        // 4. And the address really is the load balancer's: a client that resolves the alias
        //    name to it is answered by the listener reached there, by its rule for a request
        //    carrying the gating header and by its default action for one without.
        let gated = dial(case, row.alias, address, &[(SECRET_HEADER, SECRET_VALUE)]).await;
        assert_eq!(
            (gated.status, gated.body.trim()),
            (GATED_STATUS, GATED_BODY),
            "case {case:?}: expected the rule's own answer for a request carrying \
             {SECRET_HEADER}, got {gated:?}"
        );
        let ungated = dial(case, row.alias, address, &[]).await;
        assert_eq!(
            (ungated.status, ungated.body.trim()),
            (DEFAULT_STATUS, DEFAULT_BODY),
            "case {case:?}: expected the listener's default action for a request carrying no \
             {SECRET_HEADER}, got {ungated:?}"
        );
    }
}
