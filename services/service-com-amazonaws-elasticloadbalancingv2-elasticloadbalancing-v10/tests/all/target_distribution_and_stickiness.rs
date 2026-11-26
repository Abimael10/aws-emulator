// SPDX-License-Identifier: CC0-1.0

//! How a `forward` distributes the requests of ONE target group ACROSS the registrations it
//! holds, end to end over the wire.
//!
//! The rule under test: **a `forward` whose chosen target group holds several registrations
//! the load balancer may send requests to distributes each request across them the way that
//! group's own `load_balancing.algorithm.type` names, and — where its `stickiness.enabled` is
//! on — pins a client to the registration its first request reached for the duration the
//! group states, under the `stickiness.type` it carries: the load balancer's own `AWSALB` and
//! `AWSALBCORS` cookies for `lb_cookie`, its `AWSALBAPP` cookie beside the application's own
//! cookie for `app_cookie`.** A group that is NOT sticky sends no two successive requests of
//! one client to one registration.
//!
//! Every attribute is written through `ModifyTargetGroupAttributes` and every request is a
//! data-plane request to the name the load balancer published, carrying no AWS credential —
//! exactly as a client that reads `DNSName` sends it. The two targets are two workloads of
//! the account at two addresses, each answering its own document, so WHICH registration
//! answered is readable off the body the client was sent.
//!
//! The rows differ along what the rule generalises over: each value of the algorithm
//! attribute, each cookie-based `stickiness.type`, a client holding the pin and a client
//! holding none, a pin the load balancer cannot decode, and — for the application-based kind
//! — the same pin with and without the application's own cookie beside it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ::service_com_amazonaws_ec2_amazonec2::ops::types as ec2_types;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_common::served_http_addresses::{AddressAnswer, ServedAddresses, ServesHttp};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::settle;
use crate::provisioning_loop::{action, call, emulator, first_text, members, p, REGION};

/// How long a step waits for the clock-owed move of a registration to `healthy`, and how
/// often it looks. Generous on purpose: the deadline is only here so a failure is a failure
/// rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The health-check cadence every group below is created with — the shortest
/// `HealthCheckIntervalSeconds` admits ("the range is 5-300") — and the fewest consecutive
/// successes `HealthyThresholdCount` admits, so a verdict arrives as soon as the schedule can
/// reach one.
const INTERVAL_SECONDS: &str = "5";
const HEALTHY_THRESHOLD: &str = "2";

/// The network the workloads sit in, and the port they listen on. An Application Load
/// Balancer is placed across two subnets in two zones ("at least two subnets in two
/// different Availability Zones must be specified"); the workloads sit in the first.
const VPC: &str = "vpc-0bb1b2c3d4e5f6071";
const SUBNETS: [(&str, &str, &str); 2] = [
    ("subnet-0bb1b2c3d4e5f6071", "10.81.1.0/24", "us-west-2a"),
    ("subnet-0bb1b2c3d4e5f6072", "10.81.2.0/24", "us-west-2b"),
];
const TARGET_PORT: i32 = 80;

/// The two workloads: the address each is reached at, and the document each answers with —
/// which is how a client reads WHICH registration answered its request.
const TARGETS: [(&str, &str); 2] = [("10.81.1.11", "served-by-blue"), ("10.81.1.12", "served-by-green")];

/// The duration each group's cookie-based stickiness is configured for, inside the range the
/// two attributes state ("1 second to 1 week").
const LB_COOKIE_SECONDS: &str = "3600";
const APP_COOKIE_SECONDS: &str = "600";

/// The application's own cookie an `app_cookie` group follows, named through
/// `stickiness.app_cookie.cookie_name`.
const APPLICATION_COOKIE_NAME: &str = "SESSION";

/// The load balancer's own cookies a pin travels in, as `stickiness.app_cookie.cookie_name`
/// reserves them: "Names that start with the following prefixes are not allowed: AWSALB,
/// AWSALBAPP, and AWSALBTG".
const DURATION_COOKIE: &str = "AWSALB";
const DURATION_CORS_COOKIE: &str = "AWSALBCORS";
const APPLICATION_PIN_COOKIE: &str = "AWSALBAPP";

