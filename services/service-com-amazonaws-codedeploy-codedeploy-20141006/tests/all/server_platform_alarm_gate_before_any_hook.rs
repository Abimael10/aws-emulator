// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch alarm gate of an EC2/on-premises (`Server`) deployment, driven the whole way
//! a client drives it: a launched Amazon EC2 host whose own program installed the CodeDeploy
//! agent, the readiness check a client makes on that host through Systems Manager Run Command,
//! and only then the deployment whose alarms decide whether anything runs on it.
//!
//! RULE under test, in two halves that make one path:
//!
//! 1. a shell command sent to a RUNNING managed node settles on a terminal
//!    `CommandInvocationStatus` carrying what the command wrote and the status it ended on, so
//!    the init system's answer about the agent's unit (`systemctl is-active codedeploy-agent`)
//!    is readable through `SendCommand` + `GetCommandInvocation` — which is the only way a
//!    client with no shell on the host can prove the host is usable at all;
//! 2. a `Server` deployment whose EFFECTIVE `alarmConfiguration` — the deployment's own
//!    override where it carries one, else its deployment group's — names an alarm that is in
//!    `ALARM` when the deployment starts is `Stopped` with `errorInformation.code`
//!    `ALARM_ACTIVE` naming that alarm, and NO lifecycle event hook of its revision runs on
//!    any host; under the same configuration with no alarm sounding the same revision reaches
//!    `Succeeded` and its hook does run there.
//!
//! The second half is asserted where it is observable rather than only in CodeDeploy's own
//! record: the revision's `AfterInstall` hook APPENDS the lifecycle event it ran for to a file
//! on the host, and that file is read back through Run Command — so "no hook ran" is a fact of
//! the machine, read through another service, and not the absence of a field.
//!
//! The rows differ along what the rule generalises over: the alarm STATE
//! (`smithy_query(query="shape",shape="StateValue")` of CloudWatch: OK | ALARM |
//! INSUFFICIENT_DATA), HOW MANY alarms the configuration names and which of them sound, and
//! WHERE the configuration comes from (the deployment group, or the deployment's own
//! `overrideAlarmConfiguration` of it). The alarms are created with `PutMetricAlarm` and driven
//! with `SetAlarmState`, the operations a client has, so nothing reaches into any store.
//!
//! The six services this needs on ONE engine — CodeDeploy, Amazon EC2 (the host), Amazon S3
//! (the revision), Systems Manager (the reader of the host), IAM (the roles) and CloudWatch
//! (the alarms) — are mounted by the sibling suite's shared harness
//! (`server_platform_rollback_read_through_run_command.rs`'s [`Estate`]), which also holds the
//! waits and the Run Command reader; what the alarm gate itself reads is shared with
//! `alarm_stopped_deployments.rs`. Nothing is written twice.
//!
//! Nothing here waits a fixed time: every span is a stored due instant — the launch reaching
//! `running`, the node registering, the invocation finishing, the deployment reaching its
//! outcome — so each step LOOKS AGAIN until the condition holds or the shared deadline passes,
//! and nothing asserts on elapsed time.

use serde_json::{json, Value};

use crate::alarm_stopped_deployments::{
    ALARM_ACTIVE_SENTENCE, STATE_ALARM, STATE_INSUFFICIENT_DATA, STATE_OK,
};
use crate::lambda_traffic_shifting::ALARM_ACTIVE;
use crate::server_platform_instance_deployments::{app_spec, bundle, element};
use crate::server_platform_rollback_read_through_run_command::{
    run_command, trust_policy, when_managed, when_running, when_terminal, Estate, AMI,
    EC2_API_VERSION, EC2_PRINCIPAL, IAM_API_VERSION, SSM_CORE_POLICY,
};

/// The instance type the host is launched as.
const INSTANCE_TYPE: &str = "t3.micro";

/// The bucket the one revision every row deploys is uploaded to.
const BUCKET: &str = "codedeploy-alarm-gate-revisions";

