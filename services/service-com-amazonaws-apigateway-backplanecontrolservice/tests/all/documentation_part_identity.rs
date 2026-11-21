// SPDX-License-Identifier: CC0-1.0

//! WHICH record a `CreateDocumentationPart` for an API entity reaches, asserted on the
//! wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. [`Gateway`], `create_api`, `string_member`,
//! `page_items` and `assert_failure` are this crate's one wire-test harness, reused from
//! `rest_api_lifecycle`, and the documentation-part request builders are
//! `documentation_parts`'; the helpers below are this suite's own.
//!
//! The RULE under test: a documentation part is identified by the REST API it belongs to
//! plus the COMPLETED location of the entity it documents — every member the entity type
//! makes valid, defaulted where the model documents a default — and the documentation of
//! one entity is ONE record. So a create for an entity the API lists WITHOUT content (the
//! `UNDOCUMENTED` record an import entry that declares no content map leaves, which
//! `GetDocumentationParts$locationStatus` reports separately) writes that content under the
//! identifier that record already has, rather than adding a second part for one entity;
//! and a create for an entity the API already documents is the modelled 409
//! `ConflictException`, whose sentence `refusal_message_constants` pins.
//!
//! What the rows span:
//!
//! * the entity type and so how many members the location sets — none, a name, a path, a
//!   path and a verb, three and four members — over the `DocumentationPartType` catalog;
//! * how the later create SPELLS the same entity: the members as the first call sent them,
//!   the documented defaults left out, and those defaults spelled out;
//! * the content map the later create carries, same as the stored one and different;
//! * the number of calls at one location, so neither answer changes on the third;
//! * the entity a location names, member by member: a location differing from another in
//!   exactly one member documents another entity and is its own part;
//! * which API documents the location, because the API is half of the identity.
//!
//! The sibling `documentation_parts` suite asserts the import half of the same identity (a
//! `merge` import landing its content map on the part a create made) and the location rules
//! themselves; `delete_documentation_part` asserts that the location is free for a NEW part
//! once the delete has removed the one that held it.

use serde_json::{json, Value};

use crate::documentation_parts::{
    content_map, create_part, created_part_id, get_part, import_body, import_parts, listed_ids,
};
use crate::rest_api_lifecycle::{
    assert_failure, create_api, page_items, string_member, Answer, Gateway,
};

/// The modelled success status of `CreateDocumentationPart`
/// (`smithy_query(query="operation", operation="CreateDocumentationPart")` reports
/// `http.code` 201), which a create that documents a record already there answers with too.
const CREATE_STATUS: u16 = 201;

/// The modelled failure a create for an entity the API already documents answers with, as
/// the model declares it (`ConflictException`, `httpError` 409).
const CONFLICT: (u16, &str) = (409, "ConflictException");

