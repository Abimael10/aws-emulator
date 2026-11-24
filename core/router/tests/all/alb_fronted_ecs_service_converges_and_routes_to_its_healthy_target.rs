// SPDX-License-Identifier: CC0-1.0

//! The canonical Application Load Balancer + `awsvpc` Amazon ECS topology CONVERGES: the
//! registration the service owes, the verdict the checks reach for it, and which registration of
//! a mixed group a request is sent to.
//!
//! RULE: **binding an `awsvpc` Amazon ECS service into an `ip` target group registers each of its
//! running tasks at the private IPv4 address of that task's own elastic network interface, on the
//! bound container port; every such registration then leaves `initial` for a TERMINAL verdict of
//! the load balancer's own checks — `healthy` with no reason and no description once a server
//! behind it answers the checked path, and `unhealthy` with one of `TargetHealth$Reason`'s
//! documented unhealthy reason codes and a description once the configured threshold of checks
//! could not be delivered — never resting in a non-terminal state; and a request the listener's
//! `forward` action sends to that group is answered by a registration the checks found `healthy`,
//! so a group holding healthy and unhealthy registrations at once answers with the healthy
//! target's own bytes rather than with the load balancer's own status for a target it cannot
//! connect to.**
//!
//! The grounds. `LoadBalancer$targetGroupArn` is "the full Amazon Resource Name (ARN) of the
//! Elastic Load Balancing target group … associated with a service", and its own note says what a
//! task is registered AS: "tasks that use the `awsvpc` network mode are associated with an elastic
//! network interface, not an Amazon EC2 instance", so the target is the interface's address and
//! `TargetDescription$Port` is "the port on which the target is listening".
//! `TargetHealthStateEnum` (`smithy_query(query="shape",shape="TargetHealthStateEnum")`) is
//! `[initial, healthy, unhealthy, unhealthy.draining, unused, draining, unavailable]`, and
//! `TargetHealth$Reason` (`smithy_query(query="shape",shape="TargetHealth")`) gives `initial` the
//! reason codes `Elb.RegistrationInProgress` ("the target is in the process of being registered
//! with the load balancer") and `Elb.InitialHealthChecking` ("the load balancer is still sending
//! the target the minimum number of health checks required to determine its health status"), gives
//! `unhealthy` the codes `Target.ResponseCodeMismatch`, `Target.Timeout`,
//! `Target.FailedHealthChecks` and `Elb.InternalError`, and states that for `healthy` "a reason
//! code is not provided" — `TargetHealth$Description` likewise "if the state is `healthy`, a
//! description is not provided". So `initial` is a state a registration is PASSING THROUGH, and
//! the two reasons the model gives it are the load balancer still working out an answer: a
//! registration that never leaves it has reached no verdict at all, which is what a deploy
//! script's wait-for-healthy loop hangs on.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here: Amazon ECS
//! places the task, runs its container and registers the task's address, Amazon EC2 owns the
//! network and the security-group rule that lets the checks arrive, and Elastic Load Balancing v2
//! holds the registration, checks it, reports its health and answers the client. Everything is
//! driven through the real operations on ONE shared engine, through the fixtures the suite that
//! OWNS this topology already keeps ([`crate::ecs_alb_serves_its_awsvpc_tasks_container`]), so
//! this suite holds no second copy of the network, the front door, the revision or the service.
//!
//! The rows of the first test differ along what the rule generalises over: the port the container's
//! server listens on and the binding names (the group's own port moves with it), and which server
//! the container's program starts there. The second test's rows differ along the COUNT of
//! unreachable registrations the group holds beside the task's — one, then three — because the rule
//! is about a request going to a healthy registration however many unhealthy ones sit beside it.
//!
//! Every clock-owed move — the task's start, the registration it owes, the verdict the checks reach
//! — is POLLED for until it holds, never slept for, and each deadline is generous: a row asserts
//! that the move HAPPENS, not how fast this host runs.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The topology this suite measures is the sibling suite's, whose own header invites a suite
// standing it up for another rule to call these rather than keeping a second copy of them.
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    container_hostname, create_service, dialled, ecs_ok, emulator, front_door, get,
    register_revision, reported_health, seed_network, Answered, ReportedHealth, CLUSTER, HEALTHY,
};
// The ONE percent-encoder this test crate keeps for awsQuery suites (`all/query_wire.rs`:
// "Nothing new should write another one — call this module").
use crate::query_wire::form_encode;

