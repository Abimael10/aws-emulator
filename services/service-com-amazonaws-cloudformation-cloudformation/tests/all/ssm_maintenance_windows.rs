// SPDX-License-Identifier: CC0-1.0

//! A stack that declares an `AWS::SSM::MaintenanceWindow` and an
//! `AWS::SSM::MaintenanceWindowTarget` really creates them in AWS Systems Manager.
//!
//! RULE: a resource type CloudFormation publishes is one a template may DECLARE, and a stack
//! that reaches a successful terminal state has created each declared resource through the
//! owning service's own create API — so the window and the target registered with it are both
//! readable through Systems Manager's own reads, under the identifiers that service minted and
//! the stack published, and deleting the stack removes them there. The owning service's
//! REFUSAL is the resource's failure, so a target registered with a window that service holds
//! none of fails that resource and rolls the stack back rather than reporting a target nothing
//! registered.
//!
//! The rows differ along what the rule generalises over: whether the template is JSON or YAML,
//! whether the window's schedule is a `cron` or a `rate` expression and whether it states a
//! time zone, which `MaintenanceWindowResourceType` the target registers, which form its
//! criteria take (node ids, a tag, a resource group) and whether it carries the three optional
//! labels a target may be registered with.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except each resource's physical id, which is what the stack
//! reports for the resource and what Systems Manager is then asked to resolve.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamic_references::ssm;
use crate::stack_lifecycle::{call, describe_stack, member, router, xml_blocks, xml_text};

