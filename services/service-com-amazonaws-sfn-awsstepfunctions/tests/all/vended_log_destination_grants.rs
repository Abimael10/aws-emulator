// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the one rule a control-plane call stating a `loggingConfiguration`
//! is decided by: the call is SERVED whenever the state machine's role's identity-based
//! policies allow it the CloudWatch Logs vended-log-delivery actions on the destination the
//! configuration names, and is refused — HTTP 400 `AccessDeniedException` carrying the
//! service's fixed sentence, with nothing created or changed — only where those documents
//! allow it none of them.
//!
//! The suite is driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions and IAM mounted on ONE engine (the harness
//! `execution_data_encryption` builds), so every document the decision reads is one IAM's
//! own handlers wrote from a `CreateRole`/`PutRolePolicy` pair.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the SHAPE of the grant — the whole policy the service's CloudWatch Logs logging
//!   guidance publishes for a state machine's role, the delivery-setup half of it alone,
//!   one action of it alone, the same policy SCOPED to the destination's own log group ARN,
//!   that policy scoped to a DIFFERENT group, and the same policy with an explicit deny
//!   over it;
//! * the LEVEL the configuration asks for, over every value the modelled `LogLevel` enum
//!   carries (`smithy_query(query="shape",shape="LogLevel")`);
//! * the workflow TYPE the call creates, and the operation that states the configuration
//!   (`CreateStateMachine` and `UpdateStateMachine`).
//!
//! `role_log_and_key_authorization` pins the same rule's negative side for a role allowed
//! nothing of CloudWatch Logs; this suite is about which GRANTS are access to the
//! destination, so a role that carries the published policy is never told its role is
//! unauthorized.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{observability, LOG_LEVEL_OFF};

use crate::execution_data_encryption::{Chain, SFN};
use crate::identity_policy_authorization::allowing;
use crate::role_log_and_key_authorization::{role, unrelated_policy};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT, HELLO_DEFINITION, REGION};

/// The code and status an authorization failure is refused with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// The workflow types a create can ask for, from the modelled `StateMachineType` enum
/// (`smithy_query(query="shape",shape="StateMachineType")`).
const STANDARD: &str = "STANDARD";
const EXPRESS: &str = "EXPRESS";

/// The CloudWatch Logs actions the service's logging guidance publishes for a state
/// machine's role — the policy a client attaches to the role it hands
/// `CreateStateMachine` when it configures a `loggingConfiguration`: the calls that set
/// a vended-log DELIVERY up and read it back, and the two that write the events.
fn published_actions() -> Vec<&'static str> {
    let mut actions = delivery_setup_actions();
    actions.extend(["logs:CreateLogStream", "logs:PutLogEvents"]);
    actions
}

/// The half of that policy that sets the delivery up and reads it back, with neither of
/// the event-writing actions: the grant a client gives a role whose logs are delivered
/// for it rather than written by it.
fn delivery_setup_actions() -> Vec<&'static str> {
    vec![
        "logs:CreateLogDelivery",
        "logs:GetLogDelivery",
        "logs:UpdateLogDelivery",
        "logs:DeleteLogDelivery",
        "logs:ListLogDeliveries",
        "logs:PutResourcePolicy",
        "logs:DescribeResourcePolicies",
        "logs:DescribeLogGroups",
    ]
}

/// The published policy on every resource, with an explicit deny of the destination's
/// own actions over it: a deny wins over every allow, so the role is left with no access
/// to the destination at all.
fn published_then_denied() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": published_actions(),
                "Resource": "*",
            },
            {
                "Effect": "Deny",
                "Action": published_actions(),
                "Resource": "*",
            },
        ]
    })
}

/// The ARN of a log group of this account, in the qualified form `logGroupArn` documents
/// ("The ARN must end with `:*`").
fn destination_arn(group: &str) -> String {
    format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{group}:*")
}

/// A `loggingConfiguration` at `level` delivering to `destination`.
fn logging(level: &str, destination: &str) -> Value {
    json!({
        "level": level,
        "includeExecutionData": true,
        "destinations": [{ "cloudWatchLogsLogGroup": { "logGroupArn": destination } }],
    })
}

/// One row of the grant-shape table: what the role is allowed, and whether the call that
/// names it is served.
struct GrantRow {
    case: &'static str,
    /// The role's inline policy, built for the destination the call names.
    policy: fn(&str) -> Value,
    /// The level the configuration asks for.
    level: &'static str,
    /// The workflow type the create asks for.
    machine_type: &'static str,
    /// Whether the create is served.
    served: bool,
}

