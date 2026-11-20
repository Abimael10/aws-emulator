// SPDX-License-Identifier: CC0-1.0

//! Which `Type` projection a type report answers with, end to end over HTTP.
//!
//! Two rules, each stated over a CLASS of inputs rather than over the one request that
//! exposed the defect:
//!
//! 1. a type reported by an association listing is named under the MERGED API the request
//!    addresses — the ARN that type has on the Merged API its source API's schema is
//!    merged into, which is the ARN the Merged API's own read of the type answers — while
//!    its name, its description and its definition are the source API's own;
//! 2. the `Type` an UPDATE of a definition answers with carries no `name` at all, while
//!    the one a read answers with carries it; every other member is the same.
//!
//! The rows span every kind of type a GraphQL SDL definition declares (the `graphql_types`
//! catalog: object, interface, union, enum, input object and scalar, with and without a
//! description, arguments, defaults, wrapped types and a `@deprecated` directive), both
//! `TypeDefinitionFormat` values, and — for the update — both an update that states a new
//! definition and one that states none.
//!
//! The harness (`router`, `call`, `create_api`, `create_type`, `get_type`, the type
//! catalog `rows()` and the projection assertions) is the `graphql_types` suite's, the
//! association pair and the poll-until-merged wait are the `source_api_association`
//! suite's, and the update call is the `type_update` suite's — reused rather than copied.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::graphql_api_lifecycle::router;
use super::graphql_types::{
    assert_projects, assert_updates, call, create_api, create_type, get_type, rows, Answer, Row,
    FORMATS,
};
use super::source_api_association::{associate_path, await_terminal_merge, create_pair};
use super::type_update::update_type;

/// Send one `ListTypesByAssociation` for the pair, in `format`.
async fn list_types_by_association(
    router: &MultiServiceRouter,
    merged_api_identifier: &str,
    association_id: &str,
    format: &str,
) -> Answer {
    call(
        router,
        "GET",
        &format!(
            "/v1/mergedApis/{merged_api_identifier}/sourceApiAssociations/\
             {association_id}/types?format={format}"
        ),
        None,
    )
    .await
}

/// Associate `source_id` with `merged_id` and return the association's id.
async fn associate(router: &MultiServiceRouter, source_id: &str, merged_id: &str) -> String {
    let answer = call(
        router,
        "POST",
        &associate_path(source_id),
        Some(json!({"mergedApiIdentifier": merged_id})),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case associate {source_id} with {merged_id}: expected 200, got {answer:?}"
    );
    answer.body["sourceApiAssociation"]["associationId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The type `name` an answer reports, or a panic naming the case.
fn reported<'a>(answer: &'a Answer, name: &str, case: &str) -> &'a Value {
    answer.body["types"]
        .as_array()
        .and_then(|types| {
            types
                .iter()
                .find(|listed| listed["name"].as_str() == Some(name))
        })
        .unwrap_or_else(|| panic!("case {case}: expected {name:?} in the listing, got {answer:?}"))
}

/// An association listing names every type it reports under the MERGED API the request
/// addresses, which is the ARN the Merged API's own read of that type answers with.
#[tokio::test]
async fn an_association_listing_names_each_type_under_the_merged_api() {
    let router = router();
    let (source, merged) = create_pair(&router, "type-projection-association").await;

    let held = rows();
    for row in &held {
        let created = create_type(&router, &source.id, row.definition, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case create {:?} on the source API: expected 200, got {created:?}",
            row.name
        );
    }

    let association_id = associate(&router, &source.id, &merged.id).await;
    // The merge is a transition the CLOCK owes, so the Merged API's own read of a merged
    // type is asked for only once the merge has been performed: a wait on the condition,
    // never on a fixed time.
    await_terminal_merge(
        &router,
        "type-projection-association",
        &merged.id,
        &association_id,
    )
    .await;

    for format in FORMATS {
        let case = format!("listed by association ({format})");
        let answer = list_types_by_association(&router, &merged.id, &association_id, format).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected ListTypesByAssociation 200, got {answer:?}"
        );

        for row in &held {
            let listed = reported(&answer, row.name, &case);
            // Every member is the source API's own definition, named under the Merged API.
            assert_projects(row, &merged.id, format, listed, &case);

            // The ARN is the one the type has ON the Merged API: what the Merged API's own
            // read of that name answers, and not the source API's own path.
            let read = get_type(&router, &merged.id, row.name, format).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected the Merged API to hold {:?}, got {read:?}",
                row.name
            );
            assert_eq!(
                listed["arn"], read.body["type"]["arn"],
                "case {case}: expected {:?} named under the Merged API as {:?}, got {:?}",
                row.name, read.body["type"]["arn"], listed["arn"]
            );
            assert!(
                listed["arn"]
                    .as_str()
                    .is_some_and(|arn| !arn.contains(&source.id)),
                "case {case}: expected {:?} not named under the source API {:?}, got {:?}",
                row.name,
                source.id,
                listed["arn"]
            );
        }
    }
}

/// What an update reports for the type `row` under `api_id`, in `format`: the `Type`
/// carries no `name`, while the read of the same type carries it and agrees on every
/// other member.
async fn assert_update_omits_the_name(
    router: &MultiServiceRouter,
    api_id: &str,
    row: &Row,
    definition: Option<&str>,
    format: &str,
    case: &str,
) {
    let updated = update_type(router, api_id, row.name, definition, format).await;
    assert_eq!(
        updated.status, 200,
        "case {case}: expected UpdateType 200, got {updated:?}"
    );
    assert_updates(row, api_id, format, &updated.body["type"], case);

    // The read of the same type DOES carry the name, so the absence is this operation's
    // own projection rather than a type that lost its name.
    let read = get_type(router, api_id, row.name, format).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected GetType 200, got {read:?}"
    );
    assert_eq!(
        read.body["type"]["name"],
        json!(row.name),
        "case {case}: expected the read to carry the name {:?}, got {:?}",
        row.name,
        read.body["type"]["name"]
    );
    for member in ["arn", "description", "format"] {
        assert_eq!(
            updated.body["type"][member], read.body["type"][member],
            "case {case}: expected {member} to be what the read answers ({:?}), got {:?}",
            read.body["type"][member], updated.body["type"][member]
        );
    }
}

/// An update's `Type` carries no `name`, whatever kind of definition it replaces, in
/// either format, and whether or not the request states a new definition.
#[tokio::test]
async fn an_update_reports_the_type_without_a_name() {
    let router = router();

    for format in FORMATS {
        let api_id = create_api(&router, &format!("type-projection-update-{format}")).await;
        for row in rows() {
            let created = create_type(&router, &api_id, row.definition, state::SDL_FORMAT).await;
            assert_eq!(
                created.status, 200,
                "case create {:?}: expected 200, got {created:?}",
                row.name
            );

            // An update that states the definition again, and one that states none at all
            // (`definition` is an optional member): the projection is the operation's, so
            // neither carries a name.
            assert_update_omits_the_name(
                &router,
                &api_id,
                &row,
                Some(row.definition),
                format,
                &format!("{:?} updated with its definition ({format})", row.name),
            )
            .await;
            assert_update_omits_the_name(
                &router,
                &api_id,
                &row,
                None,
                format,
                &format!("{:?} updated with no definition ({format})", row.name),
            )
            .await;
        }
    }
}
