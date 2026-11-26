// SPDX-License-Identifier: CC0-1.0

//! What a client can READ OFF THE HOST between a deployment failing and the automatic
//! rollback putting the previous revision back, driven the way a client drives it: Systems
//! Manager Run Command into the agent's guest.
//!
//! RULE under test: a rollback is a deployment of its OWN — created when the deployment it
//! rolls back reached its outcome, then dispatched, fetching the revision it redeploys and
//! installing it on each host — so the revision the FAILED deployment installed stays on the
//! host until that rollback's own Install replaces it, and the window it stays for is one the
//! client can read it in through an API whose own answer is owed a span (a Run Command
//! invocation finishes a span after the send). Only then does the same read report the
//! previous revision's content again, under a rollback deployment whose `creator` is
//! `codeDeployRollback` and whose `rollbackInfo` names the deployment that triggered it.
//!
//! The estate, the waits and the Run Command reader are the sibling suite's
//! (`server_platform_rollback_read_through_run_command`), which asserts the ENDS of that
//! sequence — the installed revision, and the previous one back after the rollback. What this
//! suite adds is the MIDDLE of it: the failing revision on the host while the deployment that
//! installed it has not been rolled back yet.
//!
//! Nothing here waits a fixed time: every span is a stored due instant, so each step looks
//! again until the condition holds or a generous deadline passes, and nothing asserts on
//! elapsed time.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::server_platform_instance_deployments::{app_spec, bundle, element};
use crate::server_platform_rollback_read_through_run_command::{
    run_command, trust_policy, when_managed, when_running, when_terminal, Estate, AMI,
    EC2_API_VERSION, EC2_PRINCIPAL, IAM_API_VERSION, SSM_CORE_POLICY,
};

/// The service role each deployment group below is created with; nothing resolves it here.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// Where both revisions of a row install their payload on the host.
const DESTINATION: &str = "/opt/rollback-window";

/// What the first revision's payload carries, and what the rollback has to put back.
const MARKER_A: &str = "revision-a-payload";

/// What the failing revision's payload carries: the content this suite has to be able to read
/// off the host before the rollback replaces it.
const MARKER_B: &str = "revision-b-payload";

/// The file a waiting hook script waits for, which the client touches to release it.
const RELEASE_MARKER: &str = "/tmp/release-the-hook";

/// How long any one condition is given. Generous by design: it is a deadline, not a
/// measurement.
const BUDGET: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL: Duration = Duration::from_millis(25);

/// One row of the table: a deployment whose hook fails AFTER the revision's files are on the
/// host, in one of the two shapes such a hook takes.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// The resource names of the row, which share this prefix so the rows cannot collide.
    name: &'static str,
    /// The lifecycle event the failing hook runs for: both of these run after `Install`, so
    /// the revision's files are on the host by the time the hook fails.
    event: &'static str,
    /// Whether that hook WAITS for the client to release it before it exits non-zero, rather
    /// than exiting non-zero as soon as the agent runs it.
    released_by_the_client: bool,
}

impl Row {
    /// The hook script this row's revision carries.
    fn script(&self) -> String {
        if self.released_by_the_client {
            format!(
                "#!/bin/bash\nwhile [ ! -f {RELEASE_MARKER} ]; do sleep 1; done\nexit 1\n"
            )
        } else {
            "#!/bin/bash\nexit 7\n".to_string()
        }
    }
}

/// The revision bundle of one leg: a `files` section copying its payload to [`DESTINATION`],
/// plus — for the leg that must FAIL — the row's own hook.
fn revision(marker: &str, failing: Option<&Row>) -> Vec<u8> {
    let hooks: Vec<(&str, &str)> = match failing {
        Some(row) => vec![(row.event, "scripts/hook.sh")],
        None => Vec::new(),
    };
    let document = app_spec(&[("payload.txt", DESTINATION)], &hooks);
    let script = failing.map(Row::script).unwrap_or_default();
    let mut entries: Vec<(&str, &str)> = vec![
        ("appspec.yml", document.as_str()),
        ("payload.txt", marker),
    ];
    if failing.is_some() {
        entries.push(("scripts/hook.sh", script.as_str()));
    }
    bundle(&entries)
}

