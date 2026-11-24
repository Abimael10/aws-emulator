// SPDX-License-Identifier: CC0-1.0

//! What an EC2/on-premises (`Server`) deployment's AppSpec DOES to the host, read the only way
//! a client with no shell on that host can read it: Systems Manager Run Command.
//!
//! RULE under test, in two halves that make one path:
//!
//! 1. a shell command sent to a RUNNING managed node settles on a terminal
//!    `CommandInvocationStatus` carrying what the command wrote and the status it ended on, so
//!    the init system's answer about the CodeDeploy agent's unit is readable through
//!    `SendCommand` + `GetCommandInvocation` — the gate a client puts in front of every
//!    `Server` deployment, because it is the only proof it has that the host is usable;
//! 2. such a deployment PERFORMS its revision's AppSpec on each selected host — every `files`
//!    entry is copied to its destination, and every script the `hooks` section declares for a
//!    lifecycle event runs there, after the install and with that event in its environment —
//!    and the instance target it reports carries one `Succeeded` lifecycle event per performed
//!    event, `Install` and each declared hook's event among them.
//!
//! Half two is asserted where it is OBSERVABLE rather than only in CodeDeploy's own record:
//! every installed destination is `cat`-ed off the host through Run Command and must hold the
//! revision's own content, and each hook writes a line naming the lifecycle event it ran for,
//! this row's token and the content it READ BACK from an installed destination — so a hook that
//! never ran, or one that ran before the files were there, cannot produce the line the row
//! expects. The reported status and the state of the machine are therefore checked against each
//! other, which is what "the target reports `Install` and `AfterInstall` succeeded" has to mean.
//!
//! The rows differ along what the rule generalises over: HOW MANY `files` entries the revision
//! declares and whether a `source` names one file or a DIRECTORY of them (the AWS CodeDeploy
//! User Guide's AppSpec `files` section: "`source` … identifies a file or directory in the
//! application revision"), WHICH lifecycle event the `hooks` section declares a script for, HOW
//! MANY scripts one event declares, and whether an entry names a user to impersonate (`runas`).
//!
//! Six services are mounted on ONE engine for it, by the harness of
//! `server_platform_rollback_read_through_run_command.rs` ([`Estate`], which also holds the
//! waits and the Run Command reader): CodeDeploy, Amazon EC2 (the host), Amazon S3 (the
//! revision), Systems Manager (the reader of the host), IAM (a node registers only as a role
//! its instance profile carries) and CloudWatch. The zip bundle is packaged by
//! `server_platform_instance_deployments.rs`'s [`bundle`]; the AppSpec document is written here
//! because that suite's builder writes no `runas`, which these rows vary.
//!
//! Nothing here waits a fixed time: every span is a stored due instant — the launch reaching
//! `running`, the node registering, the invocation finishing, the deployment reaching its
//! outcome — so each step LOOKS AGAIN until the condition holds or the shared deadline passes,
//! and nothing asserts on elapsed time.

use serde_json::json;

use crate::server_platform_instance_deployments::{bundle, element, events_of};
use crate::server_platform_rollback_read_through_run_command::{
    run_command, trust_policy, when_managed, when_running, when_terminal, Estate, AMI,
    EC2_API_VERSION, EC2_PRINCIPAL, IAM_API_VERSION, SSM_CORE_POLICY,
};

/// The instance type the one host is launched as.
const INSTANCE_TYPE: &str = "t3.micro";

/// The bucket every revision below is uploaded to.
const BUCKET: &str = "codedeploy-appspec-install-revisions";

/// The instance profile the host runs as, and the role it carries: a managed node registers as
/// that role, which is why a command can be delivered to the agent in its guest at all.
const HOST_PROFILE: &str = "appspec-install-host-profile";
/// See [`HOST_PROFILE`] — deliberately not the profile's own name.
const HOST_ROLE: &str = "appspec-install-host-role";

/// The service role every deployment group here carries; nothing of these rows resolves it.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The agent whose unit the gate in front of every EC2/on-premises deployment asks about.
const CODEDEPLOY_AGENT: &str = "codedeploy-agent";

/// The program the host's guest runs when it comes up: the documented installation of the
/// CodeDeploy agent, which is what makes the readiness check below answer `active`.
const USER_DATA: &str = "#!/bin/bash\ndnf install -y codedeploy-agent\n\
                         systemctl enable --now codedeploy-agent\n";

/// The tag the host carries and every deployment group below selects on.
const FLEET_TAG: (&str, &str) = ("Name", "appspec-install-host");