/// One workload's own server: it answers every request with the one document that workload
/// holds.
struct Workload {
    document: &'static str,
}

impl ServesHttp for Workload {
    fn serve(&self, _method: &str, _path: &str) -> Option<AddressAnswer> {
        Some(AddressAnswer {
            status: 200,
            content_type: Some("text/plain".to_string()),
            body: self.document.as_bytes().to_vec(),
        })
    }
}

/// One answered data-plane request: its status, its body, and EVERY `Set-Cookie` field it
/// carries — the load balancer may hand back more than one.
struct Answered {
    status: u16,
    body: String,
    cookies: Vec<String>,
}

impl Answered {
    /// The value of the cookie `name` names, where the answer minted one.
    fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies.iter().find_map(|field| {
            field
                .strip_prefix(&format!("{name}="))
                .and_then(|rest| rest.split(';').next())
        })
    }

    /// The whole `Set-Cookie` field of the cookie `name` names, with its attributes.
    fn field(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|field| field.starts_with(&format!("{name}=")))
            .map(String::as_str)
    }
}

/// Send one data-plane request to the name `host` publishes, with the `Cookie` header `cookie`
/// states, carrying no credential.
async fn get(router: &MultiServiceRouter, host: &str, cookie: Option<&str>) -> Answered {
    let mut builder = ::http::Request::builder()
        .method("GET")
        .uri(format!("http://{host}/"))
        .header("host", host);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let request = builder
        .body(::bytes::Bytes::new())
        .expect("the data-plane request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_owned))
        .collect();
    let body = String::from_utf8_lossy(&response.into_body()).into_owned();
    Answered {
        status,
        body,
        cookies,
    }
}

/// Seed the account's EC2 inventory with the network the workloads sit in and one interface
/// per workload address, so each registration is one the emulator holds something behind and
/// the load balancer's own health checks report it `healthy`.
fn seed_network(engine: &Arc<engine::InMemoryEngine>) {
    let state = ec2::service_state(&**engine).expect("the shared engine holds an EC2 inventory");
    let mut store = state.lock();
    store.vpcs.insert(
        VPC.to_string(),
        ec2_types::Vpc {
            vpc_id: Some(VPC.to_string()),
            cidr_block: Some("10.81.0.0/16".to_string()),
            ..ec2_types::Vpc::default()
        },
    );
    store.subnets.insert(
        SUBNETS[0].0.to_string(),
        ec2_types::Subnet {
            subnet_id: Some(SUBNETS[0].0.to_string()),
            vpc_id: Some(VPC.to_string()),
            cidr_block: Some(SUBNETS[0].1.to_string()),
            availability_zone: Some(SUBNETS[0].2.to_string()),
            ..ec2_types::Subnet::default()
        },
    );
    store.subnets.insert(
        SUBNETS[1].0.to_string(),
        ec2_types::Subnet {
            subnet_id: Some(SUBNETS[1].0.to_string()),
            vpc_id: Some(VPC.to_string()),
            cidr_block: Some(SUBNETS[1].1.to_string()),
            availability_zone: Some(SUBNETS[1].2.to_string()),
            ..ec2_types::Subnet::default()
        },
    );
    for (address, _) in TARGETS {
        store
            .insert_network_interface(ec2::NetworkInterfaceSpec {
                subnet_id: SUBNETS[0].0.to_string(),
                vpc_id: VPC.to_string(),
                availability_zone: SUBNETS[0].2.to_string(),
                region: REGION.to_string(),
                description: "a workload of the account".to_string(),
                interface_type: ec2::NETWORK_INTERFACE_TYPE_INTERFACE.to_string(),
                private_ip_addresses: vec![address.to_string()],
                ..ec2::NetworkInterfaceSpec::default()
            })
            .expect("the inventory holds the interface");
    }
}

/// Create an `ip` target group holding both workloads, carrying `attributes`, and answer its
/// ARN.
async fn target_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    attributes: &[(&str, &str)],
) -> String {
    let mut form = action("CreateTargetGroup");
    form.push(p("Name", name));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", &TARGET_PORT.to_string()));
    form.push(p("TargetType", "ip"));
    form.push(p("VpcId", VPC));
    form.push(p("HealthCheckIntervalSeconds", INTERVAL_SECONDS));
    form.push(p("HealthyThresholdCount", HEALTHY_THRESHOLD));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from CreateTargetGroup, got {} body {}",
        reply.status, reply.body
    );
    let arn = reply
        .text("TargetGroupArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TargetGroupArn in {}", reply.body));
    if !attributes.is_empty() {
        let mut form = action("ModifyTargetGroupAttributes");
        form.push(p("TargetGroupArn", &arn));
        for (index, (key, value)) in attributes.iter().enumerate() {
            form.push(p(&format!("Attributes.member.{}.Key", index + 1), key));
            form.push(p(&format!("Attributes.member.{}.Value", index + 1), value));
        }
        let reply = call(router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200 from ModifyTargetGroupAttributes, got {} body {}",
            reply.status, reply.body
        );
        // What the plane performs is what a read of the group reports, so the write is read
        // back before any request is sent.
        let mut form = action("DescribeTargetGroupAttributes");
        form.push(p("TargetGroupArn", &arn));
        let reply = call(router, &form).await;
        let reported = reply.body.clone();
        for (key, value) in attributes {
            assert!(
                reported.contains(&format!("<Key>{key}</Key>"))
                    && reported.contains(&format!("<Value>{value}</Value>")),
                "case {case:?}: expected the collection to report {key}={value}, got {reported}"
            );
        }
    }
    for (address, _) in TARGETS {
        let mut form = action("RegisterTargets");
        form.push(p("TargetGroupArn", &arn));
        form.push(p("Targets.member.1.Id", address));
        let reply = call(router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200 from RegisterTargets, got {} body {}",
            reply.status, reply.body
        );
    }
    arn
}

