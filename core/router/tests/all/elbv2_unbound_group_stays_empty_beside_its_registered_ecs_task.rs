// SPDX-License-Identifier: CC0-1.0

//! The NEGATIVE control of an Amazon ECS service's `loadBalancers` binding: an Elastic Load
//! Balancing v2 target group no service's bindings name reports nothing, on every read, beside a
//! bound service whose running task IS reported.
//!
//! RULE: **a target group no service's `loadBalancers` names reports an EMPTY
//! `TargetHealthDescriptions` on every read — while a bound service's task is coming up, at the
//! instant that task's own registration settles at the verdict the load balancer's checks reached,
//! and on every read after — whatever port that unbound group was created on and whatever health
//! check it carries; and a service that declares no `loadBalancers` runs its task registered
//! NOWHERE and reports `loadBalancers` as an empty list.**
//!
//! The grounds. `LoadBalancer$targetGroupArn` is "the full Amazon Resource Name (ARN) of the
//! Elastic Load Balancing target group … associated with a service", and its own note says what a
//! task of such a service is registered AS: "tasks that use the `awsvpc` network mode are
//! associated with an elastic network interface, not an Amazon EC2 instance", so the target is that
//! interface's private address and `TargetDescription$Port` is "the port on which the target is
//! listening". `DescribeTargetHealth` (`smithy_query(query="operation",operation="DescribeTargetHealth")`)
//! takes a required `TargetGroupArn` and answers `TargetHealthDescriptions[].{Target{Id,Port},
//! TargetHealth{State,Reason,Description}}`, so what a group holds is exactly what it reports. The
//! binding is therefore the WHOLE of what puts a task in a group: a group nothing binds reports an
//! empty list rather than an entry in some state of its own, and that contrast is what makes a
//! binding observable at all. `DescribeTargetHealth` is also a REPORTING call, so reading a group
//! may not change what the next caller reads.
//!
//! The rows differ along what the rule generalises over: the configuration the UNBOUND group
//! carries — the port it is created on (the bound port and another), and its health check (the one
//! the bound group runs, the group's own defaults, and one naming a port nothing listens on under a
//! matcher no server answers). No configuration of a group nothing binds makes it hold a target, so
//! every row is read on every pass of the convergence window below and again afterwards.
//!
//! Three crates have to agree for the rule to hold, which is why this suite lives here: Amazon EC2
//! owns the network the tasks are placed in and the security-group rule that lets the checks
//! arrive, Amazon ECS owns the tasks, their interfaces' addresses and the bindings that decide
//! where they are registered, and Elastic Load Balancing v2 holds the registrations and reports
//! them. Everything is driven through the real operations over ONE shared engine, through the
//! fixtures the suite that OWNS this topology already keeps
//! ([`crate::ecs_alb_serves_its_awsvpc_tasks_container`]), so this suite holds no second copy of
//! the network, the front door or the revision.
//!
//! Every clock-owed move — the tasks' start, the registration the binding owes, the verdict the
//! checks reach — is POLLED for until it holds, never slept for, and the deadline is generous: the
//! suite asserts that the move HAPPENS, not how fast this host runs.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The suite that OWNS this topology's fixtures invites a suite standing it up for another rule to
// call these rather than keeping a second copy of them.
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    ecs_ok, elbv2, emulator, front_door, register_revision, reported_health, seed_network,
    serving_command, Network, ReportedHealth, CLUSTER, CONTAINER, HEALTHY,
};
// The ONE reader this test crate keeps for awsQuery answers (`all/query_wire.rs`).
use crate::query_wire::opt_element;

/// The revision every service below runs, and the services the rule is read on: the one that
/// declares a `loadBalancers` binding and the one that declares none.
const FAMILY: &str = "unboundctlfamily";
const BOUND_SERVICE: &str = "unboundctlbound";
const UNBOUND_SERVICE: &str = "unboundctlunbound";

/// The front door the bound service binds: the `ip` target group its tasks are registered into and
/// the load balancer whose listener forwards to it.
const BOUND_GROUP: &str = "unboundctl-bound";
const BOUND_LOAD_BALANCER: &str = "unboundctl-bound-lb";

/// The `containerPort` the revision declares and the binding names, which is the port the
/// registration receives traffic on.
const CONTAINER_PORT: i64 = 80;

/// What `desiredCount` each service asks for, and so how many registrations the bound group holds.
const DESIRED_COUNT: usize = 1;

/// The marker the container's program writes into the document its server serves, so the document
/// the health check is answered with is one the task's own container wrote.
const MARKER: &str = "UNBOUND-CONTROL-SERVED";

/// How long the convergence below is waited for, and how often it is looked at. Generous on
/// purpose: the deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// How many times every group is read AGAIN once the bound registration has settled, with nothing
/// else called in between: `DescribeTargetHealth` reports what a group holds, so no number of reads
/// of an unbound group may conjure an entry into it or move the bound one.
const REREADS: usize = 8;

