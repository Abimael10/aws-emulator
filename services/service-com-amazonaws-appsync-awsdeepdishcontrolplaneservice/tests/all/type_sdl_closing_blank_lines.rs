// SPDX-License-Identifier: CC0-1.0

//! What CLOSES the body of a rendered type definition, end to end over HTTP.
//!
//! The rule asserted here is stated over a CLASS of definitions rather than over the one
//! request that exposed the defect: in the SDL a READ of a type answers with, the blank
//! lines between a body's last member and its closing brace are read off THAT member — the
//! brace follows the blank line a member that NAMES A TYPE (a field of an object or
//! interface type, an input field of an input object type) is written with, and one
//! FURTHER blank line when the type that member names is non-null, while a member that is
//! a name alone (an enum value) is followed by the brace directly. The blank line between
//! two consecutive members is unaffected by any of it.
//!
//! The table spans what the rule generalises over: the kinds whose bodies hold members
//! that name a type and the kind whose body holds names alone, bodies of one, two and
//! three members, the non-null member first, in the middle and LAST in its body, and every
//! form a type reference is written in — a name, a non-null name, a list, a list of
//! non-nulls and a non-null list. The documents are asserted byte for byte, since the
//! bytes are what a client diffs or hashes, and every type is read through `GetType` and
//! through `ListTypes`, which answer one document for one type.
//!
//! Every assertion goes through the router the production emulator mounts.

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

// The router, the API-creating helper and the type calls are the crate's shared test
// harness, reused rather than copied.
use super::graphql_api_lifecycle::router;
use super::graphql_types::{create_api, create_type, get_type};
use super::type_sdl_rendering::{definition_of, list_types};

/// One row: a definition as a request writes it, and the document a read of the type it
/// declares answers with.
#[derive(Debug, Clone)]
struct Row {
    /// What the row covers, named in every assertion of it.
    label: &'static str,
    /// The name the definition declares, which the type is addressed by.
    name: &'static str,
    /// The definition the type is created from.
    stated: &'static str,
    /// The SDL document a read of the stored type answers with.
    rendered: &'static str,
}

/// The rows the rule is read over.
///
/// Source: the member forms GraphQL's SDL grammar gives a type body — a field definition
/// and an input value definition, which name a type, and an enum value definition, which
/// is a name alone — and GraphQL's type-reference grammar, which writes a reference as a
/// name, wraps one in a list (`[T]`) and makes one non-null (`T!`). The non-null reference
/// is the one the rule reads, so the rows place it first, in the middle and last in a
/// body.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "an interface whose only field names a non-null type",
            name: "Node",
            stated: "interface Node { id: ID! }",
            rendered: "interface Node\n\n{\nid: ID!\n\n\n}\n",
        },
        Row {
            label: "an object type whose only field names a non-null type",
            name: "Keyed",
            stated: "type Keyed {\n  id: ID!\n}\n",
            rendered: "type Keyed\n\n{\nid: ID!\n\n\n}\n",
        },
        Row {
            label: "an object type whose only field names a nullable type",
            name: "Titled",
            stated: "type Titled { title: String }",
            rendered: "type Titled\n\n{\ntitle: String\n\n}\n",
        },
        Row {
            label: "a non-null field first, a nullable one closing the body",
            name: "Blog",
            stated: "type Blog { id: ID! title: String }",
            rendered: "type Blog\n\n{\nid: ID!\n\n\ntitle: String\n\n}\n",
        },
        Row {
            label: "a nullable field first, a non-null one closing the body",
            name: "Reversed",
            stated: "type Reversed { title: String id: ID! }",
            rendered: "type Reversed\n\n{\ntitle: String\n\n\nid: ID!\n\n\n}\n",
        },
        Row {
            label: "a non-null field in the MIDDLE of three, a non-null one closing",
            name: "Fields",
            stated: "type Fields { alpha: ID! beta: String gamma: Int! }",
            rendered: "type Fields\n\n{\nalpha: ID!\n\n\nbeta: String\n\n\ngamma: Int!\n\n\n}\n",
        },
        Row {
            label: "fields naming every form a type reference is written in, a non-null \
                    list closing the body",
            name: "Wrapped",
            stated: "type Wrapped {\n\
                     \x20 plain: String\n\
                     \x20 required: String!\n\
                     \x20 list: [String]\n\
                     \x20 listOfRequired: [String!]\n\
                     \x20 requiredList: [String]!\n\
                     }\n",
            rendered: "type Wrapped\n\n{\n\
                       plain: String\n\n\n\
                       required: String!\n\n\n\
                       list: [String]\n\n\n\
                       listOfRequired: [String!]\n\n\n\
                       requiredList: [String]!\n\n\n\
                       }\n",
        },
        Row {
            label: "an input object type whose last input field names a non-null type",
            name: "WidgetInput",
            stated: "input WidgetInput { tags: [String] title: String! }",
            rendered: "input WidgetInput\n\n{\ntags: [String]\n\n\ntitle: String!\n\n\n}\n",
        },
        Row {
            label: "an input object type whose last input field names a list",
            name: "PostInput",
            stated: "input PostInput { title: String! tags: [String!] = [] }",
            rendered: "input PostInput\n\n{\ntitle: String!\n\n\ntags: [String!] = []\n\n}\n",
        },
        Row {
            label: "an enum type, whose values name no type at all",
            name: "Shelf",
            stated: "enum Shelf { NEW OLD }",
            rendered: "enum Shelf\n\n{\nNEW\n\nOLD\n}\n",
        },
    ]
}

