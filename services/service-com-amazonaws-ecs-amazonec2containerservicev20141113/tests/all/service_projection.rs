// SPDX-License-Identifier: CC0-1.0

//! What a SERVICE projection carries, and the lifecycle the service's own clock drives.
//!
//! Three rules, each asserted over a table of inputs rather than one recorded case:
//!
//! 1. Every member AWS materialises for a service is computed from the request and the
//!    deployment controller and then STORED, so a read reports the same value the create
//!    did: `createdBy` (the calling principal), `deploymentController`, the
//!    `deploymentConfiguration` defaults, `healthCheckGracePeriodSeconds`, `roleArn` and
//!    `currentServiceRevisions`. The two members that belong to the ROLLING update the
//!    ECS controller performs — `strategy` and `deploymentCircuitBreaker` — and the
//!    scheduler-owned rollout members are carried only by a service whose controller is
//!    `ECS`.
//! 2. `Deployment$rolloutState`: "When a service deployment is started, it begins in an
//!    IN_PROGRESS state. When the service reaches a steady state, the deployment
//!    transitions to a COMPLETED state" — the steady state is owed by the CLOCK, and
//!    reaching it is what writes the service's events.
//! 3. `DeleteService` KEEPS the record: "Services in the DRAINING or INACTIVE status can
//!    still be viewed with the DescribeServices API operation", and `UpdateService`
//!    refuses a service that is not `ACTIVE`.
//! 4. `Service$platformVersion` is the version the service's tasks are placed with: the
//!    version a service the ECS scheduler places ASKED for (the `LATEST` alias when it
//!    named none), and for a service whose tasks come from a task set the concrete version
//!    those tasks RUN. The ARN a task set names its service by follows which of the two
//!    created it.
//! 5. The members Amazon ECS materialises into a service are resolved once, when the record
//!    is written, so EVERY operation that answers with a `Service` renders that one record:
//!    `availabilityZoneRebalancing` (`ENABLED` where the create named none, kept by an
//!    update that names none), the resolved `platformVersion`, the long-ARN form the task
//!    set Amazon ECS mints names its service by, and every instant the record carries,
//!    encoded at no finer than millisecond resolution.
//!
//! Every case goes through the shared wire harness in [`crate::ecs_wire`], the same front
//! door a client uses, so nothing reaches into the state struct and a response that is
//! not computed from persisted state cannot pass.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    assert_millisecond_resolution, awsvpc_network_configuration, caller_principal, count, list,
    ok, refused, register_capacity, register_definition, service, string, ACCOUNT,
    SIGNED_REGION,
};

/// Long enough for one clock-owed transition (`state::TRANSITION_SPAN` is 500 ms) to
/// fall due, with margin for a loaded test machine.
const ONE_SPAN: Duration = Duration::from_millis(900);

/// How often a poll re-reads a service.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll keeps re-reading before it gives up.
///
/// A rolling update is several steps — the replacement is placed, comes up, and only then
/// is the task it replaces retired — so the rollout that REPLACES a task set is polled for
/// rather than timed. Generous, since every poll stops the instant its condition holds.
const POLL_BUDGET: Duration = Duration::from_secs(30);

/// Describe one service, asserting it is reported (rather than failed) and returning it.
async fn describe(
    router: &MultiServiceRouter,
    cluster: &str,
    service_name: &str,
    case: &str,
) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": cluster, "services": [service_name] }),
        case,
    )
    .await;
    assert!(
        list(&body, "failures", case).is_empty(),
        "case {case:?}: a stored service is described, not failed, got {body}"
    );
    let services = list(&body, "services", case);
    assert_eq!(
        services.len(),
        1,
        "case {case:?}: expected exactly one described service, got {body}"
    );
    services[0].clone()
}

/// The event messages a service reports, newest first.
fn event_messages(service: &Value, case: &str) -> Vec<String> {
    list(service, "events", case)
        .iter()
        .map(|event| string(event, "message", case))
        .collect()
}

