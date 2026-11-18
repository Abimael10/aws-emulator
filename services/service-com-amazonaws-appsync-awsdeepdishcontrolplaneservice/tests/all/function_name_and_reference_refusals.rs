// SPDX-License-Identifier: CC0-1.0

//! How a function's own `name` and the data source it REFERENCES are refused, end to end
//! over the restJson1 wire.
//!
//! Two rules are asserted here, each over a table rather than over the one request that
//! filed the issue:
//!
//! 1. a `name` outside the form `ResourceName` declares — its `@length` 1..65536 or its
//!    `@pattern` `^[_A-Za-z][_0-9A-Za-z]*$`
//!    (`smithy_query(query="shape", shape="ResourceName")`) — is refused by EITHER
//!    operation that carries a function's name (`CreateFunction`, `UpdateFunction`) with
//!    the operation's own modelled client error, AppSync's `BadRequestException` at HTTP
//!    400, carrying ONE sentence that names the RESOURCE whose name was refused and
//!    neither the expression nor the submitted value. `ValidationException` is no
//!    operation's modelled error on this service
//!    (`smithy_query(query="operation", operation="CreateFunction")` — `ApiKey`,
//!    `BadRequestException` 400, `ConcurrentModificationException` 409,
//!    `InternalFailureException`, `NotFoundException` 404, `UnauthorizedException`), so a
//!    client dispatching on the modelled error type never matches that code;
//! 2. a `dataSourceName` the API does not hold is a 404 `NotFoundException` whose
//!    sentence states the absence and NOTHING of the request — the data source is a
//!    dependency this request names rather than the resource it addresses — while an
//!    operation that ADDRESSES a data source (`GetDataSource`, `DeleteDataSource`) names
//!    the absent one.
//!
//! The rows differ along what the rules generalise over: the OPERATION, WHICH character
//! outside the expression the name carries and WHERE it sits, the LENGTH (empty, one past
//! the maximum), and — for the second rule — whether the named data source was never
//! created or belongs to another API of the same account.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, router, Answer};

/// AppSync's own modelled error for a request that is not well formed, and its status
/// (`smithy_query(query="shape", shape="BadRequestException")`, `@httpError` 400).
const BAD_REQUEST: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u16 = 400;

/// The error code the framework would name a constraint refusal with — a shape no
/// operation of this service models, which is why no refusal below may carry it.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// `ResourceName`'s published maximum length (`@length` 1..65536).
const RESOURCE_NAME_MAX_LEN: usize = 65_536;

/// The sentence the service refuses a FUNCTION's malformed name with.
const INVALID_FUNCTION_NAME: &str =
    "The Function name is invalid. Please refer to the documentation for more details.";

/// The sentence a request that REFERENCES an absent data source is refused with.
const DATA_SOURCE_NOT_FOUND: &str = "Data source not found.";

/// One GraphQL API, one data source under it, and one function on that data source.
async fn fixture(router: &MultiServiceRouter, name: &str) -> (String, String, String) {
    let api_id = create_api(router, name).await;
    let data_source = "orders".to_string();
    let created = call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": data_source, "type": "NONE"})),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case seed the data source: expected 200, got {created:?}"
    );
    let function = call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/functions"),
        Some(function_body("resolve_orders", &data_source)),
    )
    .await;
    assert_eq!(
        function.status, 200,
        "case seed the function: expected 200, got {function:?}"
    );
    let function_id = function.body["functionConfiguration"]["functionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (api_id, data_source, function_id)
}

/// A `CreateFunction`/`UpdateFunction` body naming `name` and `data_source`.
fn function_body(name: &str, data_source: &str) -> Value {
    json!({
        "name": name,
        "dataSourceName": data_source,
        "functionVersion": "2018-05-29",
    })
}

/// Read one function back.
async fn get_function(router: &MultiServiceRouter, api_id: &str, function_id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/v1/apis/{api_id}/functions/{function_id}"),
        None,
    )
    .await
}

/// The names OUTSIDE the form `ResourceName` declares: each class of character the
/// expression does not admit, at each position it can sit, and each length bound crossed.
fn malformed_names() -> Vec<(&'static str, String)> {
    vec![
        ("a hyphen inside the name", "resolve-orders".to_string()),
        ("a leading digit", "1resolve".to_string()),
        ("a dot", "resolve.orders".to_string()),
        ("a space", "resolve orders".to_string()),
        ("an at sign at the end", "resolve@".to_string()),
        ("a non-ASCII letter", "résolve".to_string()),
        ("an emoji", "resolve🎉".to_string()),
        ("a newline inside the name", "resolve\norders".to_string()),
        (
            "an empty name, short of the minimum length",
            String::new(),
        ),
        (
            "one character past the maximum length",
            "f".repeat(RESOURCE_NAME_MAX_LEN + 1),
        ),
    ]
}

