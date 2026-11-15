// SPDX-License-Identifier: CC0-1.0

//! `SendCommand` is a signed call, and it is decided against the CALLER's own identity-based
//! IAM policies before anything is resolved.
//!
//! RULE: the action is this operation's own (`ssm:SendCommand`) and the resource is the EC2
//! instance ARN of each node the request NAMES — the account's resources for a request that
//! names none and describes its nodes instead — so a caller no readable document allows is
//! refused the modelled `AccessDeniedException`, naming the principal, the action, the
//! resource and the rule that withheld the grant, and the refused send mints no command at
//! all. A caller whose documents allow it reaches the send, which is then answered by the Run
//! Command slice.
//!
//! Every case goes through the front door: `AmazonSSM.SendCommand` over `awsJson1_1`, signed
//! with the access key id of the principal under test, with the role and its policy created
//! over IAM's `awsQuery` wire and the session minted by STS's own `AssumeRole` — so the
//! documents this service reads are the ones those services wrote.
//!
//! The rows differ along what the rule generalises over: which principal signs the call (the
//! account's own published credential, a session carrying no policy, one allowed everything,
//! one explicitly denied, one allowed on another instance, one allowed on this one), and
//! whether the request names its node or describes it.

use serde_json::{json, Value};

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};

/// The published command document every case sends.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// What a principal a case signs as is: the access key id it presents, and the ARN a refusal
/// must blame it by — what `sts:GetCallerIdentity` reports for that key.
struct Principal {
    access_key_id: String,
    arn: String,
}

/// Launch an instance that becomes a managed node and answer its id.
async fn node(emulator: &Emulator, case: &str) -> String {
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "send-authorization-node", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = emulator.launch(case, &image, Some(&profile)).await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let reported = emulator.nodes(case, REGION).await;
    assert!(
        reported.contains_key(&instance),
        "case {case:?}: the launched instance must become a managed node first, got {:?}",
        reported.keys().collect::<Vec<_>>()
    );
    instance
}

/// Create a role carrying `policy` as its inline policy (when a case gives one), assume it
/// through STS, and answer the session's access key id with the session ARN.
async fn assumed_role(
    emulator: &Emulator,
    case: &str,
    role_name: &str,
    policy: Option<&str>,
) -> Principal {
    let account = engine::emulator_account_id();
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"arn:aws:iam::{account}:root\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}"
    );
    emulator
        .iam(
            case,
            "CreateRole",
            &[
                ("RoleName", role_name),
                ("AssumeRolePolicyDocument", &trust),
            ],
        )
        .await;
    if let Some(policy) = policy {
        emulator
            .iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", role_name),
                    ("PolicyName", "inline"),
                    ("PolicyDocument", policy),
                ],
            )
            .await;
    }
    let assumed = emulator
        .sts(
            case,
            "AssumeRole",
            &[
                ("RoleArn", &format!("arn:aws:iam::{account}:role/{role_name}")),
                ("RoleSessionName", "probe"),
            ],
        )
        .await;
    Principal {
        access_key_id: Emulator::answered(&assumed, "AccessKeyId")
            .unwrap_or_else(|| panic!("case {case:?}: AssumeRole answers an AccessKeyId: {assumed}")),
        arn: Emulator::answered(&assumed, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: AssumeRole answers the session Arn: {assumed}")),
    }
}

/// The ARN of the EC2 instance `instance_id` names — the resource an authorization decision
/// about a send to that node is made about.
fn instance_arn(instance_id: &str) -> String {
    format!(
        "arn:aws:ec2:{REGION}:{}:instance/{instance_id}",
        engine::emulator_account_id()
    )
}

/// The wire error code of a failure body, as a client branches on it.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .map(|named| named.rsplit('#').next().unwrap_or(named).to_string())
        .unwrap_or_default()
}

/// How many commands this Region's Run Command history holds, signed as the account's own
/// credential — which is what shows that a refused send wrote nothing.
async fn commands_held(emulator: &Emulator, case: &str) -> usize {
    let (status, answered) = emulator.ssm(REGION, "ListCommands", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListCommands must be served, got {status}: {answered}"
    );
    answered["Commands"].as_array().map_or(0, Vec::len)
}