/// The utility a read off the host runs: the file is `cat`-ed and what it wrote is the
/// invocation's standard output.
const READ_COMMAND: &str = "cat";

/// The lifecycle event at which the agent copies the revision's files to their destinations,
/// which every row's target must report `Succeeded` because every row declares `files`.
const INSTALL_EVENT: &str = "Install";

/// The `LifecycleEventStatus` a performed event ends on
/// (`smithy_query(query="shape",shape="LifecycleEventStatus")`: Pending | InProgress | Succeeded
/// | Failed | Skipped | Unknown).
const SUCCEEDED: &str = "Succeeded";

/// One hook a row's revision declares: the lifecycle event it runs for, and the user the entry
/// names for it where it names one ("`runas` … the user to impersonate when running the
/// script").
struct Hook {
    event: &'static str,
    runas: Option<&'static str>,
}

/// One row: the revision's `files` section, the bundle entries its sources resolve in, what the
/// host must hold once the deployment has run, and the hooks the revision declares.
struct Row {
    case: &'static str,
    /// The `files` entries, as `(source inside the bundle, destination directory)`.
    files: &'static [(&'static str, &'static str)],
    /// The payload entries of the bundle, as `(path inside the bundle, contents)`.
    payloads: &'static [(&'static str, &'static str)],
    /// What the host must hold afterwards, as `(path on the host, contents)`; the FIRST of them
    /// is the destination every hook of this row reads back.
    installed: &'static [(&'static str, &'static str)],
    /// The directory this row's hooks write their lines in.
    sentinels: &'static str,
    /// The hooks the `hooks` section declares, in document order.
    hooks: &'static [Hook],
}

/// An AppSpec document for the `Server` platform: its `files` entries as
/// `(source, destination)` and its `hooks` section as the scripts `locations` pairs each hook
/// with, in the form the AWS CodeDeploy User Guide's AppSpec reference writes them.
///
/// Scripts declared for the SAME lifecycle event are written under one key of the section, in
/// the order they are given — a mapping cannot carry the same key twice, and the order the
/// section lists them in is the order the agent runs them in.
fn app_spec(files: &[(&str, &str)], locations: &[(&Hook, String)]) -> String {
    let mut document = String::from("version: 0.0\nos: linux\n");
    if !files.is_empty() {
        document.push_str("files:\n");
        for (source, destination) in files {
            document.push_str(&format!(
                "  - source: {source}\n    destination: {destination}\n"
            ));
        }
    }
    if !locations.is_empty() {
        document.push_str("hooks:\n");
        let mut written: Vec<&str> = Vec::new();
        for (hook, _) in locations {
            if written.contains(&hook.event) {
                continue;
            }
            written.push(hook.event);
            document.push_str(&format!("  {}:\n", hook.event));
            for (same, location) in locations
                .iter()
                .filter(|(other, _)| other.event == hook.event)
            {
                document.push_str(&format!("    - location: {location}\n      timeout: 180\n"));
                if let Some(runas) = same.runas {
                    document.push_str(&format!("      runas: {runas}\n"));
                }
            }
        }
    }
    document
}

/// The script one hook runs: it writes ONE line naming the lifecycle event the agent handed it,
/// `token`, and the content it read back from `witness` — a destination this revision's `files`
/// section installs.
///
/// So the line exists only where the hook really ran on the host, for that event, after the
/// install put `witness` there.
fn hook_script(sentinels: &str, sentinel: &str, witness: &str, token: &str) -> String {
    format!(
        "#!/bin/bash\nset -eu\nmkdir -p {sentinels}\n\
         printf '%s %s %s\\n' \"$LIFECYCLE_EVENT\" '{token}' \"$(cat {witness})\" > {sentinel}\n"
    )
}

