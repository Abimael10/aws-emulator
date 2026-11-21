// SPDX-License-Identifier: CC0-1.0

//! A resolver's references to the OTHER resources of its API, end to end over HTTP: the
//! data source it resolves against, and the function it is reported as a reference to.
//!
//! The rules asserted here are stated over CLASSES of input rather than over the one
//! request that exposed the defect:
//!
//! 1. a resolver resolves against a data source OF the API the request addresses
//!    (`/v1/apis/{apiId}/types/{typeName}/resolvers`), so a `dataSourceName` that API does
//!    not hold — a name held only by ANOTHER API among them — is a 404 `NotFoundException`
//!    naming it, nothing is written, and the same request against the API that DOES hold
//!    the name is served; the rule is over the member, so a create and an update are
//!    refused alike;
//! 2. a data source a resolver of its API resolves against cannot be deleted: the call is
//!    a 400 `BadRequestException` naming the type and the field that address the referring
//!    resolver, and the data source is left readable — whatever type and field it is
//!    attached to;
//! 3. `ListResolversByFunction` is scoped to one function OF the API, so a `functionId`
//!    that API does not hold is a 404 `NotFoundException` with AppSync's fixed sentence,
//!    and the empty page is reserved for a function that EXISTS and no resolver invokes.
//!
//! Every assertion goes through the router the production emulator mounts, so these
//! exercise routing, restJson1 (de)serialization, the boundary validator, the handlers and
//! the shared engine together. The harness (`call`, `create_api`, `router`, `Answer`) is the
//! error-contract suite's and the data-source and function helpers are the function
//! suite's — reused rather than copied.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, create_api, router, Answer};
use super::functions::{create_data_source, create_function, Row, FUNCTION_VERSION};

/// The types the resolvers under test are attached to, so a rule stated over a resolver is
/// not stated over one type.
const TYPES: &[&str] = &["Query", "Mutation"];

/// Send one `CreateResolver` with `body` on the type `type_name`.
async fn create_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    type_name: &str,
    body: Value,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
        Some(body),
    )
    .await
}

/// Send one `UpdateResolver` with `body` for the resolver on `type_name`/`field`.
async fn update_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    type_name: &str,
    field: &str,
    body: Value,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{type_name}/resolvers/{field}"),
        Some(body),
    )
    .await
}

/// A UNIT resolver body on `field` resolving against `data_source`.
fn unit_body(field: &str, data_source: &str) -> Value {
    json!({
        "fieldName": field,
        "kind": state::UNIT_KIND,
        "dataSourceName": data_source,
        "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
    })
}

/// List the resolvers attached to the fields of `type_name`.
async fn list_resolvers(router: &MultiServiceRouter, api_id: &str, type_name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
        None,
    )
    .await
}

/// Rule 1: a `dataSourceName` the addressed API does not hold names no data source the
/// resolver could resolve against, and nothing is written for the request that named it.
#[tokio::test]
async fn a_resolver_resolves_only_against_a_data_source_of_its_own_api() {
    let router = router();
    // Two APIs, each holding a data source of its own: the name one holds is the name the
    // other does not.
    let first = create_api(&router, "ds-scope-first").await;
    let second = create_api(&router, "ds-scope-second").await;
    create_data_source(&router, &first, "ds_first").await;
    create_data_source(&router, &second, "ds_second").await;

    // The rows differ along what the rule generalises over: which API holds the name, the
    // type the resolver is attached to, and a name NO API holds at all.
    let cases: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "a data source held only by the other API",
            TYPES[0],
            "ds_second",
            Some("ds_second"),
        ),
        (
            "a data source held only by the other API, on another type",
            TYPES[1],
            "ds_second",
            Some("ds_second"),
        ),
        (
            "a name no API holds",
            TYPES[0],
            "ds_nowhere",
            Some("ds_nowhere"),
        ),
        (
            "the data source this API does hold",
            TYPES[0],
            "ds_first",
            None,
        ),
    ];

    for (case, type_name, data_source, refused) in cases {
        let field = format!("field_{data_source}");
        let answer = create_resolver(&router, &first, type_name, unit_body(&field, data_source))
            .await;
        match refused {
            Some(name) => {
                let expected = state::no_data_source_named_message(name);
                // AppSync's own sentence, spelled out once rather than only read back from
                // the helper, so the wording itself is pinned and not just its composition.
                assert_eq!(
                    expected,
                    format!("No data source found named {name}"),
                    "case {case:?}: expected AppSync's sentence for the name {name:?}"
                );
                assert_eq!(
                    (
                        answer.status,
                        answer.error_type.as_str(),
                        answer.message.as_str()
                    ),
                    (404, "NotFoundException", expected.as_str()),
                    "case {case:?}: expected 404 NotFoundException {expected:?}, got {answer:?}"
                );
                // Nothing was written: the field carries no resolver, so a listing of the
                // API reports no field bound to a data source it does not own.
                let listed = list_resolvers(&router, &first, type_name).await;
                let fields: Vec<&str> = listed.body["resolvers"]
                    .as_array()
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| entry["fieldName"].as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                assert!(
                    !fields.contains(&field.as_str()),
                    "case {case:?}: expected no resolver on {field:?}, got {listed:?}"
                );
            }
            None => assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the create to be served, got {answer:?}"
            ),
        }
    }

    // The same name IS served on the API that holds it, so what the rule turns on is the
    // API the request addresses and not the name.
    let answer = create_resolver(
        &router,
        &second,
        TYPES[0],
        unit_body("field_ds_second", "ds_second"),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case the same name on the API that holds it: expected 200, got {answer:?}"
    );
}