/// What `cat` of the installed payload reports on the host, with the lines stripped; `None`
/// when the read itself did not succeed, which is what a path the host holds nothing at
/// answers.
async fn payload_on_the_host(estate: &Estate, case: &str, instance: &str) -> Option<String> {
    let invocation = run_command(
        estate,
        case,
        instance,
        &format!("cat {DESTINATION}/payload.txt"),
    )
    .await;
    (invocation["Status"].as_str() == Some("Success")).then(|| {
        invocation["StandardOutputContent"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string()
    })
}

/// Look again until the payload on the host reads `expected`, answering what every read
/// reported on the way there.
async fn when_payload_is(
    estate: &Estate,
    case: &str,
    instance: &str,
    expected: &str,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut read: Vec<String> = Vec::new();
    loop {
        let answered = payload_on_the_host(estate, case, instance).await;
        read.push(answered.clone().unwrap_or_else(|| "<nothing>".to_string()));
        if answered.as_deref() == Some(expected) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected:?} to be readable on {instance}, read {read:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Create a deployment of `key` on `application`/`group`, answering its minted id.
async fn deploy(
    estate: &Estate,
    case: &str,
    application: &str,
    group: &str,
    bucket: &str,
    key: &str,
) -> String {
    estate
        .codedeploy(
            case,
            "CreateDeployment",
            json!({
                "applicationName": application,
                "deploymentGroupName": group,
                "revision": {
                    "revisionType": "S3",
                    "s3Location": {"bucket": bucket, "key": key, "bundleType": "zip"},
                },
            }),
        )
        .await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// RULE: see the module docs — the revision a failed deployment installed is readable on the
/// host until the rollback's own Install puts the previous revision back.
#[tokio::test]
async fn the_failed_revision_is_readable_on_the_host_until_the_rollback_replaces_it() {
    // Rows differ along the shape of the hook that fails after the files are installed: one
    // that exits non-zero as soon as the agent runs it, and one that WAITS for the client
    // before it does — the client can only release it while the revision it installed is
    // still on the host, which is the window this rule is about.
    let rows: &[Row] = &[
        Row {
            case: "a hook that exits non-zero as soon as it is run",
            name: "prompt",
            event: "AfterInstall",
            released_by_the_client: false,
        },
        Row {
            case: "a hook that waits for the client to release it, then exits non-zero",
            name: "waiting",
            event: "ValidateService",
            released_by_the_client: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let estate = Estate::new();
        let role = format!("{}-host-role", row.name);
        let profile = format!("{}-host-profile", row.name);
        let tag = format!("{}-host", row.name);
        let bucket = format!("{}-revisions", row.name);
        let application = format!("{}-app", row.name);
        let group = "fleet";

        // The instance profile the host runs as: a managed node registers as the role IAM
        // holds for it, which is why a command can be delivered to the agent in its guest.
        let trust = trust_policy(EC2_PRINCIPAL);
        for (action, params) in [
            (
                "CreateRole",
                vec![
                    ("RoleName", role.as_str()),
                    ("AssumeRolePolicyDocument", &trust[..]),
                ],
            ),
            (
                "AttachRolePolicy",
                vec![("RoleName", role.as_str()), ("PolicyArn", SSM_CORE_POLICY)],
            ),
            (
                "CreateInstanceProfile",
                vec![("InstanceProfileName", profile.as_str())],
            ),
            (
                "AddRoleToInstanceProfile",
                vec![
                    ("InstanceProfileName", profile.as_str()),
                    ("RoleName", role.as_str()),
                ],
            ),
        ] {
            estate
                .form(case, "iam", action, IAM_API_VERSION, &params)
                .await;
        }

        let launched = estate
            .form(
                case,
                "ec2",
                "RunInstances",
                EC2_API_VERSION,
                &[
                    ("ImageId", AMI),
                    ("InstanceType", "t3.micro"),
                    ("MinCount", "1"),
                    ("MaxCount", "1"),
                    ("IamInstanceProfile.Name", profile.as_str()),
                    ("TagSpecification.1.ResourceType", "instance"),
                    ("TagSpecification.1.Tag.1.Key", "Name"),
                    ("TagSpecification.1.Tag.1.Value", tag.as_str()),
                ],
            )
            .await;
        let host = element(&launched, "instanceId")
            .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an id: {launched}"));
        when_running(&estate, case, &host).await;
        when_managed(&estate, case, &host).await;

        // The two revisions, each under its own key: the rollback redeploys the first one's
        // object, so the second one cannot be allowed to replace it.
        estate
            .upload(case, &bucket, "revision-a.zip", &revision(MARKER_A, None))
            .await;
        estate
            .upload(
                case,
                &bucket,
                "revision-b.zip",
                &revision(MARKER_B, Some(row)),
            )
            .await;

        estate
            .codedeploy(
                case,
                "CreateApplication",
                json!({"applicationName": application, "computePlatform": "Server"}),
            )
            .await;
        estate
            .codedeploy(
                case,
                "CreateDeploymentGroup",
                json!({
                    "applicationName": application,
                    "deploymentGroupName": group,
                    "serviceRoleArn": SERVICE_ROLE,
                    "ec2TagFilters": [{"Key": "Name", "Value": tag, "Type": "KEY_AND_VALUE"}],
                    "autoRollbackConfiguration": {
                        "enabled": true,
                        "events": ["DEPLOYMENT_FAILURE"],
                    },
                }),
            )
            .await;

        // The first deployment succeeds and its payload is the one on the host.
        let first = deploy(&estate, case, &application, group, &bucket, "revision-a.zip").await;
        let info = when_terminal(&estate, case, &first).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the first deployment must succeed, got {info:?}"
        );
        let read = when_payload_is(&estate, case, &host, MARKER_A).await;
        assert_eq!(
            read.last().map(String::as_str),
            Some(MARKER_A),
            "case {case:?}: expected the first revision's payload on the host, read {read:?}"
        );

        // The second installs its own payload and then fails its hook. Its payload has to be
        // readable on the host while that is happening: for the waiting hook, the client could
        // not release it otherwise.
        let second = deploy(&estate, case, &application, group, &bucket, "revision-b.zip").await;
        let read = when_payload_is(&estate, case, &host, MARKER_B).await;
        assert_eq!(
            read.last().map(String::as_str),
            Some(MARKER_B),
            "case {case:?}: expected the failing revision's payload to be readable on the \
             host before its rollback, read {read:?}"
        );
        if row.released_by_the_client {
            let released = run_command(
                &estate,
                case,
                &host,
                &format!("touch {RELEASE_MARKER}"),
            )
            .await;
            assert_eq!(
                released["Status"].as_str(),
                Some("Success"),
                "case {case:?}: releasing the hook on {host} must succeed, got {released:?}"
            );
        }

        // It then fails, and the rollback it triggers carries the attribution of both legs.
        let info = when_terminal(&estate, case, &second).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Failed"),
            "case {case:?}: the second deployment must fail on its host, got {info:?}"
        );
        let rollback = info["rollbackInfo"]["rollbackDeploymentId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a rollback deployment must be reported: {info:?}")
            })
            .to_string();
        assert_ne!(
            rollback, second,
            "case {case:?}: the rollback is a deployment of its own, got {info:?}"
        );
        let rolled_back = when_terminal(&estate, case, &rollback).await;
        assert_eq!(
            rolled_back["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the rollback deployment must succeed, got {rolled_back:?}"
        );
        assert_eq!(
            rolled_back["creator"].as_str(),
            Some("codeDeployRollback"),
            "case {case:?}: the rollback is the service's own deployment, got {rolled_back:?}"
        );
        assert_eq!(
            rolled_back["rollbackInfo"]["rollbackTriggeringDeploymentId"].as_str(),
            Some(second.as_str()),
            "case {case:?}: the rollback names what triggered it, got {rolled_back:?}"
        );
        // And the two instants it REPORTS say the same thing as the host does: a rollback is
        // created when the failure is recorded and DISPATCHED a span later — it has to be
        // dispatched and fetch the revision it redeploys before its Install can replace
        // anything — so it is not started at the instant it is created, which is the window
        // the read above happened in. Both instants come from the deployment's own record,
        // so this says nothing about how fast the host running the test is.
        let created_at = rolled_back["createTime"].as_f64().unwrap_or_default();
        let started_at = rolled_back["startTime"].as_f64().unwrap_or_default();
        assert!(
            started_at - created_at >= 1.0,
            "case {case:?}: expected the rollback to be dispatched a span after it was \
             created, got createTime {created_at} and startTime {started_at}"
        );

        // And the previous revision's payload is back on the host.
        let read = when_payload_is(&estate, case, &host, MARKER_A).await;
        assert_eq!(
            read.last().map(String::as_str),
            Some(MARKER_A),
            "case {case:?}: expected the rollback to put the previous payload back, read \
             {read:?}"
        );
    }
}
