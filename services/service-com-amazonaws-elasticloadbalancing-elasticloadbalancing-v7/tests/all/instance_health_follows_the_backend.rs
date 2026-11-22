// SPDX-License-Identifier: CC0-1.0

//! What `DescribeInstanceHealth` reports for a registered instance, driven over the wire
//! against the BACKEND the registration names.
//!
//! The rule asserted here: **the state of a registered instance is the outcome of the check
//! the load balancer's own `HealthCheck$Target` describes** — "a health check simply attempts
//! to open a TCP connection to the instance on the specified port" for a `TCP` or `SSL`
//! target, and for an `HTTP`/`HTTPS` target "a HTTP GET request is issued to the instance on
//! the given port and path", where "any answer other than \"200 OK\" … is considered
//! unhealthy" (`smithy_query(query="doc",shape="HealthCheck",member="Target")`). So the
//! member reads `InService` with `ReasonCode` and `Description` of `N/A` once
//! `HealthyThreshold` consecutive checks were answered, `OutOfService` with `ReasonCode`
//! `Instance` and the condition the checks ran into once `UnhealthyThreshold` consecutive
//! checks were not, and the registration's own checks are still in progress until one of
//! those thresholds is reached.
//!
//! What can answer such a check is the server the instance's own user data installed and
//! started, which EC2 — the service that owns instances — holds for it, so a backend that
//! answers is launched with the program that starts one and a backend that answers nothing is
//! launched bare.
//!
//! The rows differ along what the rule generalises over: the target's protocol (all four),
//! the port it names (the one the backend serves and one it does not), its ping path (one the
//! backend holds a document at and one it does not), what the backend is doing (serving,
//! running no server at all, stopped, terminated, an id the account holds no instance for)
//! and what the load balancer's two thresholds are configured to, including both bounds
//! `HealthyThreshold`/`UnhealthyThreshold` are modelled with (`@range(min: 2, max: 10)`).
//! Every backend is launched, stopped and terminated through EC2's OWN API on the same
//! engine, so what a row asserts rests on what EC2's own reader reports rather than on a
//! value this suite made up.
//!
//! Where a row stands on its load balancer's timeline is PLACED: the stored registration is
//! stamped so that exactly as many checks as the row is about have fallen due
//! ([`crate::state::InstanceSet::restamp`], through the harness's `place_registration`), so
//! what a read settles does not depend on how long the suite took to get there and nothing is
//! slept for.

use service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::state::{
    DESCRIPTION_INSTANCE_STOPPED, DESCRIPTION_INSTANCE_TERMINATED, HEALTH_FAILED_HEALTH_CHECKS,
    HEALTH_IN_SERVICE, HEALTH_REGISTRATION_IN_PROGRESS, InstanceHealth, REASON_CODE_INSTANCE,
    STATE_OUT_OF_SERVICE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::wire::{
    call, emulator_with_ec2, kid_text, launch_backend, members, param, parse, place_registration,
    serving_user_data, settle_ec2_transition,
};

/// The region every call here is signed for.
const REGION: &str = "us-east-1";

/// Drive one EC2 instance-state action over `instance_id`: the shared seam, in this suite's
/// own region.
async fn move_instance(router: &MultiServiceRouter, case: &str, action: &str, instance_id: &str) {
    super::wire::move_instance(router, REGION, case, action, instance_id).await;
}

/// The health triple of one `InstanceState` member: `(State, ReasonCode, Description)`.
type Triple = (Option<String>, Option<String>, Option<String>);

/// The triple a stored `InstanceHealth` is reported as on the wire, so every expectation
/// is the service's own constant rather than a sentence retyped here.
fn triple(health: InstanceHealth) -> Triple {
    (
        Some(health.state.to_string()),
        Some(health.reason_code.to_string()),
        Some(health.description.to_string()),
    )
}

/// The health of an instance whose checks the load balancer decided against, with the
/// condition those checks ran into: `OutOfService` for the INSTANCE's own reason, which is
/// what `InstanceState$ReasonCode` words as `Instance`.
fn out_of_service(description: &str) -> Triple {
    (
        Some(STATE_OUT_OF_SERVICE.to_string()),
        Some(REASON_CODE_INSTANCE.to_string()),
        Some(description.to_string()),
    )
}

/// What the backend behind a registration is doing — what a check of it can find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// An instance whose own user data started a server on `port`, holding a document at
    /// each path of `paths`.
    Serving {
        port: i32,
        paths: &'static [&'static str],
    },
    /// An instance EC2 launched and nothing else: it is running, and no program of its guest
    /// started a server, so nothing of it is listening on any port.
    Idle,
    /// A serving instance that was then stopped through EC2.
    Stopped,
    /// A serving instance that was then terminated through EC2.
    Terminated,
    /// An id the account holds no instance for at all.
    Absent,
}

