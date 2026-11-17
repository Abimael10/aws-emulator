// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the SHAPE of the identifiers this service MINTS.
//!
//! The rules these cases assert:
//!
//! * **A created resource answers an identifier of the family API Gateway v2 answers for
//!   that kind of resource**: lowercase alphanumerics, the short opaque form for a
//!   resource nested inside another one — inside an api (an authorizer, a model, a
//!   deployment, a route response, an integration response, a route, an integration) or
//!   inside a custom domain name (an api mapping, a routing rule) — and the longer form
//!   for a resource the account holds. `Id` is an unconstrained string in the model
//!   (`smithy_query(query="shape", shape="Id")`), so the per-kind length is this service's
//!   convention, stated once in `api_lifecycle::minted_id_len` and asserted here against
//!   every create that mints one.
//! * **A minted identifier denotes ONE resource**: no two resources of a run carry the
//!   same id, so a client that caches, indexes or round-trips ids never collides two of
//!   them — not even the first child of one api against the first child of another, which
//!   a counter shared by every collection would answer identically.
//! * **No character position of a minted id is stuck on one character**: the ids of one
//!   collection vary in every position, so none of them carries a constant run that the
//!   id family has no room for.
//! * Every minted id RESOLVES: the resource reads back under it through its own reader.
//!
//! The tables span what the rules generalise over: the kind of resource (every create
//! that mints an id), the scope the id is minted in (several apis, several owning routes
//! and integrations, two custom domain names), and the number of ids drawn from one
//! collection. The recorded `CreateAuthorizer` / `CreateModel` / `CreateRouteResponse`
//! calls that filed the first issue are three rows of the first table, and the recorded
//! `CreateApiMapping` call that filed the second is one row of the domain-name table.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{
    as_str, assert_minted_id, call, emulator, minted_id_len, strip_nulls, REGION,
};

/// How many ids one collection is asked for when checking that no position of an id is
/// stuck. Eight stays inside the tightest per-api quota any of these kinds has
/// (`MAX_AUTHORIZERS_PER_API` is ten) and makes a position that varies at all almost
/// certain to be seen varying.
const SAMPLE: usize = 8;

/// A well-formed Lambda authorizer URI, the form `AuthorizerUri` documents: "For REQUEST
/// authorizers, this must be a well-formed Lambda function URI".
fn lambda_uri(function: &str) -> String {
    format!(
        "arn:aws:apigateway:us-east-1:lambda:path/2015-03-31/functions/\
         arn:aws:lambda:us-east-1:123456789012:function:{function}/invocations"
    )
}

/// POST one request that must be accepted with 201 and answer the created body.
async fn created(emulator: &MultiServiceRouter, case: &str, uri: &str, request: Value) -> Value {
    let (status, body) = call(emulator, "POST", uri, Some(request)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from POST {uri}, got {status} {body}"
    );
    body
}

/// One minted id: the member it is reported under, the id itself, and the reader that
/// must resolve the resource back under it.
struct Minted {
    member: &'static str,
    id: String,
    reader: String,
}

/// Create one WebSocket api named `name` and one resource of every kind nested in it,
/// answering each minted id with the reader that resolves it.
///
/// WebSocket, because a model and a route response are WebSocket-only resources
/// (`RouteResponse$ModelSelectionExpression`: "Supported only for WebSocket APIs") while
/// every other kind here exists under either protocol.
async fn mint_family(emulator: &MultiServiceRouter, case: &str, name: &str) -> Vec<Minted> {
    let api = created(
        emulator,
        case,
        "/v2/apis",
        json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
    )
    .await;
    let api_id = as_str(&api, "apiId");

    let integration = created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/integrations"),
        json!({ "integrationType": "MOCK" }),
    )
    .await;
    let integration_id = as_str(&integration, "integrationId");

    // The route fronts that integration: a WEBSOCKET api serves a frame only through the
    // target of the route that matched it, so a deployment of this api is accepted only
    // while every route of it names one.
    let route = created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/routes"),
        json!({ "routeKey": "$connect", "target": format!("integrations/{integration_id}") }),
    )
    .await;
    let route_id = as_str(&route, "routeId");

    let authorizer = created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/authorizers"),
        json!({
            "name": "connect-auth",
            "authorizerType": "REQUEST",
            "authorizerUri": lambda_uri("connect-auth"),
            "identitySource": ["route.request.header.Auth"]
        }),
    )
    .await;

    let model = created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/models"),
        json!({ "name": "Empty", "schema": "{}" }),
    )
    .await;

    let deployment = created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/deployments"),
        json!({}),
    )
    .await;

    let route_response = created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
        json!({ "routeResponseKey": "$default" }),
    )
    .await;

    let integration_response = created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses"),
        json!({ "integrationResponseKey": "$default" }),
    )
    .await;

    let mut minted = vec![
        Minted {
            member: "apiId",
            id: api_id.clone(),
            reader: format!("/v2/apis/{api_id}"),
        },
        Minted {
            member: "routeId",
            id: route_id.clone(),
            reader: format!("/v2/apis/{api_id}/routes/{route_id}"),
        },
        Minted {
            member: "integrationId",
            id: integration_id.clone(),
            reader: format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        },
    ];
    for (member, body) in [
        ("authorizerId", &authorizer),
        ("modelId", &model),
        ("deploymentId", &deployment),
    ] {
        let id = as_str(body, member);
        let collection = match member {
            "authorizerId" => "authorizers",
            "modelId" => "models",
            _ => "deployments",
        };
        minted.push(Minted {
            member,
            reader: format!("/v2/apis/{api_id}/{collection}/{id}"),
            id,
        });
    }
    let route_response_id = as_str(&route_response, "routeResponseId");
    minted.push(Minted {
        member: "routeResponseId",
        reader: format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses/{route_response_id}"),
        id: route_response_id,
    });
    let integration_response_id = as_str(&integration_response, "integrationResponseId");
    minted.push(Minted {
        member: "integrationResponseId",
        reader: format!(
            "/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses/\
             {integration_response_id}"
        ),
        id: integration_response_id,
    });
    minted
}

