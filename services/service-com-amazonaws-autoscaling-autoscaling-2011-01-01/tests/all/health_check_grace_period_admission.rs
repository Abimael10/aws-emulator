// SPDX-License-Identifier: CC0-1.0

//! The admission check `SetInstanceHealth` makes against the group's health check grace
//! period.
//!
//! RULE: `HealthCheckGracePeriod` is the wait a group gives an instance it has brought into
//! service before a failed health check takes that instance out of service, and "if the Auto
//! Scaling group of the specified instance has a `HealthCheckGracePeriod` specified for the
//! group, by default, this call respects the grace period. Set this to `False`, to have the
//! call not respect the grace period associated with the group"
//! (`smithy_query(query="doc",shape="SetInstanceHealthQuery",member="ShouldRespectGracePeriod")`).
//! So a call that RESPECTS the period and would take an instance younger than it out of
//! service is REFUSED — HTTP 400 `ValidationError`, carrying the sentence that states the
//! instance's age and the period the group is configured with — and writes nothing: the
//! member keeps the status it held and no replacement is started. Every other call is
//! admitted and records the status it states: one that does not respect the period, one
//! against an instance already past it, one against a group whose period is zero or
//! unconfigured, and one stating a status that leaves the instance IN service, which the
//! period has nothing to hold off.
//!
//! The table spans what the rule generalises over: how long the configured period is (none
//! at all, zero, seconds, minutes), whether the request respects it (by saying so, by saying
//! nothing, and by declining), and which status is recorded. The boundary — a member on each
//! side of one configured period — is its own test, which looks AGAIN until the member is
//! past the period rather than timing anything. That a refused call starts no replacement is
//! asserted where the replacement itself is the subject,
//! `all/scale_out_launches_ec2_instances/instance_health.rs`.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, emulator, p, query, seed_subnets, tag_value, xml_unescape};

/// The launch template every group of this suite launches from: one the harness puts in the
/// account's own Amazon EC2 store.
const TEMPLATE: &str = "lt-1";

/// The two `HealthStatus` values `SetInstanceHealth` documents, and the `LifecycleState` of a
/// member that is in service — written out here rather than taken from the crate, so no
/// assertion takes the implementation's own word for them.
const HEALTHY: &str = "Healthy";
const UNHEALTHY: &str = "Unhealthy";
const IN_SERVICE: &str = "InService";

/// The error code this service refuses a request it cannot satisfy with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The group process that acts on a failed health check — it terminates the condemned member
/// and launches its replacement — spelled as `SuspendProcesses` names it. The table below
/// suspends it, so a status a row records stands while the row reads it back; the test that
/// is about the replacement does NOT suspend it.
const PROCESS_REPLACE_UNHEALTHY: &str = "ReplaceUnhealthy";

/// How long a case waits for a condition the clock owes before calling it a failure. A
/// BACKSTOP, not a wait: every loop below looks again until its condition holds.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The fixed text around the two spans the refusal states.
const AGE_PREFIX: &str = "The age of the instance (";

/// The whole sentence a respected grace period refuses a call with, for an instance the
/// service measured as `age_seconds` old in a group configured with `grace_period`.
///
/// Written out here rather than taken from the service's own builder: a test that re-used the
/// implementation's literal would assert nothing.
fn grace_period_message(age_seconds: u64, grace_period: i32) -> String {
    format!(
        "{AGE_PREFIX}{age_seconds} seconds) is less than the configured grace period of \
         {grace_period} seconds"
    )
}

/// The age one refusal states, read back out of its sentence: the value is the service's own
/// measurement of this run, so a case weighs it against the period rather than against a
/// number written out here.
fn stated_age(case: &str, message: &str, grace_period: i32) -> u64 {
    let digits: String = message
        .strip_prefix(AGE_PREFIX)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the refusal must state the instance's age, got {message:?}")
        })
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let age: u64 = digits.parse().unwrap_or_else(|_| {
        panic!("case {case:?}: the age the refusal states must be a number, got {message:?}")
    });
    assert_eq!(
        message,
        grace_period_message(age, grace_period),
        "case {case:?}: expected the refusal {:?}, got {message:?}",
        grace_period_message(age, grace_period)
    );
    age
}