/// Create an Application Load Balancer whose HTTP listener forwards to `tg_arn`, and answer
/// its ARN and the name it published.
async fn load_balancer(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    tg_arn: &str,
) -> (String, String) {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    for (index, (subnet, _, _)) in SUBNETS.iter().enumerate() {
        form.push(p(&format!("Subnets.member.{}", index + 1), subnet));
    }
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from CreateLoadBalancer, got {} body {}",
        reply.status, reply.body
    );
    let arn = reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("case {case:?}: no LoadBalancerArn in {}", reply.body));
    let dns_name = reply
        .text("DNSName")
        .unwrap_or_else(|| panic!("case {case:?}: no DNSName in {}", reply.body));
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", tg_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from CreateListener, got {} body {}",
        reply.status, reply.body
    );
    (arn, dns_name)
}

/// Look at the target group until every registration it holds reports `healthy` — the signal
/// a client waits for — rather than sleeping for the span its checks take.
async fn healthy(router: &MultiServiceRouter, case: &str, arn: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut form = action("DescribeTargetHealth");
        form.push(p("TargetGroupArn", arn));
        let reply = call(router, &form).await;
        let states: Vec<String> = members(&reply.body, "TargetHealthDescriptions")
            .iter()
            .map(|description| first_text(description, "State").unwrap_or_default())
            .collect();
        let settled = states.len() == TARGETS.len() && states.iter().all(|state| state == "healthy");
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected every registration to report healthy, got {states:?}"
            );
            return;
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One target group a case addresses: the case it belongs to, the group's own name, and the
/// attributes it is configured with.
type Configured<'g> = (&'g str, &'g str, Vec<(&'g str, &'g str)>);

/// One deployment: the router, the handles keeping the two workloads' servers listening, and
/// the published name of each load balancer a case addresses.
struct Deployment {
    router: MultiServiceRouter,
    /// Held because dropping a handle withdraws the workload's server.
    _listening: Vec<::service_common::served_http_addresses::ServedAddressHandle>,
    /// The load balancer in front of a group nothing configured: the documented default
    /// algorithm, stickiness off.
    round_robin: String,
    /// The one in front of a `least_outstanding_requests` group.
    least_outstanding: String,
    /// The one in front of a `weighted_random` group.
    weighted_random: String,
    /// The one in front of a group with duration-based stickiness.
    lb_cookie: String,
    /// The one in front of a group with application-based stickiness.
    app_cookie: String,
}