/// How long a case looks for a stack to reach a terminal state, far longer than the span the
/// clock owes that move after — so the case does not depend on how fast the host runs.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row of the table: the template a stack is created from, and what the rule says the two
/// resources it declares must then be in Systems Manager.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The logical ids the template declares the two resources under.
    window_logical_id: &'static str,
    target_logical_id: &'static str,
    /// What `GetMaintenanceWindow` must report for the window the row declares.
    expected_window_name: &'static str,
    expected_schedule: &'static str,
    expected_timezone: Option<&'static str>,
    expected_duration: i64,
    expected_cutoff: i64,
    expected_unassociated: bool,
    /// What `DescribeMaintenanceWindowTargets` must report for its one target.
    expected_resource_type: &'static str,
    expected_target_key: &'static str,
    expected_target_values: &'static [&'static str],
    expected_owner_information: Option<&'static str>,
    expected_target_name: Option<&'static str>,
    expected_target_description: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a JSON cron window with a tag-targeted INSTANCE target carrying every label",
        stack_name: "mw-cron-tagged",
        template: || {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Description": "a maintenance window and the nodes it patches",
                "Resources": {
                    "PatchWindow": {
                        "Type": "AWS::SSM::MaintenanceWindow",
                        "Properties": {
                            "Name": "mw-cron-tagged-window",
                            "Description": "the weekly patch window",
                            "Schedule": "cron(0 2 ? * SUN *)",
                            "Duration": 3,
                            "Cutoff": 1,
                            "AllowUnassociatedTargets": false
                        }
                    },
                    "PatchTarget": {
                        "Type": "AWS::SSM::MaintenanceWindowTarget",
                        "Properties": {
                            "WindowId": { "Ref": "PatchWindow" },
                            "ResourceType": "INSTANCE",
                            "Name": "patch-target",
                            "Description": "the nodes this window patches",
                            "OwnerInformation": "platform",
                            "Targets": [{
                                "Key": "tag:Environment",
                                "Values": ["production", "staging"]
                            }]
                        }
                    }
                },
                "Outputs": {
                    "WindowId": { "Value": { "Ref": "PatchWindow" } },
                    "TargetId": { "Value": { "Ref": "PatchTarget" } }
                }
            })
            .to_string()
        },
        window_logical_id: "PatchWindow",
        target_logical_id: "PatchTarget",
        expected_window_name: "mw-cron-tagged-window",
        expected_schedule: "cron(0 2 ? * SUN *)",
        expected_timezone: None,
        expected_duration: 3,
        expected_cutoff: 1,
        expected_unassociated: false,
        expected_resource_type: "INSTANCE",
        expected_target_key: "tag:Environment",
        expected_target_values: &["production", "staging"],
        expected_owner_information: Some("platform"),
        expected_target_name: Some("patch-target"),
        expected_target_description: Some("the nodes this window patches"),
    },
    Row {
        case: "a YAML rate window in a stated time zone, targeting node ids and carrying no \
               labels",
        stack_name: "mw-rate-nodes",
        template: || {
            [
                "Resources:",
                "  RateWindow:",
                "    Type: AWS::SSM::MaintenanceWindow",
                "    Properties:",
                "      Name: mw-rate-nodes-window",
                "      Schedule: rate(7 days)",
                "      ScheduleTimezone: Europe/Berlin",
                "      Duration: 2",
                "      Cutoff: 0",
                "      AllowUnassociatedTargets: true",
                "  RateTarget:",
                "    Type: AWS::SSM::MaintenanceWindowTarget",
                "    Properties:",
                "      WindowId: !Ref RateWindow",
                "      ResourceType: INSTANCE",
                "      Targets:",
                "        - Key: InstanceIds",
                "          Values:",
                "            - i-0123456789abcdef0",
                "Outputs:",
                "  WindowId:",
                "    Value: !Ref RateWindow",
                "  TargetId:",
                "    Value: !Ref RateTarget",
            ]
            .join("\n")
        },
        window_logical_id: "RateWindow",
        target_logical_id: "RateTarget",
        expected_window_name: "mw-rate-nodes-window",
        expected_schedule: "rate(7 days)",
        expected_timezone: Some("Europe/Berlin"),
        expected_duration: 2,
        expected_cutoff: 0,
        expected_unassociated: true,
        expected_resource_type: "INSTANCE",
        expected_target_key: "InstanceIds",
        expected_target_values: &["i-0123456789abcdef0"],
        expected_owner_information: None,
        expected_target_name: None,
        expected_target_description: None,
    },
    Row {
        case: "a RESOURCE_GROUP target of a window with no description",
        stack_name: "mw-resource-group",
        template: || {
            json!({
                "Resources": {
                    "GroupWindow": {
                        "Type": "AWS::SSM::MaintenanceWindow",
                        "Properties": {
                            "Name": "mw-resource-group-window",
                            "Schedule": "cron(30 4 ? * MON *)",
                            "Duration": 6,
                            "Cutoff": 2,
                            "AllowUnassociatedTargets": true
                        }
                    },
                    "GroupTarget": {
                        "Type": "AWS::SSM::MaintenanceWindowTarget",
                        "Properties": {
                            "WindowId": { "Ref": "GroupWindow" },
                            "ResourceType": "RESOURCE_GROUP",
                            "OwnerInformation": "the estate group",
                            "Targets": [{
                                "Key": "resource-groups:Name",
                                "Values": ["the-estate"]
                            }]
                        }
                    }
                },
                "Outputs": {
                    "WindowId": { "Value": { "Ref": "GroupWindow" } },
                    "TargetId": { "Value": { "Ref": "GroupTarget" } }
                }
            })
            .to_string()
        },
        window_logical_id: "GroupWindow",
        target_logical_id: "GroupTarget",
        expected_window_name: "mw-resource-group-window",
        expected_schedule: "cron(30 4 ? * MON *)",
        expected_timezone: None,
        expected_duration: 6,
        expected_cutoff: 2,
        expected_unassociated: true,
        expected_resource_type: "RESOURCE_GROUP",
        expected_target_key: "resource-groups:Name",
        expected_target_values: &["the-estate"],
        expected_owner_information: Some("the estate group"),
        expected_target_name: None,
        expected_target_description: None,
    },
];