/// The key that revision is uploaded under.
const REVISION_KEY: &str = "alarm-gate-revision.zip";

/// The instance profile the host runs as, and the role it carries: a managed node registers as
/// that role, which is why a command can be delivered to the agent in its guest at all.
const HOST_PROFILE: &str = "alarm-gate-host-profile";
/// See [`HOST_PROFILE`] — deliberately not the profile's own name.
const HOST_ROLE: &str = "alarm-gate-host-role";

/// The service role the deployment group runs as, created in IAM so the alarm POLL the gate
/// makes is a call a role of this account really allows.
const SERVICE_ROLE: &str = "alarm-gate-service-role";

/// The inline policy on that role, and the call the gate needs it for: the gate reads the state
/// of the alarms the configuration names, which is `cloudwatch:DescribeAlarms`.
const SERVICE_ROLE_POLICY: &str = "alarm-gate-poll-alarms";

/// The service principal CodeDeploy acts as.
const CODEDEPLOY_PRINCIPAL: &str = "codedeploy.amazonaws.com";

/// The agent whose unit the gate in front of every EC2/on-premises deployment asks about, and
/// the unit name the init system knows it by.
const CODEDEPLOY_AGENT: &str = "codedeploy-agent";

/// The program the host's guest runs when it comes up: the documented installation of the
/// CodeDeploy agent, which is what makes the readiness check below answer `active`.
const USER_DATA: &str = "#!/bin/bash\ndnf install -y codedeploy-agent\n\
                         systemctl enable --now codedeploy-agent\n";

/// The tag the host carries and the deployment group selects on.
const FLEET_TAG: (&str, &str) = ("Name", "alarm-gate-host");

/// Where the revision installs its payload on the host.
const DESTINATION: &str = "/opt/alarm-gate";

/// The directory the revision's `AfterInstall` hook records itself in, and the file in it the
/// hook APPENDS the lifecycle event it ran for to: one line per hook run, so what the host
/// holds says how many times a hook of this revision ran.
const HOOK_DIRECTORY: &str = "/opt/alarm-gate-hooks";
/// See [`HOOK_DIRECTORY`].
const HOOK_MARKER: &str = "/opt/alarm-gate-hooks/ran";

/// The lifecycle event the revision declares a hook for, and so the only line its marker can
/// carry (the AppSpec `hooks` keys are the ones the AWS CodeDeploy User Guide lists).
const HOOK_EVENT: &str = "AfterInstall";

/// The alarm the deployment GROUP's configuration names.
const GROUP_ALARM: &str = "alarm-gate-group";
/// A second alarm, so a configuration can name more than one.
const OTHER_ALARM: &str = "alarm-gate-other";
/// A third, so the one that sounds need not be the first one named.
const THIRD_ALARM: &str = "alarm-gate-third";

/// Every alarm this suite creates, in the order it creates them.
const ALARMS: &[&str] = &[GROUP_ALARM, OTHER_ALARM, THIRD_ALARM];

/// The identity-based policy allowing the one call the alarm gate makes on another service.
const POLL_ALARMS_POLICY: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
                                  \"Allow\",\"Action\":\"cloudwatch:DescribeAlarms\",\
                                  \"Resource\":\"*\"}]}";

/// The revision every row deploys: a `files` entry copying its payload to [`DESTINATION`] and
/// one `AfterInstall` hook that appends the lifecycle event it ran for to [`HOOK_MARKER`].
fn revision_bundle() -> Vec<u8> {
    let document = app_spec(
        &[("payload.txt", DESTINATION)],
        &[(HOOK_EVENT, "scripts/after.sh")],
    );
    let hook = format!(
        "#!/bin/bash\nset -e\nmkdir -p {HOOK_DIRECTORY}\n\
         echo \"$LIFECYCLE_EVENT\" >> {HOOK_MARKER}\n"
    );
    bundle(&[
        ("appspec.yml", document.as_str()),
        ("payload.txt", "alarm-gate-payload"),
        ("scripts/after.sh", hook.as_str()),
    ])
}

