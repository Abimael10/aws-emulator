// SPDX-License-Identifier: CC0-1.0

//! The whole path a client walks before it can observe `minimumHealthyHosts` at all: a launched
//! Amazon EC2 FLEET, the readiness gate a client puts in front of an EC2/on-premises (`Server`)
//! deployment — every host an `Online` Systems Manager managed node whose in-guest agent unit
//! answers an in-guest command — and then the paced deployments over exactly those hosts.
//!
//! RULE under test, in two halves that make one path:
//!
//! 1. an Amazon EC2 instance that is RUNNING with an instance profile whose role may make the
//!    agent's registration call is a Systems Manager managed node, so
//!    `DescribeInstanceInformation` narrowed to a set of such instance ids reports ONE
//!    `InstanceInformation` per id with `PingStatus` `Online`
//!    (`smithy_query(query="shape",shape="InstanceInformation")` of Systems Manager: the
//!    "connection status of SSM Agent"), and a shell command sent to each of those nodes leaves
//!    the transient `CommandInvocationStatus` values for a terminal one carrying what the
//!    command wrote in the guest — which is the only readiness check a client with no shell on
//!    its hosts can make;
//! 2. a `Server` deployment over the fleet those hosts form takes at most (fleet size - the
//!    resolved deployment configuration's `minimumHealthyHosts`) of them out of service at one
//!    instant and never fewer than one, so at every sampled instant `BatchGetDeploymentTargets`
//!    reports no more than that many targets `InProgress`, `GetDeployment`'s
//!    `deploymentOverview` counts partition the fleet, no target moves backwards, and a
//!    deployment that has to pace its fleet is held open — `createTime` to `completeTime`, as
//!    the service itself reports those instants — longer than the same revision deployed to the
//!    whole fleet at once. Every host of the fleet then holds the revision, which is what says
//!    the waves went round all of it.
//!
//! `minimumHealthyHosts` is "the minimum number or percentage of instances that should be
//! healthy at any time during a deployment", stated as `HOST_COUNT` ("the minimum number of
//! healthy instances as an absolute value") or `FLEET_PERCENT` ("as a percentage of the total
//! number of instances in the deployment", converted to instances with fractional instances
//! rounded up), and the model states the arithmetic itself: "In an example of nine instances, if
//! a HOST_COUNT of six is specified, deploy to up to three instances at a time"
//! (`smithy_query(query="shape",shape="MinimumHealthyHosts")`). The six counts of
//! `deploymentOverview` are all `InstanceCount`
//! (`smithy_query(query="shape",shape="DeploymentOverview")`).
//!
//! The rows differ along what the rule generalises over: the FORM the bound is stated in (a
//! configuration of the account's own stated as `HOST_COUNT` and as `FLEET_PERCENT`, and one of
//! CodeDeploy's published catalogue) and the concurrency it leaves over one fleet (one host at a
//! time, two at a time, and the whole fleet at once) — so the fleet is cut into three waves, two
//! waves and one, and the inequality between a paced leg and the unpaced one is asserted over
//! every paced row.
//!
//! The estate — CodeDeploy, Amazon EC2 (the fleet), Amazon S3 (the revisions), Systems Manager
//! (the reader of each host), IAM (the instance profile) and CloudWatch — is mounted on ONE
//! engine by the shared harness of `server_platform_rollback_read_through_run_command.rs`
//! ([`Estate`]), whose waits and Run Command reader this suite reuses; the AppSpec and bundle
//! builders are `server_platform_instance_deployments.rs`'s. Nothing is written twice.
//!
//! Nothing here waits a fixed time and nothing asserts on how long this process took: every span
//! is a stored due instant — a launch reaching `running`, a node registering, an invocation
//! finishing, a deployment reaching its outcome — so each step LOOKS AGAIN until its condition
//! holds or a generous deadline passes, and the two windows compared at the end are the
//! `createTime` and `completeTime` the service itself reports.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::server_platform_instance_deployments::{app_spec, bundle, element};
use crate::server_platform_rollback_read_through_run_command::{
    run_command, trust_policy, when_managed, when_running, Estate, AMI, EC2_API_VERSION,
    EC2_PRINCIPAL, IAM_API_VERSION, SSM_CORE_POLICY,
};

/// The instance type every host of the fleet is launched as.
const INSTANCE_TYPE: &str = "t3.micro";

/// How many hosts the fleet holds. Three is the smallest fleet whose bounds tell "one host at a
/// time" from "two at a time" from "the whole fleet at once" apart.
const HOSTS: usize = 3;

/// The bucket every revision below is uploaded to.
const BUCKET: &str = "codedeploy-paced-fleet-revisions";

