// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING `TestConnection` refuses a connection outside the
//! unified surface with, beside the other sentences the same operation refuses with.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: a refusal's message is the sentence the service words for THAT error
//! class, with only the variable parts the class has — and the restriction on which
//! connections this operation can test has NONE: it states the restriction about the
//! operation generically ("This operation"), so the one byte-identical sentence answers
//! every kind outside the unified surface, whichever form the definition under test arrived
//! in and whatever the request named.
//!
//! The rows below span what the rule generalises over: every legacy `ConnectionType` the
//! model declares, the two forms a definition arrives in (inline, and a stored connection
//! addressed by name), the catalog named explicitly or left to default, and the name the
//! stored connection carries — none of which may reach the sentence. The recorded case (a
//! stored JDBC connection) is one row.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{messages, LEGACY_CONNECTION_TYPES};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID};

/// AWS's own sentence for this refusal, spelled out here rather than read from the service's
/// constant: what the rule requires is this exact text on the wire, so the expectation may
/// not be the very value under test.
const UNIFIED_ONLY: &str = "This operation is only supported for the Unified Connections";

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

/// The message of one `TestConnection` that must be the unified-only refusal: the status and
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

/// Every connection outside the unified surface earns AWS's one fixed sentence, byte for
/// byte, whichever form the definition arrived in and whatever the request named.
#[tokio::test]
async fn the_unified_only_refusal_is_aws_fixed_sentence() {
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
            actual, UNIFIED_ONLY,
            "case {case}: expected {UNIFIED_ONLY:?}, got {actual:?}"
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
            actual, UNIFIED_ONLY,
            "case {case}: expected {UNIFIED_ONLY:?}, got {actual:?}"
        );
    }

    // A STORED definition of a legacy kind, addressed by name: the recorded case, and the
    // name it carries reaches the sentence no more than the type does.
    for connection_type in STORABLE_LEGACY_TYPES {
        for name in [
            format!("stored-{connection_type}"),
            format!("a-much-longer-{}-connection-name", connection_type.to_lowercase()),
        ] {
            given_connection(&router, &name, connection_type).await;
            let case = format!("stored {connection_type} as {name}");
            let actual = refusal_message(&case, &router, &json!({ "ConnectionName": &name })).await;
            assert_eq!(
                actual, UNIFIED_ONLY,
                "case {case}: expected {UNIFIED_ONLY:?}, got {actual:?}"
            );

            let case = format!("stored {connection_type} as {name} in the named catalog");
            let actual = refusal_message(
                &case,
                &router,
                &json!({ "ConnectionName": &name, "CatalogId": ACCOUNT_ID }),
            )
            .await;
            assert_eq!(
                actual, UNIFIED_ONLY,
                "case {case}: expected {UNIFIED_ONLY:?}, got {actual:?}"
            );
        }
    }

    // The service states the sentence in exactly one place, which is what the handler's two
    // branches both read.
    assert_eq!(
        messages::TEST_CONNECTION_UNIFIED_ONLY, UNIFIED_ONLY,
        "the service's own constant must carry AWS's sentence, got {:?}",
        messages::TEST_CONNECTION_UNIFIED_ONLY
    );
}

/// The restriction's sentence is its OWN, not one of the other `TestConnection` refusals
/// reworded: a caller that branches on the message has to be able to tell them apart.
#[tokio::test]
async fn the_unified_only_sentence_differs_from_the_operations_other_refusals() {
    let router = emulator();
    given_connection(&router, "stored-jdbc", "JDBC").await;

    let restriction =
        refusal_message("stored JDBC", &router, &json!({ "ConnectionName": "stored-jdbc" })).await;

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
        ),
        (
            "a complete unified definition",
            json!({
                "TestConnectionInput": {
                    "ConnectionType": "SALESFORCE",
                    "ConnectionProperties": {},
                },
            }),
        ),
        (
            "a name the catalog holds nothing under",
            json!({ "ConnectionName": "absent-connection" }),
        ),
    ];

    for (case, body) in others {
        let (status, answer) = call(&router, "TestConnection", &body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {answer}"
        );
        let other = error_message(&answer);
        assert_ne!(
            other, restriction,
            "case {case}: must not carry the unified-only sentence, got {answer}"
        );
        assert!(
            !error_code(&answer).contains("ValidationException"),
            "case {case}: must not carry the restriction's error class, got {answer}"
        );
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
        let actual = refusal_message(&case, &router, &json!({ "ConnectionName": "stored-jdbc" })).await;
        assert_eq!(
            actual, UNIFIED_ONLY,
            "case {case}: expected {UNIFIED_ONLY:?}, got {actual:?}"
        );
    }
    let after = ok(&router, "GetConnection", &json!({ "Name": "stored-jdbc" })).await;
    assert_eq!(
        after, before,
        "the refused tests must leave the stored connection unchanged, got {after} after \
         {before}"
    );
}