/// Rule 1, over the UPDATE: the member carries the same rule whichever operation writes
/// the resolver, and a refused update leaves the stored resolver exactly as it was.
#[tokio::test]
async fn an_update_resolves_only_against_a_data_source_of_its_own_api() {
    let router = router();
    let api_id = create_api(&router, "ds-scope-update").await;
    let other = create_api(&router, "ds-scope-update-other").await;
    create_data_source(&router, &api_id, "ds_held").await;
    create_data_source(&router, &other, "ds_elsewhere").await;

    let field = "updated";
    let created = create_resolver(&router, &api_id, TYPES[0], unit_body(field, "ds_held")).await;
    assert_eq!(
        created.status, 200,
        "case attach the resolver the update restates: expected 200, got {created:?}"
    );
    let stored = created.body["resolver"].clone();

    for name in ["ds_elsewhere", "ds_nowhere"] {
        let expected = state::no_data_source_named_message(name);
        let answer = update_resolver(
            &router,
            &api_id,
            TYPES[0],
            field,
            unit_body(field, name),
        )
        .await;
        assert_eq!(
            (
                answer.status,
                answer.error_type.as_str(),
                answer.message.as_str()
            ),
            (404, "NotFoundException", expected.as_str()),
            "case an update naming {name:?}: expected 404 NotFoundException {expected:?}, \
             got {answer:?}"
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/types/{}/resolvers/{field}", TYPES[0]),
            None,
        )
        .await;
        assert_eq!(
            (read.status, &read.body["resolver"]),
            (200, &stored),
            "case an update naming {name:?}: expected the stored resolver unchanged, got {read:?}"
        );
    }
}