/// `CreateDocumentationPart`, asserting the modelled status and answering the whole body,
/// so a case can read the id, the location and the content map from one call.
/// `documentation_parts::created_part_id` answers the id alone.
async fn written_part(
    gateway: &Gateway,
    api: &str,
    location: Value,
    properties: &str,
    case: &str,
) -> Value {
    let answer = create_part(gateway, api, location, properties).await;
    assert_eq!(
        answer.status, CREATE_STATUS,
        "case {case}: expected {CREATE_STATUS}, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The `(id, location, properties)` `GetDocumentationPart` reports for `id`, asserting the
/// read was served — the round trip every case reads its part back through.
async fn read_back(gateway: &Gateway, api: &str, id: &str, case: &str) -> (String, Value, String) {
    let answer: Answer = get_part(gateway, api, id).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: GetDocumentationPart expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    (
        string_member(&answer.body, "id", case),
        answer.body["location"].clone(),
        string_member(&answer.body, "properties", case),
    )
}

/// How many parts the API documents, read through `GetDocumentationParts` with a page size
/// wide enough for every part a case writes.
async fn documented_count(gateway: &Gateway, api: &str, case: &str) -> usize {
    listed_ids(gateway, api, "limit=500", case).await.len()
}

/// The ids `GetDocumentationParts` reports under one `locationStatus` — `DOCUMENTED` for
/// "DocumentationPart resources with content", `UNDOCUMENTED` for those "without content"
/// (`GetDocumentationPartsRequest$locationStatus`).
async fn ids_with_status(gateway: &Gateway, api: &str, status: &str, case: &str) -> Vec<String> {
    listed_ids(
        gateway,
        api,
        &format!("limit=500&locationStatus={status}"),
        case,
    )
    .await
}

/// One entity under test: the location a call names it by, the other spellings of that same
/// location, and the location every part of it is stored and reported with.
struct Entity {
    /// Named in every assertion of the row.
    case: &'static str,
    /// The location a call sends.
    first: Value,
    /// Spellings of the SAME entity: each differs from `first` only in whether a member the
    /// type makes valid is spelled out at its documented default.
    other_spellings: Vec<Value>,
    /// The members the entity type makes valid, each defaulted where a call left it out.
    stored: Value,
}

/// The entities under test, spanning the location forms and the other spellings of each.
///
/// The valid-field set of each type and the default of each member are
/// `DocumentationPartLocation`'s own documentation
/// (`smithy_query(query="shape", shape="com.amazonaws.apigateway#DocumentationPartLocation")`):
/// `path` defaults to "/" "for the root resource", `method` to "*" "for any method",
/// `statusCode` to "*" "for any status code", and `name` documents no default — so a type
/// whose only valid member is `name` (`AUTHORIZER`, `MODEL`) and one with no valid member at
/// all (`API`) have exactly ONE spelling, and the rows below carry none for them.
fn entities() -> Vec<Entity> {
    vec![
        Entity {
            case: "the api itself, whose location sets no member at all",
            first: json!({ "type": "API" }),
            other_spellings: Vec::new(),
            stored: json!({ "type": "API" }),
        },
        Entity {
            case: "a model, named and nothing else",
            first: json!({ "type": "MODEL", "name": "Pet" }),
            other_spellings: Vec::new(),
            stored: json!({ "type": "MODEL", "name": "Pet" }),
        },
        Entity {
            case: "an authorizer, named and nothing else",
            first: json!({ "type": "AUTHORIZER", "name": "petAuth" }),
            other_spellings: Vec::new(),
            stored: json!({ "type": "AUTHORIZER", "name": "petAuth" }),
        },
        Entity {
            case: "the root resource, whose one valid member is defaulted",
            first: json!({ "type": "RESOURCE" }),
            other_spellings: vec![json!({ "type": "RESOURCE", "path": "/" })],
            stored: json!({ "type": "RESOURCE", "path": "/" }),
        },
        Entity {
            case: "one resource, by its path",
            first: json!({ "type": "RESOURCE", "path": "/pets" }),
            other_spellings: Vec::new(),
            stored: json!({ "type": "RESOURCE", "path": "/pets" }),
        },
        Entity {
            case: "every method of one resource, the verb defaulted",
            first: json!({ "type": "METHOD", "path": "/owners" }),
            other_spellings: vec![json!({ "type": "METHOD", "path": "/owners", "method": "*" })],
            stored: json!({ "type": "METHOD", "path": "/owners", "method": "*" }),
        },
        Entity {
            case: "every method of the root resource, both members defaulted",
            first: json!({ "type": "METHOD" }),
            other_spellings: vec![
                json!({ "type": "METHOD", "path": "/" }),
                json!({ "type": "METHOD", "method": "*" }),
                json!({ "type": "METHOD", "path": "/", "method": "*" }),
            ],
            stored: json!({ "type": "METHOD", "path": "/", "method": "*" }),
        },
        Entity {
            case: "every response of one method, the status code defaulted",
            first: json!({ "type": "RESPONSE", "path": "/owners", "method": "POST" }),
            other_spellings: vec![json!({
                "type": "RESPONSE",
                "path": "/owners",
                "method": "POST",
                "statusCode": "*",
            })],
            stored: json!({
                "type": "RESPONSE",
                "path": "/owners",
                "method": "POST",
                "statusCode": "*",
            }),
        },
        Entity {
            case: "one response of one method, by its status code",
            first: json!({
                "type": "RESPONSE",
                "path": "/pets",
                "method": "GET",
                "statusCode": "404",
            }),
            other_spellings: Vec::new(),
            stored: json!({
                "type": "RESPONSE",
                "path": "/pets",
                "method": "GET",
                "statusCode": "404",
            }),
        },
        Entity {
            case: "one header of every response of every method, a name and three defaults",
            first: json!({ "type": "RESPONSE_HEADER", "name": "X-Page" }),
            other_spellings: vec![
                json!({ "type": "RESPONSE_HEADER", "name": "X-Page", "path": "/" }),
                json!({
                    "type": "RESPONSE_HEADER",
                    "name": "X-Page",
                    "path": "/",
                    "method": "*",
                    "statusCode": "*",
                }),
            ],
            stored: json!({
                "type": "RESPONSE_HEADER",
                "path": "/",
                "method": "*",
                "statusCode": "*",
                "name": "X-Page",
            }),
        },
        Entity {
            case: "one query parameter of one method, every valid member spelled out",
            first: json!({
                "type": "QUERY_PARAMETER",
                "path": "/pets",
                "method": "GET",
                "name": "petId",
            }),
            other_spellings: Vec::new(),
            stored: json!({
                "type": "QUERY_PARAMETER",
                "path": "/pets",
                "method": "GET",
                "name": "petId",
            }),
        },
    ]
}

/// RULE, the refusal half: a create for an entity the API already documents is the modelled
/// conflict — however the location is spelled and whatever content map it carries — and the
/// stored part is left exactly as it was.
#[tokio::test]
async fn a_create_for_an_entity_the_api_documents_is_the_modelled_conflict() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-part-conflicts").await;

    let rows = entities();
    for (rank, entity) in rows.iter().enumerate() {
        let case = entity.case;
        let stored_properties = content_map(&format!("{case}: documented"));
        let first = written_part(
            &gateway,
            &api,
            entity.first.clone(),
            &stored_properties,
            case,
        )
        .await;
        let id = string_member(&first, "id", case);
        assert_eq!(
            first["location"], entity.stored,
            "case {case}: expected the stored location {:?}, got {:?}",
            entity.stored, first["location"]
        );

        // Every later create that names this entity: the location as the first call sent
        // it and each other spelling of it, carrying the stored content map and a
        // different one — the refusal turns on neither.
        let mut later: Vec<Value> = vec![entity.first.clone()];
        later.extend(entity.other_spellings.iter().cloned());
        for spelling in later {
            for (round, properties) in [
                ("the same content map", stored_properties.clone()),
                ("another content map", content_map(&format!("{case}: again"))),
            ] {
                let case = format!("{case}, spelled {spelling:?} with {round}");
                let answer =
                    create_part(&gateway, &api, spelling.clone(), &properties).await;
                assert_failure(&answer, CONFLICT.0, CONFLICT.1, &case);

                // The refused call changed nothing: the part reads back under its own id
                // with the content map it was created with, and the collection has not
                // grown.
                let (read_id, read_location, read_properties) =
                    read_back(&gateway, &api, &id, &case).await;
                assert_eq!(
                    read_id, id,
                    "case {case}: GetDocumentationPart reported the identifier {read_id:?}"
                );
                assert_eq!(
                    read_location, entity.stored,
                    "case {case}: expected the location {:?}, got {read_location:?}",
                    entity.stored
                );
                assert_eq!(
                    read_properties, stored_properties,
                    "case {case}: expected the content map {stored_properties:?}, got \
                     {read_properties:?}"
                );
                let documented = documented_count(&gateway, &api, &case).await;
                assert_eq!(
                    documented,
                    rank + 1,
                    "case {case}: expected {} documented entities, got {documented}",
                    rank + 1
                );
            }
        }
    }
}

/// RULE, the writing half: a create for an entity the API lists WITHOUT content writes that
/// content under the identifier that record already has — however the location is spelled —
/// so the API documents the entity once, and the entity is documented from then on.
#[tokio::test]
async fn a_create_documents_the_record_an_import_left_without_content() {
    for (axis, spelling_of) in [
        ("as the entry spelled it", 0usize),
        ("spelled another way", 1usize),
    ] {
        for entity in entities() {
            let case = format!("{} ({axis})", entity.case);
            // The spelling this case's create sends: the entry's own, and — where the
            // entity has one — another spelling of the same entity.
            let spelling = match spelling_of {
                0 => entity.first.clone(),
                _ => match entity.other_spellings.first() {
                    Some(other) => other.clone(),
                    // A type with one spelling has nothing to vary along; the row above
                    // already drove it.
                    None => continue,
                },
            };

            let gateway = Gateway::new();
            let (api, _root) = create_api(&gateway, "documentation-part-undocumented").await;

            // An import entry that declares NO content map: "an entry that declares no
            // content map documents nothing yet", which is the record
            // `locationStatus=UNDOCUMENTED` reports.
            let answer = import_parts(
                &gateway,
                &api,
                None,
                false,
                &import_body(&[(entity.first.clone(), Value::Null)]),
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case}: ImportDocumentationParts expected 200, got {} with {:?}",
                answer.status, answer.body
            );
            let listed = ids_with_status(&gateway, &api, "UNDOCUMENTED", &case).await;
            assert_eq!(
                listed.len(),
                1,
                "case {case}: expected the import to leave one record without content, got \
                 {listed:?}"
            );
            let imported = listed[0].clone();
            assert!(
                ids_with_status(&gateway, &api, "DOCUMENTED", &case)
                    .await
                    .is_empty(),
                "case {case}: expected nothing documented before the create"
            );

            // The create documents THAT record: its identifier, the entity's stored
            // location, and the content map the create carried.
            let properties = content_map(&case);
            let body = written_part(&gateway, &api, spelling.clone(), &properties, &case).await;
            assert_eq!(
                body["id"].as_str(),
                Some(imported.as_str()),
                "case {case}: expected the identifier the API already lists for the entity \
                 ({imported}), got {:?}",
                body["id"]
            );
            assert_eq!(
                body["location"], entity.stored,
                "case {case}: expected the stored location {:?}, got {:?}",
                entity.stored, body["location"]
            );
            assert_eq!(
                body["properties"].as_str(),
                Some(properties.as_str()),
                "case {case}: expected the content map the create carried {properties:?}, \
                 got {:?}",
                body["properties"]
            );

            // Round trip, and one part for the entity: the record is now DOCUMENTED, under
            // the identifier it has always had.
            let (read_id, read_location, read_properties) =
                read_back(&gateway, &api, &imported, &case).await;
            assert_eq!(
                read_id, imported,
                "case {case}: GetDocumentationPart reported the identifier {read_id:?}"
            );
            assert_eq!(
                read_location, entity.stored,
                "case {case}: GetDocumentationPart expected the location {:?}, got \
                 {read_location:?}",
                entity.stored
            );
            assert_eq!(
                read_properties, properties,
                "case {case}: GetDocumentationPart expected the content map {properties:?}, \
                 got {read_properties:?}"
            );
            let documented = ids_with_status(&gateway, &api, "DOCUMENTED", &case).await;
            assert_eq!(
                documented,
                vec![imported.clone()],
                "case {case}: expected the entity documented once, got {documented:?}"
            );
            assert!(
                ids_with_status(&gateway, &api, "UNDOCUMENTED", &case)
                    .await
                    .is_empty(),
                "case {case}: expected nothing left without content"
            );
            let total = documented_count(&gateway, &api, &case).await;
            assert_eq!(
                total, 1,
                "case {case}: expected the API to document one entity, got {total}"
            );

            // And from then on the entity IS documented, so a further create for it is the
            // conflict the other half of the rule answers.
            let again = create_part(
                &gateway,
                &api,
                spelling,
                &content_map(&format!("{case}: once documented")),
            )
            .await;
            assert_failure(
                &again,
                CONFLICT.0,
                CONFLICT.1,
                &format!("{case}: a create once the entity is documented"),
            );
        }
    }
}

