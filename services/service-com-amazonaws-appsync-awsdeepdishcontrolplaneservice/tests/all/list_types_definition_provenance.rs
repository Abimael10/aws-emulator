// SPDX-License-Identifier: CC0-1.0

//! The document `ListTypes` reports a type's definition as, over the calls that can have
//! STATED that definition last.
//!
//! The rule asserted here is the one `type_sdl_rendering.rs` states and whose table this
//! suite reuses: a report of a stored type computes `Type$definition` from the definition
//! the service PARSED, in the format the request names — in SDL, the service's own layout
//! (the declaring keyword and the type's name on a line of their own, a blank line, the
//! body's opening brace on its own line, each member unindented and followed by a blank
//! line, consecutive members separated by one further blank line, the closing brace on its
//! own line, and the whole document terminated by a newline) — and never the text a
//! request supplied.
//!
//! What this suite adds to the suites that assert the layout for a type a create or a
//! schema document installed is the dimension of PROVENANCE a listing also has: the type's
//! definition can last have been stated by an `UpdateType`, which replaces the text the
//! record keeps. Each row of the shared table is listed after the update restated the same
//! type in another text, and after an update replaced a seeded type of another kind
//! altogether, so the reported document can only have come from the parsed definition. The
//! last test walks the same stored types in both `TypeDefinitionFormat` values, because the
//! format chooses the representation and not the provenance of the text.
//!
//! The table, the router, the type calls, the listing lookup and the update call are the
//! crate's shared harness, reused rather than copied.

use serde_json::Value;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::graphql_api_lifecycle::router;
use super::graphql_types::{create_api, create_type, get_type, FORMATS};
use super::list_types_sdl_layout::reported;
use super::type_sdl_rendering::{definition_of, list_types, rows, Row};
use super::type_update::update_type;

/// Every type one `ListTypes` for `api_id` reports in `format`.
async fn listing(
    router: &MultiServiceRouter,
    api_id: &str,
    format: &str,
    case: &str,
) -> Vec<Value> {
    let answer = list_types(router, api_id, format).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected ListTypes 200, got {answer:?}"
    );
    answer.body["types"].as_array().cloned().unwrap_or_default()
}

/// Assert the listing of `api_id` reports `row` with the service's own rendering — the one
/// the single read of the same type answers — whatever text `stated` the last call wrote.
async fn assert_listed_rendering(
    router: &MultiServiceRouter,
    api_id: &str,
    row: &Row,
    stated: &str,
    case: &str,
) {
    let collected = listing(router, api_id, state::SDL_FORMAT, case).await;
    let listed = reported(&collected, row.name, case);
    let definition = definition_of(listed);
    assert_eq!(
        definition, row.rendered,
        "case {case}: expected ListTypes to report {:?}, got {definition:?}",
        row.rendered
    );
    assert!(
        definition.ends_with('\n'),
        "case {case}: expected the document to be newline-terminated, got {definition:?}"
    );
    if stated != row.rendered {
        assert_ne!(
            definition, stated,
            "case {case}: expected the stated text NOT to be echoed, got {definition:?}"
        );
    }
    assert_eq!(
        listed["format"].as_str(),
        Some(state::SDL_FORMAT),
        "case {case}: expected the SDL format reported back, got {listed:?}"
    );

    // A listing and a single read report one type with one document.
    let read = get_type(router, api_id, row.name, state::SDL_FORMAT).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected GetType 200, got {read:?}"
    );
    assert_eq!(
        definition_of(&read.body["type"]),
        definition,
        "case {case}: expected GetType to report what the listing reports, got {:?}",
        definition_of(&read.body["type"])
    );
}

