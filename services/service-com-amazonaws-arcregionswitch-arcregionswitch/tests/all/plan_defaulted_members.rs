// SPDX-License-Identifier: CC0-1.0

//! The members of a plan this service MATERIALISES, driven over the wire.
//!
//! THE RULE UNDER TEST: a plan carries its application-health alarm set
//! (`Plan$associatedAlarms`, "The associated application health alarms for a plan"), its
//! trigger list (`Plan$triggers`, "The triggers for a plan") and its service-quota-check
//! flag (`Plan$serviceQuotaChecksEnabled`) on EVERY read, whatever the write that stored it
//! supplied: a plan configured with no alarms reports an empty map and one configured with
//! no triggers an empty list — an empty collection, not an absent member — and a plan whose
//! write said nothing about quota checks reports the service default, which is that they
//! are on. A write that DOES supply one of them stores what it supplied.
//!
//! It is a rule about the stored record, so it holds of every read of the plan: the `plan`
//! `CreatePlan` and `UpdatePlan` answer with, the one `GetPlan` reports, and the one
//! embedded in a `GetPlanExecution`.

use serde_json::{json, Value};

use crate::switch_support::{approval_step, plan_request, role_arn, start_execution, workflow_of, Emulator, ACCOUNT};

/// The two Regions every plan here covers.
const REGIONS: [&str; 2] = ["us-east-1", "us-west-2"];

/// One alarm a plan associates, in the form `AssociatedAlarmMap`'s value takes
/// (`smithy_query(query="shape", shape="AssociatedAlarm",
/// model_path="services/service-com-amazonaws-arcregionswitch-arcregionswitch/smithy-model/com-amazonaws-arcregionswitch-arcregionswitch.json")`:
/// `resourceIdentifier` and `alarmType` are required, `alarmType` one of
/// `applicationHealth | trigger`).
fn alarms() -> Value {
    json!({
        "health": {
            "resourceIdentifier": format!("arn:aws:cloudwatch:us-east-1:{ACCOUNT}:alarm:app-health"),
            "alarmType": "applicationHealth",
        }
    })
}

/// One trigger a plan carries, in the form `TriggerList`'s member takes
/// (`smithy_query(query="shape", shape="Trigger",
/// model_path="services/service-com-amazonaws-arcregionswitch-arcregionswitch/smithy-model/com-amazonaws-arcregionswitch-arcregionswitch.json")`:
/// `targetRegion`, `action`, `conditions` and `minDelayMinutesBetweenExecutions` are
/// required, and a condition names an alarm and the `red | green` condition it must be in).
fn triggers() -> Value {
    json!([{
        "description": "shift away when the alarm is red",
        "targetRegion": "us-west-2",
        "action": "activate",
        "conditions": [{"associatedAlarmName": "health", "condition": "red"}],
        "minDelayMinutesBetweenExecutions": 30,
    }])
}

/// One row of the table: which of the three members the write supplies, and what every read
/// of the plan then reports for each of them.
struct Row {
    case: &'static str,
    /// The members added to the `CreatePlan`/`UpdatePlan` request.
    supplied: fn() -> Vec<(&'static str, Value)>,
    expected_alarms: fn() -> Value,
    expected_triggers: fn() -> Value,
    expected_quota_checks: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "none of the three supplied",
        supplied: Vec::new,
        expected_alarms: || json!({}),
        expected_triggers: || json!([]),
        expected_quota_checks: true,
    },
    Row {
        case: "quota checks turned off explicitly",
        supplied: || vec![("serviceQuotaChecksEnabled", json!(false))],
        expected_alarms: || json!({}),
        expected_triggers: || json!([]),
        expected_quota_checks: false,
    },
    Row {
        case: "quota checks turned on explicitly",
        supplied: || vec![("serviceQuotaChecksEnabled", json!(true))],
        expected_alarms: || json!({}),
        expected_triggers: || json!([]),
        expected_quota_checks: true,
    },
    Row {
        case: "an alarm but no trigger",
        supplied: || vec![("associatedAlarms", alarms())],
        expected_alarms: alarms,
        expected_triggers: || json!([]),
        expected_quota_checks: true,
    },
    Row {
        case: "a trigger but no alarm",
        supplied: || vec![("triggers", triggers())],
        expected_alarms: || json!({}),
        expected_triggers: triggers,
        expected_quota_checks: true,
    },
    Row {
        case: "an empty alarm map and an empty trigger list, sent explicitly",
        supplied: || vec![("associatedAlarms", json!({})), ("triggers", json!([]))],
        expected_alarms: || json!({}),
        expected_triggers: || json!([]),
        expected_quota_checks: true,
    },
    Row {
        case: "all three supplied",
        supplied: || {
            vec![
                ("associatedAlarms", alarms()),
                ("triggers", triggers()),
                ("serviceQuotaChecksEnabled", json!(false)),
            ]
        },
        expected_alarms: alarms,
        expected_triggers: triggers,
        expected_quota_checks: false,
    },
];