/// Look at the stack until it reaches a TERMINAL state, answering the whole answer.
///
/// The stack's own clock-owed move is what this waits for, so the case waits for the
/// CONDITION rather than for a span: it looks again until the status stops ending in
/// `_IN_PROGRESS`, or until a deadline far longer than that span passes.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let (status, described) = describe_stack(router, stack).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must be served, got {status}: {described}"
        );
        let reached = xml_text(&described, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to reach a terminal state with nothing but the \
             clock running, still {reached:?}: {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The physical resource id the stack reports for `logical_id`.
async fn physical_id(router: &MultiServiceRouter, case: &str, stack: &str, logical_id: &str) -> String {
    let (status, resources) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources must be served, got {status}: {resources}"
    );
    xml_blocks(&resources, "member")
        .into_iter()
        .find(|block| xml_text(block, "LogicalResourceId").as_deref() == Some(logical_id))
        .and_then(|block| xml_text(&block, "PhysicalResourceId"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the stack must report a physical id for {logical_id:?}: {resources}")
        })
}

/// The value the stack publishes for one output.
fn output(described: &str, key: &str) -> Option<String> {
    xml_blocks(described, "member")
        .into_iter()
        .find(|block| xml_text(block, "OutputKey").as_deref() == Some(key))
        .and_then(|block| xml_text(&block, "OutputValue"))
}

/// Whether `id` is shaped like the `MaintenanceWindowId` Systems Manager mints
/// (`^mw-[0-9a-f]{17}$`, from that service's own model).
fn is_window_id(id: &str) -> bool {
    id.strip_prefix("mw-").is_some_and(|digits| {
        digits.len() == 17
            && digits
                .chars()
                .all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase())
    })
}

/// Whether `id` is shaped like the `MaintenanceWindowTargetId` it mints (the 8-4-4-4-12
/// hexadecimal of that model's own pattern).
fn is_target_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(group, width)| {
                group.len() == width && group.chars().all(|digit| digit.is_ascii_hexdigit())
            })
}

/// The error code an awsJson answer carries, as a client branches on it.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|code| code.rsplit('#').next().unwrap_or(code).to_string())
        .unwrap_or_default()
}

