// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS an Application Load Balancer publishes for an Amazon ECS service, and what the
//! client that was handed that address is answered with.
//!
//! RULE: **once a target group's registration of an `awsvpc` task's address is reported
//! `healthy`, a request a client sends to the address the load balancer PUBLISHED is forwarded to
//! a registration of its listener's `forward` action and answered with the response of the server
//! that task's container RUNS on the port the registration receives traffic on — the TARGET's own
//! status and bytes, read out of the container's own filesystem — however the client addressed the
//! load balancer and however many tasks the service's desired count places behind the group. The
//! load balancer's own `502` is the answer for a target it cannot establish a connection to, so a
//! registration reported `healthy` is never answered with one.**
//!
//! The grounds. `LoadBalancer$DNSName` is "the public DNS name of the load balancer", which is the
//! address a client is handed and sends its traffic to; `CreateListener` performs a default rule's
//! actions for a request no rule of the listener matches, and a `forward` action names the target
//! group the request is sent to; `LoadBalancer$targetGroupArn` binds a service's tasks into that
//! group and "tasks that use the `awsvpc` network mode are associated with an elastic network
//! interface", so the registration is the task's own address; and `TargetDescription$Port` is "the
//! port on which the target is listening", where what is listening is what the container's own
//! `command` — "the command that's passed to the container" — started. `TargetHealth$State`
//! `healthy` is the state of a target the load balancer may send requests to, so the two cannot
//! disagree: a target reported healthy is one a request reaches.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here: Amazon ECS
//! runs the container and records the server it listens with at its task's own address, Amazon EC2
//! owns the network and the security-group rule that lets the load balancer reach it, and Elastic
//! Load Balancing v2 publishes the address, holds the registration and answers the client.
//! Everything is driven through the real operations on ONE shared engine, through the helpers the
//! sibling suite that stands this very topology up already keeps
//! ([`crate::ecs_alb_serves_its_awsvpc_tasks_container`]), so this suite holds no second copy of
//! them.
//!
//! The rows of the first test differ along what the rule generalises over: HOW the client
//! addresses the load balancer (a real connection DIALLED to the published authority, the same
//! authority sent as a `Host`, and the reserved path namespace naming the load balancer on a host
//! that names no service) and WHICH document it asks for (the page the container's program wrote
//! at the root of the directory its server serves, one it wrote deeper in that directory, and a
//! path the container holds nothing at — the TARGET's own `404`, which is a status of the target
//! and not of the load balancer). The second test varies the COUNT: a desired count scaled past
//! one places a second task, and the answer is still a task's own document.
//!
//! Every clock-owed move — the task's start, the registration it owes, the health its checks
//! determine — is POLLED for until it holds, never slept for, and each deadline is generous: a row
//! asserts that the move HAPPENS, not how fast this host runs.

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The topology this suite measures is the sibling suite's: its own header invites a suite
// standing it up for another rule to call these rather than keeping a second copy of them —
// including `dialled`, the one hand-written raw HTTP/1.1 exchange on a published address, and the
// poll of the health a group's registrations settle at.
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    container_hostname, create_service, dialled, ecs_ok, emulator, front_door, get, health,
    healthy_target, healthy_targets, register_revision, seed_network, Answered, CLUSTER, HEALTHY,
};

/// The names the first test's resources take, and the names the scaled one's take: each test
/// drives an engine of its own, and distinct names keep a failure's message unambiguous.
const FAMILY: &str = "reachedfamily";
const GROUP_NAME: &str = "reached-group";
const LOAD_BALANCER_NAME: &str = "reached-lb";
const SERVICE_NAME: &str = "reachedsvc";
const SCALED_FAMILY: &str = "scaledfamily";
const SCALED_GROUP_NAME: &str = "scaled-group";
const SCALED_LOAD_BALANCER_NAME: &str = "scaled-lb";
const SCALED_SERVICE_NAME: &str = "scaledsvc";

/// The port the container's server listens on, which is the `containerPort` the revision declares,
/// the port the binding names and the target group's own.
const PORT: i64 = 80;

/// The marker the container's own program writes into every document it serves, so a body anything
/// else produced fails the row. The command below carries it literally, because a container's
/// `command` is text the revision holds.
const MARKER: &str = "ALB-REACHED-TASK";

/// The container's program: it writes its own page at the root of the directory the `nginx` image
/// serves and a second copy of it deeper in that directory, each carrying the container's own
/// hostname, and then runs that image's own server in the foreground — which is what keeps the
/// task `RUNNING` and what answers the load balancer.
const COMMAND: &[&str] = &[
    "/bin/sh",
    "-c",
    "mkdir -p /usr/share/nginx/html/pages && \
     echo \"ALB-REACHED-TASK $(hostname)\" > /usr/share/nginx/html/index.html && \
     echo \"ALB-REACHED-TASK $(hostname)\" > /usr/share/nginx/html/pages/deep.html && \
     nginx -g 'daemon off;'",
];

