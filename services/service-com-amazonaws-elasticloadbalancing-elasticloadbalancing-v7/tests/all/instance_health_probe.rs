// SPDX-License-Identifier: CC0-1.0

//! ONE `DescribeInstanceHealth` answer about SEVERAL registered backends of one load
//! balancer.
//!
//! The rule under test: the health of an instance is the verdict of that instance's OWN
//! health checks, so one answer carries one `InstanceState` per registered instance — in
//! registration order, each with the triple its own backend and its own timeline earned.
//! Several registrations of one load balancer therefore report DIFFERENT states in the same
//! answer: an instance whose checks are answered is `InService` / `N/A` / `N/A` once
//! `HealthyThreshold` of them agree, one whose backend answers none reports `OutOfService`
//! / `Instance` with the condition the checks ran into once `UnhealthyThreshold` of them
//! do, and one registered later is still coming up while the others have settled.
//!
//! That is the BATCH half of the rule the sibling suite `instance_health_follows_the_backend`
//! asserts one backend at a time: a check is the one the load balancer's own
//! `HealthCheck$Target` describes, and what answers it is the server the instance's own user
//! data started. The backends here are launched, started and stopped through EC2's OWN API on
//! the same engine, exactly as that suite does, so what a row asserts rests on what EC2's own
//! reader reports.

use smithy_server_runtime::MultiServiceRouter;

use super::wire::{
    call, emulator_with_ec2, kid_text, launch_instance, launch_serving_instance, members,
    move_instance, param, parse, place_registration, serving_user_data, settle_ec2_transition,
};

/// The region every call here is signed for.
const REGION: &str = "us-east-1";

/// The load balancer every instance below is registered with.
const LOAD_BALANCER: &str = "mixed-fleet-elb";

/// The thresholds its health check is configured with — deliberately different, so a
/// verdict reached on one cannot be the other's.
const HEALTHY_THRESHOLD: i32 = 2;
const UNHEALTHY_THRESHOLD: i32 = 3;

/// The port the load balancer's health check connects to, and the ping path it asks for
/// there: a backend that ANSWERS its checks is one whose own program serves that document on
/// that port.
const CHECKED_PORT: i32 = 8080;
const CHECKED_PATH: &str = "/health";

/// The health triple of one `InstanceState` member: `(State, ReasonCode, Description)`.
type Triple = (Option<String>, Option<String>, Option<String>);

fn triple(state: &str, reason_code: &str, description: &str) -> Triple {
    (
        Some(state.to_string()),
        Some(reason_code.to_string()),
        Some(description.to_string()),
    )
}

/// The triples `InstanceState$State`, `$ReasonCode` and `$Description` enumerate for each
/// situation below, written out here so what is asserted is the wire text AWS sends rather
/// than the constant the implementation happens to carry.
fn in_service() -> Triple {
    triple("InService", "N/A", "N/A")
}

fn registration_in_progress() -> Triple {
    triple(
        "OutOfService",
        "ELB",
        "Instance registration is still in progress.",
    )
}

fn stopped_backend() -> Triple {
    triple("OutOfService", "Instance", "Instance is in stopped state.")
}

fn failed_health_checks() -> Triple {
    triple(
        "OutOfService",
        "Instance",
        "Instance has failed at least the UnhealthyThreshold number of health checks \
         consecutively.",
    )
}

/// Place ONE stored registration so that exactly `checks` of the health checks it owes have
/// fallen due: the shared seam, read against this suite's own load balancer.
fn place(engine: &engine::InMemoryEngine, instance_id: &str, checks: u32) {
    place_registration(engine, REGION, LOAD_BALANCER, instance_id, checks);
}

/// Register every id of `instance_ids` with the load balancer in ONE call.
async fn register(router: &MultiServiceRouter, instance_ids: &[String]) {
    let mut params = vec![param("LoadBalancerName", LOAD_BALANCER)];
    for (index, instance_id) in instance_ids.iter().enumerate() {
        params.push(param(
            &format!("Instances.member.{}.InstanceId", index + 1),
            instance_id,
        ));
    }
    let registered = call(
        router,
        REGION,
        "RegisterInstancesWithLoadBalancer",
        &params,
    )
    .await;
    assert_eq!(
        registered.status, 200,
        "registering {instance_ids:?} must answer 200, got {}",
        registered.body
    );
}

/// The `(InstanceId, triple)` of every member of a filter-less describe, in the order the
/// answer lists them.
async fn reported_fleet(router: &MultiServiceRouter) -> Vec<(String, Triple)> {
    let answer = call(
        router,
        REGION,
        "DescribeInstanceHealth",
        &[param("LoadBalancerName", LOAD_BALANCER)],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "DescribeInstanceHealth must answer 200, got {}",
        answer.body
    );
    let doc = parse(&answer.body);
    members(answer.result(&doc), "InstanceStates")
        .into_iter()
        .map(|member| {
            (
                kid_text(member, "InstanceId").unwrap_or_default(),
                (
                    kid_text(member, "State"),
                    kid_text(member, "ReasonCode"),
                    kid_text(member, "Description"),
                ),
            )
        })
        .collect()
}

