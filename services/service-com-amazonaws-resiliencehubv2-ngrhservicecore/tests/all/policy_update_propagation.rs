// SPDX-License-Identifier: CC0-1.0

//! `UpdatePolicy`, and the values OTHER stored resources derive from the policy it edited.
//!
//! The rule under test: a response member whose value is DERIVED from another stored
//! resource is resolved from that resource every time the body is projected, so editing the
//! source resource shows up in the next read of every resource that references it, without
//! those referencing resources being written to. Applied here to the two derivations this
//! slice has:
//!
//! * a `Service`'s `effectivePolicyValues` — one entry per target the attached policy sets,
//!   carrying that target's stored value, the policy's `name` and the account the value came
//!   from — so an `UpdatePolicy` that sets, changes or drops a target changes, adds or
//!   removes that entry for every service whose `policyArn` names the policy;
//! * a `Policy`'s `associatedServiceCount` — the number of services referencing it — which
//!   an update to the policy leaves counting the same services.
//!
//! Each `UpdatePolicy` block the caller sends REPLACES the stored one wholesale and each
//! block it omits is left alone, so a replacement that names fewer target members removes
//! the entries those members used to derive.
//!
//! The derivation table, the wire helpers and the `effectivePolicyValues` assertion are the
//! ones `effective_policy_values.rs` already keeps (both files are modules of this crate's
//! one `tests/all` binary): the rule is spelled once, so neither suite can pass against the
//! other's stale copy of it. What is new here — the `UpdatePolicy` route, the block merge an
//! update performs, and the update table — lives below.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised exactly
//! as a client would exercise them. The suites are TABLES: rows differ along which blocks the
//! update carries, whether a block is added, changed, narrowed or emptied, the modelled
//! bounds of the target values, and each disaster recovery approach — not one recorded
//! request.

use serde_json::{json, Map, Value};

use crate::effective_policy_values::{
    assert_effective, call, create_policy, create_service, derived_from, emulator, get_policy,
    get_service, same_json, service_arn_of, service_of, AVAILABILITY_SLO_MAX,
    AVAILABILITY_SLO_MIN, DISASTER_RECOVERY_APPROACHES, SOURCE_SELF,
};
// The `UpdatePolicy` route is spelled once for this test binary, beside the suite that
// first drove it.
use crate::policy_update_and_deletion::UPDATE_POLICY;
use smithy_server_runtime::MultiServiceRouter;

/// The target blocks an `UpdatePolicy` request can replace.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdatePolicy")` — the request members
/// targeting a shape of policy targets (`availabilitySlo`, `multiAz`, `multiRegion`,
/// `dataRecovery`), as opposed to `policyArn`, `description` and `sharingEnabled`.
const POLICY_TARGET_BLOCKS: [&str; 4] = [
    "availabilitySlo",
    "multiAz",
    "multiRegion",
    "dataRecovery",
];

/// A `CreatePolicy` body: the policy's name, plus the target blocks `blocks` carries.
fn policy_body(name: &str, blocks: &Value) -> Value {
    let mut body = json!({"name": name});
    for (block, value) in blocks.as_object().expect("the blocks are a JSON object") {
        body[block] = value.clone();
    }
    body
}

/// `UpdatePolicy` with `patch` applied to `policy_arn`, answering the `Policy` it reported.
async fn update_policy(
    router: &MultiServiceRouter,
    case: &str,
    policy_arn: &str,
    patch: &Value,
) -> Value {
    let mut body = patch.clone();
    body["policyArn"] = json!(policy_arn);
    let (status, answered) = call(router, "POST", UPDATE_POLICY, Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the update, got {status} {answered:?}"
    );
    let policy = &answered["policy"];
    assert!(
        policy.is_object(),
        "case {case:?}: the @required policy member is absent from {answered:?}"
    );
    policy.clone()
}

/// The policy's target blocks after `patch` is applied to the blocks it was `created` with:
/// each block the patch carries replaces the stored one wholesale, each block it omits is
/// left as stored.
fn merged_blocks(created: &Value, patch: &Value) -> Value {
    let mut merged = Map::new();
    for block in POLICY_TARGET_BLOCKS {
        if let Some(value) = patch.get(block).or_else(|| created.get(block)) {
            merged.insert(block.to_string(), value.clone());
        }
    }
    Value::Object(merged)
}