/// Create one group of one instance, carrying `grace_period` as its `HealthCheckGracePeriod`
/// when the case states one, and answer the id of the member it holds once that member is in
/// service.
async fn group_of_one(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    grace_period: Option<i32>,
) -> String {
    let subnets = seed_subnets(engine, 1);
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "1"),
        p("VPCZoneIdentifier", subnets[0].id.clone()),
    ];
    if let Some(grace_period) = grace_period {
        params.push(p("HealthCheckGracePeriod", grace_period.to_string()));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating {name} expected HTTP 200, got {status}: {body}"
    );
    let deadline = Instant::now() + DEADLINE;
    loop {
        let members = reported_members(router, case, name).await;
        if let [(instance_id, lifecycle_state, _)] = members.as_slice() {
            if lifecycle_state == IN_SERVICE {
                return instance_id.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {name} must settle on one member in service, got {members:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Suspend the process that acts on a failed health check for one group.
async fn suspend_replace_unhealthy(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = query(
        router,
        "SuspendProcesses",
        &[
            ("AutoScalingGroupName", name),
            ("ScalingProcesses.member.1", PROCESS_REPLACE_UNHEALTHY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: suspending {PROCESS_REPLACE_UNHEALTHY} for {name} expected HTTP 200, \
         got {status}: {body}"
    );
}

/// The members one group reports, as `(id, lifecycle state, health status)`.
async fn reported_members(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<(String, String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {name} back expected HTTP 200, got {status}: {body}"
    );
    tag_value(&body, "Instances")
        .split("</member>")
        .filter(|member| member.contains("<InstanceId"))
        .map(|member| {
            (
                tag_value(member, "InstanceId"),
                tag_value(member, "LifecycleState"),
                tag_value(member, "HealthStatus"),
            )
        })
        .collect()
}

/// What one group reports for ONE of its members.
async fn reported_member(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) -> (String, String) {
    let members = reported_members(router, case, name).await;
    members
        .iter()
        .find(|(id, _, _)| id == instance_id)
        .map(|(_, lifecycle_state, health_status)| {
            (lifecycle_state.clone(), health_status.clone())
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: {name} must still hold {instance_id}, got {members:?}")
        })
}

/// Record `health_status` for `instance_id`, stating `ShouldRespectGracePeriod` only when the
/// case names it, and answer the status and body of the call.
async fn set_instance_health(
    router: &MultiServiceRouter,
    instance_id: &str,
    health_status: &str,
    respect: Option<bool>,
) -> (u16, String) {
    let mut params = vec![
        p("InstanceId", instance_id),
        p("HealthStatus", health_status),
    ];
    if let Some(respect) = respect {
        params.push(p("ShouldRespectGracePeriod", respect.to_string()));
    }
    query(router, "SetInstanceHealth", &borrowed(&params)).await
}

/// One row of the admission table.
struct Row {
    case: &'static str,
    /// The `HealthCheckGracePeriod` the group is created with, or none at all.
    grace_period: Option<i32>,
    /// What the request says about respecting that period, or nothing at all.
    respect: Option<bool>,
    /// The status the request records.
    health_status: &'static str,
    /// Whether the period is what stops the call.
    refused: bool,
}

/// A respected grace period refuses a call that would take a member younger than it out of
/// service, and admits every other call.
#[tokio::test]
async fn a_respected_grace_period_admits_or_refuses_the_recorded_status() {
    let rows = [
        Row {
            case: "minutes of grace, respected by saying nothing",
            grace_period: Some(600),
            respect: None,
            health_status: UNHEALTHY,
            refused: true,
        },
        Row {
            case: "minutes of grace, respected outright",
            grace_period: Some(600),
            respect: Some(true),
            health_status: UNHEALTHY,
            refused: true,
        },
        Row {
            case: "a minute of grace, respected outright",
            grace_period: Some(60),
            respect: Some(true),
            health_status: UNHEALTHY,
            refused: true,
        },
        Row {
            case: "minutes of grace the call declines to respect",
            grace_period: Some(600),
            respect: Some(false),
            health_status: UNHEALTHY,
            refused: false,
        },
        Row {
            case: "a period of no seconds, respected outright",
            grace_period: Some(0),
            respect: Some(true),
            health_status: UNHEALTHY,
            refused: false,
        },
        Row {
            case: "no period configured at all, respected by saying nothing",
            grace_period: None,
            respect: None,
            health_status: UNHEALTHY,
            refused: false,
        },
        Row {
            case: "minutes of grace, and a status that leaves the member in service",
            grace_period: Some(600),
            respect: Some(true),
            health_status: HEALTHY,
            refused: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's.
        let (router, engine) = emulator();
        let name = format!("asg-grace-admission-{index}");
        let instance_id = group_of_one(&router, &engine, row.case, &name, row.grace_period).await;
        // The admitted rows record a status that stands: what the group's check then DOES
        // with it is the subject of the third test, so the process that acts on it is
        // suspended here.
        suspend_replace_unhealthy(&router, row.case, &name).await;

        let (status, body) =
            set_instance_health(&router, &instance_id, row.health_status, row.respect).await;

        if row.refused {
            let grace_period = row
                .grace_period
                .expect("a refused row states the period it was refused under");
            assert_eq!(
                status, 400,
                "case {:?}: expected HTTP 400, got {status}: {body}",
                row.case
            );
            let code = tag_value(&body, "Code");
            assert_eq!(
                code, VALIDATION_ERROR,
                "case {:?}: expected the {VALIDATION_ERROR} code, got {code:?}: {body}",
                row.case
            );
            let message = xml_unescape(&tag_value(&body, "Message"));
            let age = stated_age(row.case, &message, grace_period);
            assert!(
                age < u64::try_from(grace_period).expect("a period that fits in a u64"),
                "case {:?}: the age the refusal states must be inside the period it names, \
                 got {age} against {grace_period}",
                row.case
            );
            // Nothing was written: the member keeps the status it came into service with.
            let (lifecycle_state, health_status) =
                reported_member(&router, row.case, &name, &instance_id).await;
            assert_eq!(
                (lifecycle_state.as_str(), health_status.as_str()),
                (IN_SERVICE, HEALTHY),
                "case {:?}: a refused call writes nothing, got {lifecycle_state:?} and \
                 {health_status:?}",
                row.case
            );
        } else {
            assert_eq!(
                status, 200,
                "case {:?}: expected HTTP 200, got {status}: {body}",
                row.case
            );
            let (lifecycle_state, health_status) =
                reported_member(&router, row.case, &name, &instance_id).await;
            assert_eq!(
                (lifecycle_state.as_str(), health_status.as_str()),
                (IN_SERVICE, row.health_status),
                "case {:?}: an admitted call records the status it states, got \
                 {lifecycle_state:?} and {health_status:?}",
                row.case
            );
        }
    }
}

/// A member PAST the period the call respects is admitted under that same configured period:
/// the refusal is about the member's age, not about the group having a period at all.
#[tokio::test]
async fn a_member_past_the_respected_period_is_admitted_under_it() {
    // Short enough that a member outlives it inside one test, and long enough that the first
    // call is made well inside it.
    let grace_period = 3;
    let case = "a member that outlives the period the call respects";

    let (router, engine) = emulator();
    let name = "asg-grace-boundary";
    let instance_id = group_of_one(&router, &engine, case, name, Some(grace_period)).await;
    suspend_replace_unhealthy(&router, case, name).await;

    // Looked at AGAIN until the member is past the period: every refusal on the way states an
    // age inside the period, and the call is admitted once the member is outside it.
    let deadline = Instant::now() + DEADLINE;
    let mut refusals = 0_usize;
    loop {
        let (status, body) = set_instance_health(&router, &instance_id, UNHEALTHY, Some(true)).await;
        if status == 200 {
            break;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: a call inside the period is refused with HTTP 400, got {status}: \
             {body}"
        );
        let message = xml_unescape(&tag_value(&body, "Message"));
        let age = stated_age(case, &message, grace_period);
        assert!(
            age < u64::try_from(grace_period).expect("a period that fits in a u64"),
            "case {case:?}: the age the refusal states must be inside the period it names, \
             got {age} against {grace_period}"
        );
        refusals += 1;
        assert!(
            Instant::now() < deadline,
            "case {case:?}: a member past the {grace_period}-second period must be admitted, \
             refused {refusals} time(s)"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    let (lifecycle_state, health_status) = reported_member(&router, case, name, &instance_id).await;
    assert_eq!(
        (lifecycle_state.as_str(), health_status.as_str()),
        (IN_SERVICE, UNHEALTHY),
        "case {case:?}: the admitted call records the status it states, got \
         {lifecycle_state:?} and {health_status:?}"
    );
}