#[tokio::test]
async fn every_create_answers_an_id_of_its_kinds_family_and_the_resource_reads_back_under_it() {
    let emulator = emulator();

    for minted in mint_family(&emulator, "one api of every child kind", "id-family").await {
        let case = format!("the minted {}", minted.member);
        assert_minted_id(&case, minted.member, &minted.id);

        let (status, fetched) = call(&emulator, "GET", &minted.reader, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GET {}, got {status} {fetched}",
            minted.reader
        );
        assert_eq!(
            as_str(&fetched, minted.member),
            minted.id,
            "case {case:?}: the reader must report the id it was addressed by, got {fetched}"
        );
    }
}

/// How many apis the collision case mints a whole family under: three, so a first, a
/// second and a third collection of each kind are compared, not just two.
const SCOPES: usize = 3;

#[tokio::test]
async fn no_two_resources_of_one_run_carry_the_same_minted_id() {
    let emulator = emulator();

    // id -> the case that already minted it. Every collection counts its own ids from its
    // own seed, so the n-th resource of one api and the n-th of another are the pair a
    // shared counter answers identically.
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut minted_count = 0usize;
    for scope in 0..SCOPES {
        let case = format!("api {scope}");
        for minted in mint_family(&emulator, &case, &format!("id-collision-{scope}")).await {
            minted_count += 1;
            let owner = format!("{case}'s {}", minted.member);
            if let Some(other) = seen.insert(minted.id.clone(), owner.clone()) {
                panic!(
                    "case {owner:?}: the id {:?} was already minted for {other:?} — a minted \
                     identifier must denote one resource",
                    minted.id
                );
            }
        }
    }
    assert_eq!(
        seen.len(),
        minted_count,
        "case {SCOPES:?} apis: expected every one of the {minted_count} minted ids to be \
         distinct, got {} distinct",
        seen.len()
    );
}