/// Re-read one service until `settled` holds of it, and answer the service it settled as.
async fn poll_until(
    router: &MultiServiceRouter,
    cluster: &str,
    service_name: &str,
    case: &str,
    what: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let view = describe(router, cluster, service_name, case).await;
        if settled(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what}, got {view}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the projection table: how the service is created, and what the members AWS
/// materialises must then read as.
#[derive(Debug)]
struct ProjectionCase {
    name: &'static str,
    service_name: &'static str,
    /// The `deploymentController` the request names, if any.
    controller: Option<&'static str>,
    /// The `role` the request names, if any.
    role: Option<&'static str>,
    /// The `healthCheckGracePeriodSeconds` the request names, if any.
    grace_period: Option<i64>,
    /// The `deploymentConfiguration` the request names, if any.
    configuration: Option<Value>,
    /// The `loadBalancers` the request names, if any. A CODE_DEPLOY-controlled service
    /// REQUIRES one (CodeDeploy shifts traffic between two target groups), so that row
    /// carries a binding — onto the revision that declares the container port it names.
    load_balancers: Option<Value>,
    /// The controller the stored service must report.
    expect_controller: &'static str,
    /// The `roleArn` the stored service must report, if any.
    expect_role_arn: Option<String>,
    /// The `healthCheckGracePeriodSeconds` the stored service must report.
    expect_grace_period: i64,
    /// The `deploymentConfiguration.strategy` the stored service must report, if any.
    expect_strategy: Option<&'static str>,
    /// The `deploymentConfiguration.bakeTimeInMinutes` the stored service must report.
    expect_bake_time: i64,
}

#[tokio::test]
async fn a_created_service_materialises_its_documented_defaults_and_reads_them_back() {
    // BOTH stores: the CODE_DEPLOY row below must carry a load-balancer binding, and a
    // binding names a target group the Elastic Load Balancing v2 service has to hold.
    let router = crate::load_balancer_target_groups::emulator();
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": "projected" }),
        "setup",
    )
    .await;
    let definition_arn = register_definition(&router, "projectedfamily", "setup").await;
    // A revision whose container declares a container PORT, for the row that binds the
    // service to a load balancer: a binding may only name a port the revision declares.
    let bound_definition_arn = {
        let body = ok(
            &router,
            "RegisterTaskDefinition",
            json!({
                "family": "projectedboundfamily",
                "containerDefinitions": [{
                    "name": "web",
                    "image": "public.ecr.aws/nginx/nginx:latest",
                    "memory": 128,
                    "portMappings": [{ "containerPort": 80 }],
                }],
            }),
            "setup",
        )
        .await;
        string(&body["taskDefinition"], "taskDefinitionArn", "setup")
    };
    // The `instance` target group the CODE_DEPLOY row binds to, created through the ELBv2
    // service's own API on the same engine.
    let code_deploy_target_group = crate::load_balancer_target_groups::create_target_group(
        &router,
        "projected-tg",
        "instance",
        "setup",
    )
    .await;

    // The service-linked role ARN, from the `role` doc: "If your account has already
    // created the Amazon ECS service-linked role, that role is used for your service
    // unless you specify a role here. The service-linked role is required if ... the
    // service is configured to use ... an external deployment controller".
    let service_linked_role = format!(
        "arn:aws:iam::{ACCOUNT}:role/aws-service-role/ecs.amazonaws.com/AWSServiceRoleForECS"
    );

    // The rows differ along the deployment controller (which decides the rollout
    // members), the form of the `role` reference (absent, a name, a path-prefixed name, a
    // full ARN), the grace period (absent, the default value, another value) and how much
    // of the deployment configuration the request supplies.
    let cases = [
        ProjectionCase {
            name: "the ECS controller with nothing else supplied",
            service_name: "defaults",
            controller: None,
            role: None,
            grace_period: None,
            configuration: None,
            load_balancers: None,
            expect_controller: "ECS",
            expect_role_arn: None,
            expect_grace_period: 0,
            expect_strategy: Some("ROLLING"),
            expect_bake_time: 0,
        },
        ProjectionCase {
            name: "an EXTERNAL controller takes the service-linked role",
            service_name: "external",
            controller: Some("EXTERNAL"),
            role: None,
            grace_period: None,
            configuration: None,
            load_balancers: None,
            expect_controller: "EXTERNAL",
            expect_role_arn: Some(service_linked_role.clone()),
            expect_grace_period: 0,
            // The rolling-update members belong to the ECS controller alone.
            expect_strategy: None,
            expect_bake_time: 0,
        },
        ProjectionCase {
            name: "a CODE_DEPLOY controller takes the service-linked role",
            service_name: "codedeploy",
            controller: Some("CODE_DEPLOY"),
            role: None,
            grace_period: Some(0),
            configuration: None,
            // CodeDeploy drives the deployment by shifting traffic between two target
            // groups, so the service it controls has to be behind a load balancer. The
            // group is a real `instance` one: the revision is in the `bridge` network
            // mode, whose tasks register by the container instance that runs them.
            load_balancers: Some(json!([{
                "targetGroupArn": code_deploy_target_group,
                "containerName": "web",
                "containerPort": 80,
            }])),
            expect_controller: "CODE_DEPLOY",
            expect_role_arn: Some(service_linked_role.clone()),
            expect_grace_period: 0,
            expect_strategy: None,
            expect_bake_time: 0,
        },
        ProjectionCase {
            name: "a role named by its name",
            service_name: "namedrole",
            controller: None,
            role: Some("ecsServiceRole"),
            grace_period: Some(120),
            configuration: None,
            load_balancers: None,
            expect_controller: "ECS",
            expect_role_arn: Some(format!("arn:aws:iam::{ACCOUNT}:role/ecsServiceRole")),
            expect_grace_period: 120,
            expect_strategy: Some("ROLLING"),
            expect_bake_time: 0,
        },
        ProjectionCase {
            name: "a role named by a path-prefixed name",
            service_name: "pathrole",
            controller: None,
            role: Some("/team/ecsServiceRole"),
            grace_period: Some(2_147_483_647),
            configuration: None,
            load_balancers: None,
            expect_controller: "ECS",
            expect_role_arn: Some(format!("arn:aws:iam::{ACCOUNT}:role/team/ecsServiceRole")),
            expect_grace_period: 2_147_483_647,
            expect_strategy: Some("ROLLING"),
            expect_bake_time: 0,
        },
        ProjectionCase {
            name: "a role named by its full ARN",
            service_name: "arnrole",
            controller: None,
            role: Some("arn:aws:iam::123456789012:role/custom"),
            grace_period: None,
            configuration: None,
            load_balancers: None,
            expect_controller: "ECS",
            expect_role_arn: Some("arn:aws:iam::123456789012:role/custom".to_string()),
            expect_grace_period: 0,
            expect_strategy: Some("ROLLING"),
            expect_bake_time: 0,
        },
        ProjectionCase {
            name: "a deployment configuration that supplies the rolling members itself",
            service_name: "configured",
            controller: None,
            role: None,
            grace_period: None,
            configuration: Some(json!({
                "strategy": "BLUE_GREEN",
                "bakeTimeInMinutes": 15,
                "deploymentCircuitBreaker": { "enable": true, "rollback": true },
            })),
            load_balancers: None,
            expect_controller: "ECS",
            expect_role_arn: None,
            expect_grace_period: 0,
            expect_strategy: Some("BLUE_GREEN"),
            expect_bake_time: 15,
        },
    ];

    for test in &cases {
        let case = test.name;
        let mut request = json!({
            "cluster": "projected",
            "serviceName": test.service_name,
            "desiredCount": 1,
            "launchType": "EC2",
        });
        // An EXTERNAL-controller service's tasks come from a task set, not from the
        // service, so it names no task definition.
        if test.controller != Some("EXTERNAL") {
            request["taskDefinition"] = json!(definition_arn);
        }
        if let Some(controller) = test.controller {
            request["deploymentController"] = json!({ "type": controller });
        }
        if let Some(role) = test.role {
            request["role"] = json!(role);
        }
        if let Some(seconds) = test.grace_period {
            request["healthCheckGracePeriodSeconds"] = json!(seconds);
        }
        if let Some(configuration) = test.configuration.clone() {
            request["deploymentConfiguration"] = configuration;
        }
        if let Some(load_balancers) = test.load_balancers.clone() {
            request["loadBalancers"] = load_balancers;
            // The binding names a container port, so the service runs the revision that
            // declares one.
            request["taskDefinition"] = json!(bound_definition_arn);
        }

        let created = ok(&router, "CreateService", request, case).await;
        let created = created["service"].clone();
        // Create and read must agree member for member: the response is the stored
        // record, not the request.
        let read = describe(&router, "projected", test.service_name, case).await;
        for reported in [&created, &read] {
            assert_eq!(
                string(reported, "createdBy", case),
                caller_principal(),
                "case {case:?}: createdBy must name the authenticated caller, got {reported}"
            );
            assert_eq!(
                string(&reported["deploymentController"], "type", case),
                test.expect_controller,
                "case {case:?}: unexpected deploymentController in {reported}"
            );
            assert_eq!(
                reported["roleArn"].as_str(),
                test.expect_role_arn.as_deref(),
                "case {case:?}: unexpected roleArn in {reported}"
            );
            assert_eq!(
                count(reported, "healthCheckGracePeriodSeconds", case),
                test.expect_grace_period,
                "case {case:?}: unexpected healthCheckGracePeriodSeconds in {reported}"
            );

            let configuration = &reported["deploymentConfiguration"];
            assert_eq!(
                count(configuration, "bakeTimeInMinutes", case),
                test.expect_bake_time,
                "case {case:?}: unexpected bakeTimeInMinutes in {reported}"
            );
            assert_eq!(
                configuration["strategy"].as_str(),
                test.expect_strategy,
                "case {case:?}: unexpected deployment strategy in {reported}"
            );
            match test.expect_strategy {
                Some(_) => {
                    let breaker = &configuration["deploymentCircuitBreaker"];
                    assert!(
                        breaker["enable"].is_boolean() && breaker["rollback"].is_boolean(),
                        "case {case:?}: an ECS-controller service carries a deployment \
                         circuit breaker, got {reported}"
                    );
                    // "By default, the threshold configuration uses a type of
                    // BOUNDED_PERCENT with a value of 50."
                    if test.configuration.is_none() {
                        assert_eq!(
                            breaker["enable"], json!(false),
                            "case {case:?}: the circuit breaker is off unless configured, \
                             got {reported}"
                        );
                        let threshold = &breaker["thresholdConfiguration"];
                        assert_eq!(
                            (threshold["type"].as_str(), threshold["value"].as_i64()),
                            (Some("BOUNDED_PERCENT"), Some(50)),
                            "case {case:?}: unexpected default threshold in {reported}"
                        );
                    }
                }
                None => assert!(
                    configuration["deploymentCircuitBreaker"].is_null(),
                    "case {case:?}: the circuit breaker is for the rolling update of the \
                     ECS controller only, got {reported}"
                ),
            }

            // The ECS scheduler owns the rollout: one deployment and one service
            // revision, counted off the task set it placed. The other controllers' tasks
            // come from a task set, so they carry neither.
            let revisions = list(reported, "currentServiceRevisions", case);
            let deployments = list(reported, "deployments", case);
            if test.expect_controller == "ECS" {
                assert_eq!(
                    deployments.len(),
                    1,
                    "case {case:?}: expected one deployment, got {reported}"
                );
                assert_eq!(
                    revisions.len(),
                    1,
                    "case {case:?}: expected one current service revision, got {reported}"
                );
                let revision_arn = string(&revisions[0], "arn", case);
                let prefix = format!(
                    "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service-revision/projected/{}/",
                    test.service_name
                );
                assert!(
                    revision_arn.starts_with(&prefix),
                    "case {case:?}: expected a service-revision ARN under {prefix:?}, \
                     got {revision_arn:?}"
                );
            } else {
                assert!(
                    deployments.is_empty() && revisions.is_empty(),
                    "case {case:?}: a service whose tasks come from a task set carries no \
                     deployment and no service revision, got {reported}"
                );
            }
        }
        if test.expect_controller == "ECS" {
            // The count the revision and the deployment report is the one the SCHEDULER has
            // taken on: the create is answered before its pass, so it reports none of the
            // count yet, and the read that follows reports the count the service asked for.
            let reported_counts = |view: &Value| {
                let revisions = list(view, "currentServiceRevisions", case);
                let deployments = list(view, "deployments", case);
                (
                    count(&revisions[0], "requestedTaskCount", case),
                    count(&deployments[0], "desiredCount", case),
                )
            };
            assert_eq!(
                reported_counts(&created),
                (0, 0),
                "case {case:?}: the create is answered before the scheduler has taken the \
                 count on, got {created}"
            );
            assert_eq!(
                reported_counts(&read),
                (1, 1),
                "case {case:?}: the revision requests the service's count once the \
                 scheduler has taken it on, got {read}"
            );
        }
    }
}

