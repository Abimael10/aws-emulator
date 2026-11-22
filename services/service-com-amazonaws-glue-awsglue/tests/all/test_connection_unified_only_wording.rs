// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING `TestConnection` refuses a connection outside the
//! unified surface with, beside the other sentences the same operation refuses with.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: a refusal's message is the sentence the service words for THAT refusal
//! as the request form it answers, so where the service words one restriction with two
//! sentences — one per form — each form renders its own and neither may be served by the
//! other's. The restriction on which connections this operation can test is exactly that: a
//! definition handed to the call INLINE is refused with the sentence whose subject is the
//! restricted operation, named as the API names it, and a STORED connection addressed by name
//! with the sentence whose subject is the generic one. Both are entirely fixed text — neither
//! takes a variable part from the request or from state — and the two differ, which is what
//! lets a caller tell the two forms apart on the message alone.
//!
//! The rows below span what the rule generalises over: every legacy `ConnectionType` the
//! model declares, the two forms a definition arrives in, the catalog named explicitly or left
//! to default, and the name the stored connection carries — none of which may reach either
//! sentence. The recorded cases (an inline JDBC definition, and a stored JDBC connection) are
//! one row each.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{messages, LEGACY_CONNECTION_TYPES};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID};

/// AWS's own sentence for the refusal of an INLINE `TestConnectionInput`, spelled out here
/// rather than read from the service's constant: what the rule requires is this exact text on
/// the wire, so the expectation may not be the very value under test.
const INLINE_UNIFIED_ONLY: &str = "TestConnection is only supported for the Unified Connections";

/// AWS's own sentence for the refusal of a STORED connection addressed by `ConnectionName`,
/// spelled out for the same reason.
const STORED_UNIFIED_ONLY: &str = "This operation is only supported for the Unified Connections";

/// The legacy connection kinds a `CreateConnection` admits with the JDBC-shaped properties
/// below, so a stored definition of each can be addressed by name. The rest of
/// [`LEGACY_CONNECTION_TYPES`] is covered inline.
const STORABLE_LEGACY_TYPES: [&str; 3] = ["JDBC", "MONGODB", "NETWORK"];

/// The properties a stored legacy connection carries, as `ConnectionInput`'s documentation
/// lists them for a JDBC-shaped kind: "All of (`HOST`, `PORT`, `JDBC_ENGINE`) or
/// `JDBC_CONNECTION_URL`" and "All of (`USERNAME`, `PASSWORD`) or `SECRET_ID`".
fn jdbc_properties() -> Value {
    json!({
        "JDBC_CONNECTION_URL": "jdbc:postgresql://warehouse.example.internal:5432/analytics",
        "USERNAME": "analytics",
        "PASSWORD": "a-password",
    })
}

/// Register one connection of `connection_type` under `name`.
async fn given_connection(router: &MultiServiceRouter, name: &str, connection_type: &str) {
    ok(
        router,
        "CreateConnection",
        &json!({
            "ConnectionInput": {
                "Name": name,
                "ConnectionType": connection_type,
                "ConnectionProperties": jdbc_properties(),
            },
        }),
    )
    .await;
}

/// The message of one `TestConnection` that must be a unified-only refusal: the status and
/// the error class are asserted here, so each row only states the sentence it expects.
async fn refusal_message(case: &str, router: &MultiServiceRouter, body: &Value) -> String {
    let (status, answer) = call(router, "TestConnection", body).await;
    assert_eq!(
        status, 400,
        "case {case}: expected 400, got {status} with {answer}"
    );
    assert!(
        error_code(&answer).contains("ValidationException"),
        "case {case}: expected ValidationException, got {answer}"
    );
    error_message(&answer)
}