/// The region every request below is signed for, and the version an awsQuery request carries.
const REGION: &str = "us-east-1";
const ELB_VERSION: &str = "2015-12-01";

/// The statuses this suite reads: what a control-plane call answers, what the container's server
/// answers the document it holds with, and the one the LOAD BALANCER answers for a target it could
/// not establish a connection to — which is what no request of a group holding a healthy
/// registration may be answered with.
const OK: u16 = 200;
const TARGET_UNREACHABLE: u16 = 502;

/// The `TargetHealthStateEnum` value a registration whose checks could not be delivered settles
/// at, and the two states a converging registration may be seen in on the way to a verdict.
const UNHEALTHY: &str = "unhealthy";
const INITIAL: &str = "initial";

/// The `TargetHealth$Reason` codes the model gives the `initial` state:
/// "`Elb.RegistrationInProgress` - The target is in the process of being registered with the load
/// balancer" and "`Elb.InitialHealthChecking` - The load balancer is still sending the target the
/// minimum number of health checks required to determine its health status".
const INITIAL_REASONS: [&str; 2] = ["Elb.RegistrationInProgress", "Elb.InitialHealthChecking"];

/// The `TargetHealth$Reason` codes the model gives the `unhealthy` state:
/// "`Target.ResponseCodeMismatch` - The health checks did not return an expected HTTP code",
/// "`Target.Timeout` - The health check requests timed out", "`Target.FailedHealthChecks` - The
/// load balancer received an error while establishing a connection to the target or the target
/// response was malformed" and "`Elb.InternalError` - The health checks failed due to an internal
/// error".
const UNHEALTHY_REASONS: [&str; 4] = [
    "Target.ResponseCodeMismatch",
    "Target.Timeout",
    "Target.FailedHealthChecks",
    "Elb.InternalError",
];

/// How long a clock-owed move is waited for before a row gives up, and how often it looks again.
/// Generous on purpose: the deadline is here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// The marker every container's program below writes into the document its server serves, beside
/// the hostname the runtime gave that container — so a body anything else produced fails the row.
const MARKER: &str = "ALB-ECS-CONVERGED";

/// The host numbers the addresses registered BESIDE the task's take: the high end of the range
/// the task's own address comes out of, which a placement allocates from the bottom.
const UNREACHABLE_HOSTS: [u8; 3] = [240, 241, 242];

/// Addresses of the very range the task's address comes out of that no interface holds, so a
/// registration of one is in the target group's own network and the checks still reach nothing.
///
/// Derived from the address the service's task was placed at rather than written out, so the row
/// stands wherever the fixture places its tasks; each is asserted to be an address of its own.
fn unreachable_beside(case: &str, task_address: &str) -> Vec<String> {
    let (prefix, _) = task_address.rsplit_once('.').unwrap_or_else(|| {
        panic!("case {case:?}: the task's address must be an IPv4 one, got {task_address:?}")
    });
    let addresses: Vec<String> = UNREACHABLE_HOSTS
        .iter()
        .map(|host| format!("{prefix}.{host}"))
        .collect();
    assert!(
        addresses.iter().all(|address| address != task_address),
        "case {case:?}: every address registered beside the task's must be another one, got \
         {addresses:?} beside {task_address:?}"
    );
    addresses
}

/// One row of the convergence test: the names its resources take, the port its container's server
/// listens on, and the program that starts it.
struct Converging {
    case: &'static str,
    family: &'static str,
    group_name: &'static str,
    load_balancer_name: &'static str,
    service_name: &'static str,
    /// The `containerPort` the revision declares, which the binding names and the target group
    /// takes as its own port — so the check and the traffic both go there.
    port: i64,
    /// The container's `command`, which is what starts the server that answers the checked path.
    command: &'static [&'static str],
}

const CONVERGING: &[Converging] = &[
    Converging {
        case: "a container serving the image's own server on the first port",
        family: "convergednginx",
        group_name: "converged-nginx",
        load_balancer_name: "converged-nginx-lb",
        service_name: "convergednginx",
        port: 80,
        command: &[
            "/bin/sh",
            "-c",
            "mkdir -p /usr/share/nginx/html && \
             echo \"ALB-ECS-CONVERGED $(hostname)\" > /usr/share/nginx/html/index.html && \
             nginx -g 'daemon off;'",
        ],
    },
    Converging {
        case: "a container serving the interpreter's own server module on another port",
        family: "convergedmodule",
        group_name: "converged-module",
        load_balancer_name: "converged-module-lb",
        service_name: "convergedmodule",
        port: 8080,
        command: &[
            "/bin/sh",
            "-c",
            "mkdir -p /srv/app && echo \"ALB-ECS-CONVERGED $(hostname)\" > /srv/app/index.html && \
             python3 -m http.server 8080 --directory /srv/app",
        ],
    },
];

