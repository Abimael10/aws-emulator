// SPDX-License-Identifier: CC0-1.0

//! How a DATA SOURCE's own `name` is refused, end to end over the restJson1 wire.
//!
//! The rule asserted here: a `name` outside the form `ResourceName` declares — its
//! `@length` 1..65536 or its `@pattern` `^[_A-Za-z][_0-9A-Za-z]*$`
//! (`smithy_query(query="shape", shape="ResourceName")`) — is refused by EVERY operation
//! that writes a data source under that name (`CreateDataSource`, `UpdateDataSource`)
//! with the operation's own modelled client error, AppSync's `BadRequestException` at
//! HTTP 400, carrying ONE sentence that names the RESOURCE whose name was refused and
//! neither the expression nor the submitted value. `ValidationException` is no
//! operation's modelled error on this service
//! (`smithy_query(query="operation", operation="CreateDataSource")` —
//! `BadRequestException` 400, `ConcurrentModificationException` 409,
//! `InternalFailureException` 500, `NotFoundException` 404, `UnauthorizedException` 401),
//! so a client dispatching on the modelled error type never matches that code, and the
//! constraint report's echo of the submitted name has nowhere to leak.
//!
//! The rows differ along what the rule generalises over: the OPERATION, WHICH character
//! outside the expression the name carries and WHERE it sits, and the LENGTH (empty, one
//! past the published maximum). The other half of the rule — a name INSIDE the declared
//! form is served and round-trips through `GetDataSource` — is asserted over each
//! character class the expression admits and both published length bounds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, router, Answer};
use crate::resource_tags::encode_label;

/// AppSync's own modelled error for a request that is not well formed, and its status
/// (`smithy_query(query="shape", shape="BadRequestException")`, `@httpError` 400).
const BAD_REQUEST: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u16 = 400;

/// The error code the framework would name a constraint refusal with — a shape no
/// operation of this service models, which is why no refusal below may carry it.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// `ResourceName`'s published length bounds (`@length` 1..65536).
const RESOURCE_NAME_MIN_LEN: usize = 1;
const RESOURCE_NAME_MAX_LEN: usize = 65_536;

/// The sentence the service refuses a DATA SOURCE's malformed name with, written out
/// here rather than read off the service's own constant, so a change of wording is caught
/// rather than followed.
const INVALID_DATA_SOURCE_NAME: &str =
    "The data source name is invalid. Please refer to the documentation for more details.";

/// `CreateDataSource`'s path for `api_id`.
fn create_path(api_id: &str) -> String {
    format!("/v1/apis/{api_id}/datasources")
}

/// `UpdateDataSource`'s and `GetDataSource`'s path for the data source `name`.
///
/// The name is a non-greedy `@httpLabel`, so it is percent-encoded the way an AWS SDK
/// encodes one — which is how a name carrying a character outside the unreserved set
/// reaches the handler at all.
fn data_source_path(api_id: &str, name: &str) -> String {
    format!("/v1/apis/{api_id}/datasources/{}", encode_label(name))
}

/// A `CreateDataSource`/`UpdateDataSource` body naming `name`.
///
/// `type` is required on both operations, and `NONE` needs no configuration object of
/// its own, so nothing but the name differs between the rows.
fn body(name: &str) -> Value {
    json!({"name": name, "type": "NONE"})
}

/// Read one data source back.
async fn get_data_source(router: &MultiServiceRouter, api_id: &str, name: &str) -> Answer {
    call(router, "GET", &data_source_path(api_id, name), None).await
}

/// The names OUTSIDE the form `ResourceName` declares: each class of character the
/// expression does not admit, at each position it can sit, and each length bound crossed.
fn malformed_names() -> Vec<(&'static str, String)> {
    vec![
        (
            "a hyphen inside the name",
            "bad-name-with-hyphens".to_string(),
        ),
        ("a leading digit", "1orders".to_string()),
        ("a dot", "orders.table".to_string()),
        ("a space", "order table".to_string()),
        ("a colon at the end", "orders:".to_string()),
        ("a non-ASCII letter", "cliënts".to_string()),
        ("an emoji", "orders🎉".to_string()),
        ("a newline inside the name", "orders\ntable".to_string()),
        ("an empty name, short of the minimum length", String::new()),
        (
            "one character past the maximum length",
            "d".repeat(RESOURCE_NAME_MAX_LEN + 1),
        ),
    ]
}

/// How long a name a request line still carries comfortably.
const ADDRESSABLE_NAME_LEN: usize = 1_024;

/// Whether `name` can be carried as a path label at all: the name is a non-greedy
/// `@httpLabel` on `UpdateDataSource`, and a request line has a length limit of its own,
/// so the rows that cross the shape's length bound reach only the operation that carries
/// the name in its BODY.
fn fits_a_request_line(name: &str) -> bool {
    !name.is_empty() && name.len() <= ADDRESSABLE_NAME_LEN
}

