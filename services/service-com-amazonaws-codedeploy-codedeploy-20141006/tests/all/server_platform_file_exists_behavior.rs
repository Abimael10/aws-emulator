// SPDX-License-Identifier: CC0-1.0

//! What `fileExistsBehavior` decides on an EC2/on-premises (`Server`) host, driven and read
//! the whole way a client drives and reads it: a launched Amazon EC2 host whose own program
//! installed the CodeDeploy agent, the readiness check a client makes on that host through
//! Systems Manager Run Command, and then the deployments whose Install meets a file that is
//! already at a destination their revision declares.
//!
//! RULE under test, in two halves that make one path:
//!
//! 1. a shell command sent to a RUNNING managed node settles on a terminal
//!    `CommandInvocationStatus` (`smithy_query(query="shape",shape="CommandInvocationStatus")`
//!    of Systems Manager: Pending | InProgress | Delayed | Success | Cancelled | TimedOut |
//!    Failed | Cancelling, of which the first three are transient) carrying what the command
//!    wrote and the status it ended on — so `systemctl is-active codedeploy-agent` is readable
//!    through `SendCommand` + `GetCommandInvocation`, which is the only way a client with no
//!    shell on the host can prove the host is usable, and the only way it can then see what a
//!    deployment left there;
//! 2. the Install of a `Server` deployment treats a destination the host HOLDS and the
//!    group's previous successful deployment did NOT put there as a file of the host's own,
//!    and `fileExistsBehavior` (`smithy_query(query="shape",shape="FileExistsBehavior")`:
//!    DISALLOW | OVERWRITE | RETAIN, DISALLOW being what a deployment stating none runs under)
//!    decides what happens to it: DISALLOW fails the deployment with those bytes left where
//!    they were, OVERWRITE replaces them with the revision's, RETAIN keeps them as part of
//!    the new deployment. A destination that very group's last successful deployment DID
//!    install is the group's own file rather than a foreign one, so the next revision replaces
//!    it whatever behavior the deployment states.
//!
//! The rows of the first case differ along what the rule generalises over: the behavior the
//! deployment states (none, and each of the three the enum lists) and WHO put the bytes at the
//! destination (the host itself, out of band, or this group's own last successful deployment).
//! The second case walks the sequence a client walks over ONE growing revision manifest —
//! three revisions, a conflicting path removed between two deployments of the same revision —
//! because membership of the previous successful deployment's manifest is what decides whether
//! a path is foreign at all, and that is a fact of a SEQUENCE rather than of one deployment.
//!
//! The six services this needs on ONE engine — CodeDeploy, Amazon EC2 (the host), Amazon S3
//! (the revisions), Systems Manager (the reader of the host), IAM (the instance profile) and
//! CloudWatch — are mounted by the shared harness of
//! `server_platform_rollback_read_through_run_command.rs` ([`Estate`]), which also holds the
//! waits and the Run Command reader; the AppSpec and bundle builders are
//! `server_platform_instance_deployments.rs`'s. Nothing is written twice.
//!
//! Nothing here waits a fixed time: every span is a stored due instant — the launch reaching
//! `running`, the node registering, the invocation finishing, each deployment reaching its
//! outcome — so each step LOOKS AGAIN until the condition holds or the shared deadline passes,
//! and nothing asserts on elapsed time.

use serde_json::{json, Value};

use crate::server_platform_instance_deployments::{app_spec, bundle, element, event_named};
use crate::server_platform_rollback_read_through_run_command::{
    run_command, trust_policy, when_managed, when_running, when_terminal, Estate, AMI,
    EC2_API_VERSION, EC2_PRINCIPAL, IAM_API_VERSION, SSM_CORE_POLICY,
};

/// The instance type the host is launched as.
const INSTANCE_TYPE: &str = "t3.micro";

/// The bucket every revision below is uploaded to.
const BUCKET: &str = "codedeploy-file-exists-revisions";

