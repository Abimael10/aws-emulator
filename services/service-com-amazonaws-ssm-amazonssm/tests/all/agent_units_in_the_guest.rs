// SPDX-License-Identifier: CC0-1.0

//! The AGENT UNITS of a launched node's guest, read back the way a client reads one: a shell
//! command run through Systems Manager Run Command.
//!
//! RULE under test: A GUEST'S INIT SYSTEM REPORTS A UNIT ACTIVE WHEN THE MACHINE HAS THAT UNIT
//! INSTALLED AND SOMETHING STARTED IT — for a unit that listens on no port as much as for a
//! server that does. So an agent an instance's own program installed by any of the documented
//! means, and then started, reads `active`; one installed and never started reads `inactive`;
//! one the machine never installed is a unit the init system has no file for; and a unit
//! something stopped stops reading `active`.
//!
//! Every case goes through the front door: the instance is launched over EC2's `ec2Query` wire
//! with its role and instance profile created over IAM's `awsQuery` wire, and every reading of
//! the guest is `AmazonSSM.SendCommand` plus `AmazonSSM.GetCommandInvocation` over
//! `awsJson1_1`. Nothing is seeded: the node is the managed node the launched instance became.
//!
//! The rows span what the rule generalises over — which agent, which documented installation
//! form (the package manager, the agent's own installer fetched by either downloader, and the
//! installer saved under a path of its own), and whether anything started the unit.

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use super::run_command_in_the_guest::{finish_due, finished, send};

/// The agent whose unit the gate in front of an EC2 deployment asks about.
const CODEDEPLOY_AGENT: &str = "codedeploy-agent";

/// The agent of the Systems Manager family, which the same rule covers: the rule is about a
/// unit of the machine, not about one agent.
const SSM_AGENT: &str = "amazon-ssm-agent";

/// The address the CodeDeploy agent's installer is published at for this Region, as the
/// CodeDeploy user guide's resource-kit bucket names give it
/// (`aws-codedeploy-<region>`, key `latest/install`).
const INSTALLER_ADDRESS: &str =
    "https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install";

/// The status a shell answers for a command that ended well.
const SUCCESS: i64 = 0;

/// The status the init system's control program answers a question about a unit that is not
/// running with (`systemctl is-active`, which "exit[s] with a non-zero status" for one), and
/// the status it answers a verb on a unit it has no file for with ("5" for a unit that does
/// not exist).
const UNIT_INACTIVE: i64 = 3;
const NO_SUCH_UNIT: i64 = 5;

/// One row: the program the instance's own user data gives its guest, which unit the case asks
/// about, and what the init system then answers for it.
struct AgentCase {
    case: &'static str,
    user_data: &'static str,
    unit: &'static str,
    status: i64,
    output: &'static str,
}

/// Launch an instance carrying `user_data` that becomes a managed node, and answer its id.
async fn node(emulator: &Emulator, case: &str, role: &str, user_data: &str) -> String {
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, role, &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = emulator
        .launch_with_user_data(case, &image, Some(&profile), Some(user_data))
        .await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let reported = emulator.nodes(case, REGION).await;
    assert!(
        reported.contains_key(&instance),
        "case {case:?}: the launched instance must become a managed node first, got {:?}",
        reported.keys().collect::<Vec<_>>()
    );
    instance
}

/// Run `command` in `instance`'s guest and answer the invocation the node finished with.
async fn ran(emulator: &Emulator, case: &str, instance: &str, command: &str) -> ::serde_json::Value {
    let command_id = send(emulator, case, instance, &[command]).await;
    finish_due(emulator, &command_id);
    finished(emulator, case, &command_id, instance).await
}