/// RULE: the send is decided against the caller's identity-based policies, about the ARN of
/// each node it NAMES, and a refused send mints no command.
#[tokio::test]
async fn a_caller_whose_policies_do_not_allow_the_send_is_refused() {
    let emulator = Emulator::new();
    let case = "the principal table";
    let instance = node(&emulator, case).await;
    let resource = instance_arn(&instance);

    let allow_all = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:*","Resource":"*"}]}"#;
    let allow_elsewhere = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:GetParameter","Resource":"*"}]}"#;
    let allow_all_but_deny_the_send = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:*","Resource":"*"},
        {"Effect":"Deny","Action":"ssm:SendCommand","Resource":"*"}]}"#;
    let allow_another_instance = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:SendCommand","Resource":"{}"}}]}}"#,
        instance_arn("i-0abcdef0123456789")
    );
    let allow_this_instance = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:SendCommand","Resource":"{resource}"}}]}}"#
    );

    // (case, the principal, the clause the refusal must carry — `None` where the send is
    // served)
    let principals: Vec<(&str, Principal, Option<String>)> = vec![
        (
            "the account's own published credential",
            Principal {
                access_key_id: "AKIDEXAMPLE".to_string(),
                arn: format!("arn:aws:iam::{}:root", engine::emulator_account_id()),
            },
            None,
        ),
        (
            "a session carrying no policy at all",
            assumed_role(&emulator, case, "send-noperm", None).await,
            Some("because no identity-based policy allows the ssm:SendCommand action".to_string()),
        ),
        (
            "a session allowed only another action of this service",
            assumed_role(&emulator, case, "send-elsewhere", Some(allow_elsewhere)).await,
            Some("because no identity-based policy allows the ssm:SendCommand action".to_string()),
        ),
        (
            "a session allowed everything",
            assumed_role(&emulator, case, "send-admin", Some(allow_all)).await,
            None,
        ),
        (
            "a session whose policy denies the send explicitly",
            assumed_role(
                &emulator,
                case,
                "send-denied",
                Some(allow_all_but_deny_the_send),
            )
            .await,
            Some("with an explicit deny in an identity-based policy".to_string()),
        ),
        (
            "a session allowed the send on ANOTHER instance",
            assumed_role(
                &emulator,
                case,
                "send-other-instance",
                Some(&allow_another_instance),
            )
            .await,
            Some("because no identity-based policy allows the ssm:SendCommand action".to_string()),
        ),
        (
            "a session allowed the send on THIS instance",
            assumed_role(
                &emulator,
                case,
                "send-this-instance",
                Some(&allow_this_instance),
            )
            .await,
            None,
        ),
    ];

    for (why, principal, reason) in principals {
        let before = commands_held(&emulator, why).await;
        let (status, answered) = emulator
            .ssm_as(
                &principal.access_key_id,
                REGION,
                "SendCommand",
                &json!({
                    "DocumentName": RUN_SHELL_SCRIPT,
                    "InstanceIds": [instance],
                    "Parameters": {"commands": ["echo authorized"]},
                }),
            )
            .await;
        let after = commands_held(&emulator, why).await;
        match reason {
            Some(reason) => {
                assert_eq!(
                    (status, error_code(&answered).as_str()),
                    (400, "AccessDeniedException"),
                    "case {why:?}: expected the send to be refused, got {status}: {answered}"
                );
                assert_eq!(
                    answered["Message"].as_str().unwrap_or_default(),
                    format!(
                        "User: {} is not authorized to perform: ssm:SendCommand on resource: \
                         {resource} {reason}",
                        principal.arn
                    ),
                    "case {why:?}: expected the refusal to name the principal, the action, the \
                     node's ARN and the rule, got {answered}"
                );
                assert_eq!(
                    after, before,
                    "case {why:?}: expected a refused send to mint no command"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {why:?}: expected the send to be served, got {status}: {answered}"
                );
                assert_eq!(
                    answered["Command"]["InstanceIds"],
                    json!([instance]),
                    "case {why:?}: expected the command to name the node it was sent to, got \
                     {answered}"
                );
                assert_eq!(
                    after,
                    before + 1,
                    "case {why:?}: expected the served send to mint one command"
                );
            }
        }
    }
}

/// RULE: a send that NAMES no node describes its nodes instead, which addresses the account's
/// resources rather than one record — so a grant scoped to one instance does not allow it,
/// while one covering the account's resources does.
#[tokio::test]
async fn a_send_that_describes_its_nodes_is_decided_about_the_accounts_resources() {
    let emulator = Emulator::new();
    let case = "the targets case";
    let instance = node(&emulator, case).await;
    let scoped = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:SendCommand","Resource":"{}"}}]}}"#,
        instance_arn(&instance)
    );
    let narrow = assumed_role(&emulator, case, "targets-scoped", Some(&scoped)).await;
    let wide = assumed_role(
        &emulator,
        case,
        "targets-wide",
        Some(r#"{"Version":"2012-10-17","Statement":[
            {"Effect":"Allow","Action":"ssm:SendCommand","Resource":"*"}]}"#),
    )
    .await;
    let describing = json!({
        "DocumentName": RUN_SHELL_SCRIPT,
        "Targets": [{ "Key": "InstanceIds", "Values": [instance] }],
        "Parameters": {"commands": ["echo described"]},
    });

    let (status, answered) = emulator
        .ssm_as(&narrow.access_key_id, REGION, "SendCommand", &describing)
        .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "AccessDeniedException"),
        "case {case:?}: expected a grant scoped to one instance not to allow a send that names \
         none, got {status}: {answered}"
    );
    assert_eq!(
        answered["Message"].as_str().unwrap_or_default(),
        format!(
            "User: {} is not authorized to perform: ssm:SendCommand on resource: * because no \
             identity-based policy allows the ssm:SendCommand action",
            narrow.arn
        ),
        "case {case:?}: expected the refusal to be about the account's resources, got {answered}"
    );

    let (status, answered) = emulator
        .ssm_as(&wide.access_key_id, REGION, "SendCommand", &describing)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a grant covering the account's resources to allow it, got \
         {status}: {answered}"
    );
    assert_eq!(
        answered["Command"]["TargetCount"].as_i64(),
        Some(1),
        "case {case:?}: expected the described node to be the one the command runs on, got \
         {answered}"
    );
}
