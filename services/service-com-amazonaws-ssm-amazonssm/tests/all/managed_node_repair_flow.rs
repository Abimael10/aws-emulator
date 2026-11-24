// SPDX-License-Identifier: CC0-1.0

//! The negative-then-repair managed-node flow: a privileged control host is reported
//! `Online` while its under-privileged twin in the same subnet is reported not at all, and
//! the twin appears under the same node id once its instance-profile role is granted the
//! call the agent registers with.
//!
//! RULE under test: while the account names no default host-management role, whether Systems
//! Manager reports an Amazon EC2 instance as a managed node is decided, on every read, by
//! whether the role of the instance profile that instance runs as is ALLOWED the call the
//! agent registers with — so a running host whose role is allowed it is reported `Online`,
//! a host of the same image in the same subnet whose role is not is reported not at all
//! however often the list is read, and that host appears, under the same node id, as soon as
//! its role is allowed it, by whichever kind of policy carries the grant and whether or not
//! the host is rebooted. Reading the list for one host neither registers nor withdraws
//! another: the control host stays reported, with the registration instant it first
//! reported, across every read of the whole flow.
//!
//! The rows differ along what the rule generalises over: HOW the grant reaches the already
//! running host's role (the AWS-managed policy a client attaches, the same attachment with
//! no reboot at all, an inline document of the account's own naming the call), how many hosts
//! run as the one repaired role (one, and two — the decision is the role's, not the host's),
//! and a grant that is NOT the registration call (a published AWS-managed policy allowing no
//! Systems Manager action), which repairs nothing.
//!
//! Every call goes through the wire a client uses — `awsJson1_1` for Systems Manager, an
//! `ec2Query` form POST for Amazon EC2 and an `awsQuery` form POST for IAM — so each
//! component's state is written and read through the service that owns it.
//!
//! Nothing here waits a fixed time: every span another component's clock owes is a stored due
//! instant, so each step LOOKS AGAIN until the condition holds or the deadline passes. The
//! hosts whose role is not allowed the call are launched BEFORE the control host, so by the
//! time the control's registration has been observed every earlier host's own span has passed
//! too — which is what makes "it is not reported" an answer rather than a race.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{
    create_profile, element, emulator, iam_ok, image_id, Grant, Profile, EC2_PRINCIPAL,
    GRANTS_REGISTRATION, SSM_CORE_POLICY,
};
use crate::parameter_store::{call_in_region, REGION};
use crate::ssm_core_policy_repair::{
    assert_online, ec2_ok, public_subnet, reported_when_registered, rows_for, PublicSubnet,
    AL2_PARAMETER, INSTANCE_TYPE,
};

/// The service setting that names the role Systems Manager manages an account's instances
/// through when the account has turned Default Host Management Configuration on
/// (`UpdateServiceSettingRequest$SettingId`). Every host below is managed through its own
/// instance profile instead, which is what this flow is about, so the setting is read first
/// to establish that the account names no such role.
const DEFAULT_MANAGEMENT_ROLE_SETTING: &str =
    "/ssm/managed-instance/default-ec2-instance-management-role";

/// The value `GetServiceSetting` reports for a setting whose value is a NAME while the
/// account has named nothing, and the status of a setting still at the value its service
/// team provisioned ("Default: The current setting uses a default value provisioned by the
/// Amazon Web Services service team", `ServiceSetting$Status`).
const UNSET_SETTING_VALUE: &str = "$None";
const SETTING_STATUS_DEFAULT: &str = "Default";

/// A published AWS-managed policy that allows no Systems Manager action at all — from IAM's
/// own catalogue of AWS-managed policies, whose document for it grants the S3 read verbs —
/// so a role carrying it carries a readable grant that is not the one the agent registers
/// with.
const UNRELATED_POLICY: &str = "arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess";

/// The name of the inline policy a row puts on its role.
const INLINE_POLICY_NAME: &str = "agent-registration";

/// The instance profile of the control host, whose role carries the grant from the start.
const CONTROL_PROFILE: &str = "repair-control";

/// How one already-running host's role is given — or not given — the call the agent
/// registers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repair {
    /// The AWS-managed policy a client attaches for this reaches the role, and the host is
    /// then rebooted, which is what an operator does to make the agent pick the credentials
    /// up.
    AttachedThenRebooted,
    /// The same attachment, and nothing about the host changes: only what its role allows.
    Attached,
    /// An inline document of the account's own, naming the registration call.
    InlineGrant,
    /// A published AWS-managed policy allowing no action of this service, so the role gains
    /// a grant that is not the one the agent registers with.
    UnrelatedPolicy,
}

