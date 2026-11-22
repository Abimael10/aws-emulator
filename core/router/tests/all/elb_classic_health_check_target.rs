// SPDX-License-Identifier: CC0-1.0

//! What a CLASSIC Load Balancer reports for a registered instance, through the WHOLE mounted
//! service set — the one `core/router/src/lib.rs` assembles for the binary — over Amazon EC2's
//! own wire and Elastic Load Balancing's own.
//!
//! RULE: the state a load balancer reports for a registered instance is the outcome of the
//! check its own `HealthCheck$Target` describes against that instance. For a `TCP` or `SSL`
//! target "a health check simply attempts to open a TCP connection to the instance on the
//! specified port", and for an `HTTP`/`HTTPS` target "a HTTP GET request is issued to the
//! instance on the given port and path. Any answer other than \"200 OK\" within the timeout
//! period is considered unhealthy"
//! (`smithy_query(query="doc",shape="HealthCheck",member="Target")`). So a check is answered
//! only where something behind the registration is LISTENING on that port and answers what the
//! target asks for there; once `UnhealthyThreshold` consecutive checks are not answered the
//! member reads `State=OutOfService`, `ReasonCode=Instance` and the description
//! `InstanceState$Description` enumerates for exactly that, and once `HealthyThreshold`
//! consecutive ones are, it reads `InService` with `ReasonCode` and `Description` of `N/A`.
//!
//! What can answer such a check is the server the instance itself runs, which is Amazon EC2's
//! to hold: an instance running nothing answers nothing, whatever state EC2 reports it in. That
//! is why this suite spans the two services — the backend is launched through EC2's own
//! `RunInstances`, the server inside it is started through EC2's own guest seam, and the
//! verdict is read through Elastic Load Balancing's own `DescribeInstanceHealth`.
//!
//! The rows differ along what the rule generalises over: the target's protocol (`HTTP`, `TCP`
//! and `HTTPS`), the port it names (the one the backend serves and one nothing listens on), its
//! ping path (one the server holds a document at and one it does not) and what the instance is
//! running (a server, and nothing at all).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use service_com_amazonaws_ec2_amazonec2::state as ec2;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{elements, form_encode, members_of};

/// The Region every request below is signed for, and the Availability Zone every load
/// balancer is enabled for.
const REGION: &str = "us-east-1";
const ZONE: &str = "us-east-1a";

/// The API versions an ec2Query and an awsQuery request carries.
const EC2_API_VERSION: &str = "2016-11-15";
const CLASSIC_ELB_VERSION: &str = "2012-06-01";

/// The AMI every launch here names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";

/// The port the serving backend listens on, the directory its server serves and the path it
/// holds a document at — what a check of it can be answered on.
const SERVED_PORT: i32 = 8080;
const SERVED_ROOT: &str = "/srv/site";
const SERVED_PATH: &str = "/healthy";

/// The thresholds every health check here is configured with: the fewest consecutive checks a
/// verdict can take either way (`HealthyThreshold`/`UnhealthyThreshold` are modelled
/// `@range(min: 2, max: 10)`), because what this suite asserts is WHICH verdict the checks
/// reach, not how long Elastic Load Balancing takes to reach one.
const THRESHOLD: &str = "2";

/// How long a row waits for the verdict its checks owe, and how often it looks. Generous,
/// because the deadline is only here so a failure is a failure rather than a hang: every wait
/// below looks for its CONDITION, so machine load decides when a row finishes and never
/// whether it passes.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// The health triple of one `InstanceState` member: `(State, ReasonCode, Description)`.
type Triple = (String, String, String);

/// The triple a registration the load balancer routes traffic to reports: `InService`, with
/// the value `InstanceState$ReasonCode` and `$Description` enumerate for an instance there is
/// no cause to report.
fn in_service() -> Triple {
    (
        "InService".to_string(),
        "N/A".to_string(),
        "N/A".to_string(),
    )
}

/// The triple a registration whose checks failed reports: `OutOfService` for the instance's own
/// doing, with the description `InstanceState$Description` enumerates for exactly that.
fn failed_checks() -> Triple {
    (
        "OutOfService".to_string(),
        "Instance".to_string(),
        "Instance has failed at least the UnhealthyThreshold number of health checks \
         consecutively."
            .to_string(),
    )
}