/// RULE: a malformed data-source name is the service's own `BadRequestException` with one
/// sentence naming the resource, on both operations that write one, and the refused call
/// writes nothing.
#[tokio::test]
async fn a_malformed_data_source_name_names_the_resource_under_the_services_own_code() {
    let router = router();
    let api_id = create_api(&router, "Api_data_source_name_form").await;

    // One data source the rows must leave exactly as it was.
    let seeded = "orders";
    let created = call(&router, "POST", &create_path(&api_id), Some(body(seeded))).await;
    assert_eq!(
        created.status, 200,
        "case seed the data source: expected 200, got {created:?}"
    );
    let stored = get_data_source(&router, &api_id, seeded).await;

    for (case, name) in malformed_names() {
        // A name no request line can carry — an empty one, or one past the shape's
        // length bound — can only be submitted by the operation that carries the name in
        // its BODY; every other row goes through both.
        let operations: Vec<(&str, String)> = if fits_a_request_line(&name) {
            vec![
                ("CreateDataSource", create_path(&api_id)),
                ("UpdateDataSource", data_source_path(&api_id, &name)),
            ]
        } else {
            vec![("CreateDataSource", create_path(&api_id))]
        };

        for (operation, path) in operations {
            let case = format!("{operation} with {case}");
            let answer = call(&router, "POST", &path, Some(body(&name))).await;
            assert_eq!(
                (answer.status, answer.error_type.as_str()),
                (BAD_REQUEST_STATUS, BAD_REQUEST),
                "case {case:?}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}"
            );
            assert_ne!(
                answer.error_type, VALIDATION_EXCEPTION,
                "case {case:?}: expected a code this operation models, got {answer:?}"
            );
            assert_eq!(
                answer.message, INVALID_DATA_SOURCE_NAME,
                "case {case:?}: expected the service's sentence, got {answer:?}"
            );
            assert!(
                !answer.message.contains("validation error")
                    && !answer.message.contains("constraint"),
                "case {case:?}: expected no constraint report, got {answer:?}"
            );
            assert!(
                name.is_empty() || !answer.message.contains(name.as_str()),
                "case {case:?}: expected nothing of the submitted name quoted, got {answer:?}"
            );
        }

        // Neither refusal touched the stored data source.
        let after = get_data_source(&router, &api_id, seeded).await;
        assert_eq!(
            after.body["dataSource"], stored.body["dataSource"],
            "case {case:?}: a refused call leaves the data source as it was, expected {:?}, got \
             {after:?}",
            stored.body["dataSource"]
        );
    }

    // The refused creates left the API holding the one data source it was seeded with,
    // so no refusal stored a record under a name the form does not admit.
    let listed = call(
        &router,
        "GET",
        &format!("/v1/apis/{api_id}/datasources"),
        None,
    )
    .await;
    assert_eq!(
        listed.body["dataSources"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "case the refused creates stored nothing: expected only the seeded data source, got \
         {listed:?}"
    );
}

/// RULE's other half: a name INSIDE the declared form is served, and the update path
/// addresses the record the create stored, so it round-trips through `GetDataSource`.
#[tokio::test]
async fn a_data_source_name_inside_the_declared_form_round_trips() {
    let router = router();
    let api_id = create_api(&router, "Api_data_source_name_served").await;

    for (case, name) in [
        (
            "one character, the shortest the bound admits",
            "d".repeat(RESOURCE_NAME_MIN_LEN),
        ),
        ("an underscore start", "_private_source".to_string()),
        ("digits after the first character", "ds_0_1".to_string()),
        (
            "every class the expression admits",
            "Orders_Table_42".to_string(),
        ),
        (
            "the longest name the bound admits",
            "d".repeat(RESOURCE_NAME_MAX_LEN),
        ),
    ] {
        let created = call(&router, "POST", &create_path(&api_id), Some(body(&name))).await;
        assert_eq!(
            created.status, 200,
            "case a create named {case:?}: expected 200, got {} {:?}",
            created.status, created.body
        );
        assert_eq!(
            created.body["dataSource"]["name"],
            json!(name),
            "case a create named {case:?}: expected the name back, got {:?}",
            created.body["dataSource"]
        );

        // The same value through the update path, which addresses the record the create
        // stored: the read reports what the update wrote. A name no request line carries
        // is round-tripped through the LISTING instead, the reader that needs no label.
        if fits_a_request_line(&name) {
            let updated = call(
                &router,
                "POST",
                &data_source_path(&api_id, &name),
                Some(json!({"name": name, "type": "NONE", "description": "updated"})),
            )
            .await;
            assert_eq!(
                updated.status, 200,
                "case an update named {case:?}: expected 200, got {} {:?}",
                updated.status, updated.body
            );
            let read = get_data_source(&router, &api_id, &name).await;
            assert_eq!(
                (
                    read.body["dataSource"]["name"].clone(),
                    read.body["dataSource"]["description"].clone()
                ),
                (json!(name), json!("updated")),
                "case an update named {case:?}: expected the stored record back, got {:?}",
                read.body["dataSource"]
            );
        } else {
            let listed = call(
                &router,
                "GET",
                &format!("/v1/apis/{api_id}/datasources"),
                None,
            )
            .await;
            assert!(
                listed.body["dataSources"]
                    .as_array()
                    .is_some_and(|sources| sources
                        .iter()
                        .any(|source| source["name"] == json!(name))),
                "case a create named {case:?}: expected the stored name in the listing, got \
                 {} data sources",
                listed.body["dataSources"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or_default()
            );
        }
    }
}