/// An update that restates the same type in another text does not change the document the
/// listing reports: the update answers the text it stated, every later listing the
/// rendering.
#[tokio::test]
async fn a_listing_reports_the_rendering_after_an_update_restated_the_definition() {
    let router = router();
    let api_id = create_api(&router, "list-types-provenance-restated").await;

    for row in rows() {
        let created = create_type(&router, &api_id, row.stated, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateType 200, got {created:?}",
            row.label
        );
        assert_listed_rendering(
            &router,
            &api_id,
            &row,
            row.stated,
            &format!("{} created on its own", row.label),
        )
        .await;

        // The update restates the SAME type in the text the service itself renders it in,
        // which is a different byte string from the one the create wrote for every row
        // whose request text was not already the rendering.
        let updated = update_type(
            &router,
            &api_id,
            row.name,
            Some(row.rendered),
            state::SDL_FORMAT,
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {}: expected UpdateType 200, got {updated:?}",
            row.label
        );
        assert_eq!(
            definition_of(&updated.body["type"]),
            row.rendered,
            "case {}: expected the update to answer the stated definition {:?}, got {:?}",
            row.label,
            row.rendered,
            definition_of(&updated.body["type"])
        );
        assert_listed_rendering(
            &router,
            &api_id,
            &row,
            row.rendered,
            &format!("{} restated by an update", row.label),
        )
        .await;
    }
}

/// A type an update replaced with a definition of another kind is listed with the rendering
/// of the definition the UPDATE stated, not with that text and not with the seeded type.
#[tokio::test]
async fn a_listing_reports_the_rendering_of_the_definition_an_update_installed() {
    let router = router();
    let api_id = create_api(&router, "list-types-provenance-replaced").await;
    let held = rows();

    for row in &held {
        // A type of another kind is seeded first, so the document the listing reports
        // afterwards can only come from the definition the update stated.
        let seeded = format!("scalar {}\n", row.name);
        let created = create_type(&router, &api_id, &seeded, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateType 200 for the seed, got {created:?}",
            row.label
        );
        let updated = update_type(
            &router,
            &api_id,
            row.name,
            Some(row.stated),
            state::SDL_FORMAT,
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {}: expected UpdateType 200, got {updated:?}",
            row.label
        );
    }

    for row in &held {
        let case = format!("{} installed by an update", row.label);
        assert_listed_rendering(&router, &api_id, row, row.stated, &case).await;

        // Nor is the seeded definition what the listing still reports.
        let seeded = format!("scalar {}\n", row.name);
        let collected = listing(&router, &api_id, state::SDL_FORMAT, &case).await;
        let definition = definition_of(reported(&collected, row.name, &case));
        assert_ne!(
            definition, seeded,
            "case {case}: expected the seeded definition to be replaced, got {definition:?}"
        );
    }
}

/// The format a request names chooses the representation the listing reports, and both are
/// the ones the single read of the same stored type answers: the provenance of the text a
/// request wrote does not reach either.
#[tokio::test]
async fn a_listing_reports_each_format_from_the_same_stored_types() {
    let router = router();
    let api_id = create_api(&router, "list-types-provenance-formats").await;
    let held = rows();

    for row in &held {
        let created = create_type(&router, &api_id, row.stated, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateType 200, got {created:?}",
            row.label
        );
    }

    for format in FORMATS {
        for row in &held {
            let case = format!("{} listed in {format}", row.label);
            let collected = listing(&router, &api_id, format, &case).await;
            let listed = reported(&collected, row.name, &case);
            let definition = definition_of(listed);
            assert_eq!(
                listed["format"].as_str(),
                Some(*format),
                "case {case}: expected the requested format reported back, got {listed:?}"
            );

            let read = get_type(&router, &api_id, row.name, format).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected GetType 200, got {read:?}"
            );
            assert_eq!(
                definition,
                definition_of(&read.body["type"]),
                "case {case}: expected the listing to report what GetType reports ({:?}), got \
                 {definition:?}",
                definition_of(&read.body["type"])
            );

            if *format == state::JSON_FORMAT {
                // The JSON representation is the type's own introspection document, which
                // names the type it was parsed from.
                let document: Value = serde_json::from_str(definition).unwrap_or_else(|error| {
                    panic!("case {case}: expected a JSON document, got {definition:?} ({error})")
                });
                assert_eq!(
                    document["name"].as_str(),
                    Some(row.name),
                    "case {case}: expected the JSON document to name {:?}, got {document:?}",
                    row.name
                );
                assert!(
                    document["kind"].as_str().is_some(),
                    "case {case}: expected the JSON document to carry a kind, got {document:?}"
                );
            } else {
                assert_eq!(
                    definition, row.rendered,
                    "case {case}: expected ListTypes to report {:?}, got {definition:?}",
                    row.rendered
                );
            }
        }
    }
}