/// One row: a target group NO service's `loadBalancers` names, and the configuration it carries.
struct Unbound {
    case: &'static str,
    /// The group's name, which is what its ARN is minted from.
    name: &'static str,
    /// `CreateTargetGroup$Port`, "the port on which the targets receive traffic".
    port: &'static str,
    /// The health-check parameters the group is created with, beyond the ones every row shares —
    /// empty for a row that takes the group's own defaults.
    health_check: &'static [(&'static str, &'static str)],
}

/// The unbound groups, differing along the configuration the rule generalises over.
const UNBOUND: &[Unbound] = &[
    Unbound {
        case: "an unbound group on the bound port, carrying the health check the bound group runs",
        name: "unboundctl-same-port",
        port: "80",
        health_check: &[
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", "/"),
            ("HealthCheckIntervalSeconds", "10"),
            ("HealthyThresholdCount", "2"),
            ("Matcher.HttpCode", "200"),
        ],
    },
    Unbound {
        case: "an unbound group on another port, taking the group's own default health check",
        name: "unboundctl-other-port",
        port: "8081",
        health_check: &[],
    },
    Unbound {
        case: "an unbound group whose health check names a port nothing listens on, under a matcher \
               no server answers",
        name: "unboundctl-odd-check",
        port: "80",
        health_check: &[
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", "/unserved"),
            ("HealthCheckPort", "9099"),
            ("Matcher.HttpCode", "204"),
        ],
    },
];

/// Create one unbound `ip` target group in the suite's VPC and answer its ARN.
///
/// `CreateTargetGroup` alone: no load balancer, no listener and no service binding it, which is
/// what makes it the control the rule is read against.
async fn create_unbound_group(
    router: &MultiServiceRouter,
    network: &Network,
    row: &Unbound,
) -> String {
    let mut params: Vec<(&str, &str)> = vec![
        ("Name", row.name),
        ("Protocol", "HTTP"),
        ("Port", row.port),
        ("TargetType", "ip"),
        ("VpcId", network.vpc.as_str()),
    ];
    params.extend_from_slice(row.health_check);
    let created = elbv2(router, "CreateTargetGroup", &params).await;
    opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("case {:?}: no TargetGroupArn in {created}", row.case))
}

/// Create one Fargate service of `revision` over the whole network, binding `group` when one is
/// given and declaring no `loadBalancers` block at all when none is.
async fn create_service(
    router: &MultiServiceRouter,
    network: &Network,
    service_name: &str,
    revision: &str,
    group: Option<&str>,
) -> Value {
    let mut request = json!({
        "cluster": CLUSTER,
        "serviceName": service_name,
        "taskDefinition": revision,
        "desiredCount": DESIRED_COUNT,
        "launchType": "FARGATE",
        "networkConfiguration": {
            "awsvpcConfiguration": {
                "subnets": network.subnets,
                "securityGroups": [network.task_group],
                "assignPublicIp": "ENABLED",
            }
        },
    });
    if let Some(group) = group {
        request["loadBalancers"] = json!([{
            "targetGroupArn": group,
            "containerName": CONTAINER,
            "containerPort": CONTAINER_PORT,
        }]);
        // "The period of time … that the Amazon ECS service scheduler ignores unhealthy Elastic
        // Load Balancing … health checks after a task has first started", as the scenario a client
        // deploys behind a load balancer declares it.
        request["healthCheckGracePeriodSeconds"] = json!(120);
    }
    ecs_ok(router, "CreateService", request).await
}

/// One service as `DescribeServices` reports it, or `Value::Null` for an answer carrying none.
async fn described(router: &MultiServiceRouter, service_name: &str) -> Value {
    let answered = ecs_ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [service_name] }),
    )
    .await;
    answered["services"][0].clone()
}

/// `Service$runningCount`, "the number of tasks in the cluster that are in the `RUNNING` state",
/// and `None` for an answer carrying none — which no condition below is satisfied by.
fn running_count(service: &Value) -> Option<usize> {
    service["runningCount"]
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
}

/// The private IPv4 address of the elastic network interface every RUNNING task of `service_name`
/// holds, in order — what Amazon ECS registers such a task AS, read from Amazon ECS itself.
async fn task_addresses(router: &MultiServiceRouter, service_name: &str) -> Vec<String> {
    let listed = ecs_ok(
        router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": service_name, "desiredStatus": "RUNNING" }),
    )
    .await;
    let arns: Vec<String> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
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
    addresses.sort();
    addresses
}

/// Read every unbound group and assert each reports NOTHING.
///
/// Called on every pass of the convergence wait and again afterwards, so a registration that
/// leaked into one of them at any instant of that window fails the suite rather than going unseen
/// between two looks.
async fn assert_every_unbound_group_is_empty(router: &MultiServiceRouter, groups: &[(&str, String)])
{
    for (case, group) in groups {
        let reported = reported_health(router, group).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: a target group no service's loadBalancers names reports an empty \
             TargetHealthDescriptions list, got {reported:?}"
        );
    }
}

