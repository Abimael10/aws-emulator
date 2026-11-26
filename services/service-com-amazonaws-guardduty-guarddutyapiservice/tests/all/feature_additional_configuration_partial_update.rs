// SPDX-License-Identifier: CC0-1.0

//! A feature's `AdditionalConfiguration` list is a PARTIAL update of that feature's
//! additional configurations, end to end over the wire.
//!
//! Every request goes through `MultiServiceRouter::dispatch` (the harness lives in
//! the `detector_lifecycle` suite), so routing, restJson1 (de)serialization,
//! modelled-constraint validation and the handlers are exercised together.
//!
//! The rule under test, asserted over a SET of inputs rather than one recorded call:
//! each entry of a feature-configuration update's `AdditionalConfiguration` list
//! configures ONE member of the additional-configuration enum, so the list states
//! something about the members it NAMES and nothing about the rest — every named
//! configuration is written over the stored one, and every configuration the list
//! leaves out keeps the status it already had. The same rows hold wherever a request
//! carries such a list over configuration that already exists: the detector's own
//! switches (`UpdateDetector`), a member account's (`UpdateMemberDetectors`) and an
//! organization's auto-enablement (`UpdateOrganizationConfiguration`).
//!
//! And the other side of the same rule: an entry naming a configuration the feature
//! it was given for does not support — which is every name outside the catalog the
//! entry's `Name` member targets — is refused with HTTP 400 under the modelled
//! `BadRequestException` shape, and configures nothing.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::detector_lifecycle::{
    call, create_detector, enabled_detector, error_code, error_shape, new_router, router_over,
    Router,
};
use crate::member_association::{create_members, invite_members, MEMBERS};
use crate::member_detector_configuration::{get_member_detectors, update_member_detectors};
use crate::member_relationship_scope::monitor_member;
use crate::organization_configuration::{describe_in, update_ok_in};

/// The feature whose additional configurations say who manages the runtime agent.
const RUNTIME: &str = "RUNTIME_MONITORING";

/// A second feature, carried alongside so each case also shows that a list naming
/// one feature leaves another feature's own configuration alone.
const S3: &str = "S3_DATA_EVENTS";

const ENABLED: &str = "ENABLED";
const DISABLED: &str = "DISABLED";

/// Every additional configuration a detector request may name: the
/// `FeatureAdditionalConfiguration` members
/// (`smithy_query(shape="FeatureAdditionalConfiguration")`), which
/// `DetectorAdditionalConfiguration.Name` targets. The
/// `OrgFeatureAdditionalConfiguration` catalog a member and an organization request
/// name carries the same four members.
const ADDITIONAL_CONFIGURATIONS: [&str; 4] = [
    "EKS_ADDON_MANAGEMENT",
    "ECS_FARGATE_AGENT_MANAGEMENT",
    "EC2_AGENT_MANAGEMENT",
    "RDS_DATA_RISK",
];

/// The configuration every case below starts from, one entry per member of the
/// catalog and not all at the same status, so a case that retains an entry is
/// distinguishable from one that re-creates it at a default.
const STORED: [(&str, &str); 4] = [
    ("EKS_ADDON_MANAGEMENT", DISABLED),
    ("ECS_FARGATE_AGENT_MANAGEMENT", ENABLED),
    ("EC2_AGENT_MANAGEMENT", ENABLED),
    ("RDS_DATA_RISK", DISABLED),
];

/// [`STORED`] in the name order a read reports the set in.
const STORED_IN_NAME_ORDER: [(&str, &str); 4] = [
    ("EC2_AGENT_MANAGEMENT", ENABLED),
    ("ECS_FARGATE_AGENT_MANAGEMENT", ENABLED),
    ("EKS_ADDON_MANAGEMENT", DISABLED),
    ("RDS_DATA_RISK", DISABLED),
];