/// One row: a load balancer, the target and thresholds its health check is configured with,
/// and the backend its one registered instance names.
#[derive(Debug, Clone, Copy)]
struct Case {
    /// Also the load balancer's name.
    name: &'static str,
    /// The `HealthCheck$Target` the load balancer checks with.
    target: &'static str,
    healthy_threshold: i32,
    unhealthy_threshold: i32,
    backend: Backend,
    /// The health the row's instance ends up reporting.
    expected: Reported,
}

/// What a row's checks decide — named rather than written out, so the expectation is the
/// service's own constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reported {
    /// The checks were answered: in service, with no cause to report.
    InService,
    /// The checks were not answered, as the instance's own doing.
    FailedChecks,
    /// They were not answered because the instance is stopped.
    Stopped,
    /// … or terminated.
    Terminated,
}

impl Case {
    /// The threshold this row's verdict is reached on: the healthy one for a backend that
    /// answers its checks, the unhealthy one for a backend that answers none.
    fn deciding_threshold(self) -> u32 {
        let configured = if self.expected == Reported::InService {
            self.healthy_threshold
        } else {
            self.unhealthy_threshold
        };
        u32::try_from(configured).expect("a configured threshold")
    }

    /// The triple this row's instance ends up reporting.
    fn expected(self) -> Triple {
        match self.expected {
            Reported::InService => triple(HEALTH_IN_SERVICE),
            Reported::FailedChecks => triple(HEALTH_FAILED_HEALTH_CHECKS),
            Reported::Stopped => out_of_service(DESCRIPTION_INSTANCE_STOPPED),
            Reported::Terminated => out_of_service(DESCRIPTION_INSTANCE_TERMINATED),
        }
    }
}