/// RULE, the other side: the location names the ENTITY, so a location differing in one
/// member names another entity and is a part of its own.
#[tokio::test]
async fn a_location_differing_in_one_member_is_its_own_part() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-part-neighbours").await;

    // The entity every row is read against: a location that sets every member its type
    // makes valid, so a row can differ from it in exactly one of them.
    let base = json!({
        "type": "RESPONSE_HEADER",
        "path": "/pets",
        "method": "GET",
        "statusCode": "200",
        "name": "X-Page",
    });
    let base_properties = content_map("the base entity");
    let base_id = created_part_id(
        &gateway,
        &api,
        base.clone(),
        &base_properties,
        "the base entity",
    )
    .await;

    // One row per member of the location, each the base with that member alone changed.
    let rows: &[(&str, Value)] = &[
        (
            "another type at the same address",
            json!({
                "type": "RESPONSE_BODY",
                "path": "/pets",
                "method": "GET",
                "statusCode": "200",
            }),
        ),
        (
            "another path",
            json!({
                "type": "RESPONSE_HEADER",
                "path": "/owners",
                "method": "GET",
                "statusCode": "200",
                "name": "X-Page",
            }),
        ),
        (
            "another method",
            json!({
                "type": "RESPONSE_HEADER",
                "path": "/pets",
                "method": "POST",
                "statusCode": "200",
                "name": "X-Page",
            }),
        ),
        (
            "another status code",
            json!({
                "type": "RESPONSE_HEADER",
                "path": "/pets",
                "method": "GET",
                "statusCode": "404",
                "name": "X-Page",
            }),
        ),
        (
            "another name",
            json!({
                "type": "RESPONSE_HEADER",
                "path": "/pets",
                "method": "GET",
                "statusCode": "200",
                "name": "X-Total",
            }),
        ),
    ];

    let mut minted: Vec<String> = vec![base_id.clone()];
    for (case, location) in rows {
        let body = written_part(&gateway, &api, location.clone(), &content_map(case), case).await;
        let id = string_member(&body, "id", case);
        assert!(
            !minted.contains(&id),
            "case {case}: expected a part of its own for an entity nothing documents, got \
             {id} which is already {minted:?}"
        );
        minted.push(id);
        let documented = documented_count(&gateway, &api, case).await;
        assert_eq!(
            documented,
            minted.len(),
            "case {case}: expected {} documented entities, got {documented}",
            minted.len()
        );
    }

    // The base part is untouched by any of them: its own content map, under its own id.
    let (_, location, properties) = read_back(&gateway, &api, &base_id, "the base entity").await;
    assert_eq!(
        location, base,
        "case the base entity: expected the location {base:?}, got {location:?}"
    );
    assert_eq!(
        properties, base_properties,
        "case the base entity: expected its own content map, got {properties:?}"
    );
}