/// RULE: the unit the init system reports on is the one the machine has installed and
/// something started — whichever documented form installed it.
#[tokio::test]
async fn an_agent_unit_the_guest_installed_and_started_reads_active() {
    let rows = [
        AgentCase {
            case: "the documented procedure, run from the directory the program changed into",
            user_data: "#!/bin/bash\nset -x\ndnf install -y ruby wget || yum install -y ruby wget\n\
                        cd /tmp && wget \
                        https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install\n\
                        chmod +x ./install && ./install auto\n\
                        systemctl enable --now codedeploy-agent\n",
            unit: CODEDEPLOY_AGENT,
            status: SUCCESS,
            output: "active\n",
        },
        AgentCase {
            case: "the agent's own installer, fetched by the downloader its procedure names",
            user_data: "#!/bin/bash\nyum install -y ruby wget\nwget \
                        https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install\n\
                        chmod +x ./install\n./install auto\n",
            unit: CODEDEPLOY_AGENT,
            status: SUCCESS,
            output: "active\n",
        },
        AgentCase {
            case: "the same installer fetched by the transfer program, saved under a path of \
                   its own",
            user_data: "#!/bin/bash\ncurl -o /tmp/install \
                        https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install\n\
                        chmod +x /tmp/install\n/tmp/install auto\n",
            unit: CODEDEPLOY_AGENT,
            status: SUCCESS,
            output: "active\n",
        },
        AgentCase {
            case: "the package manager and the init system",
            user_data: "#!/bin/bash\ndnf install -y codedeploy-agent\n\
                        systemctl enable --now codedeploy-agent\n",
            unit: CODEDEPLOY_AGENT,
            status: SUCCESS,
            output: "active\n",
        },
        AgentCase {
            case: "another agent of the same catalog, started by the init system",
            user_data: "#!/bin/bash\nyum install -y amazon-ssm-agent\n\
                        systemctl start amazon-ssm-agent\n",
            unit: SSM_AGENT,
            status: SUCCESS,
            output: "active\n",
        },
        AgentCase {
            case: "a unit the machine installed and nothing started",
            user_data: "#!/bin/bash\nyum install -y codedeploy-agent\n",
            unit: CODEDEPLOY_AGENT,
            status: UNIT_INACTIVE,
            output: "inactive\n",
        },
        AgentCase {
            case: "a unit the machine never installed",
            user_data: "#!/bin/bash\necho nothing installed > /tmp/boot\n",
            unit: CODEDEPLOY_AGENT,
            status: UNIT_INACTIVE,
            output: "inactive\n",
        },
        AgentCase {
            case: "the agent of a program whose install was asked of an address nothing \
                   publishes it at",
            user_data: "#!/bin/bash\nwget https://example.invalid/latest/install\n\
                        chmod +x ./install\n./install auto\n",
            unit: CODEDEPLOY_AGENT,
            status: UNIT_INACTIVE,
            output: "inactive\n",
        },
    ];

    let emulator = Emulator::new();
    for (index, row) in rows.iter().enumerate() {
        let instance = node(
            &emulator,
            row.case,
            &format!("agent-role-{index}"),
            row.user_data,
        )
        .await;
        let answered = ran(
            &emulator,
            row.case,
            &instance,
            &format!("systemctl is-active {}", row.unit),
        )
        .await;
        assert_eq!(
            answered["ResponseCode"].as_i64(),
            Some(row.status),
            "case {:?}: expected the status the init system answers, got {answered}",
            row.case
        );
        assert_eq!(
            answered["StandardOutputContent"].as_str(),
            Some(row.output),
            "case {:?}: expected {:?} from the init system, got {answered}",
            row.case,
            row.output
        );
    }
}