/// Everything the cases are answered out of, written through this service's own operations
/// over the network and the workloads the account holds.
async fn deployment() -> Deployment {
    let (router, engine) = emulator();
    seed_network(&engine);
    let addresses = ServedAddresses::of(&*engine).expect("the engine vends the served addresses");
    let listening = TARGETS
        .iter()
        .map(|(address, document)| {
            addresses.publish(address, TARGET_PORT, Arc::new(Workload { document }))
        })
        .collect();

    let groups: Vec<Configured<'_>> = vec![
        ("round robin", "dist-round-robin", Vec::new()),
        (
            "least outstanding requests",
            "dist-least-outstanding",
            vec![("load_balancing.algorithm.type", "least_outstanding_requests")],
        ),
        (
            "weighted random",
            "dist-weighted-random",
            vec![("load_balancing.algorithm.type", "weighted_random")],
        ),
        (
            "duration-based stickiness",
            "dist-lb-cookie",
            vec![
                ("stickiness.enabled", "true"),
                ("stickiness.type", "lb_cookie"),
                ("stickiness.lb_cookie.duration_seconds", LB_COOKIE_SECONDS),
            ],
        ),
        (
            "application-based stickiness",
            "dist-app-cookie",
            vec![
                ("stickiness.enabled", "true"),
                ("stickiness.type", "app_cookie"),
                ("stickiness.app_cookie.cookie_name", APPLICATION_COOKIE_NAME),
                ("stickiness.app_cookie.duration_seconds", APP_COOKIE_SECONDS),
            ],
        ),
    ];
    // Every group is created and every load balancer placed before anything waits, so the
    // spans their registrations owe their move to `healthy` run at the same time.
    let mut published: BTreeMap<&str, String> = BTreeMap::new();
    let mut arns: Vec<(&str, String, String)> = Vec::new();
    for (case, name, attributes) in &groups {
        let tg_arn = target_group(&router, case, name, attributes).await;
        let (lb_arn, dns_name) = load_balancer(&router, case, name, &tg_arn).await;
        published.insert(case, dns_name);
        arns.push((case, lb_arn, tg_arn));
    }
    for (case, lb_arn, tg_arn) in &arns {
        settle(&engine, lb_arn, SystemTime::now());
        healthy(&router, case, tg_arn).await;
    }
    let taken = |case: &str| -> String {
        published
            .get(case)
            .cloned()
            .unwrap_or_else(|| panic!("the deployment holds a load balancer for {case:?}"))
    };
    Deployment {
        round_robin: taken("round robin"),
        least_outstanding: taken("least outstanding requests"),
        weighted_random: taken("weighted random"),
        lb_cookie: taken("duration-based stickiness"),
        app_cookie: taken("application-based stickiness"),
        router,
        _listening: listening,
    }
}

/// How many requests a case sends when it is reading a DISTRIBUTION: a whole number of turns
/// of the two registrations, so a rotation that reaches both sends each the same number.
const SAMPLE: usize = 6;

/// Send `SAMPLE` requests carrying `cookie` and answer how many of them each document
/// answered.
async fn shares(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    cookie: Option<&str>,
) -> BTreeMap<String, usize> {
    let mut counted: BTreeMap<String, usize> = BTreeMap::new();
    for request in 1..=SAMPLE {
        let answered = get(router, host, cookie).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: request {request} expected the target's own 200, got {} body {:?}",
            answered.status, answered.body
        );
        *counted.entry(answered.body.clone()).or_default() += 1;
    }
    counted
}