/// An `alarmConfiguration` naming `alarms`, polling enabled and a state that cannot be
/// retrieved NOT ignored — the configuration a client gates an EC2 deployment with.
fn alarm_configuration(alarms: &[&str]) -> Value {
    json!({
        "enabled": true,
        "ignorePollAlarmFailure": false,
        "alarms": alarms.iter().map(|name| json!({"name": name})).collect::<Vec<Value>>(),
    })
}

/// Create the alarm `name`, watching a metric nothing publishes into so its state is whatever
/// `SetAlarmState` last forced.
///
/// Not [`crate::alarm_stopped_deployments::alarm_at`]: that one creates and forces an alarm in
/// one step over the OTHER harness (`codedeploy_lifecycle::Emulator`), and the rows here force
/// an already-created alarm between deployments on this one.
async fn create_alarm(estate: &Estate, case: &str, name: &str) {
    estate
        .cloudwatch(
            case,
            "PutMetricAlarm",
            json!({
                "AlarmName": name,
                "Namespace": "Sim/CodeDeployAlarmGate",
                "MetricName": "Errors",
                "Statistic": "Sum",
                "Period": 60,
                "EvaluationPeriods": 1,
                "Threshold": 1.0,
                "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                "TreatMissingData": "notBreaching",
            }),
        )
        .await;
}

/// Force the alarm `name` into `state_value`, the way a client drives an alarm.
async fn set_alarm_state(estate: &Estate, case: &str, name: &str, state_value: &str) {
    estate
        .cloudwatch(
            case,
            "SetAlarmState",
            json!({
                "AlarmName": name,
                "StateValue": state_value,
                "StateReason": "driven by the suite",
            }),
        )
        .await;
}

/// The lifecycle events the revision's hook recorded on the host, one per line of
/// [`HOOK_MARKER`] — empty where no hook of this revision has ever run there, which is a host
/// whose tree has no such file.
async fn hook_runs_on_host(estate: &Estate, case: &str, instance: &str) -> Vec<String> {
    let invocation = run_command(estate, case, instance, &format!("cat {HOOK_MARKER}")).await;
    if invocation["Status"].as_str() != Some("Success") {
        return Vec::new();
    }
    invocation["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// One row: the state each alarm is forced into before the deployment is created, the
/// configuration the deployment OVERRIDES its group's with (empty for a deployment that runs
/// under the group's own), and what the deployment must then come to.
struct Row {
    case: &'static str,
    /// `(alarm, state)` for each alarm this row drives, applied in order.
    states: &'static [(&'static str, &'static str)],
    /// The alarms the deployment's own `overrideAlarmConfiguration` names, or none.
    override_alarms: &'static [&'static str],
    /// The terminal `DeploymentStatus` the deployment must reach.
    status: &'static str,
    /// The `errorInformation.code` it must carry, where it carries one.
    error_code: Option<&'static str>,
    /// The alarms its `errorInformation.message` must name, in the order the effective
    /// configuration lists them.
    names_alarms: &'static [&'static str],
    /// Whether the revision's hook must have run on the host for this row.
    hook_runs: bool,
}