/// The paths the rows ask for: the root of the served directory, a document deeper inside it, and
/// a path the container holds nothing at.
const ROOT_PATH: &str = "/";
const DEEP_PATH: &str = "/pages/deep.html";
const MISSING_PATH: &str = "/pages/not-written.html";

/// The status the target answers a document it holds with, and the one its own static server
/// answers a path its document root holds nothing at.
const SERVED_STATUS: u16 = 200;
const NOT_FOUND_STATUS: u16 = 404;

/// The status the LOAD BALANCER answers for a target it cannot establish a connection to, which is
/// what no row of a healthy registration may be answered with. Named here so each row's message
/// says what the status it got would have meant.
const TARGET_UNREACHABLE_STATUS: u16 = 502;

/// The desired count the scaled test moves the service to.
const SCALED_DESIRED_COUNT: u64 = 2;

/// The reserved path namespace and the segment that name this data plane, which a client addresses
/// a load balancer's plane by on any host: `/_aws/elasticloadbalancing/{name}/{path}`.
const DATA_PLANE_PREFIX: &str = "/_aws/elasticloadbalancing";

/// A host that names no service of the emulator, which is what the prefixed addressing is read on
/// — its point being that it is read whatever the request's host is.
const NO_SERVICE_HOST: &str = "127.0.0.1";

/// How a client ADDRESSES the load balancer, which is the dimension the first test's rows differ
/// along.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// A real connection to the authority the control plane published, which is what the client
    /// that was handed that address opens.
    Dialled,
    /// The published authority sent as the request's `Host`.
    HostStyle,
    /// The reserved path namespace naming this load balancer.
    PrefixedPath,
}

/// One row: how the client addresses the load balancer, what it asks for, and what it must be
/// answered with.
struct Row {
    case: &'static str,
    addressing: Addressing,
    path: &'static str,
    status: u16,
}

const ROWS: &[Row] = &[
    Row {
        case: "the published address dialled for the page at the served root",
        addressing: Addressing::Dialled,
        path: ROOT_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the published address dialled for a document deeper in the served directory",
        addressing: Addressing::Dialled,
        path: DEEP_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the published address dialled for a path the container holds nothing at",
        addressing: Addressing::Dialled,
        path: MISSING_PATH,
        status: NOT_FOUND_STATUS,
    },
    Row {
        case: "the published authority as the Host, for the page at the served root",
        addressing: Addressing::HostStyle,
        path: ROOT_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the published authority as the Host, for a document deeper in the directory",
        addressing: Addressing::HostStyle,
        path: DEEP_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the published authority as the Host, for a path the container holds nothing at",
        addressing: Addressing::HostStyle,
        path: MISSING_PATH,
        status: NOT_FOUND_STATUS,
    },
    Row {
        case: "the reserved path namespace naming the load balancer, for the served root",
        addressing: Addressing::PrefixedPath,
        path: ROOT_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the reserved path namespace naming the load balancer, for the deeper document",
        addressing: Addressing::PrefixedPath,
        path: DEEP_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the reserved path namespace naming the load balancer, for a path held nothing at",
        addressing: Addressing::PrefixedPath,
        path: MISSING_PATH,
        status: NOT_FOUND_STATUS,
    },
];