/// The instance profile the host runs as, and the role it carries: a managed node registers as
/// that role, which is why a command can be delivered to the agent in its guest at all.
const HOST_PROFILE: &str = "file-exists-host-profile";
/// See [`HOST_PROFILE`] — deliberately not the profile's own name.
const HOST_ROLE: &str = "file-exists-host-role";

/// The service role every deployment group below is created with; nothing of these cases
/// resolves it.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The agent whose unit the readiness check in front of every EC2/on-premises deployment asks
/// about, and the unit name the init system knows it by.
const CODEDEPLOY_AGENT: &str = "codedeploy-agent";

/// The program the host's guest runs when it comes up: the documented installation of the
/// CodeDeploy agent, which is what makes the readiness check below answer `active`.
const USER_DATA: &str = "#!/bin/bash\ndnf install -y codedeploy-agent\n\
                         systemctl enable --now codedeploy-agent\n";

/// The tag the host carries and every deployment group below selects on.
const FLEET_TAG: (&str, &str) = ("Name", "file-exists-host");

/// The bytes the HOST puts at a destination out of band, which no deployment of the group ever
/// installed: the file `fileExistsBehavior` is about.
const FOREIGN: &str = "bytes-the-host-put-there";

/// A second such file, so a later revision can meet one of its own.
const OTHER_FOREIGN: &str = "more-bytes-the-host-put-there";

/// `Diagnostics$message` of the Install that found a file it may not replace, as the agent
/// words it, with the destination it would have written.
fn file_exists_sentence(destination: &str) -> String {
    format!(
        "The deployment failed because a specified file already exists at this location: \
         {destination}"
    )
}

/// The `errorInformation.code` a deployment that failed on its hosts carries
/// (`smithy_query(query="shape",shape="ErrorCode")` lists it among the deployment error
/// codes).
const HEALTH_CONSTRAINTS: &str = "HEALTH_CONSTRAINTS";

/// A revision whose AppSpec `files` section copies one bundle entry per name in `names` into
/// `destination`, each carrying `marker` and its own name, so what a host holds says WHICH
/// revision put it there.
fn revision(destination: &str, names: &[&str], marker: &str) -> Vec<u8> {
    let sources: Vec<String> = names.iter().map(|name| format!("src/{name}")).collect();
    let declared: Vec<(&str, &str)> = sources
        .iter()
        .map(|source| (source.as_str(), destination))
        .collect();
    let document = app_spec(&declared, &[]);
    let contents: Vec<String> = names
        .iter()
        .map(|name| format!("{marker}-{name}"))
        .collect();
    let mut entries: Vec<(&str, &str)> = vec![("appspec.yml", document.as_str())];
    for (source, payload) in sources.iter().zip(contents.iter()) {
        entries.push((source.as_str(), payload.as_str()));
    }
    bundle(&entries)
}

/// What the revision `marker` installs at `name` under its destination.
fn payload_of(marker: &str, name: &str) -> String {
    format!("{marker}-{name}")
}