fn grant_rows() -> Vec<GrantRow> {
    vec![
        GrantRow {
            case: "the published log-delivery policy on every resource, for an EXPRESS workflow",
            policy: |_destination| allowing(&published_actions(), &["*"]),
            level: observability::LOG_LEVEL_ALL,
            machine_type: EXPRESS,
            served: true,
        },
        GrantRow {
            case: "the delivery-setup half of that policy, with no event-writing action",
            policy: |_destination| allowing(&delivery_setup_actions(), &["*"]),
            level: observability::LOG_LEVEL_ALL,
            machine_type: STANDARD,
            served: true,
        },
        GrantRow {
            case: "the published policy scoped to the destination's own log group ARN",
            policy: |destination| allowing(&published_actions(), &[destination]),
            level: observability::LOG_LEVEL_ERROR,
            machine_type: STANDARD,
            served: true,
        },
        GrantRow {
            case: "the published policy scoped to a different log group",
            policy: |_destination| {
                let elsewhere = destination_arn("/aws/vendedlogs/states/elsewhere");
                allowing(&published_actions(), &[elsewhere.as_str()])
            },
            level: observability::LOG_LEVEL_ALL,
            machine_type: EXPRESS,
            served: false,
        },
        GrantRow {
            case: "the published policy with an explicit deny of the same actions over it",
            policy: |_destination| published_then_denied(),
            level: observability::LOG_LEVEL_ALL,
            machine_type: STANDARD,
            served: false,
        },
        GrantRow {
            case: "a policy allowing nothing of CloudWatch Logs",
            policy: |_destination| unrelated_policy(),
            level: observability::LOG_LEVEL_FATAL,
            machine_type: STANDARD,
            served: false,
        },
    ]
}

/// The state machine `answered` reports, read back through `DescribeStateMachine`, with
/// the configuration the create stated.
async fn assert_round_trips(
    chain: &Chain,
    case: &str,
    answered: &Value,
    level: &str,
    machine_type: &str,
    destination: &str,
) {
    let arn = string(answered, "stateMachineArn");
    let described = chain
        .sfn("DescribeStateMachine", &json!({ "stateMachineArn": arn }))
        .await;
    assert_eq!(
        (
            described.get("status").and_then(Value::as_str),
            described.get("type").and_then(Value::as_str),
        ),
        (Some("ACTIVE"), Some(machine_type)),
        "case {case:?}: expected an ACTIVE {machine_type} machine, got {described:?}"
    );
    let configuration = described.get("loggingConfiguration").unwrap_or_else(|| {
        panic!("case {case:?}: expected a logging configuration in {described:?}")
    });
    let reported_destination = configuration
        .get("destinations")
        .and_then(Value::as_array)
        .and_then(|destinations| destinations.first())
        .and_then(|first| first.get("cloudWatchLogsLogGroup"))
        .and_then(|group| group.get("logGroupArn"))
        .and_then(Value::as_str);
    assert_eq!(
        (
            configuration.get("level").and_then(Value::as_str),
            reported_destination,
        ),
        (Some(level), Some(destination)),
        "case {case:?}: expected the stated configuration to be reported back, got \
         {configuration:?}"
    );
}

/// The refusal `answered` carries is the service's fixed log-destination sentence, and
/// the machine `name` asked for exists nowhere.
async fn assert_refused(chain: &Chain, case: &str, status: u16, answered: &Value, name: &str) {
    let code = error_code(answered);
    assert_eq!(
        (
            status,
            code.as_str(),
            answered.get("message").and_then(Value::as_str)
        ),
        (
            ACCESS_DENIED_STATUS,
            ACCESS_DENIED,
            Some(observability::LOG_DESTINATION_ACCESS_DENIED)
        ),
        "case {case:?}: expected the fixed log-destination refusal, got {status} {code} with \
         {answered:?}"
    );
    let listed = chain.sfn("ListStateMachines", &json!({})).await;
    let names: Vec<String> = listed
        .get("stateMachines")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|machine| string(machine, "name"))
        .collect();
    assert!(
        !names.contains(&name.to_string()),
        "case {case:?}: expected the refused machine to exist nowhere, got {names:?}"
    );
}

/// `CreateStateMachine` serves a logging configuration for every grant that IS access to
/// the destination it names, and refuses only the role allowed none of those actions on
/// it.
#[tokio::test]
async fn a_logging_configuration_follows_the_roles_grant_on_the_destination() {
    let chain = Chain::new();
    for (index, row) in grant_rows().into_iter().enumerate() {
        let GrantRow {
            case,
            policy,
            level,
            machine_type,
            served,
        } = row;
        let destination = destination_arn(&format!("/aws/vendedlogs/states/grant-{index}"));
        let role_arn = role(&chain, &format!("grant-role-{index}"), &policy(&destination)).await;
        let name = format!("grant-gate-{index}");
        let (status, answered) = chain
            .call(
                &SFN,
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": HELLO_DEFINITION,
                    "roleArn": role_arn,
                    "type": machine_type,
                    "loggingConfiguration": logging(level, &destination),
                }),
            )
            .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be served, got {status} {answered:?}"
            );
            assert_round_trips(&chain, case, &answered, level, machine_type, &destination).await;
        } else {
            assert_refused(&chain, case, status, &answered, &name).await;
        }
    }
}