/// The triple a registration whose own checks have not reached a verdict yet reports — out of
/// service for Elastic Load Balancing's own reason, which is neither verdict.
fn still_coming_up() -> Triple {
    (
        "OutOfService".to_string(),
        "ELB".to_string(),
        "Instance registration is still in progress.".to_string(),
    )
}

/// The whole emulator over one engine, exactly as the binary assembles it — answered beside
/// that engine, which is the handle a server is started in an instance's guest through.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    (router, engine)
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses, and answer its
/// status and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One ec2Query action that must succeed.
async fn ec2_call(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Classic Elastic Load Balancing action that must succeed.
async fn elb(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancing",
        form(action, CLASSIC_ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Elastic Load Balancing {action} must succeed, got {status} with body \
         {body}"
    );
    body
}

/// Launch one instance through EC2's own API and answer the id EC2 minted for it.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let body = ec2_call(
        router,
        case,
        "RunInstances",
        &[
            ("ImageId", IMAGE_ID),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("Placement.AvailabilityZone", ZONE),
        ],
    )
    .await;
    elements(&body, "instanceId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// Start a web server on [`SERVED_PORT`] inside the guest of `instance_id`, through Amazon
/// EC2's own seam for running a program in a running guest, and wait until it is running.
///
/// Nothing of an instance answers a check before something starts a server on it, so this is
/// what a client does with a machine it has launched. The instance's own boot is owed to the
/// clock, so this waits for the CONDITION — a guest that performed the program — rather than
/// for a span, looking through EC2's own `DescribeInstances` so the launch the clock owes is
/// materialised exactly as it is for any other caller.
async fn start_server_in_guest(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    instance_id: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        ec2_call(
            router,
            case,
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        )
        .await;
        let started = {
            let state = ec2::service_state(&**engine).expect("the EC2 store of this emulator");
            let mut store = state.lock();
            let ran = store
                .run_in_instance_guest(
                    instance_id,
                    &[
                        format!("mkdir -p {SERVED_ROOT}"),
                        format!("echo ok > {SERVED_ROOT}{SERVED_PATH}"),
                        format!(
                            "python3 -m http.server {SERVED_PORT} --directory {SERVED_ROOT}"
                        ),
                    ],
                )
                .is_some_and(|run| run.status == 0);
            ran && store
                .instance_guest_server_ports(instance_id)
                .contains(&SERVED_PORT)
        };
        if started {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the guest of {instance_id} to be listening on \
             {SERVED_PORT}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Create one Classic Load Balancer checking `target`, and register `instance_id` with it.
async fn load_balancer(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    target: &str,
    instance_id: &str,
) {
    elb(
        router,
        case,
        "CreateLoadBalancer",
        &[
            ("LoadBalancerName", name),
            ("AvailabilityZones.member.1", ZONE),
            ("Listeners.member.1.Protocol", "HTTP"),
            ("Listeners.member.1.LoadBalancerPort", "80"),
            ("Listeners.member.1.InstanceProtocol", "HTTP"),
            (
                "Listeners.member.1.InstancePort",
                &SERVED_PORT.to_string(),
            ),
        ],
    )
    .await;
    elb(
        router,
        case,
        "ConfigureHealthCheck",
        &[
            ("LoadBalancerName", name),
            ("HealthCheck.Target", target),
            ("HealthCheck.Interval", "5"),
            ("HealthCheck.Timeout", "2"),
            ("HealthCheck.HealthyThreshold", THRESHOLD),
            ("HealthCheck.UnhealthyThreshold", THRESHOLD),
        ],
    )
    .await;
    elb(
        router,
        case,
        "RegisterInstancesWithLoadBalancer",
        &[
            ("LoadBalancerName", name),
            ("Instances.member.1.InstanceId", instance_id),
        ],
    )
    .await;
}

/// The triple the load balancer `name` reports for `instance_id` now, read through Elastic
/// Load Balancing's own `DescribeInstanceHealth`.
async fn reported_health(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) -> Triple {
    let body = elb(
        router,
        case,
        "DescribeInstanceHealth",
        &[
            ("LoadBalancerName", name),
            ("Instances.member.1.InstanceId", instance_id),
        ],
    )
    .await;
    let members = members_of(&body);
    assert_eq!(
        members.len(),
        1,
        "case {case:?}: one InstanceState per described instance, got {body}"
    );
    let member = members[0];
    assert_eq!(
        elements(member, "InstanceId"),
        vec![instance_id.to_string()],
        "case {case:?}: the member must be about the instance the request named, got {body}"
    );
    let read = |tag: &str| {
        elements(member, tag)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: no {tag} in {body}"))
    };
    (read("State"), read("ReasonCode"), read("Description"))
}

/// Poll `name`'s health of `instance_id` until its checks have reached a VERDICT, and answer
/// the triple it then reports.
///
/// A verdict is a threshold of checks away and each check is owed to the clock, so this waits
/// for the condition — a member that no longer reports the registration as still coming up —
/// rather than for a span.
async fn settled_health(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) -> Triple {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = reported_health(router, case, name, instance_id).await;
        if reported != still_coming_up() {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the checks of {instance_id} against {name} to reach a \
             verdict, it still reports {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One row: the load balancer, the target it checks with, whether its registered instance runs
/// a server at all, and the triple the checks must reach.
struct Row {
    case: &'static str,
    load_balancer: &'static str,
    /// The `HealthCheck$Target` the load balancer is configured with.
    target: &'static str,
    /// Whether the registered instance is the one running a server.
    serving: bool,
}

/// RULE: the state a load balancer reports for a registered instance is the outcome of the
/// check its own target describes against what that instance is running.
#[tokio::test]
async fn instance_health_is_the_outcome_of_the_configured_check_of_the_instance() {
    let rows: Vec<(Row, Triple)> = vec![
        (
            Row {
                case: "an HTTP target the instance's own server answers",
                load_balancer: "clbhc-answered",
                target: "HTTP:8080/healthy",
                serving: true,
            },
            in_service(),
        ),
        (
            Row {
                // "A health check simply attempts to open a TCP connection to the instance on
                // the specified port": the connection is the whole of the check.
                case: "a TCP target on the port the instance listens on",
                load_balancer: "clbhc-connected",
                target: "TCP:8080",
                serving: true,
            },
            in_service(),
        ),
        (
            Row {
                case: "an HTTPS target on a port nothing of the instance listens on",
                load_balancer: "clbhc-wrong-port",
                target: "HTTPS:8443/healthy",
                serving: true,
            },
            failed_checks(),
        ),
        (
            Row {
                // The server answers, with its own not-found status for a path it holds no
                // document at: "any answer other than \"200 OK\" … is considered unhealthy".
                case: "an HTTP target whose ping path the server holds nothing at",
                load_balancer: "clbhc-wrong-path",
                target: "HTTP:8080/not-served",
                serving: true,
            },
            failed_checks(),
        ),
        (
            Row {
                case: "an instance running no server at all",
                load_balancer: "clbhc-no-server",
                target: "HTTP:8080/healthy",
                serving: false,
            },
            failed_checks(),
        ),
    ];

    let (router, engine) = emulator();
    // Two backends the rows share: one running a server on the port it serves, and one
    // running nothing. Both are the account's own Amazon EC2 instances, launched through
    // that service's own API.
    let serving = launch(&router, "the serving backend").await;
    let idle = launch(&router, "the backend running nothing").await;
    start_server_in_guest(&router, &engine, "the serving backend", &serving).await;

    // Every row's load balancer is configured and its registration made FIRST, each row
    // asserting that no check of its own has fallen due yet, so the checks the clock owes
    // them all run over the same span rather than one row's after another's.
    for (row, _) in &rows {
        let instance_id = if row.serving { &serving } else { &idle };
        load_balancer(
            &router,
            row.case,
            row.load_balancer,
            row.target,
            instance_id,
        )
        .await;

        // No check has fallen due yet, whatever the instance is running: the registration's
        // own checks are still in progress.
        let fresh = reported_health(&router, row.case, row.load_balancer, instance_id).await;
        assert_eq!(
            fresh,
            still_coming_up(),
            "case {:?}: right after registration, got {fresh:?}",
            row.case
        );
    }

    for (row, expected) in &rows {
        let instance_id = if row.serving { &serving } else { &idle };
        let settled = settled_health(&router, row.case, row.load_balancer, instance_id).await;
        assert_eq!(
            &settled, expected,
            "case {:?}: the checks of {:?} must reach {expected:?}, got {settled:?}",
            row.case, row.target
        );

        // The verdict is READ, not re-decided: polling again answers the same thing.
        let polled = reported_health(&router, row.case, row.load_balancer, instance_id).await;
        assert_eq!(
            polled, settled,
            "case {:?}: a second poll must answer what the first one did",
            row.case
        );
    }
}
