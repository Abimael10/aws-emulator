// SPDX-License-Identifier: CC0-1.0

//! Whose verdict the REVISION of an EC2/on-premises (`Server`) deployment gets, driven over
//! the wire.
//!
//! RULE under test: a `Server` revision is a BUNDLE the CodeDeploy agent downloads and reads
//! ON a target host — `DownloadBundle` is one of the lifecycle events reserved for the agent —
//! so what CodeDeploy reports about that bundle follows from a host whose agent read it. A
//! deployment whose deployment group resolves no such host is therefore answered for its
//! FLEET and not for its bundle: it fails with the `NO_INSTANCES` `ErrorCode`
//! (`smithy_query(query="shape",shape="ErrorCode")`) and that failure's own fixed sentence,
//! whatever the bucket serves at the key its revision names — and `INVALID_REVISION` is
//! reachable only once a host exists. The same reading decides what a TARGET reports: a
//! registered on-premises instance the deployment has been dispatched to, which no agent has
//! reported anything for, is `InProgress`
//! (`smithy_query(query="shape",shape="TargetStatus")`: Pending | InProgress | Succeeded |
//! Failed | Skipped | Unknown | Ready) rather than `Failed` for a bundle nothing read.
//!
//! Every case goes through the front door a client uses, over the three services the rule
//! spans — CodeDeploy, Amazon EC2 (the hosts a group's `ec2TagFilters` select) and Amazon S3
//! (the object the revision names) — through the sibling suite's [`Fleet`] harness, so the
//! object really is in a bucket this emulator holds and the verdict really is the one a client
//! reads.
//!
//! Nothing here waits a fixed time: each step looks again until the condition holds or a
//! generous deadline passes, and nothing asserts on elapsed time.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::server_platform_instance_deployments::{app_spec, bundle, Fleet};

/// How long any one condition is given. Generous by design: it is a deadline, not a
/// measurement.
const BUDGET: Duration = Duration::from_secs(60);

/// How long one look-again cycle pauses before asking again.
const POLL: Duration = Duration::from_millis(25);

/// The whole sentence CodeDeploy answers a deployment whose group resolved no instance with,
/// spelled out here so a change to the wording fails this suite. It interpolates nothing.
const NO_INSTANCES_MESSAGE: &str = "The deployment failed because no instances were found for \
     your deployment group. Check your deployment group settings to make sure the tags for \
     your Amazon EC2 instances or Auto Scaling groups correctly identify the instances you \
     want to deploy to, and then try again.";

/// The whole sentence it answers a revision whose AppSpec it could not locate with; also
/// fixed text.
const UNLOCATABLE_REVISION_MESSAGE: &str = "The AppSpec file cannot be located in the \
     specified S3 bucket. Verify your AppSpec file is present and that the name and key value \
     pair specified for your S3 bucket are correct. The S3 bucket must be in your current \
     region";

/// A revision bundle this emulator can read: one `files` entry and nothing else.
fn readable_bundle() -> Vec<u8> {
    bundle(&[
        ("appspec.yml", &app_spec(&[("payload.txt", "/opt/verdict")], &[])),
        ("payload.txt", "payload"),
    ])
}

/// The smallest archive a client can upload that holds no AppSpec file at all: an empty zip,
/// which is an archive this emulator unpacks to nothing.
fn empty_archive() -> Vec<u8> {
    let mut object = b"PK\x05\x06".to_vec();
    object.extend_from_slice(&[0_u8; 18]);
    object
}

/// What a row uploads at the key its revision names.
enum Object {
    /// Nothing at all: the bucket is there and holds nothing at that key.
    Absent,
    /// Bytes that are no archive this emulator unpacks.
    NotAnArchive,
    /// An archive that unpacks to no AppSpec file.
    WithoutAnAppSpec,
    /// An archive whose root holds an AppSpec file but under a directory, so the bundle's own
    /// root holds none.
    AppSpecUnderADirectory,
    /// A bundle this emulator reads.
    Readable,
}

impl Object {
    /// The bytes to upload, or `None` for a row that uploads nothing.
    fn bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Absent => None,
            Self::NotAnArchive => Some(b"not-a-zip".to_vec()),
            Self::WithoutAnAppSpec => Some(empty_archive()),
            Self::AppSpecUnderADirectory => Some(bundle(&[(
                "revision/appspec.yml",
                &app_spec(&[("payload.txt", "/opt/verdict")], &[]),
            )])),
            Self::Readable => Some(readable_bundle()),
        }
    }
}

/// One row of the table: what the group selects, what the bucket serves, and the outcome the
/// deployment reaches.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// The name the row's resources are made under.
    name: &'static str,
    /// Whether a running Amazon EC2 instance the group's filter selects is launched for it:
    /// the host whose agent reads the bundle.
    host: bool,
    /// The selector the deployment group carries, as the member a client sets.
    selector: fn(&str) -> Value,
    /// What the bucket serves at the key the revision names.
    object: Object,
    /// The terminal status the deployment reaches.
    status: &'static str,
    /// The `errorInformation` it carries: the code and its own fixed sentence, or `None` for a
    /// deployment that reports none.
    error: Option<(&'static str, &'static str)>,
}