/// Create the instance profile the host runs as, launch the host, wait for it, and prove
/// through Run Command that the CodeDeploy agent's unit is active on it — half one of the
/// rule, and the gate a client passes before it deploys anything to a `Server` group.
async fn ready_host(estate: &Estate, case: &str) -> String {
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
    // the program is stated once and encoded for the call.
    let user_data = ::aws_smithy_types::base64::encode(USER_DATA);
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
    let instance = element(&launched, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an id: {launched}"));
    when_running(estate, case, &instance).await;
    when_managed(estate, case, &instance).await;

    let readiness = run_command(
        estate,
        case,
        &instance,
        &format!("systemctl is-active {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        readiness["Status"].as_str(),
        Some("Success"),
        "case {case:?}: the readiness command must leave the transient statuses for a terminal \
         Success, got {readiness:?}"
    );
    assert_eq!(
        readiness["ResponseCode"].as_i64(),
        Some(0),
        "case {case:?}: expected the status the init system answers for a running unit, got \
         {readiness:?}"
    );
    assert_eq!(
        readiness["StandardOutputContent"]
            .as_str()
            .unwrap_or_default()
            .trim(),
        "active",
        "case {case:?}: expected the init system to report the agent's unit active, got \
         {readiness:?}"
    );
    instance
}

/// What `path` reads on the host, through the agent in its guest — `None` where the host holds
/// no such file, which is a `cat` that ended on anything but a success.
async fn host_file(estate: &Estate, case: &str, instance: &str, path: &str) -> Option<String> {
    let invocation = run_command(estate, case, instance, &format!("cat {path}")).await;
    if invocation["Status"].as_str() != Some("Success") {
        return None;
    }
    Some(
        invocation["StandardOutputContent"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
    )
}

/// Put `contents` at `path` on the host OUT OF BAND — the host's own shell, through Run
/// Command, with no deployment involved — and prove the host holds them.
///
/// One invocation reads the file back itself, so what the host holds is proven by the same
/// command that wrote it rather than by a second one.
async fn write_on_host(estate: &Estate, case: &str, instance: &str, path: &str, contents: &str) {
    let directory = path.rsplit_once('/').map_or("/", |(parent, _)| parent);
    let command = format!("mkdir -p {directory} && echo {contents} > {path} && cat {path}");
    let invocation = run_command(estate, case, instance, &command).await;
    assert_eq!(
        invocation["Status"].as_str(),
        Some("Success"),
        "case {case:?}: {command:?} must succeed on the host, got {invocation:?}"
    );
    assert_eq!(
        invocation["StandardOutputContent"]
            .as_str()
            .unwrap_or_default()
            .trim(),
        contents,
        "case {case:?}: the host must hold the bytes it was given at {path}, got {invocation:?}"
    );
}

/// Take `path` off the host out of band: a `rm` that succeeded is a file that was there and is
/// gone.
async fn remove_on_host(estate: &Estate, case: &str, instance: &str, path: &str) {
    let invocation = run_command(estate, case, instance, &format!("rm {path}")).await;
    assert_eq!(
        invocation["Status"].as_str(),
        Some("Success"),
        "case {case:?}: removing {path} must succeed on the host, got {invocation:?}"
    );
}

/// Create the application and one deployment group selecting the host by its tag.
async fn application_and_group(estate: &Estate, case: &str, application: &str, group: &str) {
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
                "ec2TagFilters": [
                    {"Key": FLEET_TAG.0, "Value": FLEET_TAG.1, "Type": "KEY_AND_VALUE"},
                ],
                "deploymentStyle": {
                    "deploymentType": "IN_PLACE",
                    "deploymentOption": "WITHOUT_TRAFFIC_CONTROL",
                },
            }),
        )
        .await;
}

/// Deploy the revision at `key` to `group` under `behavior` — stating none where it is `None`,
/// which is the deployment that runs under the default — and answer `(deploymentId, info)`
/// once it has reached an outcome.
async fn deploy(
    estate: &Estate,
    case: &str,
    application: &str,
    group: &str,
    key: &str,
    behavior: Option<&str>,
) -> (String, Value) {
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "revision": {
            "revisionType": "S3",
            "s3Location": {"bucket": BUCKET, "key": key, "bundleType": "zip"},
        },
    });
    if let Some(behavior) = behavior {
        request["fileExistsBehavior"] = json!(behavior);
    }
    let deployment = estate
        .codedeploy(case, "CreateDeployment", request)
        .await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let info = when_terminal(estate, case, &deployment).await;
    (deployment, info)
}

/// The Install event of `deployment` on `instance`, as the deployment's own target reports it.
async fn install_event(estate: &Estate, case: &str, deployment: &str, instance: &str) -> Value {
    let target = estate
        .codedeploy(
            case,
            "GetDeploymentTarget",
            json!({"deploymentId": deployment, "targetId": instance}),
        )
        .await["deploymentTarget"]["instanceTarget"]
        .clone();
    event_named(&target, "Install")
        .unwrap_or_else(|| panic!("case {case:?}: the Install event is reported: {target:?}"))
}

