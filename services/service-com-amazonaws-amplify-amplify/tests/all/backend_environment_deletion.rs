// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `DeleteBackendEnvironment`.
//!
//! Every case drives the real wire path (the shared harness in
//! `tests/all/app_branch_lifecycle.rs`), so what is asserted is the JSON an AWS SDK
//! client parses, not an internal function's return value.
//!
//! The rule under test: a delete of a named resource under an Amplify app REMOVES that
//! record from the app its URI names and answers the record exactly as it stood at
//! delete time — so afterwards every reader of that app (the record's own read and the
//! app's listing) reports it as absent, its name is free for a new create, and the app's
//! other records and every other app are untouched. A delete addressed to an app or to a
//! record that is not in state removes nothing and answers the modelled 404, naming the
//! entity the request addressed; a delete is therefore NOT idempotent-successful — the
//! second one is that same 404.
//!
//! The table spans the class the rule generalises over — `environmentName` at the
//! shortest and the longest name this service will create (two and ten lowercase
//! letters, the form `backend_environment_name_rule` has under test) and at ordinary
//! names in between, with `stackName` and `deploymentArtifacts` absent, supplied singly
//! and supplied together — rather than one recorded request. The modelled bounds come
//! from
//! `smithy_query(query="operation",operation="DeleteBackendEnvironment,CreateBackendEnvironment")`.

use serde_json::{json, Value};

use crate::app_branch_lifecycle::{string_member, Emulator, Reply};

/// `CreateBackendEnvironment` over the wire.
async fn create_environment(emulator: &Emulator, app_id: &str, request: &Value) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/apps/{app_id}/backendenvironments"),
            Some(request),
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

/// The `environmentName`s `ListBackendEnvironments` reports for an app, in page order.
async fn listed_names(emulator: &Emulator, app_id: &str) -> Vec<String> {
    let reply = emulator
        .call("GET", &format!("/apps/{app_id}/backendenvironments"), None)
        .await;
    assert_eq!(
        reply.status, 200,
        "app {app_id}: expected HTTP 200 from ListBackendEnvironments, got {reply:?}"
    );
    reply
        .body
        .get("backendEnvironments")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("app {app_id}: ListBackendEnvironments must carry a `backendEnvironments` list, got {reply:?}"))
        .iter()
        .map(|record| string_member(record, "environmentName"))
        .collect()
}

/// One environment the delete table creates and then deletes.
struct DeletionCase {
    case: &'static str,
    environment_name: String,
    request: Value,
}

/// The `DeleteBackendEnvironment` table: the names and optional members a delete has to
/// behave identically over.
fn deletion_cases() -> Vec<DeletionCase> {
    let max_name = "e".repeat(10);
    vec![
        DeletionCase {
            case: "an ordinary name, no optional members",
            environment_name: "staging".to_string(),
            request: json!({ "environmentName": "staging" }),
        },
        DeletionCase {
            case: "a name of two characters, the shortest the service creates",
            environment_name: "qa".to_string(),
            request: json!({ "environmentName": "qa" }),
        },
        DeletionCase {
            case: "stackName and deploymentArtifacts both supplied",
            environment_name: "dev".to_string(),
            request: json!({
                "environmentName": "dev",
                "stackName": "amplify-demo-dev-123456",
                "deploymentArtifacts": "amplify-demo-dev-123456-deployment",
            }),
        },
        DeletionCase {
            case: "only stackName supplied",
            environment_name: "prod".to_string(),
            request: json!({
                "environmentName": "prod",
                "stackName": "amplify-demo-prod-654321",
            }),
        },
        DeletionCase {
            case: "only deploymentArtifacts supplied",
            environment_name: "integ".to_string(),
            request: json!({
                "environmentName": "integ",
                "deploymentArtifacts": "amplify-demo-integration-deployment",
            }),
        },
        DeletionCase {
            case: "a stackName carrying case, dots, dashes and underscores",
            environment_name: "featuretst".to_string(),
            request: json!({
                "environmentName": "featuretst",
                "stackName": "amplify-demo-Feature.Test-1_env",
            }),
        },
        DeletionCase {
            case: "a name at ten characters, the longest the service creates",
            environment_name: max_name.clone(),
            request: json!({ "environmentName": max_name }),
        },
    ]
}

