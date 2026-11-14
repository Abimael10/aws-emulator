// SPDX-License-Identifier: CC0-1.0

//! How a read of a GraphQL API's introspection schema is CLASSIFIED when there is no
//! schema to answer with, and the creation status such an API reports.
//!
//! The rule: a schema is a property OF an API, so which of the two is missing decides
//! the class of the refusal. An `apiId` that names no API this account holds is a 404
//! `NotFoundException` — there is no resource to introspect; an API that EXISTS and holds
//! no type system is a 400 `GraphQLSchemaException` ("The GraphQL schema is not valid.")
//! carrying the parser's diagnostic for the empty document the call reads as its schema,
//! because the API is there and it is its schema that is unusable. The two are the only
//! thing that tells a bootstrap client "the id is wrong" from "this API has no schema
//! yet", so they are asserted on the identical call, in either representation
//! `OutputType` names.
//!
//! Beside that read: an API no schema creation has ever run for is in `NOT_APPLICABLE` —
//! none of the creation states applies to it — and once a creation the clock has
//! performed has installed a type system, the identical read is served 200 with the
//! schema in its `@httpPayload` blob, in both representations.
//!
//! The harness is the schema suites' own — the router the production emulator mounts, the
//! `StartSchemaCreation`/`GetSchemaCreationStatus` pair and the poll that waits for the
//! creation the clock owes, plus the raw-body read the schema blob needs — reused rather
//! than copied.

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use super::error_contract::{create_api, router};
use super::introspection_schema::get_schema;
use super::schema_creation::{await_creation, creation_status, start_creation};

/// Every value of the `OutputType` enum — the whole set the required `format` member of
/// `GetIntrospectionSchema` can take.
///
/// Source: `smithy_query(query="shape", shape="OutputType")`.
const OUTPUT_TYPES: &[&str] = &["SDL", "JSON"];

/// The `SchemaStatus` an API no creation has ever run for is in.
///
/// Source: `smithy_query(query="shape", shape="SchemaStatus")`, whose value set is
/// `ACTIVE | DELETING | FAILED | NOT_APPLICABLE | PROCESSING | SUCCESS`. Written out here
/// rather than read off the service's own constants, so a change is caught rather than
/// followed.
const NOT_APPLICABLE: &str = "NOT_APPLICABLE";

/// The diagnostic the refusal carries for an API that holds no type system: the parse of
/// the EMPTY document such a call reads as its schema, which stops at that document's
/// origin.
const NO_SCHEMA_MESSAGE: &str = "errors=[InvalidSyntaxError{ message=Invalid Syntax ,locations=\
                                 [SourceLocation{line=1, column=0}]}]";

/// The document the creation installs: a schema binding its query root, and the type that
/// root names.
const SCHEMA_DOCUMENT: &[u8] = b"schema { query: Query }\ntype Query { hello: String }\n";

#[tokio::test]
async fn an_api_holding_no_type_system_is_a_schema_refusal_rather_than_a_missing_api() {
    let router = router();
    let api_id = create_api(&router, "Api_no_schema_classification").await;

    // No creation has ever run for the API, so none of the creation states applies to it.
    let status = creation_status(&router, &api_id).await;
    assert_eq!(
        (status.status, status.body["status"].as_str()),
        (200, Some(NOT_APPLICABLE)),
        "case a freshly created API: expected 200 {NOT_APPLICABLE}, got {status:?}"
    );

    // The API is there and its schema is not, in either representation asked for.
    for format in OUTPUT_TYPES {
        let refused = get_schema(&router, &api_id, &format!("format={format}")).await;
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (400, "GraphQLSchemaException"),
            "case no schema in {format}: expected 400 GraphQLSchemaException, got {refused:?}"
        );
        let message = refused.message();
        assert_eq!(
            message, NO_SCHEMA_MESSAGE,
            "case no schema in {format}: expected the diagnostic {NO_SCHEMA_MESSAGE:?}, got \
             {message:?}"
        );
    }

    // An id that could name an API but names none this account holds is the OTHER class,
    // on the identical call: ids well inside the id length, from one character to exactly
    // it, and one drawn from outside the id alphabet.
    let absent = [
        ("one character".to_string(), "q".to_string()),
        (
            "outside the id alphabet".to_string(),
            "nonexistent0000000000".to_string(),
        ),
        (
            "exactly the id length".to_string(),
            "z".repeat(state::API_ID_LEN),
        ),
    ];
    for (label, id) in &absent {
        for format in OUTPUT_TYPES {
            let missing = get_schema(&router, id, &format!("format={format}")).await;
            assert_eq!(
                (missing.status, missing.error_type.as_str()),
                (404, "NotFoundException"),
                "case {label} in {format}: expected 404 NotFoundException, got {missing:?}"
            );
        }
    }

    // Once a creation the clock has performed has installed a type system, the identical
    // read is served the schema that was created.
    let started = start_creation(&router, &api_id, SCHEMA_DOCUMENT).await;
    assert_eq!(
        started.status, 200,
        "case starting the creation: expected 200, got {started:?}"
    );
    await_creation(&router, &api_id, "the schema of the classified API").await;
    for format in OUTPUT_TYPES {
        let served = get_schema(&router, &api_id, &format!("format={format}")).await;
        assert_eq!(
            served.status, 200,
            "case the created schema in {format}: expected 200, got {served:?}"
        );
        let text = served.text();
        assert!(
            text.contains("hello"),
            "case the created schema in {format}: expected the created field, got {text:?}"
        );
    }
}