/// RULE: the API is the other half of the identity, so one location documented under two
/// APIs is two parts, and a call under one of them reaches only that API's part.
#[tokio::test]
async fn one_location_under_two_apis_is_two_parts() {
    let gateway = Gateway::new();
    let (first_api, _) = create_api(&gateway, "documentation-part-owner-one").await;
    let (second_api, _) = create_api(&gateway, "documentation-part-owner-two").await;

    // Rows span the location forms again, because the per-API half of the identity holds
    // for every one of them.
    for entity in entities() {
        let case = entity.case;
        let here = created_part_id(
            &gateway,
            &first_api,
            entity.first.clone(),
            &content_map(&format!("{case}: here")),
            &format!("{case} under the first api"),
        )
        .await;
        let there_properties = content_map(&format!("{case}: there"));
        let there = created_part_id(
            &gateway,
            &second_api,
            entity.first.clone(),
            &there_properties,
            &format!("{case} under the second api"),
        )
        .await;
        assert_ne!(
            here, there,
            "case {case}: expected one part per API for the location, got {here} twice"
        );

        // A further create under the FIRST API is decided against the FIRST API's records:
        // that entity is documented there, so it is the conflict — and the second API's
        // part is untouched by it.
        let refused = create_part(
            &gateway,
            &first_api,
            entity.first.clone(),
            &content_map(&format!("{case}: here again")),
        )
        .await;
        assert_failure(
            &refused,
            CONFLICT.0,
            CONFLICT.1,
            &format!("{case} repeated under the first api"),
        );
        let (_, _, properties) = read_back(
            &gateway,
            &second_api,
            &there,
            &format!("{case} under the second api"),
        )
        .await;
        assert_eq!(
            properties, there_properties,
            "case {case}: expected the second API's part untouched, got {properties:?}"
        );

        // Each part is readable only through the API that documents it.
        for (owner, other, part) in [
            (&first_api, &second_api, &here),
            (&second_api, &first_api, &there),
        ] {
            let answer = get_part(&gateway, other, part).await;
            assert_eq!(
                answer.status, 404,
                "case {case}: expected the part {part} of {owner} to be unreadable through \
                 {other}, got {} with {:?}",
                answer.status, answer.body
            );
        }
    }
}

