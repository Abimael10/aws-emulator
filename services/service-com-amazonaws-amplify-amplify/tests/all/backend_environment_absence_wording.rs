// SPDX-License-Identifier: CC0-1.0

//! The wording Amplify refuses an absent BACKEND ENVIRONMENT with.
//!
//! Every case drives the real wire path (the shared harness in
//! `tests/all/app_branch_lifecycle.rs`), so what is asserted is the JSON an AWS SDK
//! client parses.
//!
//! The rule under test: when an operation addresses a backend environment by
//! `environmentName` under an app that exists and the app holds no environment of that
//! name, the answer is HTTP 404 with error code `NotFoundException` and the message
//! `BackendEnvironment <environmentName> does not exist.` — the entity as the single
//! modelled shape name, the `environmentName` echoed verbatim from the request, and the
//! predicate `does not exist.` closed by one period. The model spells out only the
//! exception itself (`smithy_query(query="operation",operation="GetBackendEnvironment,
//! DeleteBackendEnvironment")`: `NotFoundException`, `httpError` 404), so the wording is
//! recorded behaviour; the status and the code come from the model.
//!
//! The rule holds for EVERY way of addressing an absent environment — both operations
//! that take an `environmentName`, on a name that was never created and on one a
//! successful delete has just removed — and it is specific to this entity: an absent APP
//! named by the same requests keeps Amplify's generic `<Entity> <identifier> not found.`
//! wording, which is asserted here too so the two constructions cannot be collapsed.
//!
//! The table spans the class the rule generalises over — `environmentName` is modelled
//! `length 1..255, pattern ^(?s).+$`
//! (`smithy_query(query="operation",operation="GetBackendEnvironment")`) — at the
//! modelled length minimum, at lower-case names of several lengths, at names carrying
//! case, digits and punctuation, and at the modelled maximum of 255. A name the service
//! would not CREATE is addressed the same way as one it would: the form rule binds the
//! name a create brings into being (`backend_environment_name_rule`), not the one a read
//! or a delete quotes.

use serde_json::json;

use crate::app_branch_lifecycle::{Emulator, Reply};

/// The message the rule says a 404 for the backend environment `environment_name` must
/// carry: fixed text, then the name echoed from the request, then fixed text.
fn expected_absence_message(environment_name: &str) -> String {
    format!("BackendEnvironment {environment_name} does not exist.")
}

/// `CreateBackendEnvironment` over the wire.
async fn create_environment(emulator: &Emulator, app_id: &str, environment_name: &str) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/apps/{app_id}/backendenvironments"),
            Some(&json!({ "environmentName": environment_name })),
        )
        .await
}

/// `GetBackendEnvironment` over the wire.
async fn get_environment(emulator: &Emulator, app_id: &str, environment_name: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/apps/{app_id}/backendenvironments/{environment_name}"),
            None,
        )
        .await
}

/// `DeleteBackendEnvironment` over the wire.
async fn delete_environment(emulator: &Emulator, app_id: &str, environment_name: &str) -> Reply {
    emulator
        .call(
            "DELETE",
            &format!("/apps/{app_id}/backendenvironments/{environment_name}"),
            None,
        )
        .await
}

/// Assert `reply` is the modelled 404 the rule requires for `environment_name`.
fn assert_environment_absent(case: &str, reply: &Reply, environment_name: &str) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected HTTP 404 for an environment that is not in state, got {reply:?}"
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("NotFoundException"),
        "case {case:?}: expected the x-amzn-errortype header to name NotFoundException, got {:?}",
        reply.error_code
    );
    let expected = expected_absence_message(environment_name);
    let actual = reply.message();
    assert_eq!(
        actual.as_deref(),
        Some(expected.as_str()),
        "case {case:?}: expected the request-derived message {expected:?}, got {actual:?}"
    );
}

/// The `environmentName` table: rows differing along the length, the character classes
/// and the case the modelled `EnvironmentName` (`length 1..255, pattern ^(?s).+$`)
/// admits. None of them is the recorded run's generated name.
fn environment_name_cases() -> Vec<(&'static str, String)> {
    vec![
        ("a name of one character, the modelled length minimum", "e".to_string()),
        ("two lower-case letters", "qa".to_string()),
        ("ten lower-case letters", "stagingabc".to_string()),
        ("mixed case with punctuation", "Feature.Test-1_env".to_string()),
        ("digits only", "20260101".to_string()),
        ("a name at the modelled length maximum of 255", "z".repeat(255)),
    ]
}