/// Send one request under the reserved path namespace that NAMES this load balancer, on a host
/// that names no service at all.
async fn prefixed(router: &MultiServiceRouter, load_balancer_name: &str, path: &str) -> Answered {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!(
            "http://{NO_SERVICE_HOST}{DATA_PLANE_PREFIX}/{load_balancer_name}{path}"
        ))
        .header("host", NO_SERVICE_HOST)
        .body(Bytes::new())
        .expect("a well-formed data-plane request");
    let response = router.dispatch(request).await;
    Answered {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// Assert `answered` is what the row expects: the TARGET's own status, and — for a document the
/// container holds — the marker its program wrote beside the hostname of the task that served it
/// ([`container_hostname`]). For a status the target answered instead, what matters is that the
/// answer carries no document of the container at all: the marker must not appear in it.
fn assert_answered(case: &str, answered: &Answered, status: u16, hostnames: &[String]) {
    assert_eq!(
        answered.status, status,
        "case {case:?}: expected the target's own HTTP {status}, got {} with body {:?} (a \
         {TARGET_UNREACHABLE_STATUS} would be the load balancer's own answer for a target it \
         cannot connect to, which a registration reported {HEALTHY} must never earn)",
        answered.status, answered.body
    );
    let fields: Vec<&str> = answered.body.split_whitespace().collect();
    if status != SERVED_STATUS {
        assert!(
            !answered.body.contains(MARKER),
            "case {case:?}: a status the target answered for a path it holds nothing at carries no \
             document of the container, got {:?}",
            answered.body
        );
        return;
    }
    assert_eq!(
        fields.first().copied(),
        Some(MARKER),
        "case {case:?}: expected the document the container's own program wrote, got {:?}",
        answered.body
    );
    assert!(
        fields
            .get(1)
            .is_some_and(|served| hostnames.iter().any(|known| known == served)),
        "case {case:?}: expected the document of one of the registered tasks {hostnames:?}, got \
         {:?}",
        answered.body
    );
}

/// RULE: every addressing of the published load balancer is answered by the server the healthy
/// `ip` target's container runs, with that target's own status and bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_addressing_of_the_published_load_balancer_is_answered_by_the_tasks_own_server() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let revision = register_revision(&router, FAMILY, PORT, COMMAND).await;
    let setup = "an ALB forwarding to the ip target of one awsvpc ECS task";
    let (group, dns_name) = front_door(
        &router,
        &network,
        setup,
        GROUP_NAME,
        LOAD_BALANCER_NAME,
        PORT,
    )
    .await;
    create_service(&router, &network, SERVICE_NAME, &revision, &group, PORT).await;
    // The registration and its health come first: the rule is about what a target the load
    // balancer's own checks found healthy is answered with.
    let address = healthy_target(&router, setup, &group, SERVICE_NAME).await;
    let hostnames = vec![container_hostname(&address)];

    for row in ROWS {
        let answered = match row.addressing {
            Addressing::Dialled => dialled(row.case, &dns_name, row.path).await,
            Addressing::HostStyle => get(&router, &dns_name, row.path).await,
            Addressing::PrefixedPath => prefixed(&router, LOAD_BALANCER_NAME, row.path).await,
        };
        assert_answered(row.case, &answered, row.status, &hostnames);
    }

    // The health the client read as proof the path is wired is still what the group reports, so
    // nothing of serving those requests moved it.
    let after = health(&router, &group).await;
    assert_eq!(
        after,
        vec![(address.clone(), HEALTHY.to_string())],
        "case {setup:?}: serving requests must leave the registration exactly as it was, got \
         {after:?}"
    );
}

/// RULE: the same holds when the service's desired count places more than one task behind the
/// group — every registration is reported healthy, and the answer is a task's own document.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scaled_out_service_is_still_answered_by_one_of_its_tasks_own_servers() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let revision = register_revision(&router, SCALED_FAMILY, PORT, COMMAND).await;
    let case = "an ALB forwarding to a service scaled past one task";
    let (group, dns_name) = front_door(
        &router,
        &network,
        case,
        SCALED_GROUP_NAME,
        SCALED_LOAD_BALANCER_NAME,
        PORT,
    )
    .await;
    create_service(
        &router,
        &network,
        SCALED_SERVICE_NAME,
        &revision,
        &group,
        PORT,
    )
    .await;
    let first = healthy_target(&router, case, &group, SCALED_SERVICE_NAME).await;

    // The desired count moved past one: the scheduler places a second task, which takes an
    // interface and an address of its own and is registered beside the first.
    ecs_ok(
        &router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": SCALED_SERVICE_NAME,
            "desiredCount": SCALED_DESIRED_COUNT,
        }),
    )
    .await;
    let addresses = healthy_targets(
        &router,
        case,
        &group,
        SCALED_SERVICE_NAME,
        SCALED_DESIRED_COUNT as usize,
    )
    .await;
    assert!(
        addresses.contains(&first),
        "case {case:?}: the task already serving must keep its registration, got {addresses:?} \
         without {first:?}"
    );
    assert_ne!(
        addresses[0], addresses[1],
        "case {case:?}: each awsvpc task holds an interface of its own, so the two registrations \
         must name two addresses, got {addresses:?}"
    );
    let hostnames: Vec<String> = addresses
        .iter()
        .map(|address| container_hostname(address))
        .collect();

    // The client dials the one address the control plane published, and is answered by one of the
    // tasks behind it — its own document, not a status of the load balancer's.
    let dialled_answer = dialled(case, &dns_name, ROOT_PATH).await;
    assert_answered(case, &dialled_answer, SERVED_STATUS, &hostnames);
    let host_style = get(&router, &dns_name, ROOT_PATH).await;
    assert_answered(case, &host_style, SERVED_STATUS, &hostnames);
}