/// RULE: a stack declaring a maintenance window and a target of it deploys, and both resources
/// are the ones Systems Manager holds under the ids that service minted and the stack
/// published — until the stack is deleted, which removes them there.
#[tokio::test]
async fn a_declared_maintenance_window_and_its_target_are_created_in_systems_manager() {
    let router = router();
    for row in ROWS {
        let case = row.case;
        let template = (row.template)();
        let (status, created) = call(
            &router,
            "CreateStack",
            &[
                member("StackName", row.stack_name),
                member("TemplateBody", &template),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a template declaring these types must be admitted, got {status}: \
             {created}"
        );

        let described = settled(&router, case, row.stack_name).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        let window_id = physical_id(&router, case, row.stack_name, row.window_logical_id).await;
        let target_id = physical_id(&router, case, row.stack_name, row.target_logical_id).await;
        assert!(
            is_window_id(&window_id),
            "case {case:?}: expected the window's physical id to be the id Systems Manager \
             mints, got {window_id:?}"
        );
        assert!(
            is_target_id(&target_id),
            "case {case:?}: expected the target's physical id to be the id Systems Manager \
             mints, got {target_id:?}"
        );

        // What `Ref` publishes for each resource is that identifier, so a template that feeds
        // one to another resource — as this one feeds the window to its target — hands it the
        // value Systems Manager answers to.
        assert_eq!(
            output(&described, "WindowId"),
            Some(window_id.clone()),
            "case {case:?}: expected Ref of the window to be the id it was created under, got \
             {described}"
        );
        assert_eq!(
            output(&described, "TargetId"),
            Some(target_id.clone()),
            "case {case:?}: expected Ref of the target to be the id it was registered under, \
             got {described}"
        );

        // The WINDOW, through Systems Manager's own read of one.
        let (status, window) = ssm(
            &router,
            "GetMaintenanceWindow",
            &json!({ "WindowId": window_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the window the stack created must be readable, got {status}: \
             {window}"
        );
        for (member, expected) in [
            ("Name", json!(row.expected_window_name)),
            ("Schedule", json!(row.expected_schedule)),
            ("Duration", json!(row.expected_duration)),
            ("Cutoff", json!(row.expected_cutoff)),
            ("AllowUnassociatedTargets", json!(row.expected_unassociated)),
        ] {
            assert_eq!(
                window[member], expected,
                "case {case:?}: expected {member} {expected}, got {window}"
            );
        }
        assert_eq!(
            window["ScheduleTimezone"].as_str(),
            row.expected_timezone,
            "case {case:?}: expected the declared time zone, got {window}"
        );

        // The TARGET, through the one read Systems Manager publishes for one.
        let (status, listed) = ssm(
            &router,
            "DescribeMaintenanceWindowTargets",
            &json!({ "WindowId": window_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the window's targets must be readable, got {status}: {listed}"
        );
        let targets = listed["Targets"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            targets.len(),
            1,
            "case {case:?}: expected the one target the stack registered, got {listed}"
        );
        let held = &targets[0];
        assert_eq!(
            held["WindowTargetId"].as_str(),
            Some(target_id.as_str()),
            "case {case:?}: expected the target the stack published, got {held}"
        );
        assert_eq!(
            held["WindowId"].as_str(),
            Some(window_id.as_str()),
            "case {case:?}: expected the target to belong to the declared window, got {held}"
        );
        assert_eq!(
            held["ResourceType"].as_str(),
            Some(row.expected_resource_type),
            "case {case:?}: expected the declared resource type, got {held}"
        );
        assert_eq!(
            held["Targets"],
            json!([{
                "Key": row.expected_target_key,
                "Values": row.expected_target_values,
            }]),
            "case {case:?}: expected the declared criteria, got {held}"
        );
        for (name, expected) in [
            ("OwnerInformation", row.expected_owner_information),
            ("Name", row.expected_target_name),
            ("Description", row.expected_target_description),
        ] {
            assert_eq!(
                held[name].as_str(),
                expected,
                "case {case:?}: expected {name} {expected:?}, got {held}"
            );
        }

        // Deleting the stack removes both, in the service that holds them.
        let (status, deleted) = call(
            &router,
            "DeleteStack",
            &[member("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack must be served, got {status}: {deleted}"
        );
        let gone = Instant::now() + TERMINAL_DEADLINE;
        loop {
            let (_, read) = ssm(
                &router,
                "GetMaintenanceWindow",
                &json!({ "WindowId": window_id }),
            )
            .await;
            if error_code(&read) == "DoesNotExistException" {
                break;
            }
            assert!(
                Instant::now() < gone,
                "case {case:?}: expected the deleted stack's window to be gone in Systems \
                 Manager, still {read}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let (_, listed) = ssm(
            &router,
            "DescribeMaintenanceWindowTargets",
            &json!({ "WindowId": window_id }),
        )
        .await;
        assert_eq!(
            error_code(&listed),
            "DoesNotExistException",
            "case {case:?}: expected the deleted window to hold no targets to list, got {listed}"
        );
    }
}

/// RULE: the create is the OWNING service's, so its refusal is the resource's failure — a
/// target registered with a window Systems Manager holds none of fails that resource and rolls
/// the stack back rather than reporting a target nothing registered.
#[tokio::test]
async fn a_target_of_a_window_systems_manager_does_not_hold_fails_the_resource() {
    let router = router();
    let case = "a target of an absent window";
    let stack_name = "mw-absent-window";
    // A well-formed `MaintenanceWindowId` — the modelled pattern, so the request reaches that
    // service's handler — that no create of this Region minted.
    let absent = "mw-0123456789abcdef0";
    let template = json!({
        "Resources": {
            "OrphanTarget": {
                "Type": "AWS::SSM::MaintenanceWindowTarget",
                "Properties": {
                    "WindowId": absent,
                    "ResourceType": "INSTANCE",
                    "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }]
                }
            }
        }
    })
    .to_string();

    let (status, created) = call(
        &router,
        "CreateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &template),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the template is admitted — the type is one CloudFormation publishes — \
         got {status}: {created}"
    );

    let described = settled(&router, case, stack_name).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the stack to roll back rather than report a target nothing \
         registered, got {described}"
    );
    let reason = xml_text(&described, "StackStatusReason").unwrap_or_default();
    assert!(
        reason.contains("OrphanTarget"),
        "case {case:?}: expected the failure to name the resource that failed, got {reason:?}"
    );
}
