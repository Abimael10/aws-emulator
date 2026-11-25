// SPDX-License-Identifier: CC0-1.0

//! The ONE refusal this service words for a value outside the catalogue a member admits,
//! driven through an operation of each kind that checks one.
//!
//! RULE under test: whatever member is checked, a value outside the catalogue that member
//! admits is refused as `ValidationException` worded the one way — the member as the API
//! spells it, then the values the request could have sent in catalogue order joined with
//! `", "`, then the value it did send — and every value the catalogue holds is served.
//! The members differ only in that catalogue, so a value one member admits is refused for
//! a member whose catalogue does not hold it.
//!
//! The rows span what the rule generalises over: every catalogue this service checks a
//! member against — the model's `ApplicationType`, `BackintMode`, `ConnectedEntityType`
//! and `FilterOperator` enums, plus the filter-name catalogue a listing reports — so
//! catalogues of one, two, three and four values, and, for each of them, every value it
//! holds on the accepting side and, on the refusing side, a value no catalogue of this
//! service holds, the same value in the wrong case (catalogue values are case-sensitive),
//! the empty value, every value of ANOTHER catalogue the row's own does not hold, and the
//! whole catalogue sent as one value.
//!
//! The accepting side is then driven END TO END through one operation per member —
//! `RegisterApplication` (`ApplicationType`), `StopApplication` (`StopConnectedEntity`),
//! `UpdateApplicationSettings` (`BackintMode`) and `ListApplications` (`Filter$Name` and
//! `Filter$Operator`) — over the real router (the harness
//! `tests/all/application_lifecycle.rs` owns), with each accepted value read back out of
//! what the call persisted.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ssmsap_ssmsap::ops::types::{
    APPLICATION_TYPE_VALUES, BACKINT_MODE_VALUES, CONNECTED_ENTITY_TYPE_VALUES,
    FILTER_OPERATOR_VALUES,
};
use service_com_amazonaws_ssmsap_ssmsap::state::{value_within_catalogue, FILTER_NAMES};

use crate::application_lifecycle::{valid_registration, Emulator, Reply};
use crate::validation_message_rendering::{message, VALIDATION_EXCEPTION};

/// A value no catalogue of this service holds.
const UNCATALOGUED: &str = "NotACatalogueValue";

/// The `ApplicationStatus` a settled registration reports
/// (`smithy_query(query="shape", shape="ApplicationStatus")`).
const ACTIVATED: &str = "ACTIVATED";

/// How long a poll for the status the clock owes a fresh registration may go on before
/// the case fails. Generous by design: the loop waits for the CONDITION, and only a store
/// that never settles can run it out.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// Every catalogue a member of this service is checked against, each with the member name
/// the refusal words it as: the model's own enum values
/// (`smithy_query(query="shape", shape="ApplicationType,BackintMode,ConnectedEntityType,FilterOperator")`,
/// read off this crate's `ops::types` constants for them rather than written out here, so
/// a value added to an enum is exercised instead of going untested) and the filter names a
/// listing reports.
fn catalogues() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        ("ApplicationType", APPLICATION_TYPE_VALUES),
        ("BackintMode", BACKINT_MODE_VALUES),
        ("StopConnectedEntity", CONNECTED_ENTITY_TYPE_VALUES),
        ("Filter Operator", FILTER_OPERATOR_VALUES),
        ("Filter Name", FILTER_NAMES),
    ]
}

/// The values a member checked against `catalogue` must refuse: one no catalogue holds,
/// the empty value, the catalogue's own first value in the wrong case, every value of
/// another catalogue this one does not hold, and the whole catalogue sent as a single
/// value — each of them only where the catalogue does not hold it, since a catalogue of
/// one value holds its own join.
fn refused_values(catalogue: &[&str]) -> Vec<String> {
    let mut values = vec![UNCATALOGUED.to_string(), String::new()];
    if let Some(first) = catalogue.first() {
        for recased in [first.to_lowercase(), first.to_uppercase()] {
            if !catalogue.contains(&recased.as_str()) && !values.contains(&recased) {
                values.push(recased);
            }
        }
    }
    for (_, other) in catalogues() {
        for value in other {
            if !catalogue.contains(value) && !values.iter().any(|held| held == value) {
                values.push((*value).to_string());
            }
        }
    }
    // The whole catalogue as one value — which for a catalogue of ONE value IS that
    // value, and is then a value the member admits rather than one it refuses.
    let joined = catalogue.join(", ");
    if !catalogue.contains(&joined.as_str()) {
        values.push(joined);
    }
    values
}