/// An `ec2TagFilters` selector on the tag `value` names.
fn ec2_filter(value: &str) -> Value {
    json!({"ec2TagFilters": [{"Key": "Name", "Value": value, "Type": "KEY_AND_VALUE"}]})
}

/// An `onPremisesInstanceTagFilters` selector on the same tag.
fn on_premises_filter(value: &str) -> Value {
    json!({
        "onPremisesInstanceTagFilters": [
            {"Key": "Fleet", "Value": value, "Type": "KEY_AND_VALUE"},
        ],
    })
}

/// RULE: see the module docs — the bundle's verdict follows a host that read it, so a group
/// that resolved none is answered for its fleet.
#[tokio::test]
async fn a_deployment_with_no_host_is_answered_for_its_fleet_and_not_for_its_bundle() {
    // Rows differ along what the rule generalises over: whether the group resolved a host the
    // agent goes round, which selector it resolved it through, and what the bucket serves at
    // the key the revision names — nothing at all, bytes that are no archive, an archive with
    // no AppSpec in it, an AppSpec that is not in the bundle's root, and a bundle this
    // emulator reads.
    let rows: &[Row] = &[
        Row {
            case: "no host, and an object that is no archive",
            name: "no-host-not-an-archive",
            host: false,
            selector: ec2_filter,
            object: Object::NotAnArchive,
            status: "Failed",
            error: Some(("NO_INSTANCES", NO_INSTANCES_MESSAGE)),
        },
        Row {
            case: "no host, and an archive holding no AppSpec",
            name: "no-host-no-app-spec",
            host: false,
            selector: ec2_filter,
            object: Object::WithoutAnAppSpec,
            status: "Failed",
            error: Some(("NO_INSTANCES", NO_INSTANCES_MESSAGE)),
        },
        Row {
            case: "no host, and an AppSpec under a directory of the bundle",
            name: "no-host-nested-app-spec",
            host: false,
            selector: ec2_filter,
            object: Object::AppSpecUnderADirectory,
            status: "Failed",
            error: Some(("NO_INSTANCES", NO_INSTANCES_MESSAGE)),
        },
        Row {
            case: "no host, and nothing at the key the revision names",
            name: "no-host-absent-object",
            host: false,
            selector: ec2_filter,
            object: Object::Absent,
            status: "Failed",
            error: Some(("NO_INSTANCES", NO_INSTANCES_MESSAGE)),
        },
        Row {
            case: "no host, and a bundle this emulator reads",
            name: "no-host-readable-bundle",
            host: false,
            selector: ec2_filter,
            object: Object::Readable,
            status: "Failed",
            error: Some(("NO_INSTANCES", NO_INSTANCES_MESSAGE)),
        },
        Row {
            case: "an on-premises selector nothing registered matches, and no archive",
            name: "no-host-on-premises",
            host: false,
            selector: on_premises_filter,
            object: Object::NotAnArchive,
            status: "Failed",
            error: Some(("NO_INSTANCES", NO_INSTANCES_MESSAGE)),
        },
        // And once a host the agent goes round IS there, the bundle gets its own verdict:
        // this is the only way `INVALID_REVISION` is reached.
        Row {
            case: "a host, and an object that is no archive",
            name: "host-not-an-archive",
            host: true,
            selector: ec2_filter,
            object: Object::NotAnArchive,
            status: "Failed",
            error: Some(("INVALID_REVISION", UNLOCATABLE_REVISION_MESSAGE)),
        },
        Row {
            case: "a host, and nothing at the key the revision names",
            name: "host-absent-object",
            host: true,
            selector: ec2_filter,
            object: Object::Absent,
            status: "Failed",
            error: Some(("INVALID_REVISION", UNLOCATABLE_REVISION_MESSAGE)),
        },
        Row {
            case: "a host, and a bundle this emulator reads",
            name: "host-readable-bundle",
            host: true,
            selector: ec2_filter,
            object: Object::Readable,
            status: "Succeeded",
            error: None,
        },
    ];

    for row in rows {
        let case = row.case;
        // One emulator per row: the fleet, the bucket and the deployment history are all
        // account-wide, and each row is about the ones IT made.
        let fleet = Fleet::new();
        let key = format!("{}.zip", row.name);
        if let Some(object) = row.object.bytes() {
            fleet.upload(case, &key, &object).await;
        } else {
            // The bucket still has to be one this emulator HOLDS — a bucket it holds no
            // record of is no evidence about the object — so it is created with another key
            // in it and the revision names one it holds nothing at.
            fleet
                .upload(case, &format!("{}-other.zip", row.name), &readable_bundle())
                .await;
        }
        if row.host {
            let host = fleet.launch(case, &[("Name", row.name)], None).await;
            fleet.wait_until_running(case, &host).await;
        }
        let (application, group) = fleet
            .group(case, row.name, (row.selector)(row.name))
            .await;
        let deployment = fleet
            .deploy(case, &application, &group, &key, json!({}))
            .await;

        let info = fleet.terminal(case, &deployment).await;
        assert_eq!(
            info["status"].as_str(),
            Some(row.status),
            "case {case:?}: expected the deployment to end {}, got {info:?}",
            row.status
        );
        match row.error {
            Some((code, message)) => {
                assert_eq!(
                    (
                        info["errorInformation"]["code"].as_str(),
                        info["errorInformation"]["message"].as_str()
                    ),
                    (Some(code), Some(message)),
                    "case {case:?}: expected the {code} failure and its own sentence, got {:?}",
                    info["errorInformation"]
                );
            }
            None => assert!(
                info["errorInformation"].is_null(),
                "case {case:?}: expected no error information, got {:?}",
                info["errorInformation"]
            ),
        }
    }
}