/// The instance profile every host runs as, and the role it carries: a managed node registers as
/// the role IAM holds for its profile, which is what lets the agent in its guest be sent a
/// command at all.
const HOST_PROFILE: &str = "paced-fleet-host-profile";
/// See [`HOST_PROFILE`] — deliberately not the profile's own name.
const HOST_ROLE: &str = "paced-fleet-host-role";

/// The service role every deployment group below is created with; nothing of these cases
/// resolves it.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The agent whose unit the readiness check asks about, and the unit name the init system knows
/// it by.
const CODEDEPLOY_AGENT: &str = "codedeploy-agent";

/// The program each host's guest runs when it comes up: the documented installation of the
/// CodeDeploy agent, which is what makes the readiness check below answer `active`.
const USER_DATA: &str = "#!/bin/bash\ndnf install -y codedeploy-agent\n\
                         systemctl enable --now codedeploy-agent\n";

/// The tag every host carries and every deployment group below selects on.
const FLEET_TAG: (&str, &str) = ("Role", "paced-fleet");

/// The application every deployment group below belongs to.
const APPLICATION: &str = "paced-fleet";

/// The published EC2/on-premises configuration that bounds nothing, from the catalogue CodeDeploy
/// seeds: the unpaced leg every paced one is compared against.
const ALL_AT_ONCE: &str = "CodeDeployDefault.AllAtOnce";

/// The `PingStatus` a managed node whose agent is reporting carries.
const ONLINE: &str = "Online";

/// The six counts `deploymentOverview` partitions a deployment's instances into
/// (`smithy_query(query="shape",shape="DeploymentOverview")`).
const OVERVIEW_COUNTS: &[&str] = &[
    "Pending",
    "InProgress",
    "Succeeded",
    "Failed",
    "Skipped",
    "Ready",
];

/// How long any one condition is given. Generous by design: it is a deadline, not a measurement.
const BUDGET: Duration = Duration::from_secs(180);

/// How long one look-again cycle pauses before asking again. Fine enough that the window each
/// wave of targets is out of service for is sampled many times over.
const POLL: Duration = Duration::from_millis(25);

