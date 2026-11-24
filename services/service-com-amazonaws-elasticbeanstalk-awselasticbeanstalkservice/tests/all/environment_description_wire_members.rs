// SPDX-License-Identifier: CC0-1.0

//! What an environment READ reports beside the members the on-disk model
//! declares, driven end to end over the service's awsQuery HTTP surface.
//!
//! The rules under test:
//!
//! * every `EnvironmentDescription` this service reports carries the whole
//!   member set the real service's awsQuery responses carry — the modelled
//!   `EnvironmentLinks` and the two the wire carries beyond the model,
//!   `MinCapacityEnabled` and `Alerts` — for EVERY environment and whichever
//!   operation reports the shape, with a list reported PRESENT AND EMPTY where
//!   the environment has none of that thing rather than left out, so a client
//!   never has to tell an absent member from an empty one. `MinCapacityEnabled`
//!   is the environment's own minimum-capacity configuration, which is not the
//!   Auto Scaling group's minimum size: an environment whose configuration pins
//!   `MinSize` still has no minimum-capacity configuration.
//! * `DescribeEnvironmentResources` reports, beside the resources the
//!   environment is MADE of, the sources it RUNS: one `AppSource` runtime source
//!   naming the application and the version label deployed in the environment,
//!   read off the environment's own state — so the entry follows a deployment
//!   that replaces that version — and an empty list for an environment running
//!   no application version at all.
//!
//! Each table row is a different point in those classes — a version deployed and
//! none, a version replaced by an update, the web-server and the worker tier, a
//! single-instance and a load-balanced environment, an environment whose
//! configuration pins the Auto Scaling minimum, and the four operations that
//! report an environment description — not a renamed copy of the row before it.
//! The one case these tables do not hold is a NON-EMPTY `EnvironmentLinks`,
//! which only a composed environment has and which `composed_environments.rs`
//! holds the service to.
//!
//! The HTTP harness (`Emulator`, `Answer`, the status poll) is the one
//! `lifecycle.rs` owns, so nothing here reaches into the store: a member that is
//! not serialized, or one a second read would report differently, fails these
//! tests.

use crate::harness::{wait_for_status, Answer, Emulator, SOLUTION_STACK};
use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application every environment below is launched under.
const APPLICATION: &str = "wire-members-app";

/// The worker tier, by the two members that name it.
const WORKER_TIER: &[(&str, &str)] = &[("Tier.Name", "Worker"), ("Tier.Type", "SQS/HTTP")];

/// `aws:elasticbeanstalk:environment` / `EnvironmentType` at `SingleInstance`:
/// "launch one EC2 instance with no load balancer".
const SINGLE_INSTANCE: &[(&str, &str)] = &[
    (
        "OptionSettings.member.1.Namespace",
        "aws:elasticbeanstalk:environment",
    ),
    ("OptionSettings.member.1.OptionName", "EnvironmentType"),
    ("OptionSettings.member.1.Value", "SingleInstance"),
];

/// `aws:autoscaling:asg` / `MinSize` at 3 — the Auto Scaling group's minimum,
/// which is a different thing from a minimum-capacity configuration.
const MIN_SIZE_3: &[(&str, &str)] = &[
    ("OptionSettings.member.1.Namespace", "aws:autoscaling:asg"),
    ("OptionSettings.member.1.OptionName", "MinSize"),
    ("OptionSettings.member.1.Value", "3"),
];

/// Call `action`, failing the named case unless it answered 200.
async fn ok(emulator: &Emulator, case: &str, action: &str, params: &[(&str, &str)]) -> Answer {
    let answer = emulator.call(action, params).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from {action}, got {} and body {}",
        answer.status, answer.body
    );
    answer
}

/// Create [`APPLICATION`] and one application version per label in `versions`.
async fn given_application(emulator: &Emulator, case: &str, versions: &[&str]) {
    ok(
        emulator,
        case,
        "CreateApplication",
        &[("ApplicationName", APPLICATION)],
    )
    .await;
    for version_label in versions {
        ok(
            emulator,
            case,
            "CreateApplicationVersion",
            &[
                ("ApplicationName", APPLICATION),
                ("VersionLabel", version_label),
            ],
        )
        .await;
    }
}

/// Launch `environment`, optionally deploying `version_label`, with `extra`
/// parameters — answering the `EnvironmentId` and the launch's own response.
async fn launch(
    emulator: &Emulator,
    case: &str,
    environment: &str,
    version_label: Option<&str>,
    extra: &[&[(&str, &str)]],
) -> (String, Answer) {
    let mut params: Vec<(&str, &str)> = vec![
        ("ApplicationName", APPLICATION),
        ("EnvironmentName", environment),
        ("SolutionStackName", SOLUTION_STACK.as_str()),
    ];
    if let Some(version_label) = version_label {
        params.push(("VersionLabel", version_label));
    }
    for set in extra {
        params.extend_from_slice(set);
    }
    let launched = ok(emulator, case, "CreateEnvironment", &params).await;
    let id = launched.expect("EnvironmentId", case).to_string();
    (id, launched)
}

