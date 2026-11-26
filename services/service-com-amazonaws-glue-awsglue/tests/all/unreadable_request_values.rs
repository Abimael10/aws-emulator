// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a request carrying a value the modelled member it is bound to
//! cannot be READ from — the deserialization layer in front of every operation.
//!
//! RULE: a value whose JSON class the modelled type of the member it is bound to cannot be
//! read from never becomes a value at all, so the request is refused as the protocol's own
//! marshalling failure — HTTP 400 `SerializationException`, whose capital-`Message` states
//! the CLASS of value sent and the modelled type that value could not be converted to — and
//! NOT as the constraint refusal a value that was read and then broke a rule of the model
//! earns. The class is named, never the value, so one wording answers every value of that
//! class.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: the class of JSON value sent — each
//! boolean literal and each side of the numeric split among them — the modelled type of the
//! member it was bound to, and the operation that declares that member — a catalog read, a
//! catalog write, a projected listing and a job listing — because the rule belongs to the
//! protocol rather than to one operation. The controls beside them hold the OTHER layer in
//! place: a value that reads and then breaks a rule of the model, an absent required member,
//! and a well-formed request, all of which must answer exactly as before.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    call, call_reported, emulator, error_code, error_message, ok, table_names, ACCOUNT_ID, REGION,
};

/// The error code the protocol refuses a value nothing could be read from with.
const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The error this service refuses a value that WAS read and then broke one of its own rules
/// with — the layer that must stay exactly where it was.
const INVALID_INPUT_EXCEPTION: &str = "InvalidInputException";

/// The sentence `GetTables` refuses a page size naming no servable page with.
const PAGE_SIZE_REFUSAL: &str = "MaxItems has to be a positive number";

/// The sentence the catalog refuses an absent `DatabaseName` with.
const DATABASE_NAME_REQUIRED: &str = "Database name is required";

/// A database and one table in it, so a well-formed read of the same surface has something to
/// answer with.
async fn given_a_table(router: &MultiServiceRouter, database: &str, table: &str) {
    ok(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    ok(
        router,
        "CreateTable",
        &json!({ "DatabaseName": database, "TableInput": { "Name": table } }),
    )
    .await;
}

/// RULE: the refusal names the class of value sent and the modelled type it could not be
/// converted to, under the protocol's own error code and in the capital-`Message` member.
#[tokio::test]
async fn an_unreadable_value_is_refused_as_a_serialization_failure() {
    let router = emulator();
    given_a_table(&router, "unreadable_db", "unreadable_table").await;

    // (case, operation, body, the sentence the refusal states)
    let rows: [(&str, &str, Value, &str); 10] = [
        (
            "a quoted literal bound to an integer member",
            "GetTables",
            json!({ "DatabaseName": "unreadable_db", "MaxResults": "STRING_VALUE" }),
            "STRING_VALUE cannot be converted to Integer",
        ),
        (
            "a quoted literal that spells a number is still a quoted literal",
            "GetJobs",
            json!({ "MaxResults": "10" }),
            "STRING_VALUE cannot be converted to Integer",
        ),
        (
            "a true literal bound to an integer member",
            "GetDatabases",
            json!({ "MaxResults": true }),
            "TRUE_VALUE cannot be converted to Integer",
        ),
        (
            "a false literal bound to an integer member",
            "GetDatabases",
            json!({ "MaxResults": false }),
            "FALSE_VALUE cannot be converted to Integer",
        ),
        (
            "a fraction bound to an integer member",
            "GetTables",
            json!({ "DatabaseName": "unreadable_db", "MaxResults": 7.5 }),
            "DECIMAL_VALUE cannot be converted to Integer",
        ),
        (
            "a quoted literal bound to a boolean member",
            "GetTables",
            json!({ "DatabaseName": "unreadable_db", "IncludeStatusDetails": "true" }),
            "STRING_VALUE cannot be converted to Boolean",
        ),
        (
            "a number bound to a string member",
            "GetTables",
            json!({ "DatabaseName": 7 }),
            "NUMBER_VALUE cannot be converted to String",
        ),
        (
            "a flag bound to a string member of a write",
            "CreateTable",
            json!({ "DatabaseName": true, "TableInput": { "Name": "t" } }),
            "TRUE_VALUE cannot be converted to String",
        ),
        (
            "a list bound to a string member",
            "GetTable",
            json!({ "DatabaseName": "unreadable_db", "Name": ["unreadable_table"] }),
            "ARRAY_VALUE cannot be converted to String",
        ),
        (
            "an object bound to a string member",
            "DeleteTable",
            json!({ "DatabaseName": { "Name": "unreadable_db" }, "Name": "unreadable_table" }),
            "OBJECT_VALUE cannot be converted to String",
        ),
    ];

    for (case, operation, body, sentence) in rows {
        let (status, answer) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {operation} to refuse the request with 400, got {status} \
             with {answer}"
        );
        assert_eq!(
            error_code(&answer),
            SERIALIZATION_EXCEPTION,
            "case {case:?}: expected the protocol's {SERIALIZATION_EXCEPTION}, got {}",
            error_code(&answer)
        );
        assert_eq!(
            error_message(&answer),
            sentence,
            "case {case:?}: expected {sentence:?}, got {:?} in {answer}",
            error_message(&answer)
        );
    }
}