/// An INLINE definition of a kind outside the unified surface earns AWS's operation-named
/// sentence, byte for byte, whatever the definition carried and whatever catalog the request
/// addressed.
#[tokio::test]
async fn an_inline_definition_is_refused_with_the_operation_named_sentence() {
    let router = emulator();

    for connection_type in LEGACY_CONNECTION_TYPES {
        let case = format!("inline {connection_type}");
        let actual = refusal_message(
            &case,
            &router,
            &json!({
                "TestConnectionInput": {
                    "ConnectionType": connection_type,
                    "ConnectionProperties": jdbc_properties(),
                },
            }),
        )
        .await;
        assert_eq!(
            actual, INLINE_UNIFIED_ONLY,
            "case {case}: expected {INLINE_UNIFIED_ONLY:?}, got {actual:?}"
        );

        // The catalog the request addresses is no part of the sentence either.
        let case = format!("inline {connection_type} in the named catalog");
        let actual = refusal_message(
            &case,
            &router,
            &json!({
                "CatalogId": ACCOUNT_ID,
                "TestConnectionInput": {
                    "ConnectionType": connection_type,
                    "ConnectionProperties": jdbc_properties(),
                },
            }),
        )
        .await;
        assert_eq!(
            actual, INLINE_UNIFIED_ONLY,
            "case {case}: expected {INLINE_UNIFIED_ONLY:?}, got {actual:?}"
        );

        // An inline definition carrying no properties at all reaches the same restriction,
        // which is read off the kind alone.
        let case = format!("inline {connection_type} with no properties");
        let actual = refusal_message(
            &case,
            &router,
            &json!({
                "TestConnectionInput": {
                    "ConnectionType": connection_type,
                    "ConnectionProperties": {},
                },
            }),
        )
        .await;
        assert_eq!(
            actual, INLINE_UNIFIED_ONLY,
            "case {case}: expected {INLINE_UNIFIED_ONLY:?}, got {actual:?}"
        );
    }
}

/// A STORED connection of a kind outside the unified surface earns AWS's generic-subject
/// sentence, byte for byte, whatever name it carries and whatever catalog the request
/// addressed.
#[tokio::test]
async fn a_stored_connection_is_refused_with_the_generic_subject_sentence() {
    let router = emulator();

    for connection_type in STORABLE_LEGACY_TYPES {
        for name in [
            format!("stored-{connection_type}"),
            format!(
                "a-much-longer-{}-connection-name",
                connection_type.to_lowercase()
            ),
        ] {
            given_connection(&router, &name, connection_type).await;
            let case = format!("stored {connection_type} as {name}");
            let actual = refusal_message(&case, &router, &json!({ "ConnectionName": &name })).await;
            assert_eq!(
                actual, STORED_UNIFIED_ONLY,
                "case {case}: expected {STORED_UNIFIED_ONLY:?}, got {actual:?}"
            );

            let case = format!("stored {connection_type} as {name} in the named catalog");
            let actual = refusal_message(
                &case,
                &router,
                &json!({ "ConnectionName": &name, "CatalogId": ACCOUNT_ID }),
            )
            .await;
            assert_eq!(
                actual, STORED_UNIFIED_ONLY,
                "case {case}: expected {STORED_UNIFIED_ONLY:?}, got {actual:?}"
            );
        }
    }
}

/// The two sentences are DIFFERENT, and each states its own subject: the inline form names the
/// restricted operation as this API names it, the stored form the generic one and not the
/// operation's name. A client that tells the two forms apart on the message has only this to
/// go on, since both refusals carry the same status and the same error class.
#[tokio::test]
async fn each_request_form_states_its_own_subject() {
    let router = emulator();
    given_connection(&router, "stored-mongodb", "MONGODB").await;
    given_connection(&router, "stored-jdbc", "JDBC").await;

    // The operation the restriction is a restriction on, as the model names it
    // (`smithy_query(query="operation",operation="TestConnection")`).
    const OPERATION: &str = "TestConnection";

    let inline_rows: Vec<(&str, Value)> = vec![
        (
            "an inline SFTP definition",
            json!({
                "TestConnectionInput": {
                    "ConnectionType": "SFTP",
                    "ConnectionProperties": jdbc_properties(),
                },
            }),
        ),
        (
            "an inline KAFKA definition",
            json!({
                "TestConnectionInput": {
                    "ConnectionType": "KAFKA",
                    "ConnectionProperties": jdbc_properties(),
                },
            }),
        ),
    ];
    for (case, body) in inline_rows {
        let actual = refusal_message(case, &router, &body).await;
        assert!(
            actual.starts_with(&format!("{OPERATION} ")),
            "case {case}: the inline sentence names the restricted operation first, expected it \
             to open with {OPERATION:?}, got {actual:?}"
        );
        assert_eq!(
            actual, INLINE_UNIFIED_ONLY,
            "case {case}: expected {INLINE_UNIFIED_ONLY:?}, got {actual:?}"
        );
    }

    let stored_rows: Vec<(&str, Value)> = vec![
        (
            "a stored MONGODB connection",
            json!({ "ConnectionName": "stored-mongodb" }),
        ),
        (
            "a stored JDBC connection",
            json!({ "ConnectionName": "stored-jdbc" }),
        ),
    ];
    for (case, body) in stored_rows {
        let actual = refusal_message(case, &router, &body).await;
        assert!(
            !actual.starts_with(OPERATION),
            "case {case}: the stored sentence states the generic subject, not the operation's \
             name, got {actual:?}"
        );
        assert_eq!(
            actual, STORED_UNIFIED_ONLY,
            "case {case}: expected {STORED_UNIFIED_ONLY:?}, got {actual:?}"
        );
    }

    assert_ne!(
        INLINE_UNIFIED_ONLY, STORED_UNIFIED_ONLY,
        "the two forms of the restriction are worded differently, so one sentence cannot \
         answer both"
    );

    // The service states each sentence in exactly one place, which is what the handler's two
    // branches read.
    assert_eq!(
        messages::TEST_CONNECTION_INPUT_UNIFIED_ONLY, INLINE_UNIFIED_ONLY,
        "the inline form's constant must carry AWS's sentence, got {:?}",
        messages::TEST_CONNECTION_INPUT_UNIFIED_ONLY
    );
    assert_eq!(
        messages::TEST_CONNECTION_STORED_UNIFIED_ONLY, STORED_UNIFIED_ONLY,
        "the stored form's constant must carry AWS's sentence, got {:?}",
        messages::TEST_CONNECTION_STORED_UNIFIED_ONLY
    );
}