/// The rows: every protocol a target may name, a port and a ping path the backend serves and
/// ones it does not, every backend state a check can find, and thresholds at both modelled
/// bounds and between them.
fn health_cases() -> Vec<Case> {
    vec![
        Case {
            name: "an-http-target-the-backend-answers",
            target: "HTTP:80/ping",
            healthy_threshold: 2,
            unhealthy_threshold: 2,
            backend: Backend::Serving {
                port: 80,
                paths: &["/ping"],
            },
            expected: Reported::InService,
        },
        Case {
            // `HealthyThreshold` at the maximum the member is modelled with: the run of
            // answered checks the verdict waits for is the caller's configuration.
            name: "an-http-target-answered-on-another-port",
            target: "HTTP:8080/health",
            healthy_threshold: 10,
            unhealthy_threshold: 2,
            backend: Backend::Serving {
                port: 8080,
                paths: &["/health"],
            },
            expected: Reported::InService,
        },
        Case {
            name: "an-https-target-the-backend-answers",
            target: "HTTPS:8443/ok",
            healthy_threshold: 2,
            unhealthy_threshold: 2,
            backend: Backend::Serving {
                port: 8443,
                paths: &["/ok"],
            },
            expected: Reported::InService,
        },
        Case {
            // "A health check simply attempts to open a TCP connection to the instance on
            // the specified port": the backend holds no document at all, and the connection
            // is the whole of the check.
            name: "a-tcp-target-on-a-port-the-backend-listens-on",
            target: "TCP:8080",
            healthy_threshold: 2,
            unhealthy_threshold: 2,
            backend: Backend::Serving {
                port: 8080,
                paths: &[],
            },
            expected: Reported::InService,
        },
        Case {
            name: "an-ssl-target-on-a-port-nothing-listens-on",
            target: "SSL:7001",
            healthy_threshold: 2,
            unhealthy_threshold: 2,
            backend: Backend::Serving {
                port: 8080,
                paths: &[],
            },
            expected: Reported::FailedChecks,
        },
        Case {
            // The server answers, with its own not-found status for a path its document root
            // holds nothing at: "any answer other than \"200 OK\" … is considered unhealthy".
            name: "an-http-target-whose-ping-path-is-not-held",
            target: "HTTP:8080/missing",
            healthy_threshold: 2,
            unhealthy_threshold: 3,
            backend: Backend::Serving {
                port: 8080,
                paths: &["/health"],
            },
            expected: Reported::FailedChecks,
        },
        Case {
            name: "an-http-target-on-a-port-the-backend-does-not-serve",
            target: "HTTP:9090/ping",
            healthy_threshold: 2,
            unhealthy_threshold: 2,
            backend: Backend::Serving {
                port: 8080,
                paths: &["/ping"],
            },
            expected: Reported::FailedChecks,
        },
        Case {
            // A bare instance: it is running, and nothing on it answers the check.
            name: "a-backend-running-no-server-at-all",
            target: "HTTP:80/index.html",
            healthy_threshold: 2,
            unhealthy_threshold: 2,
            backend: Backend::Idle,
            expected: Reported::FailedChecks,
        },
        Case {
            name: "a-backend-that-was-stopped",
            target: "HTTP:80/ping",
            healthy_threshold: 2,
            unhealthy_threshold: 2,
            backend: Backend::Stopped,
            expected: Reported::Stopped,
        },
        Case {
            name: "a-backend-that-was-terminated",
            target: "HTTP:80/ping",
            healthy_threshold: 10,
            unhealthy_threshold: 3,
            backend: Backend::Terminated,
            expected: Reported::Terminated,
        },
        Case {
            // `UnhealthyThreshold` at the maximum the member is modelled with.
            name: "an-id-the-account-holds-no-instance-for",
            target: "HTTP:80/ping",
            healthy_threshold: 2,
            unhealthy_threshold: 10,
            backend: Backend::Absent,
            expected: Reported::FailedChecks,
        },
    ]
}

/// The backend of `case`, launched through EC2's own API and moved to the state the row
/// names: its instance id.
async fn seed_backend(router: &MultiServiceRouter, case: &Case) -> String {
    let user_data = match case.backend {
        // An id of the right form that EC2 holds no instance for: nothing was launched for
        // this row.
        Backend::Absent => return "i-00000000000000bad".to_string(),
        Backend::Serving { port, paths } => Some(serving_user_data(port, paths)),
        // Stopped and terminated backends DID serve while they were up, so what the row
        // asserts is the condition the instance's own state puts it in rather than a server
        // that was never started.
        Backend::Stopped | Backend::Terminated => Some(serving_user_data(80, &["/ping"])),
        Backend::Idle => None,
    };
    let launched = launch_backend(router, REGION, None, case.name, user_data.as_deref()).await;
    settle_ec2_transition().await;
    match case.backend {
        Backend::Stopped => move_instance(router, case.name, "StopInstances", &launched).await,
        Backend::Terminated => {
            move_instance(router, case.name, "TerminateInstances", &launched).await
        }
        _ => {}
    }
    launched
}