/// One leg of the table: how the fleet is bounded and how many of it that bound leaves out of
/// service at one instant.
struct Leg {
    /// What the row is about.
    case: &'static str,
    /// The `minimumHealthyHosts` of a configuration created for this leg, or `None` for the leg
    /// that names CodeDeploy's published [`ALL_AT_ONCE`] instead.
    minimum_healthy_hosts: Option<(&'static str, i32)>,
    /// How many of [`HOSTS`] the bound leaves deployable at one instant.
    concurrency: usize,
}

/// The table: the same fleet under bounds stated in both forms the model publishes, leaving one
/// host, two hosts and the whole fleet deployable at once.
fn legs() -> Vec<Leg> {
    vec![
        Leg {
            case: "a HOST_COUNT configuration keeping all but one host healthy",
            minimum_healthy_hosts: Some(("HOST_COUNT", 2)),
            concurrency: 1,
        },
        Leg {
            case: "a HOST_COUNT configuration keeping one host healthy",
            minimum_healthy_hosts: Some(("HOST_COUNT", 1)),
            concurrency: 2,
        },
        // 50% of three instances is one and a half, "rounded up" to two healthy hosts.
        Leg {
            case: "a FLEET_PERCENT configuration keeping half the fleet healthy, rounded up",
            minimum_healthy_hosts: Some(("FLEET_PERCENT", 50)),
            concurrency: 1,
        },
        Leg {
            case: "the published configuration that takes the whole fleet at once",
            minimum_healthy_hosts: None,
            concurrency: HOSTS,
        },
    ]
}

/// The revision one leg deploys: a `files` section copying its payload to the leg's own
/// destination — so no leg's Install meets a file another leg left behind — and a hook that
/// records the lifecycle event it ran for on the host.
fn revision(index: usize) -> Vec<u8> {
    let document = app_spec(
        &[("payload.txt", &destination(index))],
        &[("AfterInstall", "scripts/after.sh")],
    );
    let payload = payload_of(index);
    bundle(&[
        ("appspec.yml", document.as_str()),
        ("payload.txt", payload.as_str()),
        (
            "scripts/after.sh",
            "#!/bin/bash\nset -e\necho \"$LIFECYCLE_EVENT\" > /tmp/paced-fleet-event\n",
        ),
    ])
}

/// Where the revision of leg `index` installs its payload.
fn destination(index: usize) -> String {
    format!("/opt/paced-fleet-{index}")
}

/// What the revision of leg `index` carries, so what a host holds says WHICH leg put it there.
fn payload_of(index: usize) -> String {
    format!("payload-of-leg-{index}")
}

/// Create the instance profile the fleet runs as, launch [`HOSTS`] tagged hosts as it, and answer
/// their instance ids in launch order.
async fn given_fleet(estate: &Estate, case: &str) -> Vec<String> {
    let trust = trust_policy(EC2_PRINCIPAL);
    for (action, params) in [
        (
            "CreateRole",
            vec![
                ("RoleName", HOST_ROLE),
                ("AssumeRolePolicyDocument", &trust[..]),
            ],
        ),
        (
            "AttachRolePolicy",
            vec![("RoleName", HOST_ROLE), ("PolicyArn", SSM_CORE_POLICY)],
        ),
        (
            "CreateInstanceProfile",
            vec![("InstanceProfileName", HOST_PROFILE)],
        ),
        (
            "AddRoleToInstanceProfile",
            vec![
                ("InstanceProfileName", HOST_PROFILE),
                ("RoleName", HOST_ROLE),
            ],
        ),
    ] {
        estate
            .form(case, "iam", action, IAM_API_VERSION, &params)
            .await;
    }
    // "The user data to make available to the instance" travels base64 encoded on the wire, so
    // the program is stated once and encoded for each launch.
    let user_data = ::aws_smithy_types::base64::encode(USER_DATA);
    let mut hosts = Vec::with_capacity(HOSTS);
    for index in 0..HOSTS {
        let launched = estate
            .form(
                case,
                "ec2",
                "RunInstances",
                EC2_API_VERSION,
                &[
                    ("ImageId", AMI),
                    ("InstanceType", INSTANCE_TYPE),
                    ("MinCount", "1"),
                    ("MaxCount", "1"),
                    ("IamInstanceProfile.Name", HOST_PROFILE),
                    ("UserData", &user_data),
                    ("TagSpecification.1.ResourceType", "instance"),
                    ("TagSpecification.1.Tag.1.Key", FLEET_TAG.0),
                    ("TagSpecification.1.Tag.1.Value", FLEET_TAG.1),
                ],
            )
            .await;
        let host = element(&launched, "instanceId").unwrap_or_else(|| {
            panic!("case {case:?}: RunInstances reports an id for host {index}: {launched}")
        });
        hosts.push(host);
    }
    hosts
}

/// Half one of the rule: every host of `fleet` is an `Online` managed node — reported by ONE
/// `DescribeInstanceInformation` narrowed to all of their ids, the call a client's readiness gate
/// makes — and an in-guest command on each of them finishes, reporting the agent's unit active.
async fn assert_fleet_is_ready(estate: &Estate, case: &str, fleet: &[String]) {
    for host in fleet {
        when_running(estate, case, host).await;
        when_managed(estate, case, host).await;
    }

    let reported = estate
        .ssm(
            case,
            "DescribeInstanceInformation",
            json!({"Filters": [{"Key": "InstanceIds", "Values": fleet.to_vec()}]}),
        )
        .await;
    let nodes = reported["InstanceInformationList"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        nodes.len(),
        fleet.len(),
        "case {case:?}: expected one managed node per launched host, got {reported:?}"
    );
    for host in fleet {
        let node = nodes
            .iter()
            .find(|node| node["InstanceId"].as_str() == Some(host.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected {host} among the managed nodes, got {reported:?}")
            });
        assert_eq!(
            node["PingStatus"].as_str(),
            Some(ONLINE),
            "case {case:?}: expected {host}'s agent to be reporting, got {node:?}"
        );
    }

    for host in fleet {
        let readiness = run_command(
            estate,
            case,
            host,
            &format!("systemctl is-active {CODEDEPLOY_AGENT}"),
        )
        .await;
        assert_eq!(
            readiness["Status"].as_str(),
            Some("Success"),
            "case {case:?}: the readiness command on {host} must leave the transient statuses \
             for a terminal Success, got {readiness:?}"
        );
        assert_eq!(
            readiness["StandardOutputContent"]
                .as_str()
                .unwrap_or_default()
                .trim(),
            "active",
            "case {case:?}: expected the init system on {host} to report the agent's unit \
             active, got {readiness:?}"
        );
    }
}

/// The counts `deploymentOverview` reports, as (status, count) pairs, asserting that each is a
/// number and that together they partition a fleet of `fleet`.
fn assert_partitions(case: &str, overview: &Value, fleet: usize) -> Vec<(String, i64)> {
    let mut counted = Vec::with_capacity(OVERVIEW_COUNTS.len());
    let mut total = 0;
    for name in OVERVIEW_COUNTS {
        let count = overview[*name].as_i64().unwrap_or_else(|| {
            panic!("case {case:?}: expected deploymentOverview.{name} as a number, got {overview}")
        });
        total += count;
        counted.push(((*name).to_string(), count));
    }
    assert_eq!(
        usize::try_from(total).unwrap_or_default(),
        fleet,
        "case {case:?}: expected the summary to partition the fleet of {fleet}, got {overview}"
    );
    counted
}

/// How far along a target's `TargetStatus` is, so a sample can be checked for a target moving
/// BACKWARDS (`smithy_query(query="shape",shape="TargetStatus")`).
fn progress_of(case: &str, status: &str) -> usize {
    match status {
        "Pending" => 0,
        "InProgress" => 1,
        "Succeeded" => 2,
        other => panic!("case {case:?}: expected a paced target's status, got {other:?}"),
    }
}

/// Deploy the revision at `key` through `group`, sampling the deployment until it is terminal:
/// at most `concurrency` of `fleet` InProgress at any instant, a summary that partitions the
/// fleet at every instant, no target moving backwards, and every target ending `Succeeded`.
///
/// Answers the window the service reported for it — `completeTime` less `createTime`, in seconds
/// as the awsJson wire writes a timestamp — and the most targets seen out of service at once.
async fn run_leg(
    estate: &Estate,
    case: &str,
    group: &str,
    key: &str,
    fleet: &[String],
    concurrency: usize,
) -> (f64, usize) {
    let deployment = estate
        .codedeploy(
            case,
            "CreateDeployment",
            json!({
                "applicationName": APPLICATION,
                "deploymentGroupName": group,
                "revision": {
                    "revisionType": "S3",
                    "s3Location": {"bucket": BUCKET, "key": key, "bundleType": "zip"},
                },
                "fileExistsBehavior": "OVERWRITE",
            }),
        )
        .await["deploymentId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateDeployment reports a deploymentId"))
        .to_string();