/// The three members under test, as one answer reports them at `scope` — each as
/// the list of values the response carries for it, EMPTY TEXT included, so an
/// absent member and one reported empty are different answers here.
fn reported_members(answer: &Answer, scope: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let at = |member: &str| {
        answer
            .values_at_including_empty(&format!("{scope}{member}"))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    (
        at("MinCapacityEnabled"),
        at("Alerts"),
        at("EnvironmentLinks"),
    )
}

/// Assert that `answer` reports the three members once per environment it
/// describes, each with the value an environment with no links, no alerts and no
/// minimum-capacity configuration has.
fn assert_reported_members(answer: &Answer, scope: &str, environments: usize, case: &str) {
    let (min_capacity, alerts, links) = reported_members(answer, scope);
    assert_eq!(
        min_capacity,
        vec!["false".to_string(); environments],
        "case {case:?}: expected {environments} MinCapacityEnabled element(s) reporting false, \
         got {min_capacity:?} in body {}",
        answer.body
    );
    assert_eq!(
        alerts,
        vec![String::new(); environments],
        "case {case:?}: expected {environments} present-but-empty Alerts element(s), got \
         {alerts:?} in body {}",
        answer.body
    );
    assert_eq!(
        links,
        vec![String::new(); environments],
        "case {case:?}: expected {environments} present-but-empty EnvironmentLinks element(s), \
         got {links:?} in body {}",
        answer.body
    );
}

/// RULE: every operation that reports an `EnvironmentDescription` reports
/// `MinCapacityEnabled`, `Alerts` and `EnvironmentLinks` for every environment it
/// describes — the lists present and empty where the environment has none.
#[tokio::test]
async fn every_reported_environment_carries_the_whole_member_set() {
    let emulator = Emulator::new();
    let case = "the four operations that report an environment description";
    given_application(&emulator, case, &["v1"]).await;

    // The WRITE that launches one: the description it answers with carries the
    // members already.
    let (_, launched) = launch(&emulator, case, "member-set-env", Some("v1"), &[]).await;
    assert_reported_members(&launched, "", 1, "CreateEnvironment");

    // The READ of it, once the launch the clock owes has completed.
    wait_for_status(&emulator, case, "member-set-env", "Ready").await;
    let described = ok(
        &emulator,
        case,
        "DescribeEnvironments",
        &[("EnvironmentNames.member.1", "member-set-env")],
    )
    .await;
    assert_reported_members(&described, "Environments/member/", 1, "DescribeEnvironments");

    // The UPDATE of it, which answers the description the update leaves.
    let updated = ok(
        &emulator,
        case,
        "UpdateEnvironment",
        &[
            ("EnvironmentName", "member-set-env"),
            ("Description", "a described environment"),
        ],
    )
    .await;
    assert_reported_members(&updated, "", 1, "UpdateEnvironment");

    // And the TERMINATE, whose answer describes the environment it is shutting
    // down.
    wait_for_status(&emulator, case, "member-set-env", "Ready").await;
    let terminated = ok(
        &emulator,
        case,
        "TerminateEnvironment",
        &[("EnvironmentName", "member-set-env")],
    )
    .await;
    assert_reported_members(&terminated, "", 1, "TerminateEnvironment");
}

/// RULE: the member set does not depend on WHAT was launched — the tier, the
/// environment type, whether a version is deployed or whether the configuration
/// pins the Auto Scaling minimum — and one read that describes several
/// environments reports it for each of them.
#[tokio::test]
async fn the_member_set_is_reported_for_every_kind_of_environment() {
    let emulator = Emulator::new();
    let case = "every kind of environment";
    given_application(&emulator, case, &["v1"]).await;

    /// One environment the table launches.
    struct Row {
        case: &'static str,
        environment: &'static str,
        version_label: Option<&'static str>,
        extra: &'static [&'static [(&'static str, &'static str)]],
    }
    let rows = [
        Row {
            case: "a load-balanced web server with a version deployed",
            environment: "kind-load-balanced",
            version_label: Some("v1"),
            extra: &[],
        },
        Row {
            case: "an environment running no application version",
            environment: "kind-no-version",
            version_label: None,
            extra: &[],
        },
        Row {
            case: "a single-instance environment",
            environment: "kind-single",
            version_label: Some("v1"),
            extra: &[SINGLE_INSTANCE],
        },
        Row {
            case: "a worker-tier environment",
            environment: "kind-worker",
            version_label: Some("v1"),
            extra: &[WORKER_TIER],
        },
        Row {
            // The Auto Scaling group's minimum is not a minimum-capacity
            // configuration: this environment reports the member false too.
            case: "an environment pinning the Auto Scaling minimum",
            environment: "kind-min-size",
            version_label: Some("v1"),
            extra: &[MIN_SIZE_3],
        },
    ];
    for row in &rows {
        let (_, launched) =
            launch(&emulator, row.case, row.environment, row.version_label, row.extra).await;
        assert_reported_members(&launched, "", 1, row.case);
    }

    // One read of them ALL: the member set is reported once per described
    // environment, not once per response.
    let described = ok(
        &emulator,
        case,
        "DescribeEnvironments",
        &[("ApplicationName", APPLICATION)],
    )
    .await;
    assert_eq!(
        described
            .values_at("Environments/member/EnvironmentName")
            .len(),
        rows.len(),
        "case {case:?}: expected every launched environment in one report, got body {}",
        described.body
    );
    assert_reported_members(&described, "Environments/member/", rows.len(), case);
}