/// Create one load balancer named `name` with an HTTP:80 listener.
async fn create_load_balancer(router: &MultiServiceRouter, name: &str) {
    let created = call(
        router,
        REGION,
        "CreateLoadBalancer",
        &[
            param("LoadBalancerName", name),
            param("Listeners.member.1.Protocol", "HTTP"),
            param("Listeners.member.1.LoadBalancerPort", "80"),
            param("Listeners.member.1.InstancePort", "80"),
            param("AvailabilityZones.member.1", &format!("{REGION}a")),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: CreateLoadBalancer must answer 200, got {}",
        created.body
    );
}

/// Configure the health check of `case`'s load balancer: the row's own target and thresholds.
async fn configure_health_check(router: &MultiServiceRouter, case: &Case) {
    let configured = call(
        router,
        REGION,
        "ConfigureHealthCheck",
        &[
            param("LoadBalancerName", case.name),
            param("HealthCheck.Target", case.target),
            param("HealthCheck.Interval", "5"),
            param("HealthCheck.Timeout", "2"),
            param(
                "HealthCheck.UnhealthyThreshold",
                &case.unhealthy_threshold.to_string(),
            ),
            param(
                "HealthCheck.HealthyThreshold",
                &case.healthy_threshold.to_string(),
            ),
        ],
    )
    .await;
    assert_eq!(
        configured.status, 200,
        "case {:?}: ConfigureHealthCheck must answer 200, got {}",
        case.name, configured.body
    );
}

/// Register `instance_id` with `name`'s load balancer.
async fn register(router: &MultiServiceRouter, name: &str, instance_id: &str) {
    let registered = call(
        router,
        REGION,
        "RegisterInstancesWithLoadBalancer",
        &[
            param("LoadBalancerName", name),
            param("Instances.member.1.InstanceId", instance_id),
        ],
    )
    .await;
    assert_eq!(
        registered.status, 200,
        "case {name:?}: RegisterInstancesWithLoadBalancer must answer 200, got {}",
        registered.body
    );
}

/// The triple `name`'s load balancer reports for `instance_id` now.
async fn health(router: &MultiServiceRouter, name: &str, instance_id: &str) -> Triple {
    let answer = call(
        router,
        REGION,
        "DescribeInstanceHealth",
        &[
            param("LoadBalancerName", name),
            param("Instances.member.1.InstanceId", instance_id),
        ],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {name:?}: DescribeInstanceHealth must answer 200, got {}",
        answer.body
    );
    let doc = parse(&answer.body);
    let reported = members(answer.result(&doc), "InstanceStates");
    assert_eq!(
        reported.len(),
        1,
        "case {name:?}: one InstanceState per described instance, got {}",
        answer.body
    );
    let member = reported[0];
    assert_eq!(
        kid_text(member, "InstanceId"),
        Some(instance_id.to_string()),
        "case {name:?}: the member must be about the instance the request named, got {}",
        answer.body
    );
    (
        kid_text(member, "State"),
        kid_text(member, "ReasonCode"),
        kid_text(member, "Description"),
    )
}

/// The reported state of a registered instance is the outcome of the check its load
/// balancer's own target describes, decided by that load balancer's own thresholds.
#[tokio::test]
async fn instance_health_is_the_outcome_of_the_configured_check_of_the_backend() {
    let (router, engine) = emulator_with_ec2();

    for case in health_cases() {
        let instance_id = seed_backend(&router, &case).await;

        create_load_balancer(&router, case.name).await;
        configure_health_check(&router, &case).await;
        register(&router, case.name, &instance_id).await;

        // No check has fallen due yet, whatever the backend is doing: the registration's
        // own checks are still running.
        place_registration(&engine, REGION, case.name, &instance_id, 0);
        let fresh = health(&router, case.name, &instance_id).await;
        assert_eq!(
            fresh,
            triple(HEALTH_REGISTRATION_IN_PROGRESS),
            "case {:?}: right after registration, got {fresh:?}",
            case.name
        );

        // One check short of the threshold that decides this row: no verdict yet.
        let deciding = case.deciding_threshold();
        place_registration(
            &engine,
            REGION,
            case.name,
            &instance_id,
            deciding.saturating_sub(1),
        );
        let short = health(&router, case.name, &instance_id).await;
        assert_eq!(
            short,
            triple(HEALTH_REGISTRATION_IN_PROGRESS),
            "case {:?}: after {} of {deciding} deciding checks, got {short:?}",
            case.name,
            deciding.saturating_sub(1)
        );

        // The check that reaches the threshold: the verdict follows what the checks
        // observed about the backend.
        place_registration(&engine, REGION, case.name, &instance_id, deciding);
        let settled = health(&router, case.name, &instance_id).await;
        assert_eq!(
            settled,
            case.expected(),
            "case {:?}: after {deciding} checks of {:?} with target {:?}, got {settled:?}",
            case.name,
            case.backend,
            case.target
        );

        // The verdict is read, not re-decided: polling again answers the same thing, and
        // the registered set the answer was read from is untouched.
        let polled = health(&router, case.name, &instance_id).await;
        assert_eq!(
            polled, settled,
            "case {:?}: a second poll must answer what the first one did",
            case.name
        );
        let described = call(
            &router,
            REGION,
            "DescribeLoadBalancers",
            &[param("LoadBalancerNames.member.1", case.name)],
        )
        .await;
        let doc = parse(&described.body);
        let description = members(described.result(&doc), "LoadBalancerDescriptions")[0];
        assert_eq!(
            members(description, "Instances")
                .into_iter()
                .filter_map(|member| kid_text(member, "InstanceId"))
                .collect::<Vec<String>>(),
            vec![instance_id.clone()],
            "case {:?}: polling health must not change the registered set, got {}",
            case.name,
            described.body
        );
    }
}

/// The verdict FOLLOWS the backend: an instance in service whose backend stops keeps the
/// verdict its answered checks earned until `UnhealthyThreshold` consecutive checks have
/// failed, and then reports the condition they ran into.
#[tokio::test]
async fn a_verdict_moves_only_once_the_threshold_of_checks_disagrees_with_it() {
    let (router, engine) = emulator_with_ec2();
    let name = "backend-stops-serving";
    let case = Case {
        name,
        target: "HTTP:80/ping",
        healthy_threshold: 2,
        unhealthy_threshold: 3,
        backend: Backend::Serving {
            port: 80,
            paths: &["/ping"],
        },
        expected: Reported::InService,
    };

    let instance_id = seed_backend(&router, &case).await;
    create_load_balancer(&router, name).await;
    configure_health_check(&router, &case).await;
    register(&router, name, &instance_id).await;

    // How many checks this registration has owed in all: each step below places it one
    // further along, so exactly one more check falls due for the read that follows.
    let mut checks = u32::try_from(case.healthy_threshold).expect("a threshold");

    // The answered checks put it in service.
    place_registration(&engine, REGION, name, &instance_id, checks);
    let in_service = health(&router, name, &instance_id).await;
    assert_eq!(
        in_service,
        triple(HEALTH_IN_SERVICE),
        "a backend that answers the configured check must reach InService, got \
         {in_service:?}"
    );

    // The backend stops. Each check from now on fails, but the verdict holds until the
    // configured run of them has.
    move_instance(&router, name, "StopInstances", &instance_id).await;
    for failed in 1..case.unhealthy_threshold {
        checks = checks.saturating_add(1);
        place_registration(&engine, REGION, name, &instance_id, checks);
        let holding = health(&router, name, &instance_id).await;
        assert_eq!(
            holding,
            triple(HEALTH_IN_SERVICE),
            "after {failed} of {} failing checks the verdict must hold, got {holding:?}",
            case.unhealthy_threshold
        );
    }
    checks = checks.saturating_add(1);
    place_registration(&engine, REGION, name, &instance_id, checks);
    let moved = health(&router, name, &instance_id).await;
    assert_eq!(
        moved,
        out_of_service(DESCRIPTION_INSTANCE_STOPPED),
        "after {} failing checks the verdict must follow them, got {moved:?}",
        case.unhealthy_threshold
    );

    // And back: the stopped backend is started again, its guest runs its own program again,
    // and the healthy threshold's worth of answered checks returns it to service.
    move_instance(&router, name, "StartInstances", &instance_id).await;
    checks = checks
        .saturating_add(u32::try_from(case.healthy_threshold).expect("a threshold"));
    place_registration(&engine, REGION, name, &instance_id, checks);
    let recovered = health(&router, name, &instance_id).await;
    assert_eq!(
        recovered,
        triple(HEALTH_IN_SERVICE),
        "a backend that answers again must return to InService, got {recovered:?}"
    );
}