#[tokio::test]
async fn a_rollout_reaches_its_steady_state_on_the_clock_and_writes_its_events() {
    let router = service();
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": "settling" }),
        "setup",
    )
    .await;
    // "The EC2 launch type runs your tasks on Amazon EC2 instances registered to your
    // cluster", so the rollout needs capacity registered to place its tasks on.
    register_capacity(&router, "settling", "setup").await;
    let definition_arn = register_definition(&router, "settlingfamily", "setup").await;

    // The rule generalises over the number of tasks the rollout has to place, zero
    // included: a deployment is never COMPLETED in the instant it was started.
    let counts: [i64; 3] = [0, 1, 3];
    for desired in counts {
        let case = &format!("a rollout of {desired} tasks");
        let service_name = &format!("web{desired}");
        let created = ok(
            &router,
            "CreateService",
            json!({
                "cluster": "settling",
                "serviceName": service_name,
                "taskDefinition": definition_arn,
                "desiredCount": desired,
                "launchType": "EC2",
            }),
            case,
        )
        .await;
        let created = created["service"].clone();
        let deployment = list(&created, "deployments", case)[0].clone();
        assert_eq!(
            string(&deployment, "rolloutState", case),
            "IN_PROGRESS",
            "case {case:?}: a deployment begins IN_PROGRESS, got {created}"
        );
        assert!(
            event_messages(&created, case).is_empty(),
            "case {case:?}: no steady state has been reached yet, so there is no event \
             to report, got {created}"
        );

        // The CLOCK settles the rollout: the same read before the span reports it still
        // in progress.
        let early = describe(&router, "settling", service_name, case).await;
        assert_eq!(
            string(&list(&early, "deployments", case)[0], "rolloutState", case),
            "IN_PROGRESS",
            "case {case:?}: reading a service does not settle its rollout, got {early}"
        );

        let settled = poll_until(
            &router,
            "settling",
            service_name,
            case,
            "the rollout to reach its steady state",
            |view| string(&list(view, "deployments", case)[0], "rolloutState", case) == "COMPLETED",
        )
        .await;
        assert_eq!(
            string(&list(&settled, "deployments", case)[0], "rolloutState", case),
            "COMPLETED",
            "case {case:?}: a rollout that reached its steady state is COMPLETED, got \
             {settled}"
        );
        assert_eq!(
            count(&settled, "runningCount", case),
            desired,
            "case {case:?}: expected every placed task RUNNING, got {settled}"
        );
        let messages = event_messages(&settled, case);
        assert_eq!(
            messages.len(),
            2,
            "case {case:?}: reaching a steady state writes the deployment-completed and \
             steady-state events, got {messages:?}"
        );
        assert!(
            messages[0].contains("steady state") && messages[1].contains("deployment completed"),
            "case {case:?}: the event stream reads newest first, got {messages:?}"
        );
        let events = list(&settled, "events", case);
        assert!(
            events
                .iter()
                .all(|event| !string(event, "id", case).is_empty()
                    && event["createdAt"].is_number()),
            "case {case:?}: every event carries an id and a createdAt, got {events:?}"
        );
        assert_ne!(
            string(&events[0], "id", case),
            string(&events[1], "id", case),
            "case {case:?}: every event has its own id, got {events:?}"
        );

        // Reading again neither duplicates the events nor moves anything on.
        let again = describe(&router, "settling", service_name, case).await;
        assert_eq!(
            event_messages(&again, case),
            messages,
            "case {case:?}: a second read reports the same event stream, got {again}"
        );
    }

    // A new deployment starts a new rollout, which is IN_PROGRESS again and keeps the
    // events the service already recorded.
    let case = "a forced new deployment";
    let updated = ok(
        &router,
        "UpdateService",
        json!({ "cluster": "settling", "service": "web1", "forceNewDeployment": true }),
        case,
    )
    .await;
    let updated = updated["service"].clone();
    assert_eq!(
        string(&list(&updated, "deployments", case)[0], "rolloutState", case),
        "IN_PROGRESS",
        "case {case:?}: a new deployment begins IN_PROGRESS, got {updated}"
    );
    assert_eq!(
        event_messages(&updated, case).len(),
        2,
        "case {case:?}: the service keeps the events it already recorded, got {updated}"
    );
    // The rollout that REPLACES the task set the service already holds takes one step per
    // task it replaces, so the second steady state is polled for rather than timed.
    let settled = poll_until(
        &router,
        "settling",
        "web1",
        case,
        "the second steady state to be reached",
        |view| event_messages(view, case).len() >= 4,
    )
    .await;
    assert_eq!(
        event_messages(&settled, case).len(),
        4,
        "case {case:?}: the second steady state appends its own two events, got {settled}"
    );
}