/// The rows of [`environment_name_cases`] a create brings into being, so the absence
/// paths that need a DELETE first can drive them: the names inside the form this service
/// creates under (`backend_environment_name_rule`).
fn creatable_environment_name_cases() -> Vec<(&'static str, String)> {
    vec![
        ("two lower-case letters, the shortest created", "qa".to_string()),
        ("an ordinary name", "staging".to_string()),
        ("ten lower-case letters, the longest created", "stagingabc".to_string()),
    ]
}

#[tokio::test]
async fn an_environment_never_created_is_refused_with_the_entity_wording() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("backend-env-absence-host").await;

    for (label, name) in environment_name_cases() {
        for (operation, reply) in [
            (
                "GetBackendEnvironment",
                get_environment(&emulator, &app_id, &name).await,
            ),
            (
                "DeleteBackendEnvironment",
                delete_environment(&emulator, &app_id, &name).await,
            ),
        ] {
            assert_environment_absent(&format!("{label}: {operation}"), &reply, &name);
        }
    }
}

#[tokio::test]
async fn a_deleted_environment_is_refused_with_the_entity_wording() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("backend-env-deleted-absence").await;

    for (label, name) in creatable_environment_name_cases() {
        let created = create_environment(&emulator, &app_id, &name).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected HTTP 200 from CreateBackendEnvironment, got {created:?}"
        );
        let deleted = delete_environment(&emulator, &app_id, &name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected HTTP 200 from DeleteBackendEnvironment, got {deleted:?}"
        );

        // Both absence paths after a successful delete: the read of the removed record,
        // and the second delete of the same name.
        assert_environment_absent(
            &format!("{label}: GetBackendEnvironment after a delete"),
            &get_environment(&emulator, &app_id, &name).await,
            &name,
        );
        assert_environment_absent(
            &format!("{label}: DeleteBackendEnvironment twice"),
            &delete_environment(&emulator, &app_id, &name).await,
            &name,
        );
    }
}

#[tokio::test]
async fn an_absent_app_keeps_the_generic_entity_wording() {
    let emulator = Emulator::new();
    let existing_app = emulator.app_id_of_new_app("backend-env-wording-host").await;

    // `AppId` is modelled `length 1..20, pattern ^d[a-z0-9]+$`, so each of these is a
    // well-formed id for an app no create minted: the request reaches the handler and
    // the refusal names the APP, with the generic wording every other Amplify entity's
    // 404 uses.
    for absent_app in [
        "dnonexistent00000000".to_string(),
        "d0".to_string(),
        format!("d{}", "9".repeat(19)),
    ] {
        for (operation, reply) in [
            (
                "GetBackendEnvironment",
                get_environment(&emulator, &absent_app, "staging").await,
            ),
            (
                "DeleteBackendEnvironment",
                delete_environment(&emulator, &absent_app, "staging").await,
            ),
        ] {
            let case = format!("{absent_app}: {operation}");
            assert_eq!(
                reply.status, 404,
                "case {case:?}: expected HTTP 404 for an app that does not exist, got {reply:?}"
            );
            assert_eq!(
                reply.error_code.as_deref(),
                Some("NotFoundException"),
                "case {case:?}: expected NotFoundException, got {:?}",
                reply.error_code
            );
            let expected = format!("App {absent_app} not found.");
            let actual = reply.message();
            assert_eq!(
                actual.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected the app wording {expected:?}, got {actual:?}"
            );
        }
    }

    // And a sibling entity of the app that does exist keeps that same generic wording:
    // only the backend environment is worded its own way.
    let branch_absent = emulator.get_branch(&existing_app, "nosuchbranch").await;
    assert_eq!(
        branch_absent.message(),
        Some("Branch nosuchbranch not found.".to_string()),
        "case \"GetBranch on an absent branch\": the generic entity wording must be \
         unchanged, got {:?}",
        branch_absent.message()
    );
}