/// One row: hosts launched as a role that is not allowed the registration call, and what
/// then reaches that role.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The name of the role and of the instance profile built for this row's hosts.
    profile: &'static str,
    /// How many hosts run as that one profile.
    hosts: usize,
    repair: Repair,
    /// Whether the row's hosts must be reported as managed nodes once the repair is done.
    registers_after_repair: bool,
}

/// The rows, every one launched before the control host: see the module docs.
const ROWS: &[Row] = &[
    Row {
        case: "one host whose role is attached the AWS-managed policy, then rebooted",
        profile: "repair-attached-rebooted",
        hosts: 1,
        repair: Repair::AttachedThenRebooted,
        registers_after_repair: true,
    },
    Row {
        case: "one host whose role is attached the AWS-managed policy and is not rebooted",
        profile: "repair-attached",
        hosts: 1,
        repair: Repair::Attached,
        registers_after_repair: true,
    },
    Row {
        case: "one host whose role is given an inline grant of the registration call",
        profile: "repair-inline",
        hosts: 1,
        repair: Repair::InlineGrant,
        registers_after_repair: true,
    },
    Row {
        case: "two hosts running as the one role the AWS-managed policy is attached to",
        profile: "repair-shared",
        hosts: 2,
        repair: Repair::Attached,
        registers_after_repair: true,
    },
    Row {
        case: "one host whose role is attached a policy allowing no action of this service",
        profile: "repair-unrelated",
        hosts: 1,
        repair: Repair::UnrelatedPolicy,
        registers_after_repair: false,
    },
];

/// Launch one host of `profile` into `subnet` from `image`, as the instance id Amazon EC2
/// answered.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    subnet: &PublicSubnet,
    image: &str,
    profile: &str,
) -> String {
    let body = ec2_ok(
        router,
        case,
        "RunInstances",
        &[
            ("ImageId", image),
            ("InstanceType", INSTANCE_TYPE),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("SubnetId", subnet.subnet_id.as_str()),
            ("SecurityGroupId.1", subnet.group_id.as_str()),
            ("IamInstanceProfile.Name", profile),
        ],
    )
    .await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an instance id: {body}"))
}