/// Mint `SAMPLE` ids of `member`'s kind within ONE collection, in the order they were
/// minted.
async fn mint_many(emulator: &MultiServiceRouter, member: &str) -> Vec<String> {
    let case = format!("{SAMPLE} {member}s of one collection");
    if member == "apiId" {
        // The account's own collection of apis, which needs no api to hold it.
        let mut ids = Vec::with_capacity(SAMPLE);
        for index in 0..SAMPLE {
            let api = created(
                emulator,
                &case,
                "/v2/apis",
                json!({
                    "name": format!("stuck-position-api-{index}"),
                    "protocolType": "HTTP"
                }),
            )
            .await;
            ids.push(as_str(&api, "apiId"));
        }
        return ids;
    }
    let api = created(
        emulator,
        &case,
        "/v2/apis",
        json!({
            "name": format!("stuck-position-{member}"),
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
    )
    .await;
    let api_id = as_str(&api, "apiId");
    let host_integration = created(
        emulator,
        &case,
        &format!("/v2/apis/{api_id}/integrations"),
        json!({ "integrationType": "MOCK" }),
    )
    .await;
    let host_integration_id = as_str(&host_integration, "integrationId");
    // The host route fronts that integration, so this api is deployable: a WEBSOCKET api
    // serves a frame only through the target of the route that matched it.
    let host_route = created(
        emulator,
        &case,
        &format!("/v2/apis/{api_id}/routes"),
        json!({
            "routeKey": "$connect",
            "target": format!("integrations/{host_integration_id}")
        }),
    )
    .await;
    let host_route_id = as_str(&host_route, "routeId");

    let mut ids = Vec::with_capacity(SAMPLE);
    for index in 0..SAMPLE {
        let (uri, request) = match member {
            "routeId" => (
                format!("/v2/apis/{api_id}/routes"),
                json!({ "routeKey": format!("action{index}") }),
            ),
            "integrationId" => (
                format!("/v2/apis/{api_id}/integrations"),
                json!({ "integrationType": "MOCK" }),
            ),
            "authorizerId" => (
                format!("/v2/apis/{api_id}/authorizers"),
                json!({
                    "name": format!("auth{index}"),
                    "authorizerType": "REQUEST",
                    "authorizerUri": lambda_uri("stuck-auth"),
                    "identitySource": ["route.request.header.Auth"]
                }),
            ),
            "modelId" => (
                format!("/v2/apis/{api_id}/models"),
                json!({ "name": format!("Model{index}"), "schema": "{}" }),
            ),
            "deploymentId" => (format!("/v2/apis/{api_id}/deployments"), json!({})),
            "routeResponseId" => (
                format!("/v2/apis/{api_id}/routes/{host_route_id}/routeresponses"),
                json!({ "routeResponseKey": format!("key{index}") }),
            ),
            "integrationResponseId" => (
                format!(
                    "/v2/apis/{api_id}/integrations/{host_integration_id}/integrationresponses"
                ),
                json!({ "integrationResponseKey": format!("/{}/", 200 + index) }),
            ),
            other => panic!("this suite mints no collection of {other:?}"),
        };
        let body = created(emulator, &case, &uri, request).await;
        ids.push(as_str(&body, member));
    }
    ids
}

#[tokio::test]
async fn no_character_position_of_a_minted_id_is_stuck_on_one_character() {
    let emulator = emulator();

    // Every kind whose ids one collection can be asked for in bulk, so both id lengths
    // this service mints are covered: the account's own apis carry the ten-character form
    // and the rest the short one.
    for member in [
        "apiId",
        "routeId",
        "integrationId",
        "authorizerId",
        "modelId",
        "deploymentId",
        "routeResponseId",
        "integrationResponseId",
    ] {
        let ids = mint_many(&emulator, member).await;
        let case = format!("{SAMPLE} minted {member}s");
        for (index, id) in ids.iter().enumerate() {
            assert_minted_id(&format!("{case}, number {index}"), member, id);
        }
        for position in 0..minted_id_len(member) {
            let characters: Vec<char> = ids
                .iter()
                .filter_map(|id| id.chars().nth(position))
                .collect();
            let first = characters.first().copied().unwrap_or_default();
            assert!(
                characters.iter().any(|character| *character != first),
                "case {case:?}: every id carries {first:?} at position {position} \
                 — that position is stuck; got {ids:?}"
            );
        }
    }
}

// --- the children of a custom domain name -----------------------------------------
//
// An `ApiMapping` and a `RoutingRule` are resources nested inside a custom domain name
// and report their ids under members that target the unconstrained `Id` shape, so the
// same first rule governs them: the short opaque child-resource family, not the longer
// form a resource the account holds carries.

/// The two custom domain names the child table mints under, so an id minted in one scope
/// is checked against the other scope's as well as against its own siblings'.
const DOMAINS: [&str; 2] = ["first.mapped.example.com", "second.mapped.example.com"];

/// The stages every seeded api holds; a mapping and a rule action each name one.
const STAGE: &str = "$default";
const OTHER_STAGE: &str = "prod";

/// The character window the short opaque id family API Gateway v2 answers for a child
/// resource falls in: six or seven lowercase alphanumerics. Recorded behaviour of the
/// member — `Id` itself carries neither `@length` nor `@pattern` — so this window, and
/// not a modelled trait, is what a client validating such an id sees.
const CHILD_ID_WINDOW: (usize, usize) = (6, 7);

/// The published bounds of `RoutingRulePriority` (`range` 1..1000000), from
/// `smithy_query(query="shape", shape="RoutingRulePriority")`.
const PRIORITY_MIN: i64 = 1;
const PRIORITY_MAX: i64 = 1_000_000;

/// Create one custom domain name, asserting the 201 the model binds to
/// `CreateDomainName`.
async fn seed_domain_name(emulator: &MultiServiceRouter, domain: &str) {
    let (status, body) = call(
        emulator,
        "POST",
        "/v2/domainnames",
        Some(json!({
            "domainName": domain,
            // AWS Certificate Manager is the only source `CertificateArn` documents.
            "domainNameConfigurations": [{
                "certificateArn": engine::emulator_arn(
                    "acm",
                    REGION,
                    &format!("certificate/{domain}"),
                ),
            }],
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "seeding domain name {domain:?}: expected 201 from CreateDomainName, got {status} {body}"
    );
}

/// Create one HTTP api holding [`STAGE`] and [`OTHER_STAGE`] and answer its `ApiId`.
async fn seed_mapped_api(emulator: &MultiServiceRouter, name: &str) -> String {
    let api = created(
        emulator,
        name,
        "/v2/apis",
        json!({ "name": name, "protocolType": "HTTP" }),
    )
    .await;
    let api_id = as_str(&api, "apiId");
    for stage_name in [STAGE, OTHER_STAGE] {
        created(
            emulator,
            name,
            &format!("/v2/apis/{api_id}/stages"),
            json!({ "stageName": stage_name }),
        )
        .await;
    }
    api_id
}

/// A fresh emulator holding both [`DOMAINS`] and one api with both stages. Answers
/// `(emulator, api_id)`.
async fn seeded_domain_names() -> (MultiServiceRouter, String) {
    let emulator = emulator();
    for domain in DOMAINS {
        seed_domain_name(&emulator, domain).await;
    }
    let api_id = seed_mapped_api(&emulator, "child-id-api").await;
    (emulator, api_id)
}

/// One row of the domain-name child table: the collection the child is created in, the
/// member its id is reported under, the request that mints it, and the status the service
/// answers that create with.
struct ChildCase {
    label: &'static str,
    collection: &'static str,
    member: &'static str,
    /// The request body, with `{api}` standing in for the seeded `ApiId`.
    request: Value,
    created_status: u16,
}

/// The rows span both child kinds a custom domain name owns and, within each, what the
/// request varies: a mapping's base path (absent, one segment, several segments) and a
/// rule's priority (both published bounds) and condition kind (base paths, headers).
fn child_cases() -> Vec<ChildCase> {
    vec![
        ChildCase {
            label: "an api mapping on the root of the domain",
            collection: "apimappings",
            member: "apiMappingId",
            request: json!({ "apiId": "{api}", "stage": STAGE }),
            // `CreateApiMapping` is answered with 200, the status the service answers
            // this write with rather than the model's `code: 201`.
            created_status: 200,
        },
        ChildCase {
            label: "an api mapping on one base-path segment",
            collection: "apimappings",
            member: "apiMappingId",
            request: json!({ "apiId": "{api}", "stage": STAGE, "apiMappingKey": "orders" }),
            created_status: 200,
        },
        ChildCase {
            label: "an api mapping on several base-path segments",
            collection: "apimappings",
            member: "apiMappingId",
            request: json!({
                "apiId": "{api}",
                "stage": OTHER_STAGE,
                "apiMappingKey": "orders/v1/items"
            }),
            created_status: 200,
        },
        ChildCase {
            label: "a routing rule at the lowest published priority",
            collection: "routingrules",
            member: "routingRuleId",
            request: json!({
                "priority": PRIORITY_MIN,
                "actions": [{ "invokeApi": { "apiId": "{api}", "stage": STAGE } }],
                "conditions": [{ "matchBasePaths": { "anyOf": ["carts"] } }]
            }),
            created_status: 201,
        },
        ChildCase {
            label: "a routing rule at the highest published priority, matching a header",
            collection: "routingrules",
            member: "routingRuleId",
            request: json!({
                "priority": PRIORITY_MAX,
                "actions": [{
                    "invokeApi": { "apiId": "{api}", "stage": OTHER_STAGE, "stripBasePath": true }
                }],
                "conditions": [{
                    "matchHeaders": { "anyOf": [{ "header": "x-tenant", "valueGlob": "acme-*" }] }
                }]
            }),
            created_status: 201,
        },
    ]
}

/// The request of `case` with `{api}` resolved to `api_id`.
fn resolved(case: &ChildCase, api_id: &str) -> Value {
    let rendered = case.request.to_string().replace("{api}", api_id);
    serde_json::from_str(&rendered).expect("the row is valid JSON")
}

/// An id of the short opaque family a child resource carries: inside
/// [`CHILD_ID_WINDOW`], shorter than the form a resource the account holds carries, and
/// exactly the length and alphabet its kind's convention gives it
/// ([`assert_minted_id`]).
fn assert_child_id(case: &str, member: &str, id: &str, account_id_len: usize) {
    let (min, max) = CHILD_ID_WINDOW;
    let length = id.chars().count();
    assert!(
        (min..=max).contains(&length),
        "case {case:?}: expected a {min}-to-{max}-character {member} of the child-resource \
         id family, got {id:?} of {length} characters"
    );
    assert!(
        length < account_id_len,
        "case {case:?}: expected a {member} shorter than the {account_id_len}-character form \
         a resource the account holds carries, got {id:?}"
    );
    assert_minted_id(case, member, id);
}

#[tokio::test]
async fn a_child_of_a_domain_name_answers_a_short_opaque_id_and_reads_back_under_it() {
    let (emulator, api_id) = seeded_domain_names().await;

    // The form a resource the account holds directly carries, read off a resource that
    // reports one rather than written out here.
    assert_minted_id("the seeded api", "apiId", &api_id);
    let account_id_len = api_id.chars().count();
    assert_eq!(
        account_id_len,
        minted_id_len("apiId"),
        "the seeded api must carry the account-level id form, got {api_id:?}"
    );

    // id -> the case that already minted it, across both parents.
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for domain in DOMAINS {
        for case in child_cases() {
            let label = format!("{} of {domain}", case.label);
            let uri = format!("/v2/domainnames/{domain}/{}", case.collection);
            let (status, body) =
                call(&emulator, "POST", &uri, Some(resolved(&case, &api_id))).await;
            assert_eq!(
                status, case.created_status,
                "case {label:?}: expected {} from POST {uri}, got {status} {body}",
                case.created_status
            );

            let id = as_str(&body, case.member);
            assert_child_id(&label, case.member, &id, account_id_len);
            if let Some(other) = seen.insert(id.clone(), label.clone()) {
                panic!(
                    "case {label:?}: the id {id:?} was already minted for {other:?} — a minted \
                     identifier must denote one resource"
                );
            }

            // The id RESOLVES: the child reads back under it, beneath the parent whose
            // path minted it, and reports the id it was addressed by.
            let reader = format!("{uri}/{id}");
            let (status, read) = call(&emulator, "GET", &reader, None).await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected 200 from GET {reader}, got {status} {read}"
            );
            assert_eq!(
                as_str(&read, case.member),
                id,
                "case {label:?}: the reader must report the id it was addressed by, got {read}"
            );
            assert_eq!(
                strip_nulls(&read),
                strip_nulls(&body),
                "case {label:?}: expected GET {reader} to report the created record {body}, \
                 got {read}"
            );
        }
    }
}

#[tokio::test]
async fn every_id_one_domain_name_mints_for_a_child_kind_is_of_the_family_and_varies() {
    let (emulator, api_id) = seeded_domain_names().await;
    let account_id_len = api_id.chars().count();
    let domain = DOMAINS[0];

    for (collection, member) in [
        ("apimappings", "apiMappingId"),
        ("routingrules", "routingRuleId"),
    ] {
        let case = format!("{SAMPLE} {member}s of one domain name");
        let mut ids: Vec<String> = Vec::with_capacity(SAMPLE);
        for index in 0..SAMPLE {
            let request = if collection == "apimappings" {
                json!({
                    "apiId": api_id,
                    "stage": STAGE,
                    "apiMappingKey": format!("path{index}")
                })
            } else {
                json!({
                    "priority": PRIORITY_MIN + i64::try_from(index).unwrap_or_default(),
                    "actions": [{ "invokeApi": { "apiId": api_id, "stage": STAGE } }],
                    "conditions": [{ "matchBasePaths": { "anyOf": [format!("rule{index}")] } }]
                })
            };
            let uri = format!("/v2/domainnames/{domain}/{collection}");
            let (status, body) = call(&emulator, "POST", &uri, Some(request)).await;
            assert!(
                status == 200 || status == 201,
                "case {case:?}: expected a 2xx from POST {uri} number {index}, got {status} \
                 {body}"
            );
            ids.push(as_str(&body, member));
        }
        for (index, id) in ids.iter().enumerate() {
            assert_child_id(
                &format!("{case}, number {index}"),
                member,
                id,
                account_id_len,
            );
        }

        for position in 0..minted_id_len(member) {
            let characters: Vec<char> = ids
                .iter()
                .filter_map(|id| id.chars().nth(position))
                .collect();
            let first = characters.first().copied().unwrap_or_default();
            assert!(
                characters.iter().any(|character| *character != first),
                "case {case:?}: every id carries {first:?} at position {position} \
                 — that position is stuck; got {ids:?}"
            );
        }
    }
}