/// Assert that `policy` carries exactly the target `blocks`, each block reported as stored
/// and each block nothing set reported absent.
fn assert_blocks(case: &str, policy: &Value, blocks: &Value) {
    for block in POLICY_TARGET_BLOCKS {
        let expected = blocks.get(block).cloned().unwrap_or(Value::Null);
        let actual = policy.get(block).cloned().unwrap_or(Value::Null);
        assert!(
            same_json(&actual, &expected),
            "case {case:?}: expected {block} {expected} after the update, got {actual}"
        );
    }
}

/// Create the policy `name`/`blocks` describe and answer its minted ARN.
async fn given_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    blocks: &Value,
) -> String {
    create_policy(router, case, policy_body(name, blocks)).await
}

/// Create a service attached to `policy_arn` (or to no policy) and answer its minted ARN.
async fn given_service(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy_arn: Option<&str>,
) -> String {
    let (status, answered) = create_service(router, name, policy_arn).await;
    service_arn_of(case, &service_of(case, status, &answered))
}

/// The `Service` body stored under `service_arn`.
async fn read_service(router: &MultiServiceRouter, case: &str, service_arn: &str) -> Value {
    let (status, answered) = get_service(router, service_arn).await;
    service_of(case, status, &answered)
}

/// The `Policy` body stored under `policy_arn`.
async fn read_policy(router: &MultiServiceRouter, case: &str, policy_arn: &str) -> Value {
    let (status, answered) = get_policy(router, policy_arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the read, got {status} {answered:?}"
    );
    let policy = &answered["policy"];
    assert!(
        policy.is_object(),
        "case {case:?}: the @required policy member is absent from {answered:?}"
    );
    policy.clone()
}

/// One row of the propagation table: a policy, the blocks it is created with, and the
/// `UpdatePolicy` members sent to it afterwards.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    /// The policy's name — also what every value derived from it reports as `policyName`,
    /// since `UpdatePolicy` carries no `name` member to change it.
    name: &'static str,
    created: Value,
    patch: Value,
}

/// The propagation table.
///
/// Rows differ along what the update does to the policy's targets: adds a block the policy
/// never had, changes a value within a block, narrows a block so entries disappear, empties
/// a block of its target members, leaves a block alone while another changes, sets every
/// block at once at the modelled bounds, and touches no target at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a target block added to a policy that set none",
            name: "PropAddBlock",
            created: json!({}),
            patch: json!({
                "multiAz": {
                    "rtoInMinutes": 15,
                    "rpoInMinutes": 5,
                    "disasterRecoveryApproach": "WARM_STANDBY",
                }
            }),
        },
        Row {
            case: "a target value changed within a block the policy already had",
            name: "PropChangeValue",
            created: json!({"availabilitySlo": {"target": 99.9}}),
            patch: json!({"availabilitySlo": {"target": 99.99}}),
        },
        Row {
            case: "a replacement naming fewer members drops the entries they derived",
            name: "PropNarrowBlock",
            created: json!({
                "multiRegion": {
                    "rtoInMinutes": 240,
                    "rpoInMinutes": 60,
                    "disasterRecoveryApproach": "PILOT_LIGHT",
                }
            }),
            patch: json!({"multiRegion": {"rpoInMinutes": 0}}),
        },
        Row {
            case: "a block emptied of every target member derives nothing",
            name: "PropEmptyBlock",
            created: json!({"multiAz": {"rtoInMinutes": 10, "rpoInMinutes": 5}}),
            patch: json!({"multiAz": {}}),
        },
        Row {
            case: "one block changed while another is left as stored",
            name: "PropOneOfTwo",
            created: json!({
                "multiAz": {"rtoInMinutes": 10, "disasterRecoveryApproach": "HOT_STANDBY"},
                "dataRecovery": {"timeBetweenBackupsInMinutes": 60},
            }),
            patch: json!({"dataRecovery": {"timeBetweenBackupsInMinutes": 1440}}),
        },
        Row {
            case: "every block at once, each at a modelled bound",
            name: "PropEveryBlock",
            created: json!({"availabilitySlo": {"target": 50.0}}),
            patch: json!({
                "availabilitySlo": {"target": AVAILABILITY_SLO_MAX},
                "multiAz": {
                    "rtoInMinutes": 0,
                    "rpoInMinutes": 0,
                    "disasterRecoveryApproach": "ACTIVE_ACTIVE",
                },
                "multiRegion": {
                    "rtoInMinutes": 1440,
                    "rpoInMinutes": 720,
                    "disasterRecoveryApproach": "BACKUP_AND_RESTORE",
                },
                "dataRecovery": {"timeBetweenBackupsInMinutes": 1},
            }),
        },
        Row {
            case: "the availability SLO taken to its modelled minimum",
            name: "PropSloFloor",
            created: json!({"availabilitySlo": {"target": 99.95}}),
            patch: json!({"availabilitySlo": {"target": AVAILABILITY_SLO_MIN}}),
        },
        Row {
            case: "an update that touches no target block changes nothing derived",
            name: "PropNoTargets",
            created: json!({
                "availabilitySlo": {"target": 95.0},
                "multiAz": {"disasterRecoveryApproach": "ACTIVE_ACTIVE"},
            }),
            patch: json!({"description": "edited, targets untouched", "sharingEnabled": true}),
        },
    ]
}