/// RULE: a `forward` to a target group holding several registrations the load balancer may
/// send requests to distributes its requests across them the way the group's own
/// `load_balancing.algorithm.type` names.
#[tokio::test]
async fn a_forward_distributes_its_requests_across_the_groups_own_targets() {
    let deployed = deployment().await;
    let documents: Vec<&str> = TARGETS.iter().map(|(_, document)| *document).collect();
    // The two algorithms that distribute by what came before: the rotation hands each
    // registration every other request, and least-outstanding-requests does the same where
    // nothing is in flight to either of them — which is the position a group whose targets
    // answer at once is in.
    for (case, host) in [
        ("round robin, the documented default", &deployed.round_robin),
        ("least outstanding requests", &deployed.least_outstanding),
    ] {
        let counted = shares(&deployed.router, case, host, None).await;
        for document in &documents {
            assert_eq!(
                counted.get(*document).copied(),
                Some(SAMPLE / TARGETS.len()),
                "case {case:?}: expected {document:?} to answer its own share of \
                 {SAMPLE} requests, got {counted:?}"
            );
        }
    }
    // The algorithm that draws per request reaches the group's registrations and nothing
    // else: WHICH of them one request lands on is the draw's, which the draw's own rows pin
    // (`src/service/target_selection.rs`).
    let counted = shares(
        &deployed.router,
        "weighted random",
        &deployed.weighted_random,
        None,
    )
    .await;
    for answered in counted.keys() {
        assert!(
            documents.contains(&answered.as_str()),
            "case \"weighted random\": expected one of the group's own targets to answer, got \
             {answered:?}"
        );
    }
    assert_eq!(
        counted.values().sum::<usize>(),
        SAMPLE,
        "case \"weighted random\": expected every request answered, got {counted:?}"
    );
}

/// RULE: a target group whose `stickiness.enabled` is on and whose `stickiness.type` is
/// `lb_cookie` hands the client the load balancer's own `AWSALB` and `AWSALBCORS` cookies for
/// the registration that answered, and every later request carrying one of them reaches that
/// same registration; a pin the load balancer cannot decode is replaced by one minted against
/// a newly selected registration.
#[tokio::test]
async fn duration_based_stickiness_pins_a_client_to_the_target_that_answered() {
    let deployed = deployment().await;
    let host = deployed.lb_cookie.clone();
    let first = get(&deployed.router, &host, None).await;
    assert_eq!(
        first.status, 200,
        "case \"the first request\": expected the target's own 200, got {} body {:?}",
        first.status, first.body
    );
    let pin = first
        .cookie(DURATION_COOKIE)
        .unwrap_or_else(|| {
            panic!(
                "case \"the first request\": expected the load balancer to mint \
                 {DURATION_COOKIE}, got {:?}",
                first.cookies
            )
        })
        .to_string();
    assert_eq!(
        first.cookie(DURATION_CORS_COOKIE),
        Some(pin.as_str()),
        "case \"the first request\": expected the cross-origin copy to carry the same pin, \
         got {:?}",
        first.cookies
    );
    let field = first.field(DURATION_COOKIE).unwrap_or_default();
    assert!(
        field.contains(&format!("Max-Age={LB_COOKIE_SECONDS}")) && field.contains("Path=/"),
        "case \"the minted pin\": expected the duration the group states over every path, got \
         {field:?}"
    );
    let cors = first.field(DURATION_CORS_COOKIE).unwrap_or_default();
    assert!(
        cors.contains("SameSite=None") && !cors.contains("Secure"),
        "case \"the cross-origin copy over a listener in the clear\": expected SameSite=None \
         and no Secure, got {cors:?}"
    );

    // Every later request carrying the pin reaches the registration that answered the first
    // one, under either cookie the pin travels in — and the load balancer mints no new pin
    // for a client that holds one.
    for (case, carried) in [
        ("the pin replayed", format!("{DURATION_COOKIE}={pin}")),
        (
            "the pin replayed among the client's own cookies",
            format!("first=1; {DURATION_COOKIE}={pin}; last=2"),
        ),
        (
            "the cross-origin copy replayed",
            format!("{DURATION_CORS_COOKIE}={pin}"),
        ),
    ] {
        for request in 1..=SAMPLE {
            let answered = get(&deployed.router, &host, Some(&carried)).await;
            assert_eq!(
                answered.body, first.body,
                "case {case:?}: request {request} expected the pinned target {:?}, got {:?}",
                first.body, answered.body
            );
            assert!(
                answered.cookie(DURATION_COOKIE).is_none(),
                "case {case:?}: expected no new pin for a client that holds one, got {:?}",
                answered.cookies
            );
        }
    }

    // A pin the load balancer cannot decode names no registration of the group, so the
    // request is distributed and answered with a pin minted against the target it reached.
    let undecodable = get(
        &deployed.router,
        &host,
        Some(&format!("{DURATION_COOKIE}=not-a-pin-of-this-group")),
    )
    .await;
    assert_eq!(
        undecodable.status, 200,
        "case \"an undecodable pin\": expected the target's own 200, got {} body {:?}",
        undecodable.status, undecodable.body
    );
    let minted = undecodable.cookie(DURATION_COOKIE);
    assert!(
        minted.is_some() && minted != Some("not-a-pin-of-this-group"),
        "case \"an undecodable pin\": expected a freshly minted pin, got {:?}",
        undecodable.cookies
    );
}