/// RULE: the group a service's `loadBalancers` names holds that service's running task at its own
/// interface address on the bound port and settles it at the verdict the checks reached, while
/// every group nothing names stays empty on every read and the service that declares no binding
/// runs its task registered nowhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unbound_target_group_reports_nothing_while_the_bound_services_task_is_registered() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let command = serving_command(MARKER);
    let command: Vec<&str> = command.iter().map(String::as_str).collect();
    let revision = register_revision(&router, FAMILY, CONTAINER_PORT, &command).await;

    // The control groups, created before anything is placed, so they are unbound for the whole
    // window the tasks come up in.
    let mut unbound_groups: Vec<(&str, String)> = Vec::new();
    for row in UNBOUND {
        let group = create_unbound_group(&router, &network, row).await;
        unbound_groups.push((row.case, group));
    }
    let (bound_group, _published) = front_door(
        &router,
        &network,
        "the bound service's front door",
        BOUND_GROUP,
        BOUND_LOAD_BALANCER,
        CONTAINER_PORT,
    )
    .await;

    // Two otherwise identical services: one binding that group, one declaring no binding at all.
    let created = create_service(
        &router,
        &network,
        BOUND_SERVICE,
        &revision,
        Some(&bound_group),
    )
    .await;
    assert_eq!(
        created["service"]["loadBalancers"][0]["targetGroupArn"].as_str(),
        Some(bound_group.as_str()),
        "the created service must carry the binding it was created with, got {created}"
    );
    create_service(&router, &network, UNBOUND_SERVICE, &revision, None).await;

    // Converge: each service runs its task, and the bound group reports exactly one registration
    // settled at the verdict its checks reached. Every unbound group is read on EVERY pass.
    let deadline = Instant::now() + DEADLINE;
    let settled: Vec<ReportedHealth> = loop {
        assert_every_unbound_group_is_empty(&router, &unbound_groups).await;
        let bound = described(&router, BOUND_SERVICE).await;
        let unbound = described(&router, UNBOUND_SERVICE).await;
        let reported = reported_health(&router, &bound_group).await;
        let converged = running_count(&bound) == Some(DESIRED_COUNT)
            && running_count(&unbound) == Some(DESIRED_COUNT)
            && reported.len() == 1
            && reported[0].state == HEALTHY;
        if converged {
            break reported;
        }
        assert!(
            Instant::now() < deadline,
            "expected both services to report runningCount {DESIRED_COUNT} and the bound group to \
             report one registration reported {HEALTHY}, got {reported:?} with the bound service \
             {bound} and the unbound service {unbound}"
        );
        ::tokio::time::sleep(POLL).await;
    };

    // The positive control: the registration is the bound service's own task, at the address its
    // interface holds, on the port the binding named.
    let bound_addresses = task_addresses(&router, BOUND_SERVICE).await;
    assert_eq!(
        bound_addresses.len(),
        DESIRED_COUNT,
        "expected {DESIRED_COUNT} running task(s) of {BOUND_SERVICE} carrying an interface \
         address, got {bound_addresses:?}"
    );
    let entry = &settled[0];
    assert_eq!(
        entry.id, bound_addresses[0],
        "the registration must name the bound service's own interface address, got {entry:?} \
         beside {bound_addresses:?}"
    );
    assert_eq!(
        entry.port,
        CONTAINER_PORT.to_string(),
        "the registration receives traffic on the bound containerPort, got {entry:?}"
    );
    assert!(
        entry.reason.is_empty() && entry.description.is_empty(),
        "a registration reported {HEALTHY} carries no reason and no description, got {entry:?}"
    );

    // The negative control: the service that declared no binding runs its task, reports its
    // `loadBalancers` as an empty list, and that task is a target of nothing.
    let unbound_service = described(&router, UNBOUND_SERVICE).await;
    assert_eq!(
        unbound_service["loadBalancers"],
        json!([]),
        "a service created with no loadBalancers block reports loadBalancers as an empty list, \
         got {unbound_service}"
    );
    let unbound_addresses = task_addresses(&router, UNBOUND_SERVICE).await;
    assert_eq!(
        unbound_addresses.len(),
        DESIRED_COUNT,
        "expected {DESIRED_COUNT} running task(s) of {UNBOUND_SERVICE} carrying an interface \
         address, got {unbound_addresses:?}"
    );
    for address in &unbound_addresses {
        assert!(
            !bound_addresses.contains(address),
            "the two services' tasks hold interfaces of their own, got {unbound_addresses:?} \
             beside {bound_addresses:?}"
        );
        assert_ne!(
            &entry.id, address,
            "the task of a service with no loadBalancers block is a target of no group, got \
             {entry:?}"
        );
    }
    assert_every_unbound_group_is_empty(&router, &unbound_groups).await;

    // And reading reports: every further read of an unbound group answers the same empty list, and
    // the bound group's own registration is exactly the one the first read reported.
    for pass in 0..REREADS {
        assert_every_unbound_group_is_empty(&router, &unbound_groups).await;
        let reread = reported_health(&router, &bound_group).await;
        assert_eq!(
            reread, settled,
            "pass {pass}: reading a target group's health must leave what the next caller reads \
             unchanged, got {reread:?} after {settled:?}"
        );
    }
}
