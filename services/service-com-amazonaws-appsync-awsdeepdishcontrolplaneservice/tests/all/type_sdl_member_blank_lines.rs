// SPDX-License-Identifier: CC0-1.0

//! The blank lines BETWEEN two consecutive members of a rendered type body, end to end
//! over HTTP, and that they do not depend on how the type got into the API.
//!
//! The rule asserted here is the other half of the layout `type_sdl_closing_blank_lines.rs`
//! states: in the SDL a READ of a type answers with, a member that NAMES A TYPE (a field
//! of an object or interface type, an input field of an input object type) is written with
//! one blank line of its own and consecutive members are separated by one further blank
//! line — so two blank lines stand between them, whatever each of them names — while an
//! enum value is a name alone and consecutive ones stand one blank line apart. What a
//! member names decides only what CLOSES the body (the other suite's rule), never what
//! separates it from the next member.
//!
//! The table spans what the rule generalises over: both kinds of member that name a type
//! and the kind that is a name alone, every form of type reference on either side of a
//! separation (a name, a non-null name and the two wrappings of a list), and both ways a
//! type arrives — created on its own (`CreateType`) and declared inside the schema document
//! a `StartSchemaCreation` installs. The documents are asserted byte for byte, since the
//! bytes are what a client diffs or hashes.
//!
//! Every assertion goes through the router the production emulator mounts.

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

// The router, the API-creating helper, the type calls and the schema-creation helper are
// the crate's shared test harness, reused rather than copied.
use super::graphql_api_lifecycle::router;
use super::graphql_types::{create_api, create_type, get_type};
use super::type_sdl_rendering::{create_schema, definition_of};

/// One row: a definition as a request writes it, and the document a read of the type it
/// declares answers with.
#[derive(Debug, Clone)]
struct Row {
    /// What the row covers, named in every assertion of it.
    label: &'static str,
    /// The name the definition declares, which the type is addressed by.
    name: &'static str,
    /// The definition the type is created from, written on one line so the rendering can
    /// only come from the parsed type.
    stated: &'static str,
    /// The SDL document a read of the stored type answers with.
    rendered: &'static str,
}

/// The rows the separation is read over.
///
/// Source: the member forms GraphQL's SDL grammar gives a type body — a field definition
/// and an input value definition, which name a type, and an enum value definition, which
/// is a name alone — and GraphQL's type-reference grammar, which writes a reference as a
/// name, wraps one in a list (`[T]`) and makes one non-null (`T!`). Each row writes a
/// different pair of references across the separation under test, and every row's body ends
/// in the same nullable member, so only the separations differ between the rows.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "two fields naming nullable names",
            name: "Pair",
            stated: "type Pair { alpha: String beta: String last: Int }",
            rendered: "type Pair\n\n{\nalpha: String\n\n\nbeta: String\n\n\nlast: Int\n\n}\n",
        },
        Row {
            label: "a non-null field followed by a nullable one",
            name: "Keyed",
            stated: "type Keyed { id: ID! beta: String last: Int }",
            rendered: "type Keyed\n\n{\nid: ID!\n\n\nbeta: String\n\n\nlast: Int\n\n}\n",
        },
        Row {
            label: "a nullable field followed by a non-null one",
            name: "Reversed",
            stated: "type Reversed { alpha: String id: ID! last: Int }",
            rendered: "type Reversed\n\n{\nalpha: String\n\n\nid: ID!\n\n\nlast: Int\n\n}\n",
        },
        Row {
            label: "a list followed by a list of non-nulls",
            name: "Listed",
            stated: "type Listed { tags: [String] names: [String!] last: Int }",
            rendered: "type Listed\n\n{\ntags: [String]\n\n\nnames: [String!]\n\n\nlast: Int\n\n}\n",
        },
        Row {
            label: "a non-null list followed by a nullable name",
            name: "Required",
            stated: "type Required { tags: [String]! beta: String last: Int }",
            rendered: "type Required\n\n{\ntags: [String]!\n\n\nbeta: String\n\n\nlast: Int\n\n}\n",
        },
        Row {
            label: "an interface's two fields",
            name: "Timestamped",
            stated: "interface Timestamped { createdAt: AWSDateTime id: ID! last: Int }",
            rendered: "interface Timestamped\n\n{\ncreatedAt: AWSDateTime\n\n\nid: ID!\n\n\n\
                       last: Int\n\n}\n",
        },
        Row {
            label: "an input object type's two input fields",
            name: "PairInput",
            stated: "input PairInput { title: String! tags: [String] last: Int }",
            rendered: "input PairInput\n\n{\ntitle: String!\n\n\ntags: [String]\n\n\n\
                       last: Int\n\n}\n",
        },
        Row {
            label: "an enum type's three values, which name no type at all",
            name: "Color",
            stated: "enum Color { RED GREEN BLUE }",
            rendered: "enum Color\n\n{\nRED\n\nGREEN\n\nBLUE\n}\n",
        },
    ]
}

/// Read every row's type and assert the document, under the API `api_id`, naming the
/// provenance the types arrived by.
async fn assert_reads(
    router: &smithy_server_runtime::MultiServiceRouter,
    api_id: &str,
    arrival: &str,
) {
    for row in rows() {
        let case = format!("{} ({arrival})", row.label);
        let read = get_type(router, api_id, row.name, state::SDL_FORMAT).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetType 200, got {read:?}"
        );
        assert_eq!(
            definition_of(&read.body["type"]),
            row.rendered,
            "case {case}: expected GetType to answer {:?}, got {:?}",
            row.rendered,
            definition_of(&read.body["type"])
        );
    }
}

/// A type created on its own is read with its members one blank line apart from the line
/// that follows them.
#[tokio::test]
async fn consecutive_members_of_a_created_type_are_separated_the_same_way() {
    let router = router();
    let api_id = create_api(&router, "type-sdl-member-separation").await;

    for row in rows() {
        let created = create_type(&router, &api_id, row.stated, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateType 200, got {created:?}",
            row.label
        );
    }

    assert_reads(&router, &api_id, "created on its own").await;
}

/// The same documents are answered for the same types declared inside a schema document,
/// so the separation comes from the parsed type and not from the call that installed it.
#[tokio::test]
async fn a_type_declared_by_a_schema_document_is_separated_the_same_way() {
    let router = router();
    let api_id = create_api(&router, "type-sdl-member-separation-schema").await;

    let document: String = rows()
        .iter()
        .map(|row| format!("{}\n", row.stated))
        .collect();
    create_schema(&router, &api_id, &document).await;

    assert_reads(&router, &api_id, "declared by a schema document").await;
}