/// Create the host's instance profile and the deployment group's service role, answering the
/// ARN IAM reports for that role — which is the ARN the group carries.
async fn roles(estate: &Estate, case: &str) -> String {
    let ec2_trust = trust_policy(EC2_PRINCIPAL);
    let codedeploy_trust = trust_policy(CODEDEPLOY_PRINCIPAL);
    for (action, params) in [
        (
            "CreateRole",
            vec![
                ("RoleName", HOST_ROLE),
                ("AssumeRolePolicyDocument", &ec2_trust[..]),
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
        (
            "CreateRole",
            vec![
                ("RoleName", SERVICE_ROLE),
                ("AssumeRolePolicyDocument", &codedeploy_trust[..]),
            ],
        ),
        (
            "PutRolePolicy",
            vec![
                ("RoleName", SERVICE_ROLE),
                ("PolicyName", SERVICE_ROLE_POLICY),
                ("PolicyDocument", POLL_ALARMS_POLICY),
            ],
        ),
    ] {
        estate
            .form(case, "iam", action, IAM_API_VERSION, &params)
            .await;
    }
    let reported = estate
        .form(
            case,
            "iam",
            "GetRole",
            IAM_API_VERSION,
            &[("RoleName", SERVICE_ROLE)],
        )
        .await;
    element(&reported, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: GetRole must report the role's ARN: {reported}"))
}

/// Launch the one host every row deploys to and answer its instance id, once Amazon EC2 reports
/// it running and Systems Manager reports it as a managed node.
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

/// RULE: see the module docs — the readiness of a `Server` host is readable through Run
/// Command, and a deployment an alarm of its effective configuration is sounding for is
/// `Stopped` with `ALARM_ACTIVE` before any hook of its revision runs on that host.
#[tokio::test]
async fn an_alarm_sounding_at_the_start_stops_a_server_deployment_before_any_hook_runs() {
    // The rows span the alarm STATE, the NUMBER of alarms a configuration names and which of
    // them sounds, and WHERE the effective configuration comes from.
    let rows: &[Row] = &[
        Row {
            case: "the group's alarm at OK",
            states: &[
                (GROUP_ALARM, STATE_OK),
                (OTHER_ALARM, STATE_OK),
                (THIRD_ALARM, STATE_OK),
            ],
            override_alarms: &[],
            status: "Succeeded",
            error_code: None,
            names_alarms: &[],
            hook_runs: true,
        },
        Row {
            case: "the group's alarm in ALARM",
            states: &[(GROUP_ALARM, STATE_ALARM)],
            override_alarms: &[],
            status: "Stopped",
            error_code: Some(ALARM_ACTIVE),
            names_alarms: &[GROUP_ALARM],
            hook_runs: false,
        },
        Row {
            case: "the group's alarm at INSUFFICIENT_DATA, which is a state the poll retrieved",
            states: &[(GROUP_ALARM, STATE_INSUFFICIENT_DATA)],
            override_alarms: &[],
            status: "Succeeded",
            error_code: None,
            names_alarms: &[],
            hook_runs: true,
        },
        Row {
            case: "the deployment's own override of two alarms, the second of them sounding, \
                   over a group alarm that is sounding too",
            states: &[
                (GROUP_ALARM, STATE_ALARM),
                (OTHER_ALARM, STATE_OK),
                (THIRD_ALARM, STATE_ALARM),
            ],
            override_alarms: &[OTHER_ALARM, THIRD_ALARM],
            status: "Stopped",
            error_code: Some(ALARM_ACTIVE),
            names_alarms: &[THIRD_ALARM],
            hook_runs: false,
        },
        Row {
            case: "the deployment's own override of one alarm at OK, over a group alarm in \
                   ALARM",
            states: &[(GROUP_ALARM, STATE_ALARM), (OTHER_ALARM, STATE_OK)],
            override_alarms: &[OTHER_ALARM],
            status: "Succeeded",
            error_code: None,
            names_alarms: &[],
            hook_runs: true,
        },
    ];

    let estate = Estate::new();
    let case = "the alarm-gated Server deployment";

    // The two roles: the one the host runs as, so the agent in its guest registers the node and
    // can be sent a command, and the one the deployment group runs as, which the alarm poll is
    // made by.
    let service_role_arn = roles(&estate, case).await;
    let host = host(&estate, case).await;

    // HALF ONE of the rule: the readiness check a client makes on that host. The invocation has
    // to settle on a terminal status carrying what the init system wrote.
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
    assert!(
        hook_runs_on_host(&estate, case, &host).await.is_empty(),
        "case {case:?}: no hook of the revision can have run before any deployment"
    );

    // The revision, the alarms, the application and the deployment group whose configuration
    // names the group's alarm.
    estate
        .upload(case, BUCKET, REVISION_KEY, &revision_bundle())
        .await;
    for alarm in ALARMS {
        create_alarm(&estate, case, alarm).await;
    }
    estate
        .codedeploy(
            case,
            "CreateApplication",
            json!({"applicationName": "alarm-gate", "computePlatform": "Server"}),
        )
        .await;
    estate
        .codedeploy(
            case,
            "CreateDeploymentGroup",
            json!({
                "applicationName": "alarm-gate",
                "deploymentGroupName": "fleet",
                "serviceRoleArn": service_role_arn,
                "ec2TagFilters": [
                    {"Key": FLEET_TAG.0, "Value": FLEET_TAG.1, "Type": "KEY_AND_VALUE"},
                ],
                "deploymentStyle": {
                    "deploymentType": "IN_PLACE",
                    "deploymentOption": "WITHOUT_TRAFFIC_CONTROL",
                },
                "alarmConfiguration": alarm_configuration(&[GROUP_ALARM]),
            }),
        )
        .await;

    // HALF TWO: each row drives the alarms, deploys the one revision and reads the host back.
    // The marker accumulates one line per hook run, so what the host holds says how many of the
    // rows so far ran a hook at all.
    let mut expected_runs = 0_usize;
    for row in rows {
        for (alarm, state_value) in row.states {
            set_alarm_state(&estate, row.case, alarm, state_value).await;
        }
        let mut request = json!({
            "applicationName": "alarm-gate",
            "deploymentGroupName": "fleet",
            "revision": {
                "revisionType": "S3",
                "s3Location": {
                    "bucket": BUCKET,
                    "key": REVISION_KEY,
                    "bundleType": "zip",
                },
            },
            "fileExistsBehavior": "OVERWRITE",
        });
        if !row.override_alarms.is_empty() {
            request["overrideAlarmConfiguration"] = alarm_configuration(row.override_alarms);
        }
        let deployment = estate
            .codedeploy(row.case, "CreateDeployment", request)
            .await["deploymentId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let info = when_terminal(&estate, row.case, &deployment).await;
        assert_eq!(
            info["status"].as_str(),
            Some(row.status),
            "case {:?}: expected the deployment to reach {}, got {info:?}",
            row.case,
            row.status
        );
        let code = info["errorInformation"]["code"].as_str();
        assert_eq!(
            code, row.error_code,
            "case {:?}: expected the error code {:?}, got {code:?} in {info:?}",
            row.case, row.error_code
        );
        let message = info["errorInformation"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if row.names_alarms.is_empty() {
            assert!(
                message.is_empty(),
                "case {:?}: expected no error message, got {message:?}",
                row.case
            );
        } else {
            let named: String = row
                .names_alarms
                .iter()
                .map(|alarm| format!("<{alarm}>"))
                .collect::<Vec<String>>()
                .join(", ");
            assert_eq!(
                message,
                format!("{ALARM_ACTIVE_SENTENCE}{named}"),
                "case {:?}: expected the sentence naming the alarms that sounded, got \
                 {message:?}",
                row.case
            );
            for quiet in ALARMS
                .iter()
                .filter(|alarm| !row.names_alarms.contains(alarm))
            {
                assert!(
                    !message.contains(&format!("<{quiet}>")),
                    "case {:?}: an alarm that was not sounding must not be named, got \
                     {message:?}",
                    row.case
                );
            }
        }
        if row.hook_runs {
            expected_runs += 1;
        }
        let runs = hook_runs_on_host(&estate, row.case, &host).await;
        assert_eq!(
            runs.len(),
            expected_runs,
            "case {:?}: expected {expected_runs} hook run(s) recorded on the host, got {runs:?}",
            row.case
        );
        assert!(
            runs.iter().all(|event| event == HOOK_EVENT),
            "case {:?}: the host can only record the lifecycle event the revision declares a \
             hook for, got {runs:?}",
            row.case
        );
    }
}