#[tokio::test]
async fn a_deleted_service_stays_describable_as_inactive_and_refuses_updates() {
    let router = service();
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": "deleting" }),
        "setup",
    )
    .await;
    register_capacity(&router, "deleting", "setup").await;
    let definition_arn = register_definition(&router, "deletingfamily", "setup").await;

    // The rows differ along what the service still holds when it is deleted and how the
    // request addresses it. Whatever it held, the delete leaves it DRAINING: the move to
    // the terminal INACTIVE is owed by the clock.
    let cases: [(&str, &str, i64, bool, bool); 4] = [
        ("a service scaled to zero", "idle", 0, false, false),
        ("a service still running tasks, forced", "busy", 2, true, false),
        ("one task, forced", "single", 1, true, false),
        (
            "addressed by its service ARN",
            "byarn",
            0,
            false,
            true,
        ),
    ];

    for (case, service_name, desired, force, by_arn) in cases {
        ok(
            &router,
            "CreateService",
            json!({
                "cluster": "deleting",
                "serviceName": service_name,
                "taskDefinition": definition_arn,
                "desiredCount": desired,
                "launchType": "EC2",
            }),
            case,
        )
        .await;

        // "If the service is actively maintaining tasks, you can't delete it, and you
        // must update the service to a desired task count of zero."
        if desired > 0 {
            refused(
                &router,
                "DeleteService",
                json!({ "cluster": "deleting", "service": service_name }),
                "InvalidParameterException",
                case,
            )
            .await;
        }

        let mut request = if by_arn {
            json!({
                "service": format!(
                    "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service/deleting/{service_name}"
                ),
            })
        } else {
            json!({ "cluster": "deleting", "service": service_name })
        };
        if force {
            request["force"] = json!(true);
        }
        let deleted = ok(&router, "DeleteService", request.clone(), case).await;
        let deleted = deleted["service"].clone();
        assert_eq!(
            count(&deleted, "desiredCount", case),
            0,
            "case {case:?}: a deleted service keeps no tasks, got {deleted}"
        );
        let status = string(&deleted, "status", case);
        assert_eq!(
            status, "DRAINING",
            "case {case:?}: expected the deletion to leave the service DRAINING, got \
             {deleted}"
        );

        // The record is KEPT: a describe reports the service rather than a MISSING
        // failure, and the clock finishes the drain.
        tokio::time::sleep(ONE_SPAN).await;
        let read = describe(&router, "deleting", service_name, case).await;
        assert_eq!(
            string(&read, "status", case),
            "INACTIVE",
            "case {case:?}: a drained service settles INACTIVE, got {read}"
        );
        assert_eq!(
            count(&read, "runningCount", case),
            0,
            "case {case:?}: a deleted service runs nothing, got {read}"
        );
        assert_eq!(
            string(&read, "serviceArn", case),
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service/deleting/{service_name}"),
            "case {case:?}: the kept record is the one that was deleted, got {read}"
        );

        // "The specified service isn't active. You can't update a service that's
        // inactive."
        refused(
            &router,
            "UpdateService",
            json!({ "cluster": "deleting", "service": service_name, "desiredCount": 1 }),
            "ServiceNotActiveException",
            case,
        )
        .await;

        // `@idempotent`: deleting it again answers with the same INACTIVE record.
        let again = ok(&router, "DeleteService", request, case).await;
        assert_eq!(
            string(&again["service"], "status", case),
            "INACTIVE",
            "case {case:?}: a repeated delete reports the same record, got {again}"
        );
    }

    // What a delete cannot name at all.
    let refusals: [(&str, Value, &str); 3] = [
        (
            "a service that was never created",
            json!({ "cluster": "deleting", "service": "nosuchservice" }),
            "ServiceNotFoundException",
        ),
        (
            "a cluster that does not exist",
            json!({ "cluster": "nosuchcluster", "service": "idle" }),
            "ClusterNotFoundException",
        ),
        (
            "an ARN naming another cluster than the request",
            json!({
                "cluster": "deleting",
                "service": format!(
                    "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service/elsewhere/idle"
                ),
            }),
            "InvalidParameterException",
        ),
    ];
    for (case, request, expected) in refusals {
        refused(&router, "DeleteService", request, expected, case).await;
    }
}