/// What the host holds at `path`, read the way a client reads a machine it has no shell on; the
/// invocation must have reached a terminal `Success` for the read to mean anything.
async fn on_host(estate: &Estate, case: &str, instance: &str, path: &str) -> String {
    let invocation = run_command(
        estate,
        case,
        instance,
        &format!("{READ_COMMAND} {path}"),
    )
    .await;
    assert_eq!(
        invocation["Status"].as_str(),
        Some("Success"),
        "case {case:?}: reading {path} off {instance} must reach a terminal Success, got \
         {invocation:?}"
    );
    assert_eq!(
        invocation["ResponseCode"].as_i64(),
        Some(0),
        "case {case:?}: reading {path} off {instance} must report the status the utility ended \
         on, got {invocation:?}"
    );
    invocation["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Create the instance profile the host runs as, with the policy that lets the agent in its
/// guest register the node.
async fn host_profile(estate: &Estate, case: &str) {
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
}

/// Launch the one host every row deploys to and answer its instance id, once Amazon EC2 reports
/// it running and Systems Manager reports it as a managed node whose agent answers.
async fn host(estate: &Estate, case: &str) -> String {
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
    instance
}

/// RULE: see the module docs — the readiness of a `Server` host is readable through Run Command,
/// and the deployment that follows installs every file its AppSpec declares on that host and
/// runs every script its `hooks` section declares there, with the instance target reporting
/// `Install` and each declared hook's event `Succeeded`.
#[tokio::test]
async fn a_server_deployment_installs_its_files_and_runs_its_hooks_on_the_selected_host() {
    // The rows span how many `files` entries the revision declares and whether a source names a
    // file or a directory, which lifecycle event declares a hook, how many scripts one event
    // declares, and whether an entry names a user to impersonate.
    let rows: &[Row] = &[
        Row {
            case: "one file and one AfterInstall hook the entry names root for",
            files: &[("payload.txt", "/opt/appspec-one")],
            payloads: &[("payload.txt", "payload-of-the-first-revision")],
            installed: &[(
                "/opt/appspec-one/payload.txt",
                "payload-of-the-first-revision",
            )],
            sentinels: "/opt/appspec-one-hooks",
            hooks: &[Hook {
                event: "AfterInstall",
                runas: Some("root"),
            }],
        },
        Row {
            case: "two sources into two destinations, with an AfterInstall hook naming no user",
            files: &[
                ("payload.txt", "/opt/appspec-two/first"),
                ("extra/second.txt", "/opt/appspec-two/second"),
            ],
            payloads: &[
                ("payload.txt", "the-first-of-two"),
                ("extra/second.txt", "the-second-of-two"),
            ],
            installed: &[
                ("/opt/appspec-two/first/payload.txt", "the-first-of-two"),
                ("/opt/appspec-two/second/second.txt", "the-second-of-two"),
            ],
            sentinels: "/opt/appspec-two-hooks",
            hooks: &[Hook {
                event: "AfterInstall",
                runas: None,
            }],
        },
        Row {
            case: "a source naming a directory of the revision, and a ValidateService hook",
            files: &[("tree", "/opt/appspec-tree")],
            payloads: &[
                ("tree/one.txt", "the-file-at-the-top"),
                ("tree/nested/two.txt", "the-file-below-it"),
            ],
            installed: &[
                ("/opt/appspec-tree/one.txt", "the-file-at-the-top"),
                ("/opt/appspec-tree/nested/two.txt", "the-file-below-it"),
            ],
            sentinels: "/opt/appspec-tree-hooks",
            hooks: &[Hook {
                event: "ValidateService",
                runas: Some("root"),
            }],
        },
        Row {
            case: "two scripts for one event, and a second event declaring one of its own",
            files: &[("payload.txt", "/opt/appspec-many")],
            payloads: &[("payload.txt", "payload-for-three-scripts")],
            installed: &[("/opt/appspec-many/payload.txt", "payload-for-three-scripts")],
            sentinels: "/opt/appspec-many-hooks",
            hooks: &[
                Hook {
                    event: "ApplicationStart",
                    runas: None,
                },
                Hook {
                    event: "ApplicationStart",
                    runas: Some("root"),
                },
                Hook {
                    event: "ValidateService",
                    runas: None,
                },
            ],
        },
    ];

    let estate = Estate::new();
    let case = "the AppSpec-performing Server deployment";

    host_profile(&estate, case).await;
    let host = host(&estate, case).await;

    // HALF ONE of the rule: the gate a client puts in front of the deployment. The invocation
    // has to settle on a terminal status carrying what the init system wrote about the agent's
    // unit — without it a client has no way to tell a usable host from an unreachable one, and
    // the rows below are what it gates.
    let readiness = run_command(
        &estate,
        case,
        &host,
        &format!("systemctl is-active {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        readiness["Status"].as_str(),
        Some("Success"),
        "case {case:?}: the readiness command must reach a terminal Success, got {readiness:?}"
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

    estate
        .codedeploy(
            case,
            "CreateApplication",
            json!({"applicationName": "appspec-install", "computePlatform": "Server"}),
        )
        .await;

    // HALF TWO: each row deploys a revision of its own and is read back off the host. Each row
    // gets a deployment GROUP of its own, so what it finds on the host is its own deployment's
    // work and not a leftover of the row before it.
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let group = format!("fleet-{index}");
        let key = format!("appspec-{index}.zip");
        let token = format!("token-of-row-{index}");
        let witness = row
            .installed
            .first()
            .map(|(path, _)| *path)
            .expect("every row installs at least one file");

        // The hooks, each with the script location and the sentinel it writes.
        let locations: Vec<(&Hook, String)> = row
            .hooks
            .iter()
            .enumerate()
            .map(|(position, hook)| (hook, format!("scripts/hook-{position}.sh")))
            .collect();
        let scripts: Vec<(String, String)> = locations
            .iter()
            .enumerate()
            .map(|(position, (hook, location))| {
                let sentinel = format!("{}/{}-{position}", row.sentinels, hook.event);
                (
                    location.clone(),
                    hook_script(row.sentinels, &sentinel, witness, &token),
                )
            })
            .collect();
        let document = app_spec(row.files, &locations);

        let mut entries: Vec<(&str, &str)> = vec![("appspec.yml", document.as_str())];
        entries.extend(row.payloads.iter().copied());
        entries.extend(
            scripts
                .iter()
                .map(|(location, script)| (location.as_str(), script.as_str())),
        );
        estate.upload(case, BUCKET, &key, &bundle(&entries)).await;

        estate
            .codedeploy(
                case,
                "CreateDeploymentGroup",
                json!({
                    "applicationName": "appspec-install",
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
        let deployment = estate
            .codedeploy(
                case,
                "CreateDeployment",
                json!({
                    "applicationName": "appspec-install",
                    "deploymentGroupName": group,
                    "revision": {
                        "revisionType": "S3",
                        "s3Location": {
                            "bucket": BUCKET,
                            "key": key,
                            "bundleType": "zip",
                        },
                    },
                    "fileExistsBehavior": "OVERWRITE",
                }),
            )
            .await["deploymentId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let info = when_terminal(&estate, case, &deployment).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the deployment must reach Succeeded, got {info:?}"
        );

        // The target the deployment reports: the one host its group selects, and one Succeeded
        // lifecycle event per event the agent performed on it.
        let listed = estate
            .codedeploy(
                case,
                "ListDeploymentTargets",
                json!({"deploymentId": deployment}),
            )
            .await;
        let target_ids: Vec<String> = listed["targetIds"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            target_ids,
            vec![host.clone()],
            "case {case:?}: expected the deployment to report only the selected host, got \
             {target_ids:?}"
        );
        let target = estate
            .codedeploy(
                case,
                "GetDeploymentTarget",
                json!({"deploymentId": deployment, "targetId": host}),
            )
            .await["deploymentTarget"]["instanceTarget"]
            .clone();
        assert_eq!(
            target["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected the instance target to report {SUCCEEDED}, got {target:?}"
        );
        let events = events_of(&target);
        for (name, status) in &events {
            assert_eq!(
                status, SUCCEEDED,
                "case {case:?}: every event the agent performed must be {SUCCEEDED}, got \
                 {name} at {status}, in {events:?}"
            );
        }
        for event in ::std::iter::once(INSTALL_EVENT).chain(row.hooks.iter().map(|hook| hook.event))
        {
            assert!(
                events
                    .iter()
                    .any(|(name, status)| name == event && status == SUCCEEDED),
                "case {case:?}: expected {event} to be reported {SUCCEEDED}, got {events:?}"
            );
        }

        // ... and the machine itself: every destination the `files` section declares holds the
        // revision's own content.
        for (path, contents) in row.installed {
            let read = on_host(&estate, case, &host, path).await;
            assert_eq!(
                read, *contents,
                "case {case:?}: expected the host to hold the revision's own {path}, got {read:?}"
            );
        }

        // ... and each hook left its line there, naming the event it ran for and the content it
        // read back off the installed destination.
        for (position, (hook, _)) in locations.iter().enumerate() {
            let sentinel = format!("{}/{}-{position}", row.sentinels, hook.event);
            let read = on_host(&estate, case, &host, &sentinel).await;
            let expected = format!(
                "{} {token} {}",
                hook.event,
                row.installed
                    .first()
                    .map(|(_, contents)| *contents)
                    .unwrap_or_default()
            );
            assert_eq!(
                read, expected,
                "case {case:?}: expected the {} hook at {sentinel} to have run on the host \
                 after the install, got {read:?}",
                hook.event
            );
        }
    }
}