/// The runtime sources an answer reports, as
/// `(Parameter, [(ApplicationName, VersionLabel)])` per entry — read element by
/// element so a version attributed to the wrong source fails rather than
/// matching a flat list.
fn reported_runtime_sources(answer: &Answer) -> Vec<(String, Vec<(String, String)>)> {
    answer
        .elements_at("EnvironmentResources/RuntimeSources/member")
        .iter()
        .map(|source| {
            (
                source.value("Parameter").unwrap_or_default().to_string(),
                source
                    .elements_at("Versions/member")
                    .iter()
                    .map(|version| {
                        (
                            version
                                .value("ApplicationName")
                                .unwrap_or_default()
                                .to_string(),
                            version
                                .value("VersionLabel")
                                .unwrap_or_default()
                                .to_string(),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

/// The resources reported for `environment`, named by its name.
async fn described_resources(emulator: &Emulator, case: &str, environment: &str) -> Answer {
    ok(
        emulator,
        case,
        "DescribeEnvironmentResources",
        &[("EnvironmentName", environment)],
    )
    .await
}

/// The one runtime-source entry an environment running `version_label` must
/// report. The `Parameter` is read off the service rather than written out
/// here, so the table cannot drift from what it reports.
fn app_source(version_label: &str) -> Vec<(String, Vec<(String, String)>)> {
    vec![(
        eb::ops::types::RUNTIME_SOURCE_PARAMETER_APP_SOURCE.to_string(),
        vec![(APPLICATION.to_string(), version_label.to_string())],
    )]
}

/// RULE: the resources view reports the application version the environment
/// RUNS as its `AppSource` runtime source, from the environment's own state —
/// so an environment with no version deployed reports an empty list, and one
/// whose deployed version has been replaced reports the version it runs now.
#[tokio::test]
async fn the_resources_view_reports_the_deployed_runtime_source() {
    let emulator = Emulator::new();
    let case = "the deployed runtime source";
    given_application(&emulator, case, &["v1", "v2"]).await;

    /// One environment the table launches, and the runtime source it must then
    /// report.
    struct Row {
        case: &'static str,
        environment: &'static str,
        version_label: Option<&'static str>,
        extra: &'static [&'static [(&'static str, &'static str)]],
    }
    let rows = [
        Row {
            case: "a web-server environment running a version",
            environment: "runtime-web",
            version_label: Some("v1"),
            extra: &[],
        },
        Row {
            case: "a worker-tier environment running a version",
            environment: "runtime-worker",
            version_label: Some("v2"),
            extra: &[WORKER_TIER],
        },
        Row {
            case: "a single-instance environment running a version",
            environment: "runtime-single",
            version_label: Some("v1"),
            extra: &[SINGLE_INSTANCE],
        },
        Row {
            case: "an environment running no application version",
            environment: "runtime-none",
            version_label: None,
            extra: &[],
        },
    ];
    for row in &rows {
        launch(
            &emulator,
            row.case,
            row.environment,
            row.version_label,
            row.extra,
        )
        .await;
        let resources = described_resources(&emulator, row.case, row.environment).await;
        let reported = reported_runtime_sources(&resources);
        let expected = row
            .version_label
            .map(app_source)
            .unwrap_or_default();
        assert_eq!(
            reported, expected,
            "case {:?}: expected runtime sources {expected:?}, got {reported:?} in body {}",
            row.case, resources.body
        );
        // The list itself is reported for every environment, empty or not.
        assert_eq!(
            resources
                .values_at_including_empty("EnvironmentResources/RuntimeSources")
                .len(),
            1,
            "case {:?}: expected a RuntimeSources element, got body {}",
            row.case,
            resources.body
        );
    }

    // A DEPLOYMENT moves it: the entry names the version the environment runs
    // now, which is the one the update deployed, not the one it was launched
    // with.
    let case = "a version replaced by an update";
    wait_for_status(&emulator, case, "runtime-web", "Ready").await;
    ok(
        &emulator,
        case,
        "UpdateEnvironment",
        &[("EnvironmentName", "runtime-web"), ("VersionLabel", "v2")],
    )
    .await;
    let resources = described_resources(&emulator, case, "runtime-web").await;
    let reported = reported_runtime_sources(&resources);
    assert_eq!(
        reported,
        app_source("v2"),
        "case {case:?}: expected the deployed version to be reported, got {reported:?} in body {}",
        resources.body
    );
}
