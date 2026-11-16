// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateAnomalyMonitor`: what a rename writes to the monitor
//! registry, what `GetAnomalyMonitors` reports after it, and how an ARN the account holds
//! no monitor under is refused.
//!
//! Every test drives the service over its real wire protocol through the shared
//! [`crate::harness`], so nothing here reaches past the HTTP surface a client sees.

use serde_json::{json, Value};
use service_com_amazonaws_costexplorer_awsinsightsindexservice::state::{
    MONITOR_ID_AS_RESOURCE, MONITOR_TYPE_CUSTOM, MONITOR_TYPE_DIMENSIONAL,
};

use crate::harness::{error_code, error_message, Emulator};

/// The sentence Cost Explorer refuses a monitor name that is empty with. Fixed text: it
/// names the member and the condition on it, and nothing from the request.
const EMPTY_NAME_REFUSAL: &str = "MonitorName cannot by empty";

/// The status Cost Explorer serves an unknown-monitor refusal at — not the 404 the shape's
/// `@httpError` declares; see [`crate::unknown_monitor_references`], which asserts that
/// rule for the operations that resolved a monitor before this one did.
const UNKNOWN_MONITOR_STATUS: u16 = 400;

/// One monitor to rename: how it was created, and the name the update sets.
struct Case {
    label: &'static str,
    created: fn(&str) -> Value,
    new_name: &'static str,
}

/// A managed monitor body: the Amazon Web Services managed monitor an account holds one of.
fn managed(name: &str) -> Value {
    json!({"AnomalyMonitor": {
        "MonitorName": name,
        "MonitorType": MONITOR_TYPE_DIMENSIONAL,
        "MonitorDimension": "SERVICE",
    }})
}

/// A managed monitor over a key-configured dimension, with the specification
/// `AnomalyMonitor$MonitorSpecification` documents for `TAG`.
fn managed_over_a_tag(name: &str) -> Value {
    json!({"AnomalyMonitor": {
        "MonitorName": name,
        "MonitorType": MONITOR_TYPE_DIMENSIONAL,
        "MonitorDimension": "TAG",
        "MonitorSpecification": {"Tags": {"Key": "CostCenter"}},
    }})
}

/// A customer managed monitor body, with the cost-allocation-tag specification
/// `AnomalyMonitor$MonitorSpecification` documents for one.
fn custom(name: &str) -> Value {
    json!({"AnomalyMonitor": {
        "MonitorName": name,
        "MonitorType": MONITOR_TYPE_CUSTOM,
        "MonitorSpecification": {"Tags": {"Key": "Team", "Values": ["platform"]}},
    }})
}

/// The monitors a rename is asserted over, and the shapes of name it can be given.
///
/// `MonitorName` is a `GenericString` of length 0..1024 with pattern `^[\S\s]*$`
/// (`smithy_query(query="operation", operation="UpdateAnomalyMonitor")`), so the names span
/// that class: a plain rename, one carrying spaces and punctuation, and a one-character
/// name. The length bound itself is spanned in
/// [`the_name_a_rename_sets_must_be_one_the_member_admits`].
const CASES: &[Case] = &[
    Case {
        label: "a managed monitor renamed",
        created: managed,
        new_name: "managed-monitor-renamed",
    },
    Case {
        label: "a managed monitor over a tag key renamed",
        created: managed_over_a_tag,
        new_name: "tag monitor, renamed (production)",
    },
    Case {
        label: "a customer managed monitor renamed",
        created: custom,
        new_name: "c",
    },
];