#[tokio::test]
async fn delete_answers_the_record_as_it_stood_and_frees_its_name() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("backend-env-delete-host").await;

    for case in deletion_cases() {
        let label = case.case;
        let created = create_environment(&emulator, &app_id, &case.request).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected HTTP 200 from CreateBackendEnvironment, got {created:?}"
        );
        let stored = get_environment(&emulator, &app_id, &case.environment_name)
            .await
            .member("backendEnvironment");

        let deleted = delete_environment(&emulator, &app_id, &case.environment_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected HTTP 200 from DeleteBackendEnvironment, got {deleted:?}"
        );
        assert_eq!(
            deleted.member("backendEnvironment"),
            stored,
            "case {label:?}: the delete must answer the record as it stood, got {deleted:?}"
        );

        // The removal is what every reader of this app now sees.
        let gone = get_environment(&emulator, &app_id, &case.environment_name).await;
        assert_eq!(
            gone.status, 404,
            "case {label:?}: a deleted environment is not readable, got {gone:?}"
        );
        assert_eq!(
            gone.message(),
            Some(format!(
                "BackendEnvironment {} does not exist.",
                case.environment_name
            )),
            "case {label:?}: the 404 names the entity the request addressed, got {:?}",
            gone.message()
        );
        assert!(
            !listed_names(&emulator, &app_id)
                .await
                .contains(&case.environment_name),
            "case {label:?}: a deleted environment must not be listed, got {:?}",
            listed_names(&emulator, &app_id).await
        );

        // A second delete of the same name is the same modelled 404, not a success:
        // there is nothing left where the request points.
        let again = delete_environment(&emulator, &app_id, &case.environment_name).await;
        assert_eq!(
            again.status, 404,
            "case {label:?}: a second delete of the same name is refused, got {again:?}"
        );
        assert_eq!(
            again.error_code.as_deref(),
            Some("NotFoundException"),
            "case {label:?}: expected NotFoundException, got {:?}",
            again.error_code
        );

        // The name is free again: the record it held is gone, so a create under it is
        // accepted rather than refused as a duplicate.
        let recreated = create_environment(&emulator, &app_id, &case.request).await;
        assert_eq!(
            recreated.status, 200,
            "case {label:?}: the deleted name must be free for a new create, got {recreated:?}"
        );
        let recreated_record = recreated.member("backendEnvironment");
        assert_eq!(
            string_member(&recreated_record, "backendEnvironmentArn"),
            string_member(&stored, "backendEnvironmentArn"),
            "case {label:?}: the recreated environment is addressed by the same ARN, got \
             {recreated_record:?}"
        );
        let cleanup = delete_environment(&emulator, &app_id, &case.environment_name).await;
        assert_eq!(
            cleanup.status, 200,
            "case {label:?}: expected HTTP 200 deleting the recreated environment, got {cleanup:?}"
        );
    }

    assert_eq!(
        listed_names(&emulator, &app_id).await,
        Vec::<String>::new(),
        "every environment created here was deleted, so the app lists none"
    );
}