/// The task-level `cpu` and `memory` a Fargate-compatible revision declares (each member's
/// own doc: "This field is required for Fargate"), at a legal Fargate size.
const FARGATE_CPU: &str = "256";
const FARGATE_MEMORY: &str = "512";

/// The same pair for a WINDOWS revision, which Fargate runs at one vCPU or more: the
/// `cpu` doc's Windows note, as the smallest Fargate size published for a Windows container.
const WINDOWS_FARGATE_CPU: &str = "1024";
const WINDOWS_FARGATE_MEMORY: &str = "2048";

/// One row of the platform-version table: how the service's tasks are deployed, the
/// operating system its revision declares, the `platformVersion` the create asks for, and
/// the version the stored service must then report.
struct TaskSetPlatformRow {
    case: &'static str,
    /// The service's name, which is also the family of the revision registered for it.
    name: &'static str,
    /// `deploymentController.type`: `ECS`, whose own scheduler places the service's tasks,
    /// or `CODE_DEPLOY`, which deploys them through the task set Amazon ECS mints.
    controller: &'static str,
    /// The `RuntimePlatform$operatingSystemFamily` the revision declares.
    os_family: &'static str,
    /// The `platformVersion` the create names, or `None` when it names none.
    requested: Option<&'static str>,
    /// The `platformVersion` the service must report, and the one its task set runs.
    expect: &'static str,
}

/// Register one Fargate-capable `awsvpc` revision of `family` on the operating system
/// `os_family`, whose container declares the port a load-balancer binding names.
async fn register_bound_fargate_definition(
    router: &MultiServiceRouter,
    family: &str,
    os_family: &str,
    case: &str,
) {
    let mut web = crate::task_lifecycle::container("web", "public.ecr.aws/nginx/nginx:latest");
    web["portMappings"] = json!([{ "containerPort": 80, "protocol": "tcp" }]);
    let (cpu, memory) = if os_family.starts_with("WINDOWS") {
        (WINDOWS_FARGATE_CPU, WINDOWS_FARGATE_MEMORY)
    } else {
        (FARGATE_CPU, FARGATE_MEMORY)
    };
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": cpu,
            "memory": memory,
            "runtimePlatform": {
                "cpuArchitecture": "X86_64",
                "operatingSystemFamily": os_family,
            },
            "containerDefinitions": [web],
        }),
        case,
    )
    .await;
}