#[tokio::test]
async fn an_update_to_a_policy_rederives_what_every_referencing_service_reports() {
    let router = emulator();

    // A policy no update touches, with a service of its own, and a service with no policy at
    // all: the controls that show an update rederives the values taken from the EDITED
    // policy and nothing else.
    let control_case = "a service attached to a policy no update touches";
    let control_name = "PropControlPolicy";
    let control_blocks = json!({
        "availabilitySlo": {"target": 92.5},
        "multiAz": {"rtoInMinutes": 20, "disasterRecoveryApproach": "HOT_STANDBY"},
    });
    let control_policy = given_policy(&router, control_case, control_name, &control_blocks).await;
    let control_expected = derived_from(&control_blocks, control_name, SOURCE_SELF);
    let control_service =
        given_service(&router, control_case, "Svc-PropControl", Some(&control_policy)).await;
    let bare_service = given_service(&router, control_case, "Svc-PropNoPolicy", None).await;

    for row in rows() {
        let case = row.case;
        let policy_arn = given_policy(&router, case, row.name, &row.created).await;

        // Each row attaches the policy to TWO services, so the propagation is asserted over
        // every service that references the policy rather than just one.
        let before = derived_from(&row.created, row.name, SOURCE_SELF);
        let mut service_arns = Vec::new();
        for suffix in ["One", "Two"] {
            let name = format!("Svc-{}-{suffix}", row.name);
            let service_arn = given_service(&router, case, &name, Some(&policy_arn)).await;
            let created = read_service(&router, case, &service_arn).await;
            assert_effective(case, &created, &before);
            service_arns.push(service_arn);
        }

        let merged = merged_blocks(&row.created, &row.patch);
        let updated = update_policy(&router, case, &policy_arn, &row.patch).await;
        assert_blocks(case, &updated, &merged);

        // Nothing wrote to the services, yet the next read of each derives the policy as it
        // now stands.
        let expected = derived_from(&merged, row.name, SOURCE_SELF);
        for service_arn in &service_arns {
            let read_back = read_service(&router, case, service_arn).await;
            assert_effective(case, &read_back, &expected);
            assert_eq!(
                read_back["policyArn"].as_str(),
                Some(policy_arn.as_str()),
                "case {case:?}: expected the policyArn pointer {policy_arn} to survive the \
                 update, got {:?}",
                read_back["policyArn"]
            );
        }

        // The controls still derive their own policy, and their own nothing.
        let control_read = read_service(&router, control_case, &control_service).await;
        assert_effective(control_case, &control_read, &control_expected);
        let bare_read = read_service(&router, control_case, &bare_service).await;
        assert_effective(control_case, &bare_read, &json!({}));
    }
}

#[tokio::test]
async fn every_disaster_recovery_approach_reached_by_an_update_propagates() {
    // The modelled catalog, reached through an update rather than a create: each policy is
    // created carrying one approach and an RTO, and the update replaces the block with
    // another approach alone — so the approach entry changes and the RTO entry disappears.
    let router = emulator();
    for (index, approach) in DISASTER_RECOVERY_APPROACHES.into_iter().enumerate() {
        for (block, tag) in [("multiAz", "Az"), ("multiRegion", "Region")] {
            let case = format!("{block} updated to {approach}");
            let name = format!("PropApproach{index}{tag}");
            let created = json!({
                block: {"rtoInMinutes": 60, "disasterRecoveryApproach": "WARM_STANDBY"},
            });
            let policy_arn = given_policy(&router, &case, &name, &created).await;
            let service_arn =
                given_service(&router, &case, &format!("Svc-{name}"), Some(&policy_arn)).await;

            let patch = json!({block: {"disasterRecoveryApproach": approach}});
            let merged = merged_blocks(&created, &patch);
            let updated = update_policy(&router, &case, &policy_arn, &patch).await;
            assert_blocks(&case, &updated, &merged);

            let expected = derived_from(&merged, &name, SOURCE_SELF);
            let read_back = read_service(&router, &case, &service_arn).await;
            assert_effective(&case, &read_back, &expected);
        }
    }
}