#[tokio::test]
async fn deleting_one_environment_leaves_every_other_record_alone() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("backend-env-delete-many").await;
    // A second app holds the SAME names throughout: an environment is reachable only
    // through its own app, so deleting one here must not reach the other app's record.
    let other_app_id = emulator.app_id_of_new_app("backend-env-delete-other").await;

    let cases = deletion_cases();
    for case in &cases {
        for app in [&app_id, &other_app_id] {
            let created = create_environment(&emulator, app, &case.request).await;
            assert_eq!(
                created.status, 200,
                "case {:?}: expected HTTP 200 seeding on {app}, got {created:?}",
                case.case
            );
        }
    }
    let other_records: Vec<Value> = {
        let mut records = Vec::new();
        for case in &cases {
            records.push(
                get_environment(&emulator, &other_app_id, &case.environment_name)
                    .await
                    .member("backendEnvironment"),
            );
        }
        records
    };

    let mut remaining: Vec<String> = cases
        .iter()
        .map(|case| case.environment_name.clone())
        .collect();
    remaining.sort();

    for case in &cases {
        let label = case.case;
        let survivors: Vec<Value> = {
            let mut records = Vec::new();
            for name in remaining.iter().filter(|name| **name != case.environment_name) {
                records.push(
                    get_environment(&emulator, &app_id, name)
                        .await
                        .member("backendEnvironment"),
                );
            }
            records
        };

        let deleted = delete_environment(&emulator, &app_id, &case.environment_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected HTTP 200 from DeleteBackendEnvironment, got {deleted:?}"
        );
        remaining.retain(|name| *name != case.environment_name);

        assert_eq!(
            listed_names(&emulator, &app_id).await,
            remaining,
            "case {label:?}: the delete removes exactly the environment it named"
        );
        for (name, expected) in remaining.iter().zip(survivors.iter()) {
            assert_eq!(
                get_environment(&emulator, &app_id, name)
                    .await
                    .member("backendEnvironment"),
                *expected,
                "case {label:?}: deleting one environment must leave {name:?} exactly as it was"
            );
        }
        for (other_case, expected) in cases.iter().zip(other_records.iter()) {
            assert_eq!(
                get_environment(&emulator, &other_app_id, &other_case.environment_name)
                    .await
                    .member("backendEnvironment"),
                *expected,
                "case {label:?}: deleting an environment must not touch another app's {:?}",
                other_case.environment_name
            );
        }
    }
}

#[tokio::test]
async fn a_delete_of_something_not_in_state_is_the_modelled_404() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("backend-env-delete-refusal").await;
    let created = create_environment(&emulator, &app_id, &json!({ "environmentName": "staging" }))
        .await;
    assert_eq!(
        created.status, 200,
        "expected HTTP 200 seeding the environment that must survive, got {created:?}"
    );
    let stored = created.member("backendEnvironment");

    // `AppId` is modelled `length 1..20, pattern ^d[a-z0-9]+$`, so each of these is a
    // well-formed id for an app that does not exist: the request passes the boundary
    // validator and the refusal is the handler's.
    for absent_app in ["dnosuchapp1", "d0", &format!("d{}", "9".repeat(18))] {
        let refused = delete_environment(&emulator, absent_app, "staging").await;
        assert_eq!(
            refused.status, 404,
            "case {absent_app:?}: expected HTTP 404 for an app that does not exist, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("NotFoundException"),
            "case {absent_app:?}: expected NotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            Some(format!("App {absent_app} not found.")),
            "case {absent_app:?}: the 404 names the entity the request addressed, got {:?}",
            refused.message()
        );
    }

    // A name no environment of this app carries is the same modelled refusal, naming the
    // environment rather than the app.
    for absent in ["dev", "e", &"z".repeat(255), "Feature.Test-1_env"] {
        let refused = delete_environment(&emulator, &app_id, absent).await;
        assert_eq!(
            refused.status, 404,
            "case {absent:?}: expected HTTP 404 from DeleteBackendEnvironment, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("NotFoundException"),
            "case {absent:?}: expected NotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            Some(format!("BackendEnvironment {absent} does not exist.")),
            "case {absent:?}: the 404 names the entity the request addressed, got {:?}",
            refused.message()
        );
    }

    // Every refusal removed nothing: the one environment this app holds is still what a
    // reader sees, and the app still lists exactly it.
    assert_eq!(
        get_environment(&emulator, &app_id, "staging")
            .await
            .member("backendEnvironment"),
        stored,
        "a refused delete must leave the stored environment alone"
    );
    assert_eq!(
        listed_names(&emulator, &app_id).await,
        vec!["staging".to_string()],
        "a refused delete must leave the app's listing alone"
    );
}