/// The task sets a service reports, as `(status, platformVersion, serviceArn)` triples in
/// the order the service reports them.
fn task_set_views(service: &Value, case: &str) -> Vec<(String, String, String)> {
    list(service, "taskSets", case)
        .iter()
        .map(|task_set| {
            (
                string(task_set, "status", case),
                string(task_set, "platformVersion", case),
                string(task_set, "serviceArn", case),
            )
        })
        .collect()
}

/// `Service$platformVersion` is "The platform version to run your service on ... If one
/// isn't specified, the LATEST platform version is used": a service the ECS scheduler places
/// the tasks of reports the version it ASKED for, which is that alias when it named none,
/// while a service whose tasks come from a task set reports the version those tasks RUN —
/// the concrete version the alias resolves to for their operating system, which is the
/// version its task set reports.
///
/// The rows span the deployment controller, the operating system family the revision
/// declares (and so which published version `LATEST` resolves to — `1.4.0` for Linux and
/// `1.0.0` for Windows, from the `platformVersion` doc's "Fargate platform versions" page)
/// and what the create asks for: nothing, the `LATEST` alias outright, or a published
/// version by name.
#[tokio::test]
async fn a_service_deploying_through_a_task_set_reports_the_version_its_tasks_run() {
    // BOTH stores: a CodeDeploy-controlled service requires a load-balancer binding, and a
    // binding names a target group the Elastic Load Balancing v2 service has to hold.
    let router = crate::load_balancer_target_groups::emulator();
    let cluster = "platformsets";
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": cluster }),
        "setup",
    )
    .await;

    let rows: [TaskSetPlatformRow; 8] = [
        TaskSetPlatformRow {
            case: "the ECS scheduler keeps the alias a create named none left",
            name: "ecsnone",
            controller: "ECS",
            os_family: "LINUX",
            requested: None,
            expect: "LATEST",
        },
        TaskSetPlatformRow {
            case: "the ECS scheduler keeps the alias a create named outright",
            name: "ecslatest",
            controller: "ECS",
            os_family: "LINUX",
            requested: Some("LATEST"),
            expect: "LATEST",
        },
        TaskSetPlatformRow {
            case: "the ECS scheduler keeps a version named by name",
            name: "ecspinned",
            controller: "ECS",
            os_family: "LINUX",
            requested: Some("1.4.0"),
            expect: "1.4.0",
        },
        TaskSetPlatformRow {
            case: "a task-set deployment resolves the alias a create named none left",
            name: "setnone",
            controller: "CODE_DEPLOY",
            os_family: "LINUX",
            requested: None,
            expect: "1.4.0",
        },
        TaskSetPlatformRow {
            case: "a task-set deployment resolves the alias named outright",
            name: "setlatest",
            controller: "CODE_DEPLOY",
            os_family: "LINUX",
            requested: Some("LATEST"),
            expect: "1.4.0",
        },
        TaskSetPlatformRow {
            case: "a task-set deployment keeps a version named by name",
            name: "setpinned",
            controller: "CODE_DEPLOY",
            os_family: "LINUX",
            requested: Some("1.4.0"),
            expect: "1.4.0",
        },
        TaskSetPlatformRow {
            case: "a Windows task-set deployment resolves to the Windows version",
            name: "setwindows",
            controller: "CODE_DEPLOY",
            os_family: "WINDOWS_SERVER_2019_CORE",
            requested: None,
            expect: "1.0.0",
        },
        TaskSetPlatformRow {
            case: "a Windows task-set deployment keeps that version named by name",
            name: "setwindowspinned",
            controller: "CODE_DEPLOY",
            os_family: "WINDOWS_SERVER_2019_CORE",
            requested: Some("1.0.0"),
            expect: "1.0.0",
        },
    ];

    for row in &rows {
        let case = row.case;
        register_bound_fargate_definition(&router, row.name, row.os_family, case).await;
        let mut request = json!({
            "cluster": cluster,
            "serviceName": row.name,
            "taskDefinition": row.name,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "deploymentController": { "type": row.controller },
            "networkConfiguration": awsvpc_network_configuration(),
        });
        if let Some(version) = row.requested {
            request["platformVersion"] = json!(version);
        }
        let deploys_through_a_task_set = row.controller != "ECS";
        if deploys_through_a_task_set {
            // CodeDeploy drives the deployment by shifting traffic between two target
            // groups, so the service it controls has to be behind a load balancer. An
            // `awsvpc` revision's tasks register by their own network interface, so the
            // group is an `ip` one — one per row, since a group serves one service.
            let target_group = crate::load_balancer_target_groups::create_target_group(
                &router,
                &format!("{}-tg", row.name),
                "ip",
                case,
            )
            .await;
            request["loadBalancers"] = json!([{
                "targetGroupArn": target_group,
                "containerName": "web",
                "containerPort": 80,
            }]);
        }
        let created = ok(&router, "CreateService", request, case).await;
        let created = created["service"].clone();
        let service_arn = string(&created, "serviceArn", case);
        // ROUND TRIP: the create and the read report the one stored record.
        let read = describe(&router, cluster, row.name, case).await;
        for (source, reported) in [("the create", &created), ("the read", &read)] {
            assert_eq!(
                string(reported, "platformVersion", case),
                row.expect,
                "case {case:?}: expected {source} to report platformVersion {:?}, got \
                 {reported}",
                row.expect
            );
            let task_sets = task_set_views(reported, case);
            if !deploys_through_a_task_set {
                assert!(
                    task_sets.is_empty(),
                    "case {case:?}: a service its own scheduler places holds no task set, \
                     got {reported}"
                );
                continue;
            }
            // The task set Amazon ECS minted for the service: serving production traffic,
            // running the version the service reports, and naming the service by the
            // service's OWN ARN.
            assert_eq!(
                task_sets,
                vec![(
                    "PRIMARY".to_string(),
                    row.expect.to_string(),
                    service_arn.clone(),
                )],
                "case {case:?}: unexpected task sets from {source}, got {reported}"
            );
        }
    }

    // The ARN a task set names its service by follows WHO created it, so one service can
    // report both forms: the `PRIMARY` task set Amazon ECS minted carries the service's own
    // ARN, and the one a deployment controller creates carries the cluster-less form.
    let case = "a controller's own task set beside the one Amazon ECS minted";
    let service_name = "setpinned";
    let created = ok(
        &router,
        "CreateTaskSet",
        json!({
            "cluster": cluster,
            "service": service_name,
            "taskDefinition": service_name,
            "launchType": "FARGATE",
            "platformVersion": "1.4.0",
            "networkConfiguration": awsvpc_network_configuration(),
            "scale": { "value": 0.0, "unit": "PERCENT" },
        }),
        case,
    )
    .await;
    let controller_task_set_id = string(&created["taskSet"], "id", case);
    let read = describe(&router, cluster, service_name, case).await;
    let service_arn = string(&read, "serviceArn", case);
    let reported = task_set_views(&read, case);
    assert_eq!(
        reported.len(),
        2,
        "case {case:?}: expected both task sets of the service, got {read}"
    );
    assert_eq!(
        reported[0],
        (
            "PRIMARY".to_string(),
            "1.4.0".to_string(),
            service_arn.clone(),
        ),
        "case {case:?}: the task set Amazon ECS minted names the service by its own ARN, \
         got {read}"
    );
    assert_eq!(
        reported[1],
        (
            "ACTIVE".to_string(),
            "1.4.0".to_string(),
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service/{service_name}"),
        ),
        "case {case:?}: a controller's own task set names the service by the cluster-less \
         form, got {read}"
    );
    assert!(
        !controller_task_set_id.is_empty(),
        "case {case:?}: the created task set carries an id, got {created}"
    );
}