/// RULE: a target group whose `stickiness.type` is `app_cookie` hands the client the load
/// balancer's own `AWSALBAPP` cookie, and that pin is followed only while the request also
/// carries the application's own cookie the group names — a request without it is
/// distributed like any other.
#[tokio::test]
async fn application_based_stickiness_follows_the_applications_own_cookie() {
    let deployed = deployment().await;
    let host = deployed.app_cookie.clone();
    let first = get(&deployed.router, &host, None).await;
    assert_eq!(
        first.status, 200,
        "case \"the first request\": expected the target's own 200, got {} body {:?}",
        first.status, first.body
    );
    let pin = first
        .cookie(APPLICATION_PIN_COOKIE)
        .unwrap_or_else(|| {
            panic!(
                "case \"the first request\": expected the load balancer to mint \
                 {APPLICATION_PIN_COOKIE}, got {:?}",
                first.cookies
            )
        })
        .to_string();
    assert!(
        first.cookie(DURATION_COOKIE).is_none(),
        "case \"the first request\": expected no duration-based pin for an application-based \
         group, got {:?}",
        first.cookies
    );
    let field = first.field(APPLICATION_PIN_COOKIE).unwrap_or_default();
    assert!(
        field.contains(&format!("Max-Age={APP_COOKIE_SECONDS}")),
        "case \"the minted pin\": expected the duration the group states, got {field:?}"
    );

    // With the application's own cookie beside it, the pin is followed.
    let carried = format!("{APPLICATION_COOKIE_NAME}=abc; {APPLICATION_PIN_COOKIE}={pin}");
    for request in 1..=SAMPLE {
        let answered = get(&deployed.router, &host, Some(&carried)).await;
        assert_eq!(
            answered.body, first.body,
            "case \"the pin beside the application's own cookie\": request {request} expected \
             the pinned target {:?}, got {:?}",
            first.body, answered.body
        );
    }

    // Without it, the same pin is followed by nothing: the requests are distributed across
    // the group's registrations, which the rotation gives each an equal share of.
    let counted = shares(
        &deployed.router,
        "the pin without the application's own cookie",
        &host,
        Some(&format!("{APPLICATION_PIN_COOKIE}={pin}")),
    )
    .await;
    for (_, document) in TARGETS {
        assert_eq!(
            counted.get(document).copied(),
            Some(SAMPLE / TARGETS.len()),
            "case \"the pin without the application's own cookie\": expected {document:?} to \
             answer its own share of {SAMPLE} requests, got {counted:?}"
        );
    }
}

/// RULE: a group whose stickiness is OFF — the documented default — pins nothing: it hands
/// back no pin of its own and sends no client two successive requests to one registration.
#[tokio::test]
async fn a_group_whose_stickiness_is_off_hands_back_no_pin() {
    let deployed = deployment().await;
    let answered = get(&deployed.router, &deployed.round_robin, None).await;
    assert_eq!(
        answered.status, 200,
        "case \"stickiness off\": expected the target's own 200, got {} body {:?}",
        answered.status, answered.body
    );
    for name in [DURATION_COOKIE, DURATION_CORS_COOKIE, APPLICATION_PIN_COOKIE] {
        assert!(
            answered.cookie(name).is_none(),
            "case \"stickiness off\": expected no {name} pin, got {:?}",
            answered.cookies
        );
    }
    // And a client replaying another group's pin is not pinned by it either.
    let next = get(
        &deployed.router,
        &deployed.round_robin,
        Some(&format!("{DURATION_COOKIE}=whatever")),
    )
    .await;
    assert_ne!(
        next.body, answered.body,
        "case \"a pin a group that pins nothing was sent\": expected the next request to reach \
         the next target, got {:?} twice",
        answered.body
    );
}