/// RULE: a unit the machine never installed is one the init system has no unit file for, so
/// starting it starts nothing and says so.
#[tokio::test]
async fn starting_a_unit_the_machine_never_installed_starts_nothing() {
    let emulator = Emulator::new();
    let case = "a start of a unit nothing installed";
    let instance = node(
        &emulator,
        case,
        "absent-unit-role",
        "#!/bin/bash\necho installed nothing > /tmp/boot\n",
    )
    .await;
    let answered = ran(
        &emulator,
        case,
        &instance,
        &format!("systemctl start {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        answered["ResponseCode"].as_i64(),
        Some(NO_SUCH_UNIT),
        "case {case:?}: expected the status for a unit the machine has no file for, got {answered}"
    );
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some(
            "Failed to start codedeploy-agent.service: Unit codedeploy-agent.service not found.\n"
        ),
        "case {case:?}: expected the init system's own wording, got {answered}"
    );
    let answered = ran(
        &emulator,
        case,
        &instance,
        &format!("systemctl is-active {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some("inactive\n"),
        "case {case:?}: a unit nothing started must stay inactive, got {answered}"
    );
}

/// RULE: what one invocation did to the machine's units is what the next one finds — the units
/// are the machine's own state, so a unit something stopped stops reading active, and a unit
/// something started afterwards reads active again.
#[tokio::test]
async fn what_one_invocation_did_to_a_unit_is_what_the_next_one_finds() {
    let emulator = Emulator::new();
    let case = "the unit state case";
    let instance = node(
        &emulator,
        case,
        "unit-state-role",
        &format!(
            "#!/bin/bash\nwget {INSTALLER_ADDRESS}\nchmod +x ./install\n./install auto\n"
        ),
    )
    .await;

    let answered = ran(
        &emulator,
        case,
        &instance,
        &format!("systemctl is-enabled {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some("enabled\n"),
        "case {case:?}: the installer leaves the unit enabled, got {answered}"
    );

    let answered = ran(
        &emulator,
        case,
        &instance,
        &format!("systemctl stop {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        answered["ResponseCode"].as_i64(),
        Some(SUCCESS),
        "case {case:?}: stopping a unit the machine runs must succeed, got {answered}"
    );
    let answered = ran(
        &emulator,
        case,
        &instance,
        &format!("systemctl is-active {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some("inactive\n"),
        "case {case:?}: a stopped unit must not read active, got {answered}"
    );

    let answered = ran(
        &emulator,
        case,
        &instance,
        &format!("systemctl start {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        answered["ResponseCode"].as_i64(),
        Some(SUCCESS),
        "case {case:?}: starting an installed unit must succeed, got {answered}"
    );
    let answered = ran(
        &emulator,
        case,
        &instance,
        &format!("systemctl is-active {CODEDEPLOY_AGENT}"),
    )
    .await;
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some("active\n"),
        "case {case:?}: the started unit must read active again, got {answered}"
    );
}

/// RULE: reading the guest does not change it — the readiness gate a client polls answers the
/// same thing however many times it is asked, and the agent's installer is what put the unit
/// there rather than the poll.
#[tokio::test]
async fn polling_the_gate_leaves_the_unit_where_it_was() {
    let emulator = Emulator::new();
    let case = "the repeated poll case";
    let instance = node(
        &emulator,
        case,
        "poll-role",
        "#!/bin/bash\ndnf install -y codedeploy-agent\nsystemctl enable --now codedeploy-agent\n",
    )
    .await;
    for attempt in 0..3 {
        let answered = ran(
            &emulator,
            case,
            &instance,
            &format!("systemctl is-active {CODEDEPLOY_AGENT}"),
        )
        .await;
        assert_eq!(
            answered["StandardOutputContent"].as_str(),
            Some("active\n"),
            "case {case:?}: poll {attempt} must answer what the one before it did, got {answered}"
        );
    }
    // The same node, asked the way a client asks for the whole gate at once.
    let command_id = send(
        &emulator,
        case,
        &instance,
        &[&format!("systemctl is-active {CODEDEPLOY_AGENT}")],
    )
    .await;
    finish_due(&emulator, &command_id);
    let answered = finished(&emulator, case, &command_id, &instance).await;
    assert_eq!(
        answered["Status"].as_str(),
        Some("Success"),
        "case {case:?}: the invocation of a command the guest ran well is a success, got {answered}"
    );
}