/// Rule 2: a data source a resolver resolves against is not deleted, and the refusal names
/// the type and the field that address that resolver.
#[tokio::test]
async fn a_data_source_a_resolver_resolves_against_is_not_deleted() {
    // The rows differ along the type and the field the referring resolver is attached to,
    // which are the two variable parts of AppSync's sentence.
    let cases: &[(&str, &str)] = &[
        (TYPES[0], "getWidget"),
        (TYPES[0], "listWidgets"),
        (TYPES[1], "putWidget"),
    ];

    for (type_name, field) in cases {
        let router = router();
        let api_id = create_api(&router, &format!("ds-ref-{type_name}-{field}")).await;
        create_data_source(&router, &api_id, "ds_referenced").await;
        let created =
            create_resolver(&router, &api_id, type_name, unit_body(field, "ds_referenced")).await;
        assert_eq!(
            created.status, 200,
            "case {type_name}/{field}: expected the create to be served, got {created:?}"
        );

        let expected = state::resolver_data_source_in_use_message(type_name, field);
        // AppSync's own sentence, spelled out for the first row rather than only read back
        // from the helper, so the wording itself is pinned and not just its composition.
        if (*type_name, *field) == (TYPES[0], "getWidget") {
            assert_eq!(
                expected,
                "Resolver for type Query and field getWidget associated with data source",
                "expected AppSync's sentence for the resolver that blocks the delete"
            );
        }
        let answer = call(
            &router,
            "DELETE",
            &format!("/v1/apis/{api_id}/datasources/ds_referenced"),
            None,
        )
        .await;
        assert_eq!(
            (
                answer.status,
                answer.error_type.as_str(),
                answer.message.as_str()
            ),
            (400, "BadRequestException", expected.as_str()),
            "case {type_name}/{field}: expected 400 BadRequestException {expected:?}, \
             got {answer:?}"
        );

        // The refusal left the data source in place, so the caller's next call sees
        // exactly what it saw before the delete, and detaching the resolver frees it.
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/datasources/ds_referenced"),
            None,
        )
        .await;
        assert_eq!(
            (read.status, read.body["dataSource"]["name"].as_str()),
            (200, Some("ds_referenced")),
            "case {type_name}/{field}: expected the data source still readable, got {read:?}"
        );
        let detached = call(
            &router,
            "DELETE",
            &format!("/v1/apis/{api_id}/types/{type_name}/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            detached.status, 200,
            "case {type_name}/{field}: expected the resolver to be detached, got {detached:?}"
        );
        let answer = call(
            &router,
            "DELETE",
            &format!("/v1/apis/{api_id}/datasources/ds_referenced"),
            None,
        )
        .await;
        assert_eq!(
            (answer.status, &answer.body),
            (200, &json!({})),
            "case {type_name}/{field}: expected the delete to be served once nothing names \
             it, got {answer:?}"
        );
    }
}

/// Rule 3: the listing by function is refused for a `functionId` the API does not hold,
/// and answers the empty page for a function that exists and no resolver invokes.
#[tokio::test]
async fn a_listing_by_function_tells_an_absent_function_from_an_unreferenced_one() {
    let router = router();
    let api_id = create_api(&router, "fn-refs").await;
    let other = create_api(&router, "fn-refs-other").await;
    create_data_source(&router, &api_id, "ds_fn").await;
    create_data_source(&router, &other, "ds_fn").await;

    let row = Row {
        request_template: Some("{\"version\": \"2018-05-29\"}"),
        function_version: Some(FUNCTION_VERSION),
        ..Row::bare("the function the references are listed for", "fn_listed")
    };
    let created = create_function(&router, &api_id, "ds_fn", &row).await;
    assert_eq!(
        created.status, 200,
        "case the function of this API: expected CreateFunction 200, got {created:?}"
    );
    let held = created.body["functionConfiguration"]["functionId"]
        .as_str()
        .expect("the create mints a functionId")
        .to_string();
    let foreign = create_function(&router, &other, "ds_fn", &row).await;
    let foreign = foreign.body["functionConfiguration"]["functionId"]
        .as_str()
        .expect("the create mints a functionId")
        .to_string();

    // A function this API holds and no resolver invokes: the empty page.
    let answer = call(
        &router,
        "GET",
        &format!("/v1/apis/{api_id}/functions/{held}/resolvers"),
        None,
    )
    .await;
    assert_eq!(
        (answer.status, answer.body["resolvers"].as_array().map(Vec::len)),
        (200, Some(0)),
        "case a function no resolver invokes: expected an empty 200 page, got {answer:?}"
    );

    // AppSync's own fixed sentence for a function the API does not hold, spelled out rather
    // than only read back from the constant, so the wording itself is pinned.
    assert_eq!(
        state::FUNCTION_NOT_FOUND_MESSAGE, "Function not found.",
        "expected AppSync's fixed sentence for an absent function"
    );
    // The rows differ along why the API does not hold the id: a well-formed id nothing was
    // ever issued for, one held by ANOTHER API, and one that is no id this service mints.
    for function_id in [
        "ffffffffffffffffffffffffff",
        foreign.as_str(),
        "not-a-function-id",
    ] {
        let answer = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/functions/{function_id}/resolvers"),
            None,
        )
        .await;
        assert_eq!(
            (
                answer.status,
                answer.error_type.as_str(),
                answer.message.as_str()
            ),
            (404, "NotFoundException", state::FUNCTION_NOT_FOUND_MESSAGE),
            "case the listing by {function_id:?}: expected 404 NotFoundException {:?}, \
             got {answer:?}",
            state::FUNCTION_NOT_FOUND_MESSAGE
        );
    }
}