/// Register `instance` and tag it `Fleet=value`, so a group selecting that tag resolves it.
async fn given_on_premises_instance(fleet: &Fleet, case: &str, instance: &str, value: &str) {
    fleet
        .ok(
            case,
            "RegisterOnPremisesInstance",
            json!({
                "instanceName": instance,
                "iamUserArn": format!(
                    "arn:aws:iam::{}:user/{instance}",
                    ::engine::emulator_account_id()
                ),
            }),
        )
        .await;
    fleet
        .ok(
            case,
            "AddTagsToOnPremisesInstances",
            json!({
                "instanceNames": [instance],
                "tags": [{"Key": "Fleet", "Value": value, "Type": "KEY_AND_VALUE"}],
            }),
        )
        .await;
}

/// The first status the target `instance` reports that is not `Pending`, which is what a
/// client polling a dispatched target reads.
///
/// An operation over the targets of a deployment CodeDeploy has not dispatched yet is refused
/// with `DeploymentNotStartedException`, so the dispatch is waited out through `GetDeployment`
/// first — exactly as a client waits it out.
async fn first_dispatched_status(
    fleet: &Fleet,
    case: &str,
    deployment: &str,
    instance: &str,
) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let info = fleet
            .ok(case, "GetDeployment", json!({"deploymentId": deployment}))
            .await["deploymentInfo"]
            .clone();
        if info["status"].as_str() != Some("Created") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployment to be dispatched, last read {info:?}"
        );
        tokio::time::sleep(POLL).await;
    }
    loop {
        let listed = fleet
            .ok(
                case,
                "ListDeploymentTargets",
                json!({"deploymentId": deployment}),
            )
            .await;
        let enumerated = listed["targetIds"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|id| id.as_str() == Some(instance));
        if enumerated {
            let target = fleet.instance_target(case, deployment, instance).await;
            if let Some(status) = target["status"].as_str() {
                if status != "Pending" {
                    return status.to_string();
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {instance} to be dispatched to, last read {listed:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// RULE: see the module docs — a registered on-premises target the deployment has been
/// dispatched to, and that no agent has reported for, is `InProgress`.
#[tokio::test]
async fn a_dispatched_on_premises_target_no_agent_reported_for_is_in_progress() {
    // Rows differ along what the bucket serves at the key the revision names: the same
    // catalogue as the table above, since what a target reports cannot follow from a bundle
    // no agent read either.
    let rows: &[(&str, &str, Object)] = &[
        ("an object that is no archive", "op-not-an-archive", Object::NotAnArchive),
        ("an archive holding no AppSpec", "op-no-app-spec", Object::WithoutAnAppSpec),
        (
            "an AppSpec under a directory of the bundle",
            "op-nested-app-spec",
            Object::AppSpecUnderADirectory,
        ),
        ("nothing at the key the revision names", "op-absent-object", Object::Absent),
        ("a bundle this emulator reads", "op-readable-bundle", Object::Readable),
    ];

    for (case, name, object) in rows {
        // One emulator per row: the on-premises registry is account-wide.
        let fleet = Fleet::new();
        let instance = format!("host-{name}");
        let key = format!("{name}.zip");
        match object.bytes() {
            Some(bytes) => fleet.upload(case, &key, &bytes).await,
            None => {
                fleet
                    .upload(case, &format!("{name}-other.zip"), &readable_bundle())
                    .await;
            }
        }
        given_on_premises_instance(&fleet, case, &instance, name).await;
        let (application, group) = fleet.group(case, name, on_premises_filter(name)).await;
        let deployment = fleet
            .deploy(case, &application, &group, &key, json!({}))
            .await;

        let status = first_dispatched_status(&fleet, case, &deployment, &instance).await;
        assert_eq!(
            status, "InProgress",
            "case {case:?}: expected the dispatched target to be InProgress before any agent \
             reported, got {status:?}"
        );
    }
}