/// RULE (the other layer): a value that IS read and then breaks a rule of the model, and a
/// required member no value was supplied for, keep the refusals they already earn — the
/// deserialization layer answers neither.
#[tokio::test]
async fn a_value_that_reads_keeps_its_constraint_refusal() {
    let router = emulator();
    given_a_table(&router, "constraint_db", "constraint_table").await;

    // (case, the body, the code, the sentence)
    let rows: [(&str, Value, &str, &str); 2] = [
        (
            "a page size that names no servable page",
            json!({ "DatabaseName": "constraint_db", "MaxResults": 0 }),
            INVALID_INPUT_EXCEPTION,
            PAGE_SIZE_REFUSAL,
        ),
        (
            "no value at all for the required database",
            json!({ "MaxResults": 10 }),
            INVALID_INPUT_EXCEPTION,
            DATABASE_NAME_REQUIRED,
        ),
    ];

    for (case, body, code, sentence) in rows {
        let (status, answer) = call(&router, "GetTables", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {answer}"
        );
        assert_eq!(
            error_code(&answer),
            code,
            "case {case:?}: expected {code}, got {} in {answer}",
            error_code(&answer)
        );
        assert_eq!(
            error_message(&answer),
            sentence,
            "case {case:?}: expected {sentence:?}, got {:?} in {answer}",
            error_message(&answer)
        );
    }

    // A value bound to a member this protocol reads from more than one form, or to one no
    // conversion is attempted for, is left to the codec: it is still refused, but which layer
    // words it is not this suite's subject.
    let (status, answer) = call(
        &router,
        "GetTables",
        &json!({ "DatabaseName": "constraint_db", "AttributesToGet": "NAME" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a quoted literal bound to a list member: expected 400, got {status} with {answer}"
    );
}

/// RULE: a request every value of which reads is served exactly as before — the layer refuses
/// nothing it should not, and the read answers from the stored table.
#[tokio::test]
async fn a_readable_request_is_served() {
    let router = emulator();
    given_a_table(&router, "readable_db", "readable_table").await;

    // (case, the body)
    let rows: [(&str, Value); 3] = [
        (
            "every member absent but the required one",
            json!({ "DatabaseName": "readable_db" }),
        ),
        (
            "a page size and a catalog id in their modelled forms",
            json!({ "DatabaseName": "readable_db", "MaxResults": 10, "CatalogId": ACCOUNT_ID }),
        ),
        (
            "the smallest page the listing serves",
            json!({ "DatabaseName": "readable_db", "MaxResults": 1 }),
        ),
    ];

    for (case, body) in rows {
        let page = ok(&router, "GetTables", &body).await;
        assert_eq!(
            table_names(&page),
            vec!["readable_table".to_string()],
            "case {case:?}: expected the stored table, got {page}"
        );
    }
}

/// RULE: a refusal this layer answers names the request it answered, exactly as every other
/// response of this service does, and carries the media type of the surface the request
/// addressed — the protocol's own for the operations the protocol serves, and the REST/JSON
/// front end's for the operations that front end serves.
#[tokio::test]
async fn the_refusal_is_reported_as_the_surface_it_answers_for() {
    let router = emulator();

    // (case, operation, body, the media type the surface answers in)
    let rows: [(&str, &str, Value, &str); 2] = [
        (
            "an operation the protocol serves",
            "GetTables",
            json!({ "DatabaseName": "any_db", "MaxResults": "STRING_VALUE" }),
            "application/x-amz-json-1.1",
        ),
        (
            "an operation the REST/JSON front end serves",
            "SearchAssets",
            json!({ "SearchText": "anything", "MaxResults": "STRING_VALUE" }),
            "application/json",
        ),
    ];

    for (case, operation, body, media_type) in rows {
        let (status, headers, answer) = call_reported(&router, REGION, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the unreadable value to be refused, got {status} with \
             {answer}"
        );
        assert_eq!(
            error_code(&answer),
            SERIALIZATION_EXCEPTION,
            "case {case:?}: expected the protocol's {SERIALIZATION_EXCEPTION}, got {} in \
             {answer}",
            error_code(&answer)
        );
        let reported = headers
            .get("x-amzn-requestid")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            !reported.is_empty(),
            "case {case:?}: expected the response to name the request it answered, got the \
             headers {headers:?}"
        );
        let content_type = headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            content_type, media_type,
            "case {case:?}: expected the media type {media_type:?}, got {content_type:?}"
        );
    }
}
