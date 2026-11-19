// SPDX-License-Identifier: CC0-1.0

//! What a call refused because an API's SCHEMA is already being altered says, end to end
//! over HTTP.
//!
//! The rule asserted here: while a schema creation this API owes is still in progress,
//! every call that would alter the same type system is refused with the status the model
//! gives that error — `ConcurrentModificationException`, `@httpError` 409
//! (`smithy_query(query="shape", shape="ConcurrentModificationException")`) — carrying
//! AppSync's schema-specific sentence, which is entirely fixed text and states nothing of
//! the request. It is the GROUND that chooses the sentence, not the operation: a conflict
//! on another ground keeps its own wording (a custom domain's association still
//! processing, asserted by `crate::custom_domain_api_association`).
//!
//! The table's rows are the operations that can meet that ground — the submitting call
//! itself (`StartSchemaCreation`) and each type operation that writes one type of the
//! same system (`CreateType`, `UpdateType`, `DeleteType`) — so the rule is asserted over
//! the whole class rather than over the operation the defect surfaced in. Each refusal
//! leaves the creation it ran into alone, so the clock still performs it, and the same
//! calls are served once it has.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::schema_creation::{await_creation, get_type, schema_path, start_creation};
use crate::source_api_association::{call, create_api, router, Answer};

/// AppSync's modelled error for a modification that cannot proceed, and its status.
const CONFLICT: &str = "ConcurrentModificationException";
const CONFLICT_STATUS: u16 = 409;

/// The sentence a call refused because the schema is being altered carries. Written out
/// here rather than read off the service's own constant, so a change of wording is caught
/// rather than followed.
const SCHEMA_BEING_ALTERED: &str =
    "Schema is currently being altered, please wait until that is complete.";

/// The type the rows that address an existing type are seeded with, created before the
/// schema creation they run into is submitted.
const SEEDED_TYPE: &str = "Seeded";

/// The document the schema creation under way carries: it declares a type system of its
/// own, which is what makes the creation an alteration of the whole type system.
const DOCUMENT: &str = "type Query {\n  hello: String\n}\ntype Post {\n  id: ID!\n}\n";

/// Which operation a row makes its conflicting call through.
#[derive(Debug, Clone, Copy)]
enum Through {
    /// `StartSchemaCreation`: a second document for the same API.
    StartSchemaCreation,
    /// `CreateType`: one more type in the system being created.
    CreateType,
    /// `UpdateType`: a change to a type the system already holds.
    UpdateType,
    /// `DeleteType`: the removal of a type the system already holds.
    DeleteType,
}

/// Every operation that can meet a schema creation already in progress.
const EVERY_OPERATION: &[Through] = &[
    Through::StartSchemaCreation,
    Through::CreateType,
    Through::UpdateType,
    Through::DeleteType,
];

impl Through {
    /// This row's label in every assertion message.
    fn label(self) -> &'static str {
        match self {
            Self::StartSchemaCreation => "StartSchemaCreation",
            Self::CreateType => "CreateType",
            Self::UpdateType => "UpdateType",
            Self::DeleteType => "DeleteType",
        }
    }

    /// Make this operation's call against `api_id`.
    async fn call(self, router: &MultiServiceRouter, api_id: &str) -> Answer {
        match self {
            Self::StartSchemaCreation => {
                start_creation(router, api_id, b"type Query {\n  other: String\n}\n").await
            }
            Self::CreateType => {
                call(
                    router,
                    "POST",
                    &format!("/v1/apis/{api_id}/types"),
                    Some(json!({
                        "definition": "type Added {\n  id: ID\n}\n",
                        "format": state::SDL_FORMAT,
                    })),
                )
                .await
            }
            Self::UpdateType => {
                call(
                    router,
                    "POST",
                    &format!("/v1/apis/{api_id}/types/{SEEDED_TYPE}"),
                    Some(json!({
                        "definition":
                            format!("type {SEEDED_TYPE} {{\n  renamed: String\n}}\n"),
                        "format": state::SDL_FORMAT,
                    })),
                )
                .await
            }
            Self::DeleteType => {
                call(
                    router,
                    "DELETE",
                    &format!("/v1/apis/{api_id}/types/{SEEDED_TYPE}"),
                    None,
                )
                .await
            }
        }
    }
}