/// Who put the bytes the graded deployment's Install meets at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldBy {
    /// The host itself, out of band: a file no deployment of the group installed.
    TheHostItself,
    /// This group's own last successful deployment, which installed that very path.
    ThisGroupsLastDeployment,
}

/// What the destination must read once the graded deployment has reached its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// The bytes that were there before it ran.
    WhatWasThere,
    /// The graded revision's own bytes.
    TheRevisions,
}

/// RULE: see the module docs — the readiness of a `Server` host is readable through Run
/// Command, and what the Install of a deployment does with a destination already held is what
/// `fileExistsBehavior` says, for a file the host put there and not for one this group's own
/// last successful deployment installed.
#[tokio::test]
async fn file_exists_behavior_decides_a_destination_the_host_already_holds() {
    /// One row: who put the bytes at the destination, the behavior the graded deployment
    /// states, and what must then be true of the deployment, its Install and the host.
    struct Row {
        case: &'static str,
        held_by: HeldBy,
        behavior: Option<&'static str>,
        status: &'static str,
        error_code: Option<&'static str>,
        install: &'static str,
        held: Held,
    }
    // The four behaviors a deployment can run under — the three the enum lists plus the one a
    // deployment that states none runs under — against a file the HOST put there, and then the
    // same question for a path this group's own last deployment installed.
    let rows: &[Row] = &[
        Row {
            case: "a deployment stating no behavior at all, over a file the host put there",
            held_by: HeldBy::TheHostItself,
            behavior: None,
            status: "Failed",
            error_code: Some(HEALTH_CONSTRAINTS),
            install: "Failed",
            held: Held::WhatWasThere,
        },
        Row {
            case: "DISALLOW over a file the host put there",
            held_by: HeldBy::TheHostItself,
            behavior: Some("DISALLOW"),
            status: "Failed",
            error_code: Some(HEALTH_CONSTRAINTS),
            install: "Failed",
            held: Held::WhatWasThere,
        },
        Row {
            case: "OVERWRITE over a file the host put there",
            held_by: HeldBy::TheHostItself,
            behavior: Some("OVERWRITE"),
            status: "Succeeded",
            error_code: None,
            install: "Succeeded",
            held: Held::TheRevisions,
        },
        Row {
            case: "RETAIN over a file the host put there",
            held_by: HeldBy::TheHostItself,
            behavior: Some("RETAIN"),
            status: "Succeeded",
            error_code: None,
            install: "Succeeded",
            held: Held::WhatWasThere,
        },
        Row {
            case: "DISALLOW over a path this group's last successful deployment installed",
            held_by: HeldBy::ThisGroupsLastDeployment,
            behavior: Some("DISALLOW"),
            status: "Succeeded",
            error_code: None,
            install: "Succeeded",
            held: Held::TheRevisions,
        },
        Row {
            case: "no behavior at all over a path this group's last successful deployment \
                   installed",
            held_by: HeldBy::ThisGroupsLastDeployment,
            behavior: None,
            status: "Succeeded",
            error_code: None,
            install: "Succeeded",
            held: Held::TheRevisions,
        },
    ];

    let estate = Estate::new();
    let case = "the host every row of fileExistsBehavior deploys to";
    let host = ready_host(&estate, case).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // Each row gets its own destination directory and its own deployment group, so what a
        // row's group installed is this row's history and nothing else's.
        let destination = format!("/opt/file-exists/{index}");
        let conflict = format!("{destination}/extra.txt");
        let application = format!("file-exists-{index}");
        let group = "fleet";
        application_and_group(&estate, case, &application, group).await;

        // The group's FIRST deployment: where the host is to hold the conflicting path of its
        // own, the base revision declares only the other file; where this group is to have
        // installed that path itself, the base revision declares both.
        let base_names: &[&str] = match row.held_by {
            HeldBy::TheHostItself => &["app.txt"],
            HeldBy::ThisGroupsLastDeployment => &["app.txt", "extra.txt"],
        };
        let base_key = format!("base-{index}.zip");
        estate
            .upload(
                case,
                BUCKET,
                &base_key,
                &revision(&destination, base_names, "base"),
            )
            .await;
        let (_, info) = deploy(&estate, case, &application, group, &base_key, None).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the group's first deployment must succeed, got {info:?}"
        );

        // What is at the conflicting path when the graded deployment runs, and who put it
        // there — proven on the host either way, by the write itself or by a read of what the
        // group's own first deployment installed.
        let before = match row.held_by {
            HeldBy::TheHostItself => {
                write_on_host(&estate, case, &host, &conflict, FOREIGN).await;
                FOREIGN.to_string()
            }
            HeldBy::ThisGroupsLastDeployment => {
                let installed = payload_of("base", "extra.txt");
                let read = host_file(&estate, case, &host, &conflict).await;
                assert_eq!(
                    read.as_deref(),
                    Some(installed.as_str()),
                    "case {case:?}: the group's own first deployment must have installed \
                     {conflict}, got {read:?}"
                );
                installed
            }
        };

        // The graded deployment: the same two destinations, another revision's bytes.
        let graded_key = format!("graded-{index}.zip");
        estate
            .upload(
                case,
                BUCKET,
                &graded_key,
                &revision(&destination, &["app.txt", "extra.txt"], "graded"),
            )
            .await;
        let (deployment, info) =
            deploy(&estate, case, &application, group, &graded_key, row.behavior).await;
        assert_eq!(
            info["status"].as_str(),
            Some(row.status),
            "case {case:?}: expected the deployment to end {}, got {info:?}",
            row.status
        );
        let code = info["errorInformation"]["code"].as_str();
        assert_eq!(
            code, row.error_code,
            "case {case:?}: expected the error code {:?}, got {code:?} in {info:?}",
            row.error_code
        );
        let install = install_event(&estate, case, &deployment, &host).await;
        assert_eq!(
            install["status"].as_str(),
            Some(row.install),
            "case {case:?}: expected Install to be {}, got {install:?}",
            row.install
        );
        if row.install == "Failed" {
            assert_eq!(
                install["diagnostics"]["message"].as_str(),
                Some(file_exists_sentence(&conflict).as_str()),
                "case {case:?}: expected the agent's own sentence naming the destination, got \
                 {install:?}"
            );
        }
        let expected = match row.held {
            Held::WhatWasThere => before.clone(),
            Held::TheRevisions => payload_of("graded", "extra.txt"),
        };
        let read = host_file(&estate, case, &host, &conflict).await;
        assert_eq!(
            read.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the host to hold {expected:?} at {conflict} afterwards, \
             got {read:?}"
        );
    }
}