/// One row of the stored-record table: how the service is created, and what every operation
/// that answers with that service must then report for the members Amazon ECS materialises
/// into it.
struct ServiceRecordRow {
    case: &'static str,
    /// The service's name, which is also the family of the revision registered for it.
    name: &'static str,
    /// `deploymentController.type`: `ECS`, whose own scheduler places the service's tasks,
    /// or `CODE_DEPLOY`, which deploys them through the task set Amazon ECS mints.
    controller: &'static str,
    /// The `RuntimePlatform$operatingSystemFamily` the revision declares.
    os_family: &'static str,
    /// The `platformVersion` the create names, or `None` when it names none.
    requested_version: Option<&'static str>,
    /// The `availabilityZoneRebalancing` the create names, or `None` when it names none.
    requested_rebalancing: Option<&'static str>,
    /// The `platformVersion` every answer must report.
    expect_version: &'static str,
    /// The `availabilityZoneRebalancing` every answer must report.
    expect_rebalancing: &'static str,
}

/// Every instant a service record carries, as `(member, value)` pairs: the service's own
/// `createdAt`, and the `createdAt` and `updatedAt` of each deployment and each task set it
/// reports.
fn reported_instants(service_view: &Value, case: &str) -> Vec<(String, Value)> {
    let mut instants = vec![("createdAt".to_string(), service_view["createdAt"].clone())];
    for member in ["deployments", "taskSets"] {
        for (index, child) in list(service_view, member, case).iter().enumerate() {
            for instant in ["createdAt", "updatedAt"] {
                instants.push((
                    format!("{member}[{index}].{instant}"),
                    child[instant].clone(),
                ));
            }
        }
    }
    instants
}