/// RULE 1: a malformed function name is the service's own `BadRequestException` with one
/// sentence naming the resource, on both operations that carry the member, and the
/// refused call writes nothing.
#[tokio::test]
async fn a_malformed_function_name_names_the_resource_under_the_services_own_code() {
    let router = router();
    let (api_id, data_source, function_id) = fixture(&router, "Api_function_name_form").await;
    let stored = get_function(&router, &api_id, &function_id).await;

    for (case, name) in malformed_names() {
        let paths = [
            ("CreateFunction", format!("/v1/apis/{api_id}/functions")),
            (
                "UpdateFunction",
                format!("/v1/apis/{api_id}/functions/{function_id}"),
            ),
        ];
        for (operation, path) in paths {
            let case = format!("{operation} with {case}");
            let answer = call(
                &router,
                "POST",
                &path,
                Some(function_body(&name, &data_source)),
            )
            .await;
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
                answer.message, INVALID_FUNCTION_NAME,
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

        // Neither refusal touched the stored function.
        let after = get_function(&router, &api_id, &function_id).await;
        assert_eq!(
            after.body["functionConfiguration"], stored.body["functionConfiguration"],
            "case {case:?}: a refused call leaves the function as it was, expected {:?}, \
             got {after:?}",
            stored.body["functionConfiguration"]
        );
    }
}

/// RULE 1's other half: a name INSIDE the declared form is served by both operations and
/// round-trips through `GetFunction`.
#[tokio::test]
async fn a_function_name_inside_the_declared_form_round_trips() {
    let router = router();
    let (api_id, data_source, function_id) = fixture(&router, "Api_function_name_served").await;

    for (case, name) in [
        ("one character, the shortest the bound admits", "f"),
        ("an underscore start", "_private_fn"),
        ("digits after the first character", "fn_0_1"),
        ("every class the expression admits", "Resolve_Orders_42"),
    ] {
        let created = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/functions"),
            Some(function_body(name, &data_source)),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case a create named {case:?}: expected 200, got {created:?}"
        );
        let created_id = created.body["functionConfiguration"]["functionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let read = get_function(&router, &api_id, &created_id).await;
        assert_eq!(
            read.body["functionConfiguration"]["name"],
            json!(name),
            "case a create named {case:?}: expected the name back, got {read:?}"
        );

        // The same value through the update path, onto the seeded function.
        let updated = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/functions/{function_id}"),
            Some(function_body(name, &data_source)),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case an update named {case:?}: expected 200, got {updated:?}"
        );
        let read = get_function(&router, &api_id, &function_id).await;
        assert_eq!(
            read.body["functionConfiguration"]["name"],
            json!(name),
            "case an update named {case:?}: expected the name back, got {read:?}"
        );
    }
}

/// RULE 2: a referenced data source the API does not hold is a 404 whose sentence names
/// no data source, while an operation that ADDRESSES one names the absent name.
#[tokio::test]
async fn a_referenced_data_source_that_is_absent_is_reported_without_its_name() {
    let router = router();
    let (api_id, _data_source, function_id) = fixture(&router, "Api_function_reference").await;

    // A data source of ANOTHER API: held by this account, but not by this API — so the
    // rows differ along WHY the name resolves to nothing here.
    let other_api = create_api(&router, "Api_function_reference_other").await;
    let elsewhere = "ds_elsewhere";
    let created = call(
        &router,
        "POST",
        &format!("/v1/apis/{other_api}/datasources"),
        Some(json!({"name": elsewhere, "type": "NONE"})),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case seed the other API's data source: expected 200, got {created:?}"
    );

    for name in ["ds_never_created", elsewhere] {
        let paths = [
            ("CreateFunction", format!("/v1/apis/{api_id}/functions")),
            (
                "UpdateFunction",
                format!("/v1/apis/{api_id}/functions/{function_id}"),
            ),
        ];
        for (operation, path) in paths {
            let case = format!("{operation} referencing {name:?}");
            let answer = call(
                &router,
                "POST",
                &path,
                Some(function_body("resolve_elsewhere", name)),
            )
            .await;
            assert_eq!(
                (answer.status, answer.error_type.as_str()),
                (404, "NotFoundException"),
                "case {case:?}: expected 404 NotFoundException, got {answer:?}"
            );
            assert_eq!(
                answer.message, DATA_SOURCE_NOT_FOUND,
                "case {case:?}: expected the fixed sentence, got {answer:?}"
            );
            assert!(
                !answer.message.contains(name),
                "case {case:?}: expected the referenced name withheld, got {answer:?}"
            );
        }

        // An operation that ADDRESSES the data source names it, so the sentence above is
        // the referenced case's rather than the service's only not-found text.
        for (operation, method) in [("GetDataSource", "GET"), ("DeleteDataSource", "DELETE")] {
            let case = format!("{operation} addressing {name:?}");
            let answer = call(
                &router,
                method,
                &format!("/v1/apis/{api_id}/datasources/{name}"),
                None,
            )
            .await;
            assert_eq!(
                (answer.status, answer.error_type.as_str()),
                (404, "NotFoundException"),
                "case {case:?}: expected 404 NotFoundException, got {answer:?}"
            );
            assert_eq!(
                answer.message,
                format!("Data source {name} not found"),
                "case {case:?}: expected the addressed data source to be named, got {answer:?}"
            );
        }
    }
}