/// One API holding the seeded type, with a schema creation of its own still in progress.
async fn api_with_a_creation_in_progress(router: &MultiServiceRouter, label: &str) -> String {
    let api = create_api(router, label, state::GRAPHQL_API_TYPE).await;
    let seeded = call(
        router,
        "POST",
        &format!("/v1/apis/{}/types", api.id),
        Some(json!({
            "definition": format!("type {SEEDED_TYPE} {{\n  id: ID\n}}\n"),
            "format": state::SDL_FORMAT,
        })),
    )
    .await;
    assert_eq!(
        seeded.status, 200,
        "case {label:?}: expected the seeded type to be created, got {} {:?}",
        seeded.status, seeded.body
    );

    let started = start_creation(router, &api.id, DOCUMENT.as_bytes()).await;
    assert_eq!(
        (started.status, started.body["status"].as_str()),
        (200, Some(state::SCHEMA_PROCESSING)),
        "case {label:?}: expected the creation to be scheduled, got {} {:?}",
        started.status,
        started.body
    );
    api.id
}

#[tokio::test]
async fn a_call_meeting_a_schema_being_altered_carries_the_schemas_own_sentence() {
    let router = router();

    for operation in EVERY_OPERATION {
        let label = operation.label();
        let api_id = api_with_a_creation_in_progress(&router, label).await;

        let refused = operation.call(&router, &api_id).await;
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (CONFLICT_STATUS, CONFLICT),
            "case {label:?}: expected {CONFLICT_STATUS} {CONFLICT} while the creation is owed, \
             got {} {} {:?}",
            refused.status,
            refused.error_type,
            refused.body
        );
        let message = refused.body["message"].as_str().unwrap_or_default();
        assert_eq!(
            message, SCHEMA_BEING_ALTERED,
            "case {label:?}: expected the schema-alteration sentence, got {message:?}"
        );
        assert!(
            !message.contains(&api_id) && !message.contains(SEEDED_TYPE),
            "case {label:?}: expected nothing of the request quoted, got {message:?}"
        );

        // The refusal left the creation where it was, so the clock still performs it:
        // the API ends up holding the document's own types, and the second document the
        // refused `StartSchemaCreation` carried is nowhere.
        await_creation(&router, &api_id, label).await;
        for (type_name, expected) in [("Query", 200), ("Post", 200), ("Added", 404)] {
            let answer = get_type(&router, &api_id, type_name, state::SDL_FORMAT).await;
            assert_eq!(
                answer.status, expected,
                "case {label:?}, type {type_name:?}: expected {expected}, got {} {:?}",
                answer.status, answer.body
            );
        }
        let hello = get_type(&router, &api_id, "Query", state::SDL_FORMAT).await;
        let definition = hello.body["type"]["definition"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            definition.contains("hello") && !definition.contains("other"),
            "case {label:?}: expected the document the accepted call carried, got {definition:?}"
        );

        // Nothing is being altered any more, so the very same call is served.
        let accepted = operation.call(&router, &api_id).await;
        let served: Vec<u16> = vec![200, 404];
        assert!(
            served.contains(&accepted.status),
            "case {label:?}: expected the call to be served once the creation settled, got {} \
             {:?}",
            accepted.status,
            accepted.body
        );
        assert_ne!(
            accepted.status, CONFLICT_STATUS,
            "case {label:?}: expected no conflict once the creation settled, got {:?}",
            accepted.body
        );
    }
}

/// The reporting call's own view of the same window: while the creation is owed the
/// status is the non-terminal `PROCESSING`, and nothing of the schema's state is changed
/// by the refusals above.
#[tokio::test]
async fn the_reported_status_while_the_schema_is_being_altered_is_processing() {
    let router = router();
    let api_id = api_with_a_creation_in_progress(&router, "reported").await;

    let reported: Value = call(&router, "GET", &schema_path(&api_id), None).await.body;
    assert_eq!(
        reported["status"].as_str(),
        Some(state::SCHEMA_PROCESSING),
        "case reported: expected the non-terminal creation status, got {reported:?}"
    );

    await_creation(&router, &api_id, "reported").await;
    let settled: Value = call(&router, "GET", &schema_path(&api_id), None).await.body;
    assert_eq!(
        settled["status"].as_str(),
        Some(state::SCHEMA_SUCCESS),
        "case reported: expected the creation to have been performed, got {settled:?}"
    );
}