/// One service record, read through every operation that answers with one.
///
/// The members Amazon ECS materialises into a service are resolved ONCE, when the record is
/// written, and every operation that answers with a `Service` renders that one record — so
/// `CreateService`, `DescribeServices`, `UpdateService` (which, naming none of them, keeps
/// what the service was set to) and `DeleteService` report the same values:
///
/// * `availabilityZoneRebalancing` — "For create service requests, when no value is
///   specified for AvailabilityZoneRebalancing, Amazon ECS defaults the value to ENABLED",
///   and an update that names none keeps the value the service carries;
/// * `platformVersion` — the version the service's tasks are placed with, which for a
///   service whose tasks come from a task set is the concrete version the `LATEST` alias
///   resolves to for the revision's operating system, never the alias;
/// * `taskSets[].serviceArn` — the ARN of the owning service, which for the task set Amazon
///   ECS mints is the service's own long-ARN form, cluster segment included, so it parses
///   back into the (cluster, service) pair every operation addressing a service by ARN takes;
/// * every instant in the record — `Service$createdAt` and each deployment's and task set's
///   `createdAt`/`updatedAt` — encoded at no finer than millisecond resolution, which is
///   what this API's epoch-second timestamps carry.
///
/// The rows span the deployment controller (and so whether the service holds task sets at
/// all), the operating system family the revision declares, what the create asks for as a
/// platform version (nothing, the alias outright, a published version by name) and each
/// value of `AvailabilityZoneRebalancing`
/// (`smithy_query(query="shape", shape="AvailabilityZoneRebalancing")` = [ENABLED,
/// DISABLED]) beside a create that names none.
#[tokio::test]
async fn every_operation_that_answers_with_a_service_renders_the_one_stored_record() {
    // BOTH stores: a CodeDeploy-controlled service requires a load-balancer binding, and a
    // binding names a target group the Elastic Load Balancing v2 service has to hold.
    let router = crate::load_balancer_target_groups::emulator();
    let cluster = "servicerecords";
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": cluster }),
        "setup",
    )
    .await;

    let rows: [ServiceRecordRow; 4] = [
        ServiceRecordRow {
            case: "an ECS-controlled service that named neither member",
            name: "recordnone",
            controller: "ECS",
            os_family: "LINUX",
            requested_version: None,
            requested_rebalancing: None,
            expect_version: "LATEST",
            expect_rebalancing: "ENABLED",
        },
        ServiceRecordRow {
            case: "an ECS-controlled service that pinned a version and disabled rebalancing",
            name: "recordpinned",
            controller: "ECS",
            os_family: "LINUX",
            requested_version: Some("1.4.0"),
            requested_rebalancing: Some("DISABLED"),
            expect_version: "1.4.0",
            expect_rebalancing: "DISABLED",
        },
        ServiceRecordRow {
            case: "a CodeDeploy-controlled service that named neither member",
            name: "recordset",
            controller: "CODE_DEPLOY",
            os_family: "LINUX",
            requested_version: None,
            requested_rebalancing: None,
            expect_version: "1.4.0",
            expect_rebalancing: "ENABLED",
        },
        ServiceRecordRow {
            case: "a CodeDeploy-controlled Windows service that named the alias",
            name: "recordwindows",
            controller: "CODE_DEPLOY",
            os_family: "WINDOWS_SERVER_2019_CORE",
            requested_version: Some("LATEST"),
            requested_rebalancing: Some("ENABLED"),
            expect_version: "1.0.0",
            expect_rebalancing: "ENABLED",
        },
    ];

    for row in &rows {
        let setup = row.case;
        register_bound_fargate_definition(&router, row.name, row.os_family, setup).await;
        let mut request = json!({
            "cluster": cluster,
            "serviceName": row.name,
            "taskDefinition": row.name,
            // The record is what these rows are about, so no row asks for a task: a service
            // that wants none is deleted without forcing and places nothing.
            "desiredCount": 0,
            "launchType": "FARGATE",
            "deploymentController": { "type": row.controller },
            "networkConfiguration": awsvpc_network_configuration(),
        });
        if let Some(version) = row.requested_version {
            request["platformVersion"] = json!(version);
        }
        if let Some(requested) = row.requested_rebalancing {
            request["availabilityZoneRebalancing"] = json!(requested);
        }
        let deploys_through_a_task_set = row.controller != "ECS";
        if deploys_through_a_task_set {
            // CodeDeploy drives the deployment by shifting traffic between two target
            // groups, so the service it controls has to be behind a load balancer. An
            // `awsvpc` revision's tasks register by their own network interface, so the
            // group is an `ip` one — one per row, since a group serves one service.
            let target_group = crate::load_balancer_target_groups::create_target_group(
                &router,
                &format!("{}-record", row.name),
                "ip",
                setup,
            )
            .await;
            request["loadBalancers"] = json!([{
                "targetGroupArn": target_group,
                "containerName": "web",
                "containerPort": 80,
            }]);
        }

        // The four operations that answer with a `Service`, in the order one client walks
        // them: the create that wrote the record, a read of it, an update that names none of
        // the members under test, the read after that update, and the delete.
        let created = ok(&router, "CreateService", request, setup).await;
        let created = created["service"].clone();
        let read = describe(&router, cluster, row.name, setup).await;
        let updated = ok(
            &router,
            "UpdateService",
            json!({ "cluster": cluster, "service": row.name }),
            setup,
        )
        .await;
        let updated = updated["service"].clone();
        let reread = describe(&router, cluster, row.name, setup).await;
        let deleted = ok(
            &router,
            "DeleteService",
            json!({ "cluster": cluster, "service": row.name }),
            setup,
        )
        .await;
        let deleted = deleted["service"].clone();

        let service_arn = format!(
            "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service/{cluster}/{}",
            row.name
        );
        let created_at = created["createdAt"].clone();
        for (source, reported) in [
            ("CreateService", &created),
            ("DescribeServices", &read),
            ("UpdateService", &updated),
            ("DescribeServices after the update", &reread),
            ("DeleteService", &deleted),
        ] {
            let case = format!("{} through {source}", row.case);
            let case = case.as_str();
            assert_eq!(
                string(reported, "serviceArn", case),
                service_arn,
                "case {case:?}: every answer is about the one service, got {reported}"
            );
            assert_eq!(
                string(reported, "availabilityZoneRebalancing", case),
                row.expect_rebalancing,
                "case {case:?}: expected availabilityZoneRebalancing {:?}, got {reported}",
                row.expect_rebalancing
            );
            assert_eq!(
                string(reported, "platformVersion", case),
                row.expect_version,
                "case {case:?}: expected platformVersion {:?}, got {reported}",
                row.expect_version
            );
            assert_eq!(
                reported["createdAt"],
                created_at,
                "case {case:?}: one record carries one creation instant, got {reported}"
            );

            let task_sets = task_set_views(reported, case);
            if deploys_through_a_task_set {
                assert_eq!(
                    task_sets.len(),
                    1,
                    "case {case:?}: expected the task set Amazon ECS minted for the \
                     service, got {reported}"
                );
                assert_eq!(
                    task_sets[0].1,
                    row.expect_version,
                    "case {case:?}: the task set runs the version the service reports, got \
                     {reported}"
                );
                assert_eq!(
                    task_sets[0].2,
                    service_arn,
                    "case {case:?}: the task set Amazon ECS minted names its service by \
                     that service's own ARN, cluster segment included, got {reported}"
                );
            } else {
                assert!(
                    task_sets.is_empty(),
                    "case {case:?}: a service its own scheduler places holds no task set, \
                     got {reported}"
                );
            }

            for (member, instant) in reported_instants(reported, case) {
                assert!(
                    !instant.is_null(),
                    "case {case:?}: expected {member} on the record, got {reported}"
                );
                assert_millisecond_resolution(case, &member, &instant);
            }
        }
    }
}