    let deadline = Instant::now() + BUDGET;
    let mut most_in_progress = 0_usize;
    let mut furthest: Vec<usize> = vec![0; fleet.len()];
    loop {
        let info = estate
            .codedeploy(case, "GetDeployment", json!({"deploymentId": deployment}))
            .await["deploymentInfo"]
            .clone();
        let status = info["status"].as_str().unwrap_or_default().to_string();
        let overview = assert_partitions(case, &info["deploymentOverview"], fleet.len());
        let in_progress = overview
            .iter()
            .find(|(name, _)| name == "InProgress")
            .map_or(0, |(_, count)| usize::try_from(*count).unwrap_or_default());
        assert!(
            in_progress <= concurrency,
            "case {case:?}: expected at most {concurrency} of the {} host(s) out of service at \
             once, got {in_progress} in {info}",
            fleet.len()
        );
        most_in_progress = most_in_progress.max(in_progress);

        // The per-target records a client samples, which have to agree with that summary. A
        // deployment the clock has not started yet holds no readable target, which is the window
        // the not-started refusal covers.
        if status != "Created" {
            let read = estate
                .codedeploy(
                    case,
                    "BatchGetDeploymentTargets",
                    json!({"deploymentId": deployment, "targetIds": fleet.to_vec()}),
                )
                .await;
            let mut sampled = 0;
            for target in read["deploymentTargets"].as_array().unwrap_or(&Vec::new()) {
                let instance = &target["instanceTarget"];
                let target_id = instance["targetId"].as_str().unwrap_or_default();
                let reported = instance["status"].as_str().unwrap_or_default();
                let position = fleet
                    .iter()
                    .position(|id| id == target_id)
                    .unwrap_or_else(|| panic!("case {case:?}: unknown target {target_id:?}"));
                let reached = progress_of(case, reported);
                assert!(
                    reached >= furthest[position],
                    "case {case:?}: expected {target_id:?} not to move backwards, it was at {} \
                     and now reports {reported:?}",
                    furthest[position]
                );
                furthest[position] = reached;
                if reached == 1 {
                    sampled += 1;
                }
            }
            assert!(
                sampled <= concurrency,
                "case {case:?}: expected at most {concurrency} target(s) reported InProgress, \
                 got {sampled} in {read}"
            );
        }

        if matches!(status.as_str(), "Succeeded" | "Failed" | "Stopped") {
            assert_eq!(
                status, "Succeeded",
                "case {case:?}: expected the deployment over its ready fleet to succeed, got \
                 {info}"
            );
            let created = info["createTime"]
                .as_f64()
                .unwrap_or_else(|| panic!("case {case:?}: expected a createTime, got {info}"));
            let completed = info["completeTime"]
                .as_f64()
                .unwrap_or_else(|| panic!("case {case:?}: expected a completeTime, got {info}"));
            assert!(
                completed >= created,
                "case {case:?}: a deployment cannot complete before it was created, got \
                 createTime {created} and completeTime {completed}"
            );
            return (completed - created, most_in_progress);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployment to reach an outcome within {BUDGET:?}, last \
             read {info}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What `cat` of the installed payload reports in a host's guest, read through Run Command.
async fn installed_payload(estate: &Estate, case: &str, host: &str, index: usize) -> String {
    let invocation = run_command(
        estate,
        case,
        host,
        &format!("cat {}/payload.txt", destination(index)),
    )
    .await;
    assert_eq!(
        invocation["Status"].as_str(),
        Some("Success"),
        "case {case:?}: reading the installed payload off {host} must succeed, got {invocation:?}"
    );
    invocation["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// RULE: see the module docs — a running instance with an SSM-capable instance profile is an
/// `Online` managed node that answers in-guest commands, and a `Server` deployment over the
/// fleet of such hosts is paced by its configuration's `minimumHealthyHosts`.
#[tokio::test]
async fn a_ready_ec2_fleet_is_paced_by_the_deployment_configs_minimum_healthy_hosts() {
    let estate = Estate::new();
    let setup = "the readiness gate in front of a paced Server deployment";

    // Half one: the fleet, and the gate a client passes before it deploys anything to it.
    let fleet = given_fleet(&estate, setup).await;
    assert_fleet_is_ready(&estate, setup, &fleet).await;

    // Half two: the same hosts, deployed to under each bound of the table.
    estate
        .codedeploy(
            setup,
            "CreateApplication",
            json!({"applicationName": APPLICATION, "computePlatform": "Server"}),
        )
        .await;
    let mut windows: Vec<(&str, f64, usize)> = Vec::new();
    for (index, leg) in legs().iter().enumerate() {
        let case = leg.case;
        let key = format!("revision-{index}.zip");
        estate.upload(case, BUCKET, &key, &revision(index)).await;
        let configuration = match leg.minimum_healthy_hosts {
            None => ALL_AT_ONCE.to_string(),
            Some((kind, value)) => {
                let name = format!("paced-fleet-{index}");
                estate
                    .codedeploy(
                        case,
                        "CreateDeploymentConfig",
                        json!({
                            "deploymentConfigName": name,
                            "computePlatform": "Server",
                            "minimumHealthyHosts": {"type": kind, "value": value},
                        }),
                    )
                    .await;
                name
            }
        };
        let group = format!("wave-{index}");
        estate
            .codedeploy(
                case,
                "CreateDeploymentGroup",
                json!({
                    "applicationName": APPLICATION,
                    "deploymentGroupName": group,
                    "serviceRoleArn": SERVICE_ROLE,
                    "deploymentConfigName": configuration,
                    "ec2TagFilters": [
                        {"Key": FLEET_TAG.0, "Value": FLEET_TAG.1, "Type": "KEY_AND_VALUE"},
                    ],
                }),
            )
            .await;

        let (window, most_in_progress) =
            run_leg(&estate, case, &group, &key, &fleet, leg.concurrency).await;
        assert_eq!(
            most_in_progress, leg.concurrency,
            "case {case:?}: expected a wave of {} host(s) to have been out of service at once, \
             the most seen was {most_in_progress}",
            leg.concurrency
        );

        // The waves went round the WHOLE fleet: every host's guest holds this leg's revision.
        for host in &fleet {
            let read = installed_payload(&estate, case, host, index).await;
            assert_eq!(
                read,
                payload_of(index),
                "case {case:?}: expected {host} to hold the revision this deployment installed, \
                 got {read:?}"
            );
        }
        windows.push((case, window, leg.concurrency));
    }

    // The leg that took the whole fleet at once is the one every paced leg is compared against:
    // a deployment held open one span per wave cannot report a window as short as it.
    let (unpaced_case, unpaced, _) = windows
        .iter()
        .find(|(_, _, concurrency)| *concurrency == HOSTS)
        .copied()
        .unwrap_or_else(|| panic!("the table holds a leg that deploys to the whole fleet at once"));
    for (case, window, concurrency) in &windows {
        if *concurrency == HOSTS {
            continue;
        }
        assert!(
            *window > unpaced,
            "case {case:?}: a fleet of {HOSTS} deployed to {concurrency} host(s) at a time is \
             held open for one span per wave, so it cannot report {window}s when \
             {unpaced_case:?} reported {unpaced}s for the whole fleet at once"
        );
    }
}