/// The collection reports one item per ENTITY however many calls named it, each with the
/// content map its part holds — the state every assertion above counts, read here through
/// the listing itself.
#[tokio::test]
async fn the_documented_collection_reports_one_item_per_entity() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-part-collection-identity").await;

    let rows = entities();
    for entity in &rows {
        let case = entity.case;
        created_part_id(
            &gateway,
            &api,
            entity.first.clone(),
            &content_map(case),
            case,
        )
        .await;
        // Then every other call that names the entity, each refused and each leaving the
        // collection as it was.
        let mut later: Vec<Value> = vec![entity.first.clone()];
        later.extend(entity.other_spellings.iter().cloned());
        for spelling in later {
            let answer = create_part(
                &gateway,
                &api,
                spelling.clone(),
                &content_map(&format!("{case}: later")),
            )
            .await;
            assert_failure(
                &answer,
                CONFLICT.0,
                CONFLICT.1,
                &format!("{case}: spelled {spelling:?} again"),
            );
        }
    }

    let answer = gateway
        .call(
            "GET",
            &format!("/restapis/{api}/documentation/parts?limit=500"),
            None,
        )
        .await;
    let items = page_items(&answer.body, "the documented collection");
    assert_eq!(
        items.len(),
        rows.len(),
        "case the documented collection: expected {} items, got {items:?}",
        rows.len()
    );
    // The collection is ordered by the location of the entity each part documents, not by
    // the order these rows were written, so each row is looked up by its stored location.
    for entity in &rows {
        let case = entity.case;
        let listed: Vec<&Value> = items
            .iter()
            .filter(|item| item["location"] == entity.stored)
            .collect();
        assert_eq!(
            listed.len(),
            1,
            "case {case}: expected the location {:?} reported once, got {listed:?}",
            entity.stored
        );
        let created_with = content_map(case);
        assert_eq!(
            listed[0]["properties"].as_str(),
            Some(created_with.as_str()),
            "case {case}: expected the content map the part holds {created_with:?}, got {:?}",
            listed[0]["properties"]
        );
    }
}