/// EACH action that constitutes access to the log destination, allowed ALONE, is enough
/// for the call: the refusal is the role having NO access to the destination, not its
/// missing one of the published actions.
///
/// The catalog of those actions is the service's own
/// (`observability::LOG_DELIVERY_ACTIONS`), read as the row set rather than written out here
/// twice, and checked below to still be about setting the delivery up and writing the events.
#[tokio::test]
async fn one_destination_access_action_alone_is_enough() {
    let chain = Chain::new();
    // A shrink of that catalog must not silently remove rows: the actions the rule is about
    // are setting the delivery up and writing the events.
    for action in ["logs:CreateLogDelivery", "logs:PutLogEvents"] {
        assert!(
            observability::LOG_DELIVERY_ACTIONS.contains(&action),
            "case {action:?}: expected the destination-access catalog to carry it, got {:?}",
            observability::LOG_DELIVERY_ACTIONS
        );
    }
    for (index, action) in observability::LOG_DELIVERY_ACTIONS.iter().enumerate() {
        let destination = destination_arn(&format!("/aws/vendedlogs/states/single-{index}"));
        let role_arn = role(
            &chain,
            &format!("single-role-{index}"),
            &allowing(&[action], &["*"]),
        )
        .await;
        let (status, answered) = chain
            .call(
                &SFN,
                "CreateStateMachine",
                &json!({
                    "name": format!("single-gate-{index}"),
                    "definition": HELLO_DEFINITION,
                    "roleArn": role_arn,
                    "loggingConfiguration": logging(observability::LOG_LEVEL_ALL, &destination),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {action:?} alone: expected the create to be served, got {status} {answered:?}"
        );
        assert_round_trips(
            &chain,
            action,
            &answered,
            observability::LOG_LEVEL_ALL,
            STANDARD,
            &destination,
        )
        .await;
    }
}

/// EVERY level the modelled enum carries is served for a role the published policy
/// authorizes — including `OFF`, which delivers nothing and so names no destination.
#[tokio::test]
async fn every_modelled_level_is_served_for_an_authorized_role() {
    let chain = Chain::new();
    // The whole value set of `LogLevel`
    // (`smithy_query(query="shape",shape="LogLevel")`).
    let levels = [
        observability::LOG_LEVEL_ALL,
        observability::LOG_LEVEL_ERROR,
        observability::LOG_LEVEL_FATAL,
        LOG_LEVEL_OFF,
    ];
    let role_arn = role(&chain, "level-role", &allowing(&published_actions(), &["*"])).await;
    for (index, level) in levels.into_iter().enumerate() {
        let destination = destination_arn(&format!("/aws/vendedlogs/states/level-{index}"));
        let configuration = if level == LOG_LEVEL_OFF {
            json!({ "level": level, "includeExecutionData": false })
        } else {
            logging(level, &destination)
        };
        let (status, answered) = chain
            .call(
                &SFN,
                "CreateStateMachine",
                &json!({
                    "name": format!("level-gate-{index}"),
                    "definition": HELLO_DEFINITION,
                    "roleArn": role_arn,
                    "loggingConfiguration": configuration,
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case level {level:?}: expected the create to be served, got {status} {answered:?}"
        );
        let described = chain
            .sfn(
                "DescribeStateMachine",
                &json!({ "stateMachineArn": string(&answered, "stateMachineArn") }),
            )
            .await;
        assert_eq!(
            described
                .get("loggingConfiguration")
                .and_then(|reported| reported.get("level"))
                .and_then(Value::as_str),
            Some(level),
            "case level {level:?}: expected the stated level to be reported back, got \
             {described:?}"
        );
    }
}

/// `UpdateStateMachine` reads the same grant: turning logging on for a role the published
/// policy authorizes is served, and the configuration it states is what a later read
/// reports.
#[tokio::test]
async fn an_update_turns_logging_on_for_an_authorized_role() {
    let chain = Chain::new();
    let published = allowing(&published_actions(), &["*"]);
    let role_arn = role(&chain, "update-published-role", &published).await;
    let created = chain
        .sfn(
            "CreateStateMachine",
            &json!({
                "name": "update-published-gate",
                "definition": HELLO_DEFINITION,
                "roleArn": role_arn,
            }),
        )
        .await;
    let machine = string(&created, "stateMachineArn");
    let destination = destination_arn("/aws/vendedlogs/states/update-published");
    let (status, answered) = chain
        .call(
            &SFN,
            "UpdateStateMachine",
            &json!({
                "stateMachineArn": machine,
                "loggingConfiguration": logging(observability::LOG_LEVEL_ALL, &destination),
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "expected the update to be served for the published policy, got {status} {answered:?}"
    );
    let described = chain
        .sfn("DescribeStateMachine", &json!({ "stateMachineArn": machine }))
        .await;
    let reported = described
        .get("loggingConfiguration")
        .unwrap_or_else(|| panic!("expected a logging configuration in {described:?}"));
    assert_eq!(
        (
            reported.get("level").and_then(Value::as_str),
            reported
                .get("destinations")
                .and_then(Value::as_array)
                .and_then(|destinations| destinations.first())
                .and_then(|first| first.get("cloudWatchLogsLogGroup"))
                .and_then(|group| group.get("logGroupArn"))
                .and_then(Value::as_str),
        ),
        (
            Some(observability::LOG_LEVEL_ALL),
            Some(destination.as_str()),
        ),
        "expected the update's configuration to be reported back, got {reported:?}"
    );
}