/// RULE: see the module docs — whether a destination is foreign is membership of the group's
/// previous successful deployment's manifest, so the same revision that DISALLOW refused
/// succeeds once the file in its way is gone, the path it then installed is the group's own
/// from that point on, and a later revision that declares a path the host holds of its own
/// replaces it under OVERWRITE.
#[tokio::test]
async fn a_growing_manifest_adopts_the_paths_its_successful_deployments_installed() {
    let estate = Estate::new();
    let case = "three revisions over one growing manifest";
    let host = ready_host(&estate, case).await;

    let destination = "/opt/file-exists/sequence";
    let app_path = format!("{destination}/app.txt");
    let disallow_path = format!("{destination}/extra_disallow.txt");
    let overwrite_path = format!("{destination}/extra_overwrite.txt");
    let application = "file-exists-sequence";
    let group = "fleet";
    application_and_group(&estate, case, application, group).await;

    // Three revisions over one growing manifest: {app.txt}, {app.txt, extra_disallow.txt} and
    // {app.txt, extra_disallow.txt, extra_overwrite.txt}.
    for (key, names, marker) in [
        ("revision-a.zip", &["app.txt"][..], "a"),
        ("revision-b.zip", &["app.txt", "extra_disallow.txt"][..], "b"),
        (
            "revision-c.zip",
            &["app.txt", "extra_disallow.txt", "extra_overwrite.txt"][..],
            "c",
        ),
    ] {
        estate
            .upload(case, BUCKET, key, &revision(destination, names, marker))
            .await;
    }

    // Beat one: a revision with nothing in its way succeeds, stating no behavior at all.
    let (_, info) = deploy(&estate, case, application, group, "revision-a.zip", None).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the first revision has nothing in its way, got {info:?}"
    );
    let read = host_file(&estate, case, &host, &app_path).await;
    assert_eq!(
        read.as_deref(),
        Some(payload_of("a", "app.txt").as_str()),
        "case {case:?}: the first revision's own bytes must be on the host, got {read:?}"
    );

    // Beat two: the host puts bytes of its own at the path the NEXT revision adds, and that
    // revision under DISALLOW fails with those bytes left where they were.
    write_on_host(&estate, case, &host, &disallow_path, FOREIGN).await;
    let (deployment, info) = deploy(
        &estate,
        case,
        application,
        group,
        "revision-b.zip",
        Some("DISALLOW"),
    )
    .await;
    assert_eq!(
        info["status"].as_str(),
        Some("Failed"),
        "case {case:?}: DISALLOW must fail the deployment, got {info:?}"
    );
    assert_eq!(
        info["errorInformation"]["code"].as_str(),
        Some(HEALTH_CONSTRAINTS),
        "case {case:?}: expected the error code of a deployment that failed on its hosts, got \
         {info:?}"
    );
    let install = install_event(&estate, case, &deployment, &host).await;
    assert_eq!(
        install["diagnostics"]["message"].as_str(),
        Some(file_exists_sentence(&disallow_path).as_str()),
        "case {case:?}: expected the agent's own sentence naming the destination, got \
         {install:?}"
    );
    let read = host_file(&estate, case, &host, &disallow_path).await;
    assert_eq!(
        read.as_deref(),
        Some(FOREIGN),
        "case {case:?}: the bytes DISALLOW refused to replace must still be on the host, got \
         {read:?}"
    );

    // Beat three: with that file taken off the host, the SAME revision under the SAME behavior
    // succeeds and installs its own bytes there.
    remove_on_host(&estate, case, &host, &disallow_path).await;
    let (_, info) = deploy(
        &estate,
        case,
        application,
        group,
        "revision-b.zip",
        Some("DISALLOW"),
    )
    .await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: nothing is in the revision's way any more, got {info:?}"
    );
    for (path, name) in [(&app_path, "app.txt"), (&disallow_path, "extra_disallow.txt")] {
        let read = host_file(&estate, case, &host, path).await;
        assert_eq!(
            read.as_deref(),
            Some(payload_of("b", name).as_str()),
            "case {case:?}: expected that revision's own bytes at {path}, got {read:?}"
        );
    }

    // Beat four: the third revision adds one more path, which the host holds bytes of its own
    // at, and under OVERWRITE it replaces them — while the two paths the group's last
    // successful deployment installed are the group's own and are replaced as well.
    write_on_host(&estate, case, &host, &overwrite_path, OTHER_FOREIGN).await;
    let (_, info) = deploy(
        &estate,
        case,
        application,
        group,
        "revision-c.zip",
        Some("OVERWRITE"),
    )
    .await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: OVERWRITE replaces what is in the way, got {info:?}"
    );
    for (path, name) in [
        (&app_path, "app.txt"),
        (&disallow_path, "extra_disallow.txt"),
        (&overwrite_path, "extra_overwrite.txt"),
    ] {
        let read = host_file(&estate, case, &host, path).await;
        assert_eq!(
            read.as_deref(),
            Some(payload_of("c", name).as_str()),
            "case {case:?}: expected the third revision's own bytes at {path}, got {read:?}"
        );
    }
}