#[tokio::test]
async fn one_answer_carries_each_registration_s_own_verdict() {
    let (router, engine) = emulator_with_ec2();

    // Three backends a check can tell apart: one whose own server answers the check, one
    // EC2 reports stopped, and an id of the right form it holds no instance for. Each state
    // is reached through EC2's own API.
    let serving = launch_serving_instance(
        &router,
        REGION,
        "serving",
        &serving_user_data(CHECKED_PORT, &[CHECKED_PATH]),
    )
    .await;
    let stopped = launch_instance(&router, REGION, "stopped").await;
    settle_ec2_transition().await;
    move_instance(&router, REGION, "stopped", "StopInstances", &stopped).await;
    let absent = "i-00000000000000bad".to_string();

    let created = call(
        &router,
        REGION,
        "CreateLoadBalancer",
        &[
            param("LoadBalancerName", LOAD_BALANCER),
            param("Listeners.member.1.Protocol", "HTTP"),
            param("Listeners.member.1.LoadBalancerPort", "80"),
            param("Listeners.member.1.InstancePort", &CHECKED_PORT.to_string()),
            param("AvailabilityZones.member.1", &format!("{REGION}a")),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "CreateLoadBalancer must answer 200, got {}",
        created.body
    );
    let configured = call(
        &router,
        REGION,
        "ConfigureHealthCheck",
        &[
            param("LoadBalancerName", LOAD_BALANCER),
            param(
                "HealthCheck.Target",
                &format!("HTTP:{CHECKED_PORT}{CHECKED_PATH}"),
            ),
            param("HealthCheck.Interval", "10"),
            param("HealthCheck.Timeout", "5"),
            param(
                "HealthCheck.HealthyThreshold",
                &HEALTHY_THRESHOLD.to_string(),
            ),
            param(
                "HealthCheck.UnhealthyThreshold",
                &UNHEALTHY_THRESHOLD.to_string(),
            ),
        ],
    )
    .await;
    assert_eq!(
        configured.status, 200,
        "ConfigureHealthCheck must answer 200, got {}",
        configured.body
    );

    let fleet = vec![serving.clone(), stopped.clone(), absent.clone()];
    register(&router, &fleet).await;

    // One member per registered instance, in registration order — and no check has fallen
    // due yet, so every one of them is still coming up.
    for instance_id in &fleet {
        place(&engine, instance_id, 0);
    }
    let fresh = reported_fleet(&router).await;
    assert_eq!(
        fresh,
        fleet
            .iter()
            .map(|id| (id.clone(), registration_in_progress()))
            .collect::<Vec<(String, Triple)>>(),
        "right after registration every member must be coming up, got {fresh:?}"
    );

    // `HealthyThreshold` checks in: the backend that answers them is in service, while the
    // two that answer none are one check short of the unhealthy threshold.
    let healthy_run = u32::try_from(HEALTHY_THRESHOLD).expect("a modelled threshold");
    for instance_id in &fleet {
        place(&engine, instance_id, healthy_run);
    }
    let partway = reported_fleet(&router).await;
    assert_eq!(
        partway,
        vec![
            (serving.clone(), in_service()),
            (stopped.clone(), registration_in_progress()),
            (absent.clone(), registration_in_progress()),
        ],
        "after {HEALTHY_THRESHOLD} checks only the answering backend has a verdict, got \
         {partway:?}"
    );

    // The check that reaches `UnhealthyThreshold`: each failing registration now reports
    // the condition ITS checks ran into, and the serving one is untouched by them.
    let unhealthy_run = u32::try_from(UNHEALTHY_THRESHOLD).expect("a modelled threshold");
    for instance_id in &fleet {
        place(&engine, instance_id, unhealthy_run);
    }
    let settled = reported_fleet(&router).await;
    assert_eq!(
        settled,
        vec![
            (serving.clone(), in_service()),
            (stopped.clone(), stopped_backend()),
            (absent.clone(), failed_health_checks()),
        ],
        "after {UNHEALTHY_THRESHOLD} checks each member must carry its own verdict, got \
         {settled:?}"
    );

    // A fourth backend, registered now: its checks start at ITS registration, so the same
    // answer carries three settled verdicts and one registration still coming up.
    let joining = launch_serving_instance(
        &router,
        REGION,
        "joining",
        &serving_user_data(CHECKED_PORT, &[CHECKED_PATH]),
    )
    .await;
    settle_ec2_transition().await;
    register(&router, std::slice::from_ref(&joining)).await;
    place(&engine, &joining, 0);
    let mixed = reported_fleet(&router).await;
    assert_eq!(
        mixed,
        vec![
            (serving.clone(), in_service()),
            (stopped.clone(), stopped_backend()),
            (absent.clone(), failed_health_checks()),
            (joining.clone(), registration_in_progress()),
        ],
        "a registration made later must carry its own timeline, got {mixed:?}"
    );

    // And it reaches its own verdict on its own run of checks, leaving the others where
    // their checks left them.
    place(&engine, &joining, healthy_run);
    let caught_up = reported_fleet(&router).await;
    assert_eq!(
        caught_up,
        vec![
            (serving, in_service()),
            (stopped, stopped_backend()),
            (absent, failed_health_checks()),
            (joining, in_service()),
        ],
        "the later registration must reach its own verdict, got {caught_up:?}"
    );
}
