// SPDX-License-Identifier: CC0-1.0

//! What a `CreateTable` has to carry for there to be a table to create, and how a create
//! carrying none is refused.
//!
//! The rule under test: the definition of the table a create stores comes from the submitted
//! `TableInput`, else from the `CreateIcebergTableInput` an open-table-format create
//! describes the table with; a create carrying NEITHER names no table at all and is refused
//! with the operation's modelled `InvalidInputException` (HTTP 400) whose message names the
//! nested input the open-table-format path requires and states that it must not be null —
//! entirely fixed text, with nothing of the request interpolated — and stores nothing. A
//! create carrying either of the two is served and the table reads back, so the refusals are
//! about the missing definition and not about the request shape.
//!
//! The rows differ along what the rule generalises over: how far into the open-table-format
//! input the create got before running out of a definition (no format input at all, one
//! naming no format, one naming the format but no create input), which of the two inputs a
//! served create defines the table with, and — for the refusals an incomplete create earns
//! about a member of its own — which member that is.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The database every create below stores its table in.
const DATABASE: &str = "definition_input_db";

/// The sentence a create that carries no table definition must be refused with. Written here
/// independently of the handler, so this suite pins the wording and not the code.
const MISSING_CREATE_INPUT: &str = "CreateIcebergTableInput cannot be equal to null";

/// Read one table of [`DATABASE`] back, and answer the HTTP status and body.
async fn get_table(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(
        router,
        "GetTable",
        &json!({ "DatabaseName": DATABASE, "Name": name }),
    )
    .await
}

/// A create that carries no input defining the table it asks for is refused, in the one
/// sentence, and stores nothing.
#[tokio::test]
async fn a_create_carrying_no_table_definition_names_the_input_it_needs() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    // (case, the table the create addresses, the request minus DatabaseName)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            // The recorded case: neither input is there at all.
            "no table input and no open table format input",
            "no_input",
            json!({ "Name": "no_input" }),
        ),
        (
            "an open table format input naming no format",
            "no_format",
            json!({ "Name": "no_format", "OpenTableFormatInput": {} }),
        ),
        (
            "a format named with no create input under it",
            "no_create_input",
            json!({
                "Name": "no_create_input",
                "OpenTableFormatInput": {
                    "IcebergInput": { "MetadataOperation": "CREATE" }
                }
            }),
        ),
    ];

    for (case, name, request) in &rows {
        let mut body = request.clone();
        body["DatabaseName"] = json!(DATABASE);
        let (status, answer) = call(&router, "CreateTable", &body).await;
        assert_eq!(
            status, 400,
            "case {case}: a create with no table definition must be refused, got {status} \
             with {answer}"
        );
        assert!(
            error_code(&answer).ends_with("InvalidInputException"),
            "case {case}: expected InvalidInputException, got {} in {answer}",
            error_code(&answer)
        );
        assert_eq!(
            error_message(&answer),
            MISSING_CREATE_INPUT,
            "case {case}: unexpected wording for {body}"
        );

        // The refused create stored nothing, so the read that follows is a miss.
        let (status, read) = get_table(&router, name).await;
        assert_eq!(
            status, 400,
            "case {case}: a refused create stores no table, got {status} with {read}"
        );
    }
}

/// A create whose definition inputs are INCOMPLETE in a way the create's own members decide
/// is refused about the member actually missing: a format input naming no format over a
/// `TableInput` that does define the table, and an Iceberg create input with no `Name` to
/// store the table it describes under.
#[tokio::test]
async fn an_incomplete_definition_is_refused_about_the_member_that_is_missing() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    // (case, the table the create addresses, the request minus DatabaseName, the sentence).
    // Both sentences are written here independently of the handler.
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            // The table IS defined, so the create runs out of a format rather than of a
            // definition: `OpenTableFormatInput` carries exactly one format member.
            "a defined table under a format input naming no format",
            "defined_no_format",
            json!({
                "TableInput": {
                    "Name": "defined_no_format",
                    "StorageDescriptor": { "Location": "s3://lake/defined_no_format/" }
                },
                "OpenTableFormatInput": {}
            }),
            "IcebergInput cannot be equal to null",
        ),
        (
            // The Iceberg create input describes a table, and the request's own `Name` is
            // the name it would be stored under — so a create that names none is the
            // catalog's refusal of a table name it was given none of.
            "an Iceberg create input with no name to store it under",
            "unnamed",
            json!({
                "OpenTableFormatInput": {
                    "IcebergInput": {
                        "MetadataOperation": "CREATE",
                        "CreateIcebergTableInput": {
                            "Location": "s3://lake/unnamed/",
                            "Schema": {
                                "Fields": [
                                    { "Id": 1, "Name": "id", "Type": "long", "Required": true }
                                ]
                            }
                        }
                    }
                }
            }),
            "Table name cannot be equal to null or empty",
        ),
    ];

    for (case, name, request, expected) in &rows {
        let mut body = request.clone();
        body["DatabaseName"] = json!(DATABASE);
        let (status, answer) = call(&router, "CreateTable", &body).await;
        assert_eq!(
            status, 400,
            "case {case}: an incomplete create must be refused, got {status} with {answer}"
        );
        assert!(
            error_code(&answer).ends_with("InvalidInputException"),
            "case {case}: expected InvalidInputException, got {} in {answer}",
            error_code(&answer)
        );
        assert_eq!(
            error_message(&answer),
            *expected,
            "case {case}: unexpected wording for {body}"
        );

        let (status, read) = get_table(&router, name).await;
        assert_eq!(
            status, 400,
            "case {case}: a refused create stores no table, got {status} with {read}"
        );
    }
}

/// A create carrying EITHER of the two inputs that define a table is served, and the table
/// reads back under the name the request addressed it by.
#[tokio::test]
async fn a_create_carrying_either_definition_input_is_served() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    // (case, the table name, the request minus DatabaseName)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "the table input defines the table",
            "plain",
            json!({
                "TableInput": {
                    "Name": "plain",
                    "TableType": "EXTERNAL_TABLE",
                    "StorageDescriptor": { "Location": "s3://lake/plain/" }
                }
            }),
        ),
        (
            "the Iceberg create input defines the table",
            "iceberg",
            json!({
                "Name": "iceberg",
                "OpenTableFormatInput": {
                    "IcebergInput": {
                        "MetadataOperation": "CREATE",
                        "CreateIcebergTableInput": {
                            "Location": "s3://lake/iceberg/",
                            "Schema": {
                                "Fields": [
                                    { "Id": 1, "Name": "id", "Type": "long", "Required": true }
                                ]
                            }
                        }
                    }
                }
            }),
        ),
    ];

    for (case, name, request) in &rows {
        let mut body = request.clone();
        body["DatabaseName"] = json!(DATABASE);
        ok(&router, "CreateTable", &body).await;

        let (status, read) = get_table(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case}: the created table must read back, got {status} with {read}"
        );
        assert_eq!(
            read["Table"]["Name"],
            json!(name),
            "case {case}: the read must report the table the create addressed, got {read}"
        );
    }
}