/// The setting the account holds for `setting_id`, read through this service's own reader.
async fn service_setting(router: &MultiServiceRouter, setting_id: &str) -> Value {
    let (status, body) = call_in_region(
        router,
        REGION,
        "GetServiceSetting",
        &json!({ "SettingId": setting_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetServiceSetting({setting_id}) must answer, got {status} with {body}"
    );
    body["ServiceSetting"].clone()
}

/// Assert this service reports no managed node for `instance_id`.
async fn assert_absent(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let reported = rows_for(router, instance_id).await;
    assert!(
        reported.is_empty(),
        "case {case:?}: expected no managed node to be reported for {instance_id}, got \
         {reported:?}"
    );
}

/// Carry out one row's repair: what reaches the role, and the reboot where the row has one.
async fn repair(router: &MultiServiceRouter, row: &Row, instance_ids: &[String]) {
    match row.repair {
        Repair::AttachedThenRebooted | Repair::Attached => {
            iam_ok(
                router,
                row.case,
                "AttachRolePolicy",
                &[("RoleName", row.profile), ("PolicyArn", SSM_CORE_POLICY)],
            )
            .await;
        }
        Repair::InlineGrant => {
            iam_ok(
                router,
                row.case,
                "PutRolePolicy",
                &[
                    ("RoleName", row.profile),
                    ("PolicyName", INLINE_POLICY_NAME),
                    ("PolicyDocument", GRANTS_REGISTRATION),
                ],
            )
            .await;
        }
        Repair::UnrelatedPolicy => {
            iam_ok(
                router,
                row.case,
                "AttachRolePolicy",
                &[("RoleName", row.profile), ("PolicyArn", UNRELATED_POLICY)],
            )
            .await;
        }
    }
    if row.repair == Repair::AttachedThenRebooted {
        for instance_id in instance_ids {
            ec2_ok(
                router,
                row.case,
                "RebootInstances",
                &[("InstanceId.1", instance_id.as_str())],
            )
            .await;
        }
    }
}

/// RULE: with no default host-management role named, the managed-node list holds exactly the
/// hosts whose instance-profile role is allowed the registration call at the moment of the
/// read — so an under-privileged twin is absent while the control host beside it is `Online`,
/// and the twin appears once its role is allowed that call, however the grant reaches it; the
/// control host never leaves the list along the way.
#[tokio::test]
async fn the_under_privileged_twin_appears_once_its_role_is_allowed_the_registration_call() {
    let router = emulator();

    // The precondition this flow rests on: the account names no role for Systems Manager to
    // manage its instances through, so every host below is managed through its own instance
    // profile or not at all.
    let setting = service_setting(&router, DEFAULT_MANAGEMENT_ROLE_SETTING).await;
    assert_eq!(
        setting["SettingValue"].as_str(),
        Some(UNSET_SETTING_VALUE),
        "expected {DEFAULT_MANAGEMENT_ROLE_SETTING} to name no role, got {setting}"
    );
    assert_eq!(
        setting["Status"].as_str(),
        Some(SETTING_STATUS_DEFAULT),
        "expected {DEFAULT_MANAGEMENT_ROLE_SETTING} to be at the provisioned value, got \
         {setting}"
    );

    let subnet = public_subnet(&router).await;
    let image = image_id(&router, AL2_PARAMETER).await;

    // One role and instance profile per row, none of them allowed the registration call, and
    // the control host's, which carries the AWS-managed policy a client attaches for this.
    for row in ROWS {
        create_profile(
            &router,
            &Profile {
                name: row.profile,
                trusts: EC2_PRINCIPAL,
                grant: Grant::None,
            },
        )
        .await;
    }
    create_profile(
        &router,
        &Profile {
            name: CONTROL_PROFILE,
            trusts: EC2_PRINCIPAL,
            grant: Grant::AwsManaged,
        },
    )
    .await;

    // The under-privileged hosts first, the control host last.
    let mut launched: Vec<(Row, Vec<String>)> = Vec::new();
    for row in ROWS {
        let mut instance_ids = Vec::new();
        for _ in 0..row.hosts {
            instance_ids.push(launch(&router, row.case, &subnet, &image, row.profile).await);
        }
        launched.push((*row, instance_ids));
    }
    let control = launch(
        &router,
        CONTROL_PROFILE,
        &subnet,
        &image,
        CONTROL_PROFILE,
    )
    .await;

    // The control host registers and is reported Online. Its registration instant is kept:
    // every later read has to report the same one, because the registration happened once.
    let reported = reported_when_registered(&router, CONTROL_PROFILE, &control).await;
    assert_online(CONTROL_PROFILE, &control, &reported);
    let registered_at = reported["RegistrationDate"].clone();
    assert!(
        !registered_at.is_null(),
        "case {CONTROL_PROFILE:?}: expected the control host to report when it registered, \
         got {reported}"
    );

    // Every earlier host's own registration span has passed by now, so the absence of a host
    // whose role is allowed nothing is an answer rather than a race — and asking about those
    // hosts does not withdraw the control host.
    for (row, instance_ids) in &launched {
        for instance_id in instance_ids {
            assert_absent(&router, row.case, instance_id).await;
            assert_control_still_reported(&router, row.case, &control, &registered_at).await;
        }
    }

    // The repair: what the role is allowed changes while the host keeps running.
    for (row, instance_ids) in &launched {
        repair(&router, row, instance_ids).await;
    }
    for (row, instance_ids) in &launched {
        if !row.registers_after_repair {
            continue;
        }
        for instance_id in instance_ids {
            let reported = reported_when_registered(&router, row.case, instance_id).await;
            assert_online(row.case, instance_id, &reported);
        }
    }

    // A role that gained a grant which is not the registration call repairs nothing: by now
    // every repaired host has been observed Online, so this host's own span has passed.
    for (row, instance_ids) in &launched {
        if row.registers_after_repair {
            continue;
        }
        for instance_id in instance_ids {
            assert_absent(&router, row.case, instance_id).await;
        }
    }

    // And the control host is still the node it registered as, after the whole flow.
    assert_control_still_reported(&router, "the whole flow", &control, &registered_at).await;
}

/// Assert the control host is still reported as the managed node it registered as: one row,
/// `Online`, carrying the registration instant of its first read.
async fn assert_control_still_reported(
    router: &MultiServiceRouter,
    case: &str,
    control: &str,
    registered_at: &Value,
) {
    let reported = rows_for(router, control).await;
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected the control host {control} to stay reported as exactly one \
         managed node, got {reported:?}"
    );
    let row = &reported[0];
    assert_online(case, control, row);
    assert_eq!(
        &row["RegistrationDate"], registered_at,
        "case {case:?}: expected the control host {control} to keep the registration instant \
         {registered_at}, got {row}"
    );
}
