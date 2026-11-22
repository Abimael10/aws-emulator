// SPDX-License-Identifier: CC0-1.0

//! `ListResolvers` is scoped to one TYPE of one API, and the `typeName` it is addressed by
//! names a type the API holds.
//!
//! The rule these rows assert, one level above any single type name:
//!
//! > `ListResolvers`' `typeName` is a path label of its URI
//! > (`GET /v1/apis/{apiId}/types/{typeName}/resolvers`) naming a type OF the API, so a name
//! > the API holds no type under addresses no collection to page: it takes the modelled
//! > `NotFoundException` (HTTP 404) with AppSync's fixed missing-type text. A type the API
//! > DOES hold is listed — as the empty page when nothing is attached to it, which is what
//! > tells "this type carries no resolver" from "this type does not exist".
//!
//! The rows span what the rule generalises over: a held type carrying a resolver, held types
//! carrying none (a root type and a type of the schema's own), and names the API holds no
//! type under — one nothing resembles, one differing from a held type only in case, and one
//! that names a FIELD of a held type rather than a type.
//!
//! Every assertion goes through the router the production emulator mounts, over the same
//! schema-installing harness the data-plane suites use, so the type system the rows are
//! judged against is the one a `StartSchemaCreation` installed.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_api_data_plane::{appsync, emulator};
use crate::graphql_api_mapping_template_resolvers::install_schema;

/// AppSync's fixed text for a `typeName` the named API holds no type under. Written out here
/// rather than read off the service's own constant, so a change of wording is caught rather
/// than followed.
const TYPE_NOT_FOUND: &str = "Type not found.";

/// The schema the rows are judged against: two root types and a type of its own.
const SCHEMA: &str = "type Post {\n  id: ID!\n  body: String\n}\n\
     \n\
     type Query {\n  getPost(id: ID!): Post\n  getComments: String\n}\n\
     \n\
     type Mutation {\n  addPost(id: ID!): Post\n}\n";

/// The one field of the schema a resolver is attached to.
const ATTACHED_FIELD: &str = "getPost";

/// What a listing of one `typeName` must answer with.
#[derive(Debug)]
enum Listing {
    /// The type is one the API holds: a 200 page reporting exactly these fields.
    Reports(&'static [&'static str]),
    /// The API holds no type of that name: the modelled missing-type refusal.
    Refused,
}

#[tokio::test]
async fn a_listing_names_a_type_the_api_holds() {
    let router = emulator();
    let api_id = create_api(&router, "Api_list_resolvers_type_scope").await;

    let rows: &[(&str, &str, Listing)] = &[
        (
            "a held type carrying a resolver",
            "Query",
            Listing::Reports(&[ATTACHED_FIELD]),
        ),
        (
            "a held root type carrying no resolver",
            "Mutation",
            Listing::Reports(&[]),
        ),
        (
            "a held type of the schema's own, carrying no resolver",
            "Post",
            Listing::Reports(&[]),
        ),
        (
            "a name the schema does not declare",
            "TotallyUnknownType404",
            Listing::Refused,
        ),
        (
            "a held type's name in another case",
            "query",
            Listing::Refused,
        ),
        (
            "a field of a held type rather than a type",
            ATTACHED_FIELD,
            Listing::Refused,
        ),
    ];

    for (case, type_name, expected) in rows {
        let (status, body) = appsync(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
            None,
        )
        .await;
        match expected {
            Listing::Reports(fields) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the listing of {type_name} to be served, got \
                     {status} and {body}"
                );
                let listed: Vec<&str> = body["resolvers"]
                    .as_array()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected a resolvers list, got {body}")
                    })
                    .iter()
                    .map(|resolver| resolver["fieldName"].as_str().unwrap_or_default())
                    .collect();
                assert_eq!(
                    listed, *fields,
                    "case {case:?}: expected exactly the resolvers attached to {type_name}, \
                     got {listed:?}"
                );
            }
            Listing::Refused => assert_eq!(
                (
                    status,
                    body["__type"].as_str().unwrap_or_default(),
                    body["message"].as_str().unwrap_or_default(),
                ),
                (404, "NotFoundException", TYPE_NOT_FOUND),
                "case {case:?}: expected AppSync's missing-type refusal for {type_name}, got \
                 {status} and {body}"
            ),
        }
    }
}

/// A listing is refused as a missing type only under an API that EXISTS: an `apiId` the
/// account does not hold is the missing API, which is decided before the type is looked up.
#[tokio::test]
async fn the_api_is_resolved_before_the_type_is() {
    let router = emulator();
    let api_id = create_api(&router, "Api_list_resolvers_type_order").await;
    let unknown_api = "z".repeat(api_id.len());

    let (status, body) = appsync(
        &router,
        "GET",
        &format!("/v1/apis/{unknown_api}/types/TotallyUnknownType404/resolvers"),
        None,
    )
    .await;
    assert_eq!(
        (
            status,
            body["__type"].as_str().unwrap_or_default(),
            body["message"].as_str().unwrap_or_default(),
        ),
        (404, "NotFoundException", "API not found."),
        "case an unknown apiId with an unknown typeName: expected the missing API to be \
         reported, got {status} and {body}"
    );
}

/// Create one API with [`SCHEMA`], a `NONE` data source and one resolver on
/// [`ATTACHED_FIELD`], and answer its `apiId`.
async fn create_api(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = appsync(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "API_KEY"})),
    )
    .await;
    assert_eq!(status, 200, "CreateGraphqlApi must answer 200, got {created}");
    let api_id = created["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    install_schema(router, &api_id, SCHEMA).await;
    let (status, source) = appsync(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": "local", "type": "NONE"})),
    )
    .await;
    assert_eq!(status, 200, "CreateDataSource must answer 200, got {source}");
    let (status, attached) = appsync(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": ATTACHED_FIELD,
            "kind": "UNIT",
            "dataSourceName": "local",
            "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateResolver must answer 200, got {attached}"
    );
    api_id
}