/// The sentence this service refuses `value` for `member` with.
fn expected_refusal(member: &str, catalogue: &[&str], value: &str) -> String {
    format!(
        "{member} must be one of {}, but was {value}",
        catalogue.join(", ")
    )
}

/// RULE: one sentence words every refusal of a value outside a member's catalogue, and
/// every value the catalogue holds is accepted — whatever the member, and however many
/// values its catalogue has.
#[test]
fn one_sentence_words_every_catalogue_refusal() {
    for (member, catalogue) in catalogues() {
        for value in catalogue {
            let case = format!("{member} {value}");
            let accepted = value_within_catalogue(member, catalogue, value);
            assert!(
                accepted.is_ok(),
                "case {case:?}: a value the catalogue holds is accepted, got {accepted:?}"
            );
        }

        for value in refused_values(catalogue) {
            let case = format!("{member} {value:?}");
            let refusal = value_within_catalogue(member, catalogue, &value)
                .expect_err("a value outside the catalogue is refused");
            let actual = refusal.message.unwrap_or_default();
            let expected = expected_refusal(member, catalogue, &value);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}

/// RULE (the other half): a catalogue decides for its OWN member, so a value one member
/// admits is refused for a member whose catalogue does not hold it, and the refusal names
/// that member's own catalogue.
#[test]
fn a_catalogue_decides_for_its_own_member_alone() {
    let rows = catalogues();
    for (member, catalogue) in &rows {
        for (other_member, other) in &rows {
            if other_member == member {
                continue;
            }
            for value in other.iter().filter(|value| !catalogue.contains(value)) {
                let case = format!("{member} sent {value:?} of {other_member}");
                let refusal = value_within_catalogue(member, catalogue, value)
                    .expect_err("another member's value is refused");
                let actual = refusal.message.unwrap_or_default();
                let expected = expected_refusal(member, catalogue, value);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {expected:?}, got {actual:?}"
                );
                // The member whose catalogue DOES hold it serves it.
                let accepted = value_within_catalogue(other_member, other, value);
                assert!(
                    accepted.is_ok(),
                    "case {case:?}: {other_member} admits {value:?}, got {accepted:?}"
                );
            }
        }
    }
}

/// Register `id` and look at `GetApplication` until the registration has settled, so the
/// application is up and owed nothing by the time a control call is made on it.
///
/// The emulator never sleeps: a change the clock owes is materialised by the first request
/// at or after its due instant, so this looks AGAIN rather than waiting a fixed time, and
/// fails only if the status never arrives.
async fn register_settled(emulator: &Emulator, id: &str, app_type: &str) -> Reply {
    let registered = emulator
        .send("/register-application", valid_registration(id, app_type))
        .await;
    assert_eq!(
        registered.status, 200,
        "case {id:?}: expected 200 from RegisterApplication, got {registered:?}"
    );
    let started_at = Instant::now();
    loop {
        let reply = emulator
            .send("/get-application", json!({"ApplicationId": id}))
            .await;
        if reply.status == 200 && reply.app_str("Status") == ACTIVATED {
            return registered;
        }
        assert!(
            started_at.elapsed() < POLL_DEADLINE,
            "case {id:?}: expected {id:?} to report Status {ACTIVATED}, got {reply:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// One `Properties` member of the operation record `operation_id` names.
async fn recorded_property(emulator: &Emulator, operation_id: &str, member: &str) -> String {
    let reply = emulator
        .send("/get-operation", json!({"OperationId": operation_id}))
        .await;
    assert_eq!(
        reply.status, 200,
        "case {operation_id:?}: expected 200 from GetOperation, got {reply:?}"
    );
    reply
        .member("Operation")
        .and_then(|operation| operation.get("Properties"))
        .and_then(|properties| properties.get(member))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// RULE, end to end: every value a member's catalogue holds is SERVED by the operation
/// that checks that member, and the call persists the value it was sent.
#[tokio::test]
async fn every_operation_serves_every_value_its_catalogue_holds() {
    let emulator = Emulator::new();

    // `ApplicationType`, through RegisterApplication.
    for app_type in APPLICATION_TYPE_VALUES {
        let id = format!("catalogue-{app_type}");
        let registered = register_settled(&emulator, &id, app_type).await;
        assert_eq!(
            registered.app_str("Type"),
            *app_type,
            "case {app_type:?}: expected the registration to report Type {app_type:?}, got \
             {registered:?}"
        );
        let fetched = emulator
            .send("/get-application", json!({"ApplicationId": id}))
            .await;
        assert_eq!(
            fetched.app_str("Type"),
            *app_type,
            "case {app_type:?}: expected GetApplication to report Type {app_type:?}, got \
             {fetched:?}"
        );
    }

    // `Filter$Name` and `Filter$Operator`, through ListApplications: every name this
    // listing reports crossed with every operator the model declares.
    for name in FILTER_NAMES {
        for operator in FILTER_OPERATOR_VALUES {
            let case = format!("ListApplications {name} {operator}");
            let reply = emulator
                .send(
                    "/list-applications",
                    json!({"Filters": [{"Name": name, "Value": "HANA", "Operator": operator}]}),
                )
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected 200, got {}: {:?}",
                reply.status, reply.body
            );
        }
    }

    // `BackintMode`, through UpdateApplicationSettings.
    for mode in BACKINT_MODE_VALUES {
        let id = format!("backint-{mode}");
        register_settled(&emulator, &id, "HANA").await;
        let reply = emulator
            .send(
                "/update-application-settings",
                json!({
                    "ApplicationId": id,
                    "Backint": {"BackintMode": mode, "EnsureNoBackupInProcess": true},
                }),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {mode:?}: expected 200 from UpdateApplicationSettings, got {reply:?}"
        );
        let operation_id = reply
            .member("OperationIds")
            .and_then(Value::as_array)
            .and_then(|ids| ids.first())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let recorded = recorded_property(&emulator, &operation_id, "BackintMode").await;
        assert_eq!(
            recorded, *mode,
            "case {mode:?}: expected the record to report BackintMode {mode:?}, got {recorded:?}"
        );
    }

    // `StopConnectedEntity`, through StopApplication.
    for entity in CONNECTED_ENTITY_TYPE_VALUES {
        let id = format!("stop-{entity}");
        register_settled(&emulator, &id, "HANA").await;
        let reply = emulator
            .send(
                "/stop-application",
                json!({"ApplicationId": id, "StopConnectedEntity": entity}),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {entity:?}: expected 200 from StopApplication, got {reply:?}"
        );
        let operation_id = reply
            .member("OperationId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let recorded = recorded_property(&emulator, &operation_id, "StopConnectedEntity").await;
        assert_eq!(
            recorded, *entity,
            "case {entity:?}: expected the record to report StopConnectedEntity {entity:?}, got \
             {recorded:?}"
        );
    }
}

/// RULE, end to end: a value outside the catalogue a member admits is refused as
/// `ValidationException` (HTTP 400) by every operation that checks one. For an ENUM member
/// the boundary validator from the model reaches the request first and words the refusal
/// against the member, which is why the sentence itself is pinned above and here only on
/// the listing, whose filter-name catalogue no modelled constraint bounds.
#[tokio::test]
async fn every_operation_refuses_a_value_outside_its_catalogue() {
    let emulator = Emulator::new();
    let application = "catalogue-refusals";
    register_settled(&emulator, application, "HANA").await;

    let mut registration = valid_registration("uncatalogued-type", "HANA")
        .as_object()
        .cloned()
        .expect("a registration body is a JSON object");
    registration.insert("ApplicationType".into(), json!(UNCATALOGUED));

    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "RegisterApplication ApplicationType",
            "/register-application",
            Value::Object(registration),
        ),
        (
            "StopApplication StopConnectedEntity",
            "/stop-application",
            json!({"ApplicationId": application, "StopConnectedEntity": UNCATALOGUED}),
        ),
        (
            "UpdateApplicationSettings BackintMode",
            "/update-application-settings",
            json!({
                "ApplicationId": application,
                "Backint": {"BackintMode": UNCATALOGUED, "EnsureNoBackupInProcess": false},
            }),
        ),
    ];

    for (case, path, body) in rows {
        let reply = emulator.send(path, body).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected 400, got {}: {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.code(),
            VALIDATION_EXCEPTION,
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {reply:?}"
        );
        let actual = message(&reply);
        assert!(
            !actual.is_empty(),
            "case {case:?}: a refusal states what was wrong, got {reply:?}"
        );
    }

    // The listing's own refusal is the shared sentence, read off its own catalogue.
    let case = "ListApplications Filter Name";
    let reply = emulator
        .send(
            "/list-applications",
            json!({"Filters": [{"Name": UNCATALOGUED, "Value": "HANA", "Operator": "Equals"}]}),
        )
        .await;
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected 400, got {}: {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.code(),
        VALIDATION_EXCEPTION,
        "case {case:?}: expected {VALIDATION_EXCEPTION}, got {reply:?}"
    );
    let actual = message(&reply);
    let expected = expected_refusal("Filter Name", FILTER_NAMES, UNCATALOGUED);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected {expected:?}, got {actual:?}"
    );
}