/// A read of every row's type answers the document the rule states, byte for byte, and the
/// listing answers the same document for the same type.
#[tokio::test]
async fn the_brace_that_closes_a_body_follows_what_its_last_member_names() {
    let router = router();
    let api_id = create_api(&router, "type-sdl-closing-blank-lines").await;

    for row in rows() {
        let created = create_type(&router, &api_id, row.stated, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateType 200, got {created:?}",
            row.label
        );

        let read = get_type(&router, &api_id, row.name, state::SDL_FORMAT).await;
        assert_eq!(
            read.status, 200,
            "case {}: expected GetType 200, got {read:?}",
            row.label
        );
        assert_eq!(
            definition_of(&read.body["type"]),
            row.rendered,
            "case {}: expected GetType to answer {:?}, got {:?}",
            row.label,
            row.rendered,
            definition_of(&read.body["type"])
        );
    }

    let listing = list_types(&router, &api_id, state::SDL_FORMAT).await;
    assert_eq!(
        listing.status, 200,
        "case the listing: expected ListTypes 200, got {listing:?}"
    );
    let listed = listing.body["types"].as_array().cloned().unwrap_or_default();
    for row in rows() {
        let reported = listed
            .iter()
            .find(|reported| reported["name"].as_str() == Some(row.name))
            .unwrap_or_else(|| {
                panic!(
                    "case {}: expected {:?} in the listing, got {listed:?}",
                    row.label, row.name
                )
            });
        assert_eq!(
            definition_of(reported),
            row.rendered,
            "case {}: expected ListTypes to answer {:?}, got {:?}",
            row.label,
            row.rendered,
            definition_of(reported)
        );
    }
}

/// The forms one GraphQL type REFERENCE is written in, and the blank lines a body whose
/// LAST member names one closes after.
///
/// Source: GraphQL's type-reference grammar — a name, a name wrapped in a list (`[T]`) and
/// a non-null one (`T!`) — over which only the reference that is itself non-null carries
/// the further blank line.
const REFERENCE_FORMS: &[(&str, &str, usize)] = &[
    ("a nullable name", "String", 1),
    ("a non-null name", "String!", 2),
    ("a list", "[String]", 1),
    ("a list of non-nulls", "[String!]", 1),
    ("a non-null list", "[String]!", 2),
];

/// Whichever reference a body's last member names, and however many members stand before
/// it, the brace closes after the blank lines that reference carries — while the members
/// before it stay one blank line apart from the line that follows them.
#[tokio::test]
async fn how_many_members_stand_before_the_last_one_does_not_change_the_close() {
    let router = router();
    let api_id = create_api(&router, "type-sdl-closing-body-sizes").await;

    // The members written BEFORE the one under test, each naming a nullable type so only
    // the last member can carry the further blank line.
    let padding = ["before0: Int", "before1: Int"];

    for (index, (form, reference, blanks)) in REFERENCE_FORMS.iter().enumerate() {
        for held in 0..=padding.len() {
            let case = format!("{form} closing a body of {} members", held + 1);
            let name = format!("Closing{index}_{held}");
            let mut members: Vec<String> = padding
                .iter()
                .take(held)
                .map(|member| (*member).to_string())
                .collect();
            members.push(format!("under: {reference}"));
            let stated = format!("type {name} {{ {} }}", members.join(" "));
            let created = create_type(&router, &api_id, &stated, state::SDL_FORMAT).await;
            assert_eq!(
                created.status, 200,
                "case {case}: expected CreateType 200, got {created:?}"
            );

            let read = get_type(&router, &api_id, &name, state::SDL_FORMAT).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected GetType 200, got {read:?}"
            );
            let definition = definition_of(&read.body["type"]).to_string();

            // The close: the last member's line, the blank lines its reference carries,
            // then the brace and the terminating newline.
            let close = format!("under: {reference}\n{}}}\n", "\n".repeat(*blanks));
            assert!(
                definition.ends_with(&close),
                "case {case}: expected the body to close with {close:?}, got {definition:?}"
            );
            // And a member another member follows is one blank line apart from it, as it
            // is in a body of any size.
            for member in padding.iter().take(held) {
                let separated = format!("{member}\n\n\n");
                assert!(
                    definition.contains(&separated),
                    "case {case}: expected {separated:?} written in {definition:?}"
                );
            }
        }
    }
}