#[tokio::test]
async fn an_update_to_a_policy_leaves_every_other_member_of_its_services_as_stored() {
    // The other half of the rule: because the values are DERIVED at projection time, an
    // update to the policy writes to the policy only — so every stored member of every
    // referencing service, its own `updatedAt` included, reads back unchanged, while the
    // policy's own members take what the update sent and the policy keeps counting the
    // services that reference it.
    let router = emulator();
    let case = "two services referencing one updated policy";
    let name = "PropUntouchedServices";
    let created_blocks = json!({"multiAz": {"rtoInMinutes": 30, "rpoInMinutes": 10}});
    let policy_arn = given_policy(&router, case, name, &created_blocks).await;
    let created_policy = read_policy(&router, case, &policy_arn).await;

    let mut snapshots = Vec::new();
    for suffix in ["One", "Two"] {
        let service_arn =
            given_service(&router, case, &format!("Svc-{name}-{suffix}"), Some(&policy_arn)).await;
        let snapshot = read_service(&router, case, &service_arn).await;
        snapshots.push((service_arn, snapshot));
    }

    let description = "edited while two services reference it";
    let patch = json!({
        "description": description,
        "multiAz": {"rtoInMinutes": 5},
        "sharingEnabled": true,
    });
    let merged = merged_blocks(&created_blocks, &patch);
    let updated = update_policy(&router, case, &policy_arn, &patch).await;
    assert_blocks(case, &updated, &merged);

    // The non-target members the update sent are stored too, and the policy was updated
    // rather than created again: its identity and creation instant stay where they were and
    // `updatedAt` is at or after `createdAt`.
    assert_eq!(
        updated["description"].as_str(),
        Some(description),
        "case {case:?}: expected the description the update sent, got {:?}",
        updated["description"]
    );
    assert_eq!(
        updated["sharingEnabled"].as_bool(),
        Some(true),
        "case {case:?}: expected sharingEnabled true, got {:?}",
        updated["sharingEnabled"]
    );
    assert_eq!(
        updated["policyArn"], created_policy["policyArn"],
        "case {case:?}: an update must not re-mint the ARN, got {:?}",
        updated["policyArn"]
    );
    assert_eq!(
        updated["name"], created_policy["name"],
        "case {case:?}: an update carries no name member, so the name must stay, got {:?}",
        updated["name"]
    );
    assert_eq!(
        updated["createdAt"], created_policy["createdAt"],
        "case {case:?}: an update must not move createdAt, got {:?}",
        updated["createdAt"]
    );
    let created_at = updated["createdAt"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: expected an epoch-seconds createdAt, got {updated}")
    });
    let updated_at = updated["updatedAt"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: expected an epoch-seconds updatedAt, got {updated}")
    });
    assert!(
        updated_at >= created_at,
        "case {case:?}: expected updatedAt at or after createdAt {created_at}, got \
         {updated_at}"
    );

    // `associatedServiceCount` is counted from the services themselves, so the update reports
    // the two that reference the policy.
    let expected_count = i64::try_from(snapshots.len()).expect("two fits in an i64");
    assert_eq!(
        updated["associatedServiceCount"].as_i64(),
        Some(expected_count),
        "case {case:?}: expected associatedServiceCount {expected_count}, got {:?}",
        updated["associatedServiceCount"]
    );
    // The policy's own reader answers the same projection the update reported.
    let read_back_policy = read_policy(&router, case, &policy_arn).await;
    assert!(
        same_json(&read_back_policy, &updated),
        "case {case:?}: expected GetPolicy to answer the policy the update stored {updated}, \
         got {read_back_policy}"
    );

    let expected = derived_from(&merged, name, SOURCE_SELF);
    for (service_arn, snapshot) in &snapshots {
        let after = read_service(&router, case, service_arn).await;
        assert_effective(case, &after, &expected);

        let stored = snapshot.as_object().expect("a service body is an object");
        let now = after.as_object().expect("a service body is an object");
        assert_eq!(
            now.len(),
            stored.len(),
            "case {case:?}: expected the service {service_arn} to report the same members as \
             before the update {snapshot}, got {after}"
        );
        for (member, value) in stored {
            if member == "effectivePolicyValues" {
                continue;
            }
            let actual = now.get(member).cloned().unwrap_or(Value::Null);
            assert!(
                same_json(&actual, value),
                "case {case:?}: expected the service {service_arn} member {member} to stay \
                 {value}, got {actual}"
            );
        }
    }
}