/// Every monitor the account holds, in the order `GetAnomalyMonitors` reports them.
async fn all_monitors(emulator: &Emulator) -> Vec<Value> {
    emulator.ok("GetAnomalyMonitors", &json!({})).await["AnomalyMonitors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The monitor `arn` names, as `GetAnomalyMonitors` reports it.
async fn monitor(emulator: &Emulator, label: &str, arn: &str) -> Value {
    all_monitors(emulator)
        .await
        .into_iter()
        .find(|monitor| monitor["MonitorArn"].as_str() == Some(arn))
        .unwrap_or_else(|| panic!("case {label:?}: expected the monitor {arn:?} to be reported"))
}

/// RULE: `UpdateAnomalyMonitor` writes the `MonitorName` it names onto the monitor its
/// `MonitorArn` resolves to and refreshes that monitor's `LastUpdatedDate`, so a later
/// `GetAnomalyMonitors` reports the new name — and nothing else about the monitor, nor any
/// other monitor the account holds, changes.
///
/// The table spans the monitors a rename applies to (Amazon Web Services managed, managed
/// over a key-configured dimension, customer managed) and the names the member's modelled
/// constraints admit.
#[tokio::test]
async fn a_renamed_monitor_is_reported_under_its_new_name() {
    for case in CASES {
        let label = case.label;
        let emulator = Emulator::start();
        // A second monitor the update does not name, so "nothing else changes" is checked
        // against a registry holding more than the renamed monitor.
        let untouched_arn = emulator.ok("CreateAnomalyMonitor", &custom("untouched")).await
            ["MonitorArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let created_body = (case.created)("created-name");
        let created = emulator.ok("CreateAnomalyMonitor", &created_body).await;
        let arn = created["MonitorArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label:?}: MonitorArn must be present, got {created}"))
            .to_string();
        let before = monitor(&emulator, label, &arn).await;

        let answered = emulator
            .ok(
                "UpdateAnomalyMonitor",
                &json!({"MonitorArn": arn, "MonitorName": case.new_name}),
            )
            .await;
        assert_eq!(
            answered["MonitorArn"].as_str(),
            Some(arn.as_str()),
            "case {label:?}: the update answers with the monitor's own ARN, got {answered}"
        );

        let after = monitor(&emulator, label, &arn).await;
        assert_eq!(
            after["MonitorName"].as_str(),
            Some(case.new_name),
            "case {label:?}: the monitor must be reported under the name the update set, \
             got {after}"
        );
        // Everything the update does not address is the monitor it was created with.
        for member in [
            "MonitorArn",
            "MonitorType",
            "MonitorDimension",
            "MonitorSpecification",
            "CreationDate",
            "DimensionalValueCount",
        ] {
            assert_eq!(
                after[member], before[member],
                "case {label:?}: {member} must be left as it was, got {after}"
            );
        }
        // "The date when the monitor was last updated": the update is that write, so the
        // monitor is no longer reported as last written when it was created.
        let last_updated = after["LastUpdatedDate"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label:?}: LastUpdatedDate must be present"));
        assert!(
            last_updated >= before["CreationDate"].as_str().unwrap_or_default(),
            "case {label:?}: the update's date cannot precede the monitor's creation, got \
             {after}"
        );
        assert!(
            after.get("LastEvaluatedDate").is_none(),
            "case {label:?}: an update evaluates no cost data, got {after}"
        );

        let untouched = monitor(&emulator, label, &untouched_arn).await;
        assert_eq!(
            untouched["MonitorName"].as_str(),
            Some("untouched"),
            "case {label:?}: the monitor the update did not name must be left alone, got \
             {untouched}"
        );
    }
}

/// RULE: a rename is a write, so the name a read reports is the one the LAST update set —
/// renaming the same monitor again replaces the name rather than adding to it, and a
/// request naming no new name leaves the monitor exactly as it was.
#[tokio::test]
async fn the_reported_name_is_the_one_the_last_update_set() {
    let emulator = Emulator::start();
    let created = emulator.ok("CreateAnomalyMonitor", &managed("first")).await;
    let arn = created["MonitorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("MonitorArn must be present, got {created}"))
        .to_string();

    for name in ["second", "third", "second"] {
        emulator
            .ok(
                "UpdateAnomalyMonitor",
                &json!({"MonitorArn": arn, "MonitorName": name}),
            )
            .await;
        let reported = monitor(&emulator, name, &arn).await;
        assert_eq!(
            reported["MonitorName"].as_str(),
            Some(name),
            "case {name:?}: the read reports the name the last update set, got {reported}"
        );
    }

    // `MonitorName` is the only member this operation addresses, so a request carrying none
    // changes nothing at all.
    let before = monitor(&emulator, "no new name", &arn).await;
    emulator
        .ok("UpdateAnomalyMonitor", &json!({"MonitorArn": arn}))
        .await;
    assert_eq!(
        monitor(&emulator, "no new name", &arn).await,
        before,
        "an update naming no new name must leave the monitor exactly as it was"
    );
}

/// RULE: a cost anomaly monitor is NAMED, so the empty string `MonitorName`'s modelled
/// length bound admits is refused 400 `ValidationException` with Cost Explorer's fixed
/// sentence wherever a name is set — by the update and by the create alike — while every
/// name inside the member's bound (length 0..1024, pattern `^[\S\s]*$`) is accepted, and the
/// refused update leaves the stored name as it was.
///
/// The table spans the member's length bound: one character, the published maximum, and one
/// past it, beside the empty name.
#[tokio::test]
async fn the_name_a_rename_sets_must_be_one_the_member_admits() {
    /// The longest `MonitorName` the member admits, from its modelled length bound.
    const MAX_NAME_LENGTH: usize = 1024;

    let emulator = Emulator::start();
    let created = emulator.ok("CreateAnomalyMonitor", &managed("named")).await;
    let arn = created["MonitorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("MonitorArn must be present, got {created}"))
        .to_string();

    for (label, name) in [
        ("one character", "n".to_string()),
        ("the longest name the member admits", "n".repeat(MAX_NAME_LENGTH)),
    ] {
        emulator
            .ok(
                "UpdateAnomalyMonitor",
                &json!({"MonitorArn": arn, "MonitorName": name}),
            )
            .await;
        let reported = monitor(&emulator, label, &arn).await;
        assert_eq!(
            reported["MonitorName"].as_str(),
            Some(name.as_str()),
            "case {label:?}: the name the update set must be reported back, got \
             {}",
            reported["MonitorName"]
        );
    }
    let accepted = monitor(&emulator, "the accepted name", &arn).await["MonitorName"].clone();

    // The empty name, and a name one character past the member's bound: both refused, the
    // first with the service's own sentence and the second by the modelled constraint.
    for (label, name) in [
        ("the empty name", String::new()),
        (
            "one character past the longest name the member admits",
            "n".repeat(MAX_NAME_LENGTH + 1),
        ),
    ] {
        let (status, answered) = emulator
            .call(
                "UpdateAnomalyMonitor",
                &json!({"MonitorArn": arn, "MonitorName": name}),
            )
            .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {label:?}: expected a 400 ValidationException, got {status}: {answered}"
        );
        assert_eq!(
            monitor(&emulator, label, &arn).await["MonitorName"],
            accepted,
            "case {label:?}: a refused update must leave the stored name as it was"
        );
    }

    // The empty-name refusal is the service's own, so it carries the service's sentence.
    let (_, answered) = emulator
        .call(
            "UpdateAnomalyMonitor",
            &json!({"MonitorArn": arn, "MonitorName": ""}),
        )
        .await;
    assert_eq!(
        error_message(&answered),
        EMPTY_NAME_REFUSAL,
        "the empty-name refusal carries Cost Explorer's fixed sentence, got {answered}"
    );

    // The same rule on the other operation that sets a name: the create.
    let mut body = managed("placeholder");
    body["AnomalyMonitor"]["MonitorName"] = json!("");
    let (status, answered) = emulator.call("CreateAnomalyMonitor", &body).await;
    assert_eq!(
        (
            status,
            error_code(&answered).as_str(),
            error_message(&answered).as_str()
        ),
        (400, "ValidationException", EMPTY_NAME_REFUSAL),
        "a create naming the empty name is refused the same way, got {status}: {answered}"
    );
}

/// RULE: an ARN the account holds no cost anomaly monitor under is refused
/// `UnknownMonitorException` naming the monitor id under `resourceId` — including one a
/// delete already removed — and the refusal writes nothing.
///
/// The table spans the ways a `MonitorArn` the input's modelled constraints admit (length
/// 0..1024, pattern `^[\S\s]*$`) can name no monitor: a well-formed monitor ARN of this
/// account, one of another account, the ARN of a different resource kind, a value that is no
/// ARN at all, the empty string, and a monitor a delete removed.
#[tokio::test]
async fn an_update_of_a_monitor_the_account_does_not_hold_is_refused() {
    let emulator = Emulator::start();
    let kept = emulator.ok("CreateAnomalyMonitor", &managed("kept")).await["MonitorArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let removed = emulator
        .ok("CreateAnomalyMonitor", &custom("deleted-once"))
        .await["MonitorArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    emulator
        .ok("DeleteAnomalyMonitor", &json!({"MonitorArn": removed}))
        .await;

    let unknown = vec![
        (
            "a well-formed monitor ARN of this account naming no monitor",
            format!(
                "arn:aws:ce::{}:anomalymonitor/00000000-0000-8000-8000-000000000000",
                engine::emulator_account_id()
            ),
        ),
        (
            "a monitor ARN of another account",
            "arn:aws:ce::999999999999:anomalymonitor/11111111-1111-8111-8111-111111111111"
                .to_string(),
        ),
        (
            "the ARN of a different resource kind",
            engine::emulator_arn("ce", "", "costcategory/not-a-monitor"),
        ),
        ("a value that is no ARN at all", "monitor-1".to_string()),
        ("the empty string", String::new()),
        (
            "a monitor an earlier delete already removed",
            removed.clone(),
        ),
    ];

    for (label, arn) in &unknown {
        let (status, answered) = emulator
            .call(
                "UpdateAnomalyMonitor",
                &json!({"MonitorArn": arn, "MonitorName": "renamed-by-a-refused-update"}),
            )
            .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (UNKNOWN_MONITOR_STATUS, "UnknownMonitorException"),
            "case {label:?}: expected a {UNKNOWN_MONITOR_STATUS} UnknownMonitorException, got \
             {status}: {answered}"
        );
        let message = error_message(&answered);
        assert!(
            message.contains(MONITOR_ID_AS_RESOURCE),
            "case {label:?}: the refusal names the id under {MONITOR_ID_AS_RESOURCE:?}, got \
             {message:?}"
        );
    }

    let reported = all_monitors(&emulator).await;
    assert_eq!(
        reported
            .iter()
            .map(|monitor| (
                monitor["MonitorArn"].as_str().unwrap_or_default(),
                monitor["MonitorName"].as_str().unwrap_or_default()
            ))
            .collect::<Vec<_>>(),
        vec![(kept.as_str(), "kept")],
        "a refused update must write nothing"
    );
}