/// The restriction's sentences are their OWN, not one of the other `TestConnection` refusals
/// reworded: a caller that branches on the message has to be able to tell them apart, and the
/// field-level refusal of an inline definition carries a different error class as well.
#[tokio::test]
async fn the_unified_only_sentences_differ_from_the_operations_other_refusals() {
    let router = emulator();
    given_connection(&router, "stored-jdbc", "JDBC").await;

    let stored_restriction =
        refusal_message("stored JDBC", &router, &json!({ "ConnectionName": "stored-jdbc" })).await;
    let inline_restriction = refusal_message(
        "inline JDBC",
        &router,
        &json!({
            "TestConnectionInput": {
                "ConnectionType": "JDBC",
                "ConnectionProperties": jdbc_properties(),
            },
        }),
    )
    .await;

    // A unified definition missing the credentials its authentication type needs, the report
    // that a complete definition reached no data source, and the miss of a name the catalog
    // holds nothing under: the other three sentences this operation words.
    let others = [
        (
            "a missing credentials member",
            json!({
                "TestConnectionInput": {
                    "ConnectionType": "SALESFORCE",
                    "ConnectionProperties": {},
                    "AuthenticationConfiguration": { "AuthenticationType": "OAUTH2" },
                },
            }),
            Some(messages::authentication_member_required_for_type(
                "OAuth2Properties",
                "OAUTH2",
            )),
        ),
        (
            "a complete unified definition",
            json!({
                "TestConnectionInput": {
                    "ConnectionType": "SALESFORCE",
                    "ConnectionProperties": {},
                },
            }),
            None,
        ),
        (
            "a name the catalog holds nothing under",
            json!({ "ConnectionName": "absent-connection" }),
            None,
        ),
    ];

    for (case, body, expected_message) in others {
        let (status, answer) = call(&router, "TestConnection", &body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {answer}"
        );
        let other = error_message(&answer);
        for restriction in [&inline_restriction, &stored_restriction] {
            assert_ne!(
                &other, restriction,
                "case {case}: must not carry a unified-only sentence, got {answer}"
            );
        }
        assert!(
            !error_code(&answer).contains("ValidationException"),
            "case {case}: must not carry the restriction's error class, got {answer}"
        );
        if let Some(expected_message) = expected_message {
            assert_eq!(
                other, expected_message,
                "case {case}: expected {expected_message:?}, got {other:?}"
            );
        }
    }
}

/// The refused test is REPORTING only: the stored definition it read is unchanged
/// afterwards, so what a later caller sees does not depend on whether a test was asked for.
#[tokio::test]
async fn a_refused_test_leaves_the_stored_connection_as_it_was() {
    let router = emulator();
    given_connection(&router, "stored-jdbc", "JDBC").await;

    let before = ok(&router, "GetConnection", &json!({ "Name": "stored-jdbc" })).await;
    for attempt in 0..3 {
        let case = format!("attempt {attempt}");
        let actual =
            refusal_message(&case, &router, &json!({ "ConnectionName": "stored-jdbc" })).await;
        assert_eq!(
            actual, STORED_UNIFIED_ONLY,
            "case {case}: expected {STORED_UNIFIED_ONLY:?}, got {actual:?}"
        );
    }
    let after = ok(&router, "GetConnection", &json!({ "Name": "stored-jdbc" })).await;
    assert_eq!(
        after, before,
        "the refused tests must leave the stored connection unchanged, got {after} after \
         {before}"
    );
}