/// One row of the partial-update table: what the update's `AdditionalConfiguration`
/// member carries (`None` for a request that omits the member entirely), the
/// `(name, status)` pairs the stored set must then read back as — in the name order
/// a read reports — and why.
type Row = (Option<Value>, [(&'static str, &'static str); 4], &'static str);

/// The rows, differing along the COUNT of entries the update names, WHICH names they
/// are, the status each asks for (both directions, and one that re-states what is
/// stored), a repeated name, and the two shapes of a list that names nothing.
///
/// The expectation of each row is the whole stored set, so a row asserts the entries
/// the update NAMED were written and the entries it did not name were kept.
fn rows() -> Vec<Row> {
    vec![
        (
            Some(json!([{"name": "EKS_ADDON_MANAGEMENT", "status": ENABLED}])),
            [
                ("EC2_AGENT_MANAGEMENT", ENABLED),
                ("ECS_FARGATE_AGENT_MANAGEMENT", ENABLED),
                ("EKS_ADDON_MANAGEMENT", ENABLED),
                ("RDS_DATA_RISK", DISABLED),
            ],
            "one name enabled: the other three are left where they stood",
        ),
        (
            Some(json!([{"name": "EC2_AGENT_MANAGEMENT", "status": DISABLED}])),
            [
                ("EC2_AGENT_MANAGEMENT", DISABLED),
                ("ECS_FARGATE_AGENT_MANAGEMENT", ENABLED),
                ("EKS_ADDON_MANAGEMENT", DISABLED),
                ("RDS_DATA_RISK", DISABLED),
            ],
            "one name disabled",
        ),
        (
            Some(json!([{"name": "RDS_DATA_RISK", "status": ENABLED}])),
            [
                ("EC2_AGENT_MANAGEMENT", ENABLED),
                ("ECS_FARGATE_AGENT_MANAGEMENT", ENABLED),
                ("EKS_ADDON_MANAGEMENT", DISABLED),
                ("RDS_DATA_RISK", ENABLED),
            ],
            "a configuration that is not an agent-management one",
        ),
        (
            Some(json!([{"name": "ECS_FARGATE_AGENT_MANAGEMENT", "status": ENABLED}])),
            [
                ("EC2_AGENT_MANAGEMENT", ENABLED),
                ("ECS_FARGATE_AGENT_MANAGEMENT", ENABLED),
                ("EKS_ADDON_MANAGEMENT", DISABLED),
                ("RDS_DATA_RISK", DISABLED),
            ],
            "one name re-stated at the status it already carries",
        ),
        (
            Some(json!([{"name": "ECS_FARGATE_AGENT_MANAGEMENT", "status": DISABLED},
                        {"name": "RDS_DATA_RISK", "status": ENABLED}])),
            [
                ("EC2_AGENT_MANAGEMENT", ENABLED),
                ("ECS_FARGATE_AGENT_MANAGEMENT", DISABLED),
                ("EKS_ADDON_MANAGEMENT", DISABLED),
                ("RDS_DATA_RISK", ENABLED),
            ],
            "two names, in neither the stored nor the alphabetical order",
        ),
        (
            Some(json!([{"name": "EKS_ADDON_MANAGEMENT", "status": ENABLED},
                        {"name": "ECS_FARGATE_AGENT_MANAGEMENT", "status": DISABLED},
                        {"name": "EC2_AGENT_MANAGEMENT", "status": DISABLED},
                        {"name": "RDS_DATA_RISK", "status": ENABLED}])),
            [
                ("EC2_AGENT_MANAGEMENT", DISABLED),
                ("ECS_FARGATE_AGENT_MANAGEMENT", DISABLED),
                ("EKS_ADDON_MANAGEMENT", ENABLED),
                ("RDS_DATA_RISK", ENABLED),
            ],
            "every name the catalog carries: a full replacement is the same thing",
        ),
        (
            Some(json!([{"name": "EKS_ADDON_MANAGEMENT", "status": ENABLED},
                        {"name": "EKS_ADDON_MANAGEMENT", "status": DISABLED}])),
            [
                ("EC2_AGENT_MANAGEMENT", ENABLED),
                ("ECS_FARGATE_AGENT_MANAGEMENT", ENABLED),
                ("EKS_ADDON_MANAGEMENT", DISABLED),
                ("RDS_DATA_RISK", DISABLED),
            ],
            "one name twice: it is one configuration and the last entry wins",
        ),
        (
            Some(json!([])),
            STORED_IN_NAME_ORDER,
            "an empty list names nothing, so it changes nothing",
        ),
        (
            None,
            STORED_IN_NAME_ORDER,
            "no additionalConfiguration member at all",
        ),
    ]
}

/// The `additionalConfiguration` entries [`STORED`] as a request carries them.
fn stored_entries() -> Value {
    Value::Array(
        STORED
            .iter()
            .map(|(name, status)| json!({"name": name, "status": status}))
            .collect(),
    )
}

/// One feature entry of a request, carrying `additional` when the row gives one.
fn feature_entry(name: &str, status: &str, additional: Option<Value>) -> Value {
    let mut entry = json!({"name": name, "status": status});
    if let Some(additional) = additional {
        entry["additionalConfiguration"] = additional;
    }
    entry
}

/// The `(name, status)` pairs a reported feature list holds for one feature's
/// additional configurations.
fn reported_additional(reported: &Value, feature: &str, case: &str) -> Vec<(String, String)> {
    let entry = reported["features"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a features list, got {reported:?}"))
        .iter()
        .find(|entry| entry["name"].as_str() == Some(feature))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the {feature} feature, got {reported:?}")
        });
    entry["additionalConfiguration"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|cfg| {
            (
                cfg["name"].as_str().unwrap_or_default().to_string(),
                cfg["status"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The status a reported feature list holds for one feature.
fn reported_status(reported: &Value, feature: &str, case: &str) -> String {
    reported["features"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a features list, got {reported:?}"))
        .iter()
        .find(|entry| entry["name"].as_str() == Some(feature))
        .and_then(|entry| entry["status"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected the {feature} feature, got {reported:?}"))
        .to_string()
}

/// A row's expectation as the pairs a read reports.
fn want(expected: &[(&str, &str); 4]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(name, status)| (name.to_string(), status.to_string()))
        .collect()
}

/// `GetDetector` over the wire.
async fn get_detector(router: &Router, detector_id: &str, case: &str) -> Value {
    let (status, body) = call(router, "GET", &format!("/detector/{detector_id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: get-detector expected 200, got {status} {body:?}"
    );
    body
}

/// A detector created with [`STORED`] on [`RUNTIME`], read back to prove the create
/// side stored the whole set this suite's updates then partially change.
async fn detector_with_stored_configuration(router: &Router, case: &str) -> String {
    let (status, body) = create_detector(
        router,
        json!({
            "enable": true,
            "features": [
                feature_entry(RUNTIME, ENABLED, Some(stored_entries())),
                feature_entry(S3, ENABLED, None),
            ],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: create-detector expected 200, got {status} {body:?}"
    );
    let detector_id = body["detectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a detectorId, got {body:?}"))
        .to_string();
    let read = get_detector(router, &detector_id, case).await;
    assert_eq!(
        reported_additional(&read, RUNTIME, case),
        want(&STORED_IN_NAME_ORDER),
        "case {case:?}: the create must store every entry it named, got {read:?}"
    );
    detector_id
}

// ---------------------------------------------------------------------------
// The detector's own switches: UpdateDetector.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_update_configures_only_the_additional_configurations_it_names() {
    for (additional, expected, why) in rows() {
        let case = format!("update-detector additionalConfiguration {additional:?} ({why})");
        let router = new_router();
        let detector_id = detector_with_stored_configuration(&router, &case).await;

        let (status, body) = call(
            &router,
            "POST",
            &format!("/detector/{detector_id}"),
            Some(json!({
                "features": [
                    feature_entry(RUNTIME, ENABLED, additional.clone()),
                    feature_entry(S3, DISABLED, None),
                ],
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: update-detector expected 200, got {status} {body:?}"
        );

        let read = get_detector(&router, &detector_id, &case).await;
        let got = reported_additional(&read, RUNTIME, &case);
        assert_eq!(
            got,
            want(&expected),
            "case {case:?}: expected the named entries written and the unnamed ones kept, got \
             {got:?}"
        );
        assert!(
            got.len() <= ADDITIONAL_CONFIGURATIONS.len(),
            "case {case:?}: the stored set is never longer than the catalog, got {got:?}"
        );
        assert_eq!(
            reported_status(&read, RUNTIME, &case),
            ENABLED,
            "case {case:?}: the feature's own status is what the update named, got {read:?}"
        );
        assert_eq!(
            reported_status(&read, S3, &case),
            DISABLED,
            "case {case:?}: a feature named without additional configuration is still \
             configured, got {read:?}"
        );
        assert_eq!(
            reported_additional(&read, S3, &case),
            Vec::<(String, String)>::new(),
            "case {case:?}: a feature nothing configured an additional configuration on holds \
             none, got {read:?}"
        );
    }
}

/// Partial updates COMPOSE: two updates naming different configurations leave the
/// detector holding both changes, which is what a read-modify-write client that
/// sends only its delta depends on.
#[tokio::test]
async fn successive_partial_updates_accumulate() {
    let case = "two successive partial updates";
    let router = new_router();
    let detector_id = detector_with_stored_configuration(&router, case).await;

    for (name, status) in [
        ("EKS_ADDON_MANAGEMENT", ENABLED),
        ("RDS_DATA_RISK", ENABLED),
    ] {
        let (code, body) = call(
            &router,
            "POST",
            &format!("/detector/{detector_id}"),
            Some(json!({
                "features": [feature_entry(
                    RUNTIME,
                    ENABLED,
                    Some(json!([{"name": name, "status": status}])),
                )],
            })),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: update-detector {name} expected 200, got {code} {body:?}"
        );
    }

    let read = get_detector(&router, &detector_id, case).await;
    let got = reported_additional(&read, RUNTIME, case);
    assert_eq!(
        got,
        want(&[
            ("EC2_AGENT_MANAGEMENT", ENABLED),
            ("ECS_FARGATE_AGENT_MANAGEMENT", ENABLED),
            ("EKS_ADDON_MANAGEMENT", ENABLED),
            ("RDS_DATA_RISK", ENABLED),
        ]),
        "case {case:?}: expected both deltas to hold, got {got:?}"
    );
}

// ---------------------------------------------------------------------------
// A member account's switches: UpdateMemberDetectors.
// ---------------------------------------------------------------------------

/// An enabled detector holding [`MEMBERS`] in the MONITORED relationship — the
/// relationship a member account's own detector is configured in.
async fn detector_with_monitored_member(router: &Router, engine: &engine::InMemoryEngine) -> String {
    let detector_id = enabled_detector(router).await;
    let (status, body) = create_members(router, &detector_id, &MEMBERS).await;
    assert_eq!(
        status, 200,
        "create-members: expected 200, got {status} {body:?}"
    );
    let ids: Vec<&str> = MEMBERS.iter().map(|(id, _)| *id).collect();
    let (status, body) = invite_members(router, &detector_id, &ids).await;
    assert_eq!(
        status, 200,
        "invite-members: expected 200, got {status} {body:?}"
    );
    for (account_id, _) in MEMBERS {
        monitor_member(engine, &detector_id, account_id);
    }
    detector_id
}

/// The configuration `GetMemberDetectors` reports for one account.
async fn member_configuration(
    router: &Router,
    detector_id: &str,
    account_id: &str,
    case: &str,
) -> Value {
    let (status, body) = get_member_detectors(router, detector_id, &[account_id]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: get-member-detectors expected 200, got {status} {body:?}"
    );
    body["members"]
        .as_array()
        .and_then(|members| {
            members
                .iter()
                .find(|entry| entry["accountId"].as_str() == Some(account_id))
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a configuration for {account_id}, got {body:?}")
        })
        .clone()
}

#[tokio::test]
async fn a_member_update_configures_only_the_additional_configurations_it_names() {
    let account_id = MEMBERS[0].0;
    for (additional, expected, why) in rows() {
        let case =
            format!("update-member-detectors additionalConfiguration {additional:?} ({why})");
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_over(Arc::clone(&engine));
        let detector_id = detector_with_monitored_member(&router, &engine).await;

        // The configuration the partial update below is applied over.
        let (status, body) = update_member_detectors(
            &router,
            &detector_id,
            &[account_id],
            json!({"features": [
                {"name": "EKS_RUNTIME_MONITORING", "status": DISABLED},
                feature_entry(RUNTIME, ENABLED, Some(stored_entries())),
            ]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first update expected 200, got {status} {body:?}"
        );
        let read = member_configuration(&router, &detector_id, account_id, &case).await;
        assert_eq!(
            reported_additional(&read, RUNTIME, &case),
            want(&STORED_IN_NAME_ORDER),
            "case {case:?}: the first update must store every entry it named, got {read:?}"
        );

        let (status, body) = update_member_detectors(
            &router,
            &detector_id,
            &[account_id],
            json!({"features": [feature_entry(RUNTIME, ENABLED, additional.clone())]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the partial update expected 200, got {status} {body:?}"
        );

        let read = member_configuration(&router, &detector_id, account_id, &case).await;
        let got = reported_additional(&read, RUNTIME, &case);
        assert_eq!(
            got,
            want(&expected),
            "case {case:?}: expected the named entries written and the unnamed ones kept, got \
             {got:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The organization's auto-enablement: UpdateOrganizationConfiguration.
// ---------------------------------------------------------------------------

/// The Region this suite's organization calls address.
const REGION: &str = "us-east-1";

/// The auto-enablement values an organization's additional configuration carries:
/// the `OrgFeatureStatus` members (`smithy_query(shape="OrgFeatureStatus")`).
const AUTO_ENABLE: [&str; 3] = ["NEW", "ALL", "NONE"];

/// The organization's stored additional configurations, one per catalog member and
/// not all at the same auto-enablement.
fn stored_org_entries() -> Value {
    Value::Array(
        ADDITIONAL_CONFIGURATIONS
            .iter()
            .enumerate()
            .map(|(at, name)| {
                json!({"name": name, "autoEnable": AUTO_ENABLE[at % AUTO_ENABLE.len()]})
            })
            .collect(),
    )
}

/// The `(name, autoEnable)` pairs a reported organization feature list holds for one
/// feature's additional configurations.
fn reported_org_additional(reported: &Value, feature: &str, case: &str) -> Vec<(String, String)> {
    let entry = reported["features"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a features list, got {reported:?}"))
        .iter()
        .find(|entry| entry["name"].as_str() == Some(feature))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the {feature} feature, got {reported:?}")
        });
    entry["additionalConfiguration"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|cfg| {
            (
                cfg["name"].as_str().unwrap_or_default().to_string(),
                cfg["autoEnable"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn an_organization_update_configures_only_the_additional_configurations_it_names() {
    // Rows differing along the auto-enablement the partial update asks for, which is
    // every `OrgFeatureStatus` member, over the name the stored set holds at a
    // different value.
    for asked_for in AUTO_ENABLE {
        let case = format!("update-organization-configuration EKS_ADDON_MANAGEMENT {asked_for}");
        let router = new_router();
        let detector_id = enabled_detector(&router).await;
        update_ok_in(
            &router,
            REGION,
            &detector_id,
            json!({
                "autoEnableOrganizationMembers": "NEW",
                "features": [{
                    "name": RUNTIME,
                    "autoEnable": "ALL",
                    "additionalConfiguration": stored_org_entries(),
                }],
            }),
        )
        .await;

        update_ok_in(
            &router,
            REGION,
            &detector_id,
            json!({
                "autoEnableOrganizationMembers": "NEW",
                "features": [{
                    "name": RUNTIME,
                    "autoEnable": "ALL",
                    "additionalConfiguration": [
                        {"name": "EKS_ADDON_MANAGEMENT", "autoEnable": asked_for},
                    ],
                }],
            }),
        )
        .await;

        let read = describe_in(&router, REGION, &detector_id).await;
        let got = reported_org_additional(&read, RUNTIME, &case);
        // Every stored name, in the order a read reports them, with only the named
        // one moved to what the partial update asked for.
        let expected: Vec<(String, String)> = {
            let mut pairs: Vec<(String, String)> = ADDITIONAL_CONFIGURATIONS
                .iter()
                .enumerate()
                .map(|(at, name)| {
                    let auto_enable = if *name == "EKS_ADDON_MANAGEMENT" {
                        asked_for
                    } else {
                        AUTO_ENABLE[at % AUTO_ENABLE.len()]
                    };
                    (name.to_string(), auto_enable.to_string())
                })
                .collect();
            pairs.sort();
            pairs
        };
        assert_eq!(
            got, expected,
            "case {case:?}: expected the named configuration written and the unnamed ones kept, \
             got {got:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The other side of the rule: a configuration the feature does not support.
// ---------------------------------------------------------------------------

/// The modelled error SHAPE every GuardDuty client rejection is enveloped in, as
/// `x-amzn-errortype` carries it.
const ENVELOPE: &str = "BadRequestException";

/// The sentence GuardDuty refuses an additional configuration the feature does not
/// support with. Written out here rather than imported, so the suite pins the
/// WORDING a client reads and not just the constant the handler happens to hold.
const UNSUPPORTED: &str =
    "The request was rejected because additional configuration was provided for a feature that does not support it.";

/// Names no additional-configuration catalog carries, so no feature supports them:
/// every row is a value outside the four members
/// (`smithy_query(shape="FeatureAdditionalConfiguration")`), differing along the
/// FORM of the value — a near miss of a catalog member, a feature name, a member of
/// the catalog in the wrong case, the empty string, and one with trailing space.
const UNSUPPORTED_NAMES: [(&str, &str); 5] = [
    ("EKS_ADDON_MANAGEMENTS", "a near-miss of a catalog member"),
    (
        "S3_DATA_EVENTS",
        "a feature name, which is not a configuration",
    ),
    ("eks_addon_management", "a catalog member in the wrong case"),
    ("", "the empty string"),
    (
        "EKS_ADDON_MANAGEMENT ",
        "a catalog member with trailing space",
    ),
];

/// Assert a response is the 400 GuardDuty refuses an unsupported additional
/// configuration with, under the modelled error shape and with the fixed sentence.
fn assert_unsupported(status: u16, body: &Value, case: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} {body:?}"
    );
    assert_eq!(
        error_shape(body),
        ENVELOPE,
        "case {case:?}: expected the {ENVELOPE} envelope, got {body:?}"
    );
    assert_eq!(
        error_code(body),
        ENVELOPE,
        "case {case:?}: expected the {ENVELOPE} class, got {body:?}"
    );
    assert_eq!(
        body["message"].as_str().unwrap_or_default(),
        UNSUPPORTED,
        "case {case:?}: expected GuardDuty's wording for an unsupported additional \
         configuration, got {body:?}"
    );
}

#[tokio::test]
async fn a_configuration_the_feature_does_not_support_is_refused_and_writes_nothing() {
    for (name, why) in UNSUPPORTED_NAMES {
        let case = format!("update-detector additionalConfiguration name {name:?} ({why})");
        let router = new_router();
        let detector_id = detector_with_stored_configuration(&router, &case).await;

        let (status, body) = call(
            &router,
            "POST",
            &format!("/detector/{detector_id}"),
            Some(json!({
                "features": [feature_entry(
                    RUNTIME,
                    ENABLED,
                    Some(json!([{"name": name, "status": ENABLED}])),
                )],
            })),
        )
        .await;
        assert_unsupported(status, &body, &case);

        let read = get_detector(&router, &detector_id, &case).await;
        assert_eq!(
            reported_additional(&read, RUNTIME, &case),
            want(&STORED_IN_NAME_ORDER),
            "case {case:?}: a refused update must configure nothing, got {read:?}"
        );
    }
}

/// The same refusal wherever a request carries such a list: the create, the member
/// accounts' configuration and the organization's auto-enablement.
#[tokio::test]
async fn every_operation_carrying_the_list_refuses_an_unsupported_configuration() {
    let name = UNSUPPORTED_NAMES[0].0;

    // CreateDetector, which carries the same `Features` list.
    let case = format!("create-detector additionalConfiguration name {name:?}");
    let router = new_router();
    let (status, body) = create_detector(
        &router,
        json!({
            "enable": true,
            "features": [feature_entry(
                RUNTIME,
                ENABLED,
                Some(json!([{"name": name, "status": ENABLED}])),
            )],
        }),
    )
    .await;
    assert_unsupported(status, &body, &case);
    let (status, listed) = call(&router, "GET", "/detector", None).await;
    assert_eq!(
        (status, &listed["detectorIds"]),
        (200, &json!([])),
        "case {case:?}: a refused create must store no detector, got {listed:?}"
    );

    // UpdateMemberDetectors.
    let case = format!("update-member-detectors additionalConfiguration name {name:?}");
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_over(Arc::clone(&engine));
    let detector_id = detector_with_monitored_member(&router, &engine).await;
    let account_id = MEMBERS[0].0;
    let (status, body) = update_member_detectors(
        &router,
        &detector_id,
        &[account_id],
        json!({"features": [feature_entry(
            RUNTIME,
            ENABLED,
            Some(json!([{"name": name, "status": ENABLED}])),
        )]}),
    )
    .await;
    assert_unsupported(status, &body, &case);
    let read = member_configuration(&router, &detector_id, account_id, &case).await;
    assert_eq!(
        reported_additional(&read, RUNTIME, &case),
        Vec::<(String, String)>::new(),
        "case {case:?}: a refused update must configure nothing, got {read:?}"
    );

    // UpdateOrganizationConfiguration, whose entries carry an `autoEnable` instead
    // of a `status` and whose catalog is the organization one.
    let case = format!("update-organization-configuration additionalConfiguration name {name:?}");
    let router = new_router();
    let detector_id = enabled_detector(&router).await;
    let (status, body) = call(
        &router,
        "POST",
        &format!("/detector/{detector_id}/admin"),
        Some(json!({
            "autoEnableOrganizationMembers": "NEW",
            "features": [{
                "name": RUNTIME,
                "autoEnable": "ALL",
                "additionalConfiguration": [{"name": name, "autoEnable": "ALL"}],
            }],
        })),
    )
    .await;
    assert_unsupported(status, &body, &case);
    let read = describe_in(&router, REGION, &detector_id).await;
    assert_eq!(
        reported_org_additional(&read, RUNTIME, &case),
        Vec::<(String, String)>::new(),
        "case {case:?}: a refused update must configure nothing, got {read:?}"
    );
}