/// Assert the three members of `plan`, as `read` reports them.
fn assert_members(case: &str, read: &str, plan: &Value, row: &Row) {
    for (member, expected) in [
        ("associatedAlarms", (row.expected_alarms)()),
        ("triggers", (row.expected_triggers)()),
        (
            "serviceQuotaChecksEnabled",
            json!(row.expected_quota_checks),
        ),
    ] {
        assert_eq!(
            plan[member], expected,
            "case {case:?}: {read} expected {member} {expected}, got {}",
            plan[member]
        );
    }
}

/// RULE: every read of a plan reports its alarm set, its trigger list and its quota-check
/// flag, defaulted on the write that stored it.
#[tokio::test]
async fn every_read_of_a_plan_reports_its_alarms_triggers_and_quota_check_flag() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        // The plan's one workflow is a manual approval, so an execution of it stands
        // waiting rather than ending, which is the state a `GetPlanExecution` read of the
        // embedded plan is taken in.
        let workflows = json!([workflow_of("activate", vec![approval_step("Approve")])]);
        let mut request = plan_request(
            &format!("defaults-{index}"),
            REGIONS,
            &role_arn("ArcRegionSwitchRole"),
            workflows.clone(),
        );
        for (member, value) in (row.supplied)() {
            request[member] = value;
        }

        let (status, created) = emulator.call("CreatePlan", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreatePlan expected 200, got {status} with {created}"
        );
        assert_members(case, "CreatePlan", &created["plan"], row);
        let plan_arn = created["plan"]["arn"].as_str().unwrap_or_default().to_string();

        let (status, got) = emulator
            .call("GetPlan", &json!({"arn": plan_arn}))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetPlan expected 200, got {status} with {got}"
        );
        assert_members(case, "GetPlan", &got["plan"], row);

        // The plan a plan EXECUTION reports is the same stored record.
        let execution_id = start_execution(&emulator, case, &plan_arn, "us-west-2").await;
        let (status, execution) = emulator
            .call(
                "GetPlanExecution",
                &json!({"planArn": plan_arn, "executionId": execution_id}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetPlanExecution expected 200, got {status} with {execution}"
        );
        assert_members(case, "GetPlanExecution", &execution["plan"], row);

        // An UPDATE is the same write: it replaces the plan's mutable configuration, and
        // the members it does not supply are materialised exactly as creating one does.
        let (status, updated) = emulator
            .call(
                "UpdatePlan",
                &json!({
                    "arn": plan_arn,
                    "workflows": workflows,
                    "executionRole": role_arn("ArcRegionSwitchRole"),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdatePlan expected 200, got {status} with {updated}"
        );
        let defaulted = Row {
            case,
            supplied: Vec::new,
            expected_alarms: || json!({}),
            expected_triggers: || json!([]),
            expected_quota_checks: true,
        };
        assert_members(case, "UpdatePlan", &updated["plan"], &defaulted);

        let (status, after) = emulator
            .call("GetPlan", &json!({"arn": plan_arn}))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetPlan after the update expected 200, got {status} with {after}"
        );
        assert_members(case, "GetPlan after the update", &after["plan"], &defaulted);
    }
}