/// One form body: the action, the version and the parameters.
fn form(action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={ELB_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Drive one awsQuery action against the Elastic Load Balancing v2 model that must succeed, and
/// answer its body.
///
/// The one call this suite SENDS that the fixtures of this topology do not already make, which is
/// the registration a caller of its own adds beside the service's; what a group then REPORTS is
/// read through the fixture module's own reader ([`reported_health`]).
async fn elbv2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://elasticloadbalancingv2.{REGION}.amazonaws.com/"
        ))
        .header(
            "host",
            format!("elasticloadbalancingv2.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(form(action, params)))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, OK,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Every registration `group` reports, ordered by target id — the fixture module's own reading of
/// the answer ([`reported_health`]), put in one order so a row's message is stable.
async fn health(router: &MultiServiceRouter, group: &str) -> Vec<ReportedHealth> {
    let mut reported = reported_health(router, group).await;
    reported.sort();
    reported
}

/// Register every address of `addresses` with `group` through `RegisterTargets`, as a caller
/// registering a target of its own does.
async fn register(router: &MultiServiceRouter, group: &str, addresses: &[String]) {
    let keys: Vec<String> = (0..addresses.len())
        .map(|index| format!("Targets.member.{}.Id", index + 1))
        .collect();
    let mut params: Vec<(&str, &str)> = vec![("TargetGroupArn", group)];
    for (key, address) in keys.iter().zip(addresses) {
        params.push((key.as_str(), address.as_str()));
    }
    elbv2(router, "RegisterTargets", &params).await;
}

/// The private IPv4 address of the interface of every RUNNING task of `service`, which is what
/// Amazon ECS registers for such a task — polled for until the service reports `expected` of them,
/// because placing a task and giving it an interface are moves the clock owes.
async fn task_addresses(
    router: &MultiServiceRouter,
    case: &str,
    service: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = ecs_ok(
            router,
            "ListTasks",
            json!({ "cluster": CLUSTER, "serviceName": service, "desiredStatus": "RUNNING" }),
        )
        .await;
        let arns: Vec<String> = listed["taskArns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|arn| arn.as_str().map(str::to_string))
            .collect();
        if arns.len() == expected {
            let described = ecs_ok(
                router,
                "DescribeTasks",
                json!({ "cluster": CLUSTER, "tasks": arns }),
            )
            .await;
            let mut addresses: Vec<String> = described["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|task| {
                    task["attachments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|attachment| attachment["details"].as_array())
                        .flatten()
                        .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
                        .and_then(|detail| detail["value"].as_str())
                        .map(str::to_string)
                })
                .collect();
            if addresses.len() == expected {
                addresses.sort();
                return addresses;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} running task(s) of {service} carrying an \
             interface address, got {arns:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Wait until `group` reports `expected` registrations, every one of them in a TERMINAL state —
/// the verdict its checks reached — and answer them together with every distinct state and reason
/// seen on the way there.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    expected: usize,
) -> (Vec<ReportedHealth>, Vec<(String, String)>) {
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<(String, String)> = Vec::new();
    loop {
        let reported = health(router, group).await;
        for entry in &reported {
            let step = (entry.state.clone(), entry.reason.clone());
            if !seen.contains(&step) {
                seen.push(step);
            }
        }
        let terminal = reported.len() == expected
            && reported
                .iter()
                .all(|entry| entry.state == HEALTHY || entry.state == UNHEALTHY);
        if terminal {
            return (reported, seen);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} registration(s) to reach a terminal verdict \
             ({HEALTHY} or {UNHEALTHY}), got {reported:?} after seeing {seen:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Assert every state-and-reason OBSERVED while a registration converged is one the model gives a
/// converging registration — the `initial` it passes through, under one of that state's own reason
/// codes, or the `healthy` it reaches, which carries none — and that `healthy` was among them.
///
/// What this can assert is what a poll can observe: a registration already healthy at the first
/// look passes through no state this sees. What it rules out is a reading outside that pair (the
/// `unhealthy` of a verdict, or an `initial` under a reason the model does not give it) and a
/// registration that never reaches `healthy` at all.
fn assert_only_converging_states(case: &str, seen: &[(String, String)]) {
    for (state, reason) in seen {
        let converging = (state == INITIAL && INITIAL_REASONS.contains(&reason.as_str()))
            || (state == HEALTHY && reason.is_empty());
        assert!(
            converging,
            "case {case:?}: a registration whose checks are answered converges through {INITIAL:?} \
             under one of {INITIAL_REASONS:?} to {HEALTHY:?} with no reason, got the states \
             {seen:?}"
        );
    }
    assert!(
        seen.iter().any(|(state, _)| state == HEALTHY),
        "case {case:?}: expected the registration to reach {HEALTHY:?}, got the states {seen:?}"
    );
}

/// Assert `entry` is a healthy registration as the model words one: the state, with no reason code
/// ("if the target state is `healthy`, a reason code is not provided") and no description ("if the
/// state is `healthy`, a description is not provided").
fn assert_healthy(case: &str, entry: &ReportedHealth) {
    assert_eq!(
        entry.state, HEALTHY,
        "case {case:?}: expected the registration to be reported {HEALTHY}, got {entry:?}"
    );
    assert!(
        entry.reason.is_empty() && entry.description.is_empty(),
        "case {case:?}: a registration reported {HEALTHY} carries no reason and no description, \
         got {entry:?}"
    );
}

/// Assert `entry` is an unhealthy registration as the model words one: the state, one of
/// `TargetHealth$Reason`'s own unhealthy reason codes, and a description — the detail an operator
/// acts on, which is why a verdict carrying none would be no answer at all.
fn assert_unhealthy(case: &str, entry: &ReportedHealth) {
    assert_eq!(
        entry.state, UNHEALTHY,
        "case {case:?}: expected the registration to settle at {UNHEALTHY}, got {entry:?}"
    );
    assert!(
        UNHEALTHY_REASONS.contains(&entry.reason.as_str()),
        "case {case:?}: expected one of the documented unhealthy reason codes \
         {UNHEALTHY_REASONS:?}, got {entry:?}"
    );
    assert!(
        !entry.description.is_empty(),
        "case {case:?}: an {UNHEALTHY} verdict carries the detail its reason code is about, got \
         {entry:?}"
    );
}

/// Assert `answered` is the document one of `hostnames`' containers wrote: the target's own status
/// and bytes, never the load balancer's status for a target it cannot connect to.
fn assert_served_by_a_task(case: &str, answered: &Answered, hostnames: &[String]) {
    assert_eq!(
        answered.status, OK,
        "case {case:?}: expected the healthy target's own HTTP {OK}, got {} with body {:?} (a \
         {TARGET_UNREACHABLE} is the load balancer's own answer for a target it cannot connect \
         to, which a group holding a {HEALTHY} registration must never answer)",
        answered.status, answered.body
    );
    let fields: Vec<&str> = answered.body.split_whitespace().collect();
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
        "case {case:?}: expected the document of one of the healthy tasks {hostnames:?}, got {:?}",
        answered.body
    );
}

/// RULE: an `awsvpc` service bound into an `ip` target group is registered at its task's own
/// interface address on the bound container port, converges through `initial` to `healthy` with no
/// reason and no description, and the load balancer then answers a client with that container's own
/// bytes — whichever port the container serves on and whichever server its program started.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_alb_fronted_awsvpc_service_converges_to_a_healthy_target_serving_its_containers_bytes()
{
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    for row in CONVERGING {
        let revision = register_revision(&router, row.family, row.port, row.command).await;
        let (group, dns_name) = front_door(
            &router,
            &network,
            row.case,
            row.group_name,
            row.load_balancer_name,
            row.port,
        )
        .await;
        create_service(
            &router,
            &network,
            row.service_name,
            &revision,
            &group,
            row.port,
        )
        .await;

        // The address the service's own task holds, read from Amazon ECS: the registration must
        // name exactly it, not an instance id and not an address of the emulator's own choosing.
        let addresses = task_addresses(&router, row.case, row.service_name, 1).await;
        let (reported, seen) = settled(&router, row.case, &group, 1).await;
        assert_only_converging_states(row.case, &seen);
        let entry = &reported[0];
        assert_healthy(row.case, entry);
        assert_eq!(
            entry.id, addresses[0],
            "case {:?}: the registration must name the task's own interface address, got {entry:?}",
            row.case
        );
        assert_eq!(
            entry.port,
            row.port.to_string(),
            "case {:?}: the registration receives traffic on the bound container port, got \
             {entry:?}",
            row.case
        );
        // And the client handed the published address is answered by that very container.
        let hostnames = vec![container_hostname(&addresses[0])];
        let host_style = get(&router, &dns_name, "/").await;
        assert_served_by_a_task(row.case, &host_style, &hostnames);
        let dialled_answer = dialled(row.case, &dns_name, "/").await;
        assert_served_by_a_task(row.case, &dialled_answer, &hostnames);
    }
}

/// The names the routing test's resources take, the port its container serves on, and its program.
const ROUTING_FAMILY: &str = "routedfamily";
const ROUTING_GROUP: &str = "routed-group";
const ROUTING_LOAD_BALANCER: &str = "routed-lb";
const ROUTING_SERVICE: &str = "routedsvc";
const ROUTING_PORT: i64 = 80;
const ROUTING_COMMAND: &[&str] = &[
    "/bin/sh",
    "-c",
    "mkdir -p /usr/share/nginx/html && \
     echo \"ALB-ECS-CONVERGED $(hostname)\" > /usr/share/nginx/html/index.html && \
     nginx -g 'daemon off;'",
];

/// How many requests each row of the routing test sends, alternating between the two ways a client
/// addresses the published load balancer. Enough that a draw ignoring health would land on an
/// unreachable registration: with three unhealthy registrations beside one healthy one, a blind
/// draw misses them all with probability (1/4)^8, under one in sixty thousand.
const REQUESTS_PER_ROW: usize = 8;

/// RULE: a request is answered by a registration the checks found `healthy`, however many
/// registrations the group holds that they could not be delivered to — and each of those settles at
/// `unhealthy` with a documented reason rather than resting in a non-terminal state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_request_lands_on_the_healthy_ecs_target_beside_its_unreachable_registrations() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let revision = register_revision(&router, ROUTING_FAMILY, ROUTING_PORT, ROUTING_COMMAND).await;
    let setup = "an ALB forwarding to one awsvpc ECS task";
    let (group, dns_name) = front_door(
        &router,
        &network,
        setup,
        ROUTING_GROUP,
        ROUTING_LOAD_BALANCER,
        ROUTING_PORT,
    )
    .await;
    create_service(
        &router,
        &network,
        ROUTING_SERVICE,
        &revision,
        &group,
        ROUTING_PORT,
    )
    .await;
    let addresses = task_addresses(&router, setup, ROUTING_SERVICE, 1).await;
    let hostnames = vec![container_hostname(&addresses[0])];
    let (reported, seen) = settled(&router, setup, &group, 1).await;
    assert_only_converging_states(setup, &seen);
    assert_healthy(setup, &reported[0]);

    // The rows: how many registrations the checks cannot be delivered to sit beside the task's.
    // They accumulate, so the group holds the task's registration plus this many.
    let unreachable_addresses = unreachable_beside(setup, &addresses[0]);
    for unreachable in [1usize, unreachable_addresses.len()] {
        let case = format!("a healthy ECS task beside {unreachable} unreachable registration(s)");
        register(&router, &group, &unreachable_addresses[..unreachable]).await;
        let (reported, _) = settled(&router, &case, &group, unreachable + 1).await;
        for entry in &reported {
            if entry.id == addresses[0] {
                assert_healthy(&case, entry);
            } else {
                assert!(
                    unreachable_addresses.contains(&entry.id),
                    "case {case:?}: the group holds the task's registration and the ones this \
                     test registered, got {entry:?}"
                );
                assert_unhealthy(&case, entry);
            }
        }

        // Every request is answered by the one registration the checks found healthy.
        for index in 0..REQUESTS_PER_ROW {
            let answered = if index % 2 == 0 {
                get(&router, &dns_name, "/").await
            } else {
                dialled(&case, &dns_name, "/").await
            };
            assert_served_by_a_task(&case, &answered, &hostnames);
        }

        // Serving those requests moved nothing: the health a later caller reads is the same.
        let after = health(&router, &group).await;
        assert_eq!(
            after.len(),
            unreachable + 1,
            "case {case:?}: serving requests must leave every registration in place, got {after:?}"
        );
        for entry in &after {
            if entry.id == addresses[0] {
                assert_healthy(&case, entry);
            } else {
                assert_unhealthy(&case, entry);
            }
        }
    }
}
