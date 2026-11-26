// SPDX-License-Identifier: CC0-1.0

//! What a READ of a GraphQL type reports for the parts of a definition this crate used to
//! drop or to lay out by itself, end to end over HTTP.
//!
//! Two rules are asserted here, each stated over a CLASS of definitions rather than over
//! one request:
//!
//! 1. a read reports the service's own rendering of the WHOLE stored definition, so every
//!    directive the definition writes — on the type itself, on a field, on an enum value
//!    or on an input field — is written back in the SDL, with the arguments it states, and
//!    a type's own directives appear in its JSON document as well (an enum value's reach
//!    that document through its deprecation alone). A rendering that dropped a directive
//!    would report a type the caller never declared;
//! 2. the body of a rendered definition writes a member that NAMES A TYPE (a field, an
//!    input field) followed by a blank line, and a member that is a name alone (an enum
//!    value) without it, so an enum's values are one blank line apart and the closing
//!    brace follows the last value directly — and the brace that closes a body whose last
//!    member names a NON-NULL type follows one further blank line.
//!
//! The tables span the shapes each rule generalises over: every kind of type a definition
//! can declare and every place a directive can be written for rule 1, and bodies of one,
//! two and three members of each member form for rule 2. The documents are asserted byte
//! for byte, since the bytes are what a client diffs, hashes or line-parses.
//!
//! Every assertion goes through the router the production emulator mounts.

use serde_json::Value;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

// The router, the API-creating helper and the type calls are the crate's shared test
// harness, reused rather than copied.
use super::graphql_api_lifecycle::router;
use super::graphql_types::{create_api, create_type, get_type};

/// One row: a definition as a request writes it, and the two documents a read of the type
/// it declares answers with.
#[derive(Debug, Clone)]
struct Row {
    /// What the row covers, named in every assertion of it.
    label: &'static str,
    /// The name the definition declares, which the type is addressed by.
    name: &'static str,
    /// The definition the type is created from.
    stated: &'static str,
    /// The SDL document a read of the stored type answers with.
    sdl: &'static str,
    /// The JSON document a read of the stored type answers with.
    json: &'static str,
}

/// The directives a type definition can carry, in every place GraphQL's SDL grammar lets
/// one be written.
///
/// Source: GraphQL's SDL directive locations for type definitions — the definition itself,
/// a field definition, an enum value definition and an input value definition — and the
/// directives an AppSync schema writes there (`@deprecated`, which this service also
/// reports through `isDeprecated`/`deprecationReason`, and the service's own
/// authorization directives, which are directives like any other to the renderer). The
/// rows differ in kind, in where the directive is written, in how many directives one
/// element carries and in whether a directive states arguments.
fn directive_rows() -> Vec<Row> {
    vec![
        Row {
            label: "an object type carrying two directives of its own",
            name: "Todo",
            stated: "type Todo @aws_api_key @aws_iam { id: ID! }",
            sdl: "type Todo @aws_api_key @aws_iam\n\n{\nid: ID!\n\n\n}\n",
            json: "{\"name\":\"Todo\",\"kind\":\"OBJECT\",\"fields\":[\
                   {\"name\":\"id\",\"type\":{\"kind\":\"NON_NULL\",\"ofType\":{\"name\":\"ID\"}},\
                   \"args\":[],\"directives\":[],\"isDeprecated\":false}],\
                   \"interfaces\":[],\"directives\":[\
                   {\"name\":\"aws_api_key\",\"args\":[]},{\"name\":\"aws_iam\",\"args\":[]}]}",
        },
        Row {
            label: "an object type whose field carries a directive with an argument",
            name: "Note",
            stated: "type Note { legacy: String @deprecated(reason: \"use title\") }",
            sdl: "type Note\n\n{\nlegacy: String @deprecated(reason: \"use title\")\n\n}\n",
            json: "{\"name\":\"Note\",\"kind\":\"OBJECT\",\"fields\":[\
                   {\"name\":\"legacy\",\"type\":{\"name\":\"String\"},\"args\":[],\
                   \"directives\":[{\"name\":\"deprecated\",\"args\":[\
                   {\"name\":\"reason\",\"value\":\"\\\"use title\\\"\"}]}],\
                   \"isDeprecated\":true,\"deprecationReason\":\"use title\"}],\
                   \"interfaces\":[],\"directives\":[]}",
        },
        Row {
            label: "an interface type carrying a directive",
            name: "Timestamped",
            stated: "interface Timestamped @aws_iam { createdAt: AWSDateTime }",
            sdl: "interface Timestamped @aws_iam\n\n{\ncreatedAt: AWSDateTime\n\n}\n",
            json: "{\"name\":\"Timestamped\",\"kind\":\"INTERFACE\",\"fields\":[\
                   {\"name\":\"createdAt\",\"type\":{\"name\":\"AWSDateTime\"},\"args\":[],\
                   \"directives\":[],\"isDeprecated\":false}],\
                   \"directives\":[{\"name\":\"aws_iam\",\"args\":[]}]}",
        },
        Row {
            label: "an enum type whose value carries a directive with no arguments",
            name: "Shelf",
            stated: "enum Shelf @aws_api_key { NEW OLD @deprecated }",
            sdl: "enum Shelf @aws_api_key\n\n{\nNEW\n\nOLD @deprecated\n}\n",
            json: "{\"name\":\"Shelf\",\"kind\":\"ENUM\",\"enumValues\":[\
                   {\"name\":\"NEW\",\"isDeprecated\":false},\
                   {\"name\":\"OLD\",\"isDeprecated\":true,\
                   \"deprecationReason\":\"No longer supported\"}],\
                   \"directives\":[{\"name\":\"aws_api_key\",\"args\":[]}]}",
        },
        Row {
            label: "an input object type whose field carries a directive",
            name: "TodoInput",
            stated: "input TodoInput @aws_iam { title: String @aws_api_key }",
            sdl: "input TodoInput @aws_iam\n\n{\ntitle: String @aws_api_key\n\n}\n",
            json: "{\"name\":\"TodoInput\",\"kind\":\"INPUT_OBJECT\",\"inputFields\":[\
                   {\"name\":\"title\",\"type\":{\"name\":\"String\"}}],\
                   \"directives\":[{\"name\":\"aws_iam\",\"args\":[]}]}",
        },
        Row {
            label: "a union type carrying a directive, which has no body at all",
            name: "Anything",
            stated: "union Anything @aws_api_key = Todo | Note",
            sdl: "union Anything @aws_api_key = Todo | Note\n",
            json: "{\"name\":\"Anything\",\"kind\":\"UNION\",\"possibleTypes\":[\
                   {\"name\":\"Todo\"},{\"name\":\"Note\"}],\
                   \"directives\":[{\"name\":\"aws_api_key\",\"args\":[]}]}",
        },
        Row {
            label: "a scalar type carrying a directive, which has no body at all",
            name: "Money",
            stated: "scalar Money @aws_iam\n",
            sdl: "scalar Money @aws_iam\n",
            json: "{\"name\":\"Money\",\"kind\":\"SCALAR\",\"directives\":[\
                   {\"name\":\"aws_iam\",\"args\":[]}]}",
        },
    ]
}

/// Bodies of each member form, over one, two and three members.
///
/// Source: the member forms GraphQL's SDL grammar gives a type body — a field definition
/// and an input value definition, which NAME A TYPE, and an enum value definition, which
/// is a name alone — which is what the layout rule distinguishes.
fn body_rows() -> Vec<Row> {
    vec![
        Row {
            label: "an enum type holding one value",
            name: "One",
            stated: "enum One { ALPHA }",
            sdl: "enum One\n\n{\nALPHA\n}\n",
            json: "{\"name\":\"One\",\"kind\":\"ENUM\",\"enumValues\":[\
                   {\"name\":\"ALPHA\",\"isDeprecated\":false}],\
                   \"directives\":[]}",
        },
        Row {
            label: "an enum type holding two values",
            name: "Two",
            stated: "enum Two {\n  ALPHA\n  BETA\n}\n",
            sdl: "enum Two\n\n{\nALPHA\n\nBETA\n}\n",
            json: "{\"name\":\"Two\",\"kind\":\"ENUM\",\"enumValues\":[\
                   {\"name\":\"ALPHA\",\"isDeprecated\":false},\
                   {\"name\":\"BETA\",\"isDeprecated\":false}],\
                   \"directives\":[]}",
        },
        Row {
            label: "an enum type holding three values",
            name: "Three",
            stated: "enum Three { ALPHA BETA GAMMA }",
            sdl: "enum Three\n\n{\nALPHA\n\nBETA\n\nGAMMA\n}\n",
            json: "{\"name\":\"Three\",\"kind\":\"ENUM\",\"enumValues\":[\
                   {\"name\":\"ALPHA\",\"isDeprecated\":false},\
                   {\"name\":\"BETA\",\"isDeprecated\":false},\
                   {\"name\":\"GAMMA\",\"isDeprecated\":false}],\
                   \"directives\":[]}",
        },
        Row {
            label: "an object type holding three fields, which name types",
            name: "Fields",
            stated: "type Fields { alpha: ID! beta: String gamma: [Int] }",
            sdl: "type Fields\n\n{\nalpha: ID!\n\n\nbeta: String\n\n\ngamma: [Int]\n\n}\n",
            json: "{\"name\":\"Fields\",\"kind\":\"OBJECT\",\"fields\":[\
                   {\"name\":\"alpha\",\"type\":{\"kind\":\"NON_NULL\",\"ofType\":{\"name\":\"ID\"}},\
                   \"args\":[],\"directives\":[],\"isDeprecated\":false},\
                   {\"name\":\"beta\",\"type\":{\"name\":\"String\"},\"args\":[],\
                   \"directives\":[],\"isDeprecated\":false},\
                   {\"name\":\"gamma\",\"type\":{\"kind\":\"LIST\",\"ofType\":{\"name\":\"Int\"}},\
                   \"args\":[],\"directives\":[],\"isDeprecated\":false}],\
                   \"interfaces\":[],\"directives\":[]}",
        },
        Row {
            label: "an input object type holding two fields, which name types",
            name: "Inputs",
            stated: "input Inputs { alpha: ID! beta: String }",
            sdl: "input Inputs\n\n{\nalpha: ID!\n\n\nbeta: String\n\n}\n",
            json: "{\"name\":\"Inputs\",\"kind\":\"INPUT_OBJECT\",\"inputFields\":[\
                   {\"name\":\"alpha\",\"type\":{\"kind\":\"NON_NULL\",\"ofType\":{\"name\":\"ID\"}}},\
                   {\"name\":\"beta\",\"type\":{\"name\":\"String\"}}],\
                   \"directives\":[]}",
        },
    ]
}

/// The definition a reported `Type` carries.
fn definition_of(reported: &Value) -> &str {
    reported["definition"].as_str().unwrap_or_default()
}

/// Create every row's type under one API, then assert what a read answers in both
/// formats.
async fn assert_rows(rows: Vec<Row>, api_name: &str) {
    let router = router();
    let api_id = create_api(&router, api_name).await;

    // Every row's type is created first, so a definition that references another row's
    // type resolves against the API's own type system when it is read.
    for row in &rows {
        let created = create_type(&router, &api_id, row.stated, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateType 200, got {created:?}",
            row.label
        );
    }

    for row in &rows {
        let sdl = get_type(&router, &api_id, row.name, state::SDL_FORMAT).await;
        assert_eq!(
            sdl.status, 200,
            "case {}: expected GetType 200, got {sdl:?}",
            row.label
        );
        assert_eq!(
            definition_of(&sdl.body["type"]),
            row.sdl,
            "case {}: expected the SDL read to answer {:?}, got {:?}",
            row.label,
            row.sdl,
            definition_of(&sdl.body["type"])
        );

        let json = get_type(&router, &api_id, row.name, state::JSON_FORMAT).await;
        assert_eq!(
            json.status, 200,
            "case {}: expected GetType 200, got {json:?}",
            row.label
        );
        assert_eq!(
            definition_of(&json.body["type"]),
            row.json,
            "case {}: expected the JSON read to answer {:?}, got {:?}",
            row.label,
            row.json,
            definition_of(&json.body["type"])
        );
    }
}

#[tokio::test]
async fn a_read_writes_back_every_directive_the_definition_carries() {
    assert_rows(directive_rows(), "type-directive-rendering").await;
}

#[tokio::test]
async fn a_rendered_body_writes_a_member_that_names_a_type_apart_from_one_that_does_not() {
    assert_rows(body_rows(), "type-body-layout").await;
}

#[tokio::test]
async fn the_rendering_a_read_answers_declares_the_same_directives_again() {
    let router = router();
    let first = create_api(&router, "type-directive-round-trip-first").await;
    let second = create_api(&router, "type-directive-round-trip-second").await;

    // The document a read answers is itself a definition of the same type, directives
    // included: creating from it under another API renders the same document again, so a
    // client can round-trip a type through this service without losing what its schema
    // states about it.
    for row in directive_rows() {
        let created = create_type(&router, &first, row.stated, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateType 200, got {created:?}",
            row.label
        );
        let rendered = definition_of(
            &get_type(&router, &first, row.name, state::SDL_FORMAT)
                .await
                .body["type"],
        )
        .to_string();

        let again = create_type(&router, &second, &rendered, state::SDL_FORMAT).await;
        assert_eq!(
            again.status, 200,
            "case {}: expected the rendered document to be a definition, got {again:?}",
            row.label
        );
        let read = get_type(&router, &second, row.name, state::SDL_FORMAT).await;
        assert_eq!(
            definition_of(&read.body["type"]),
            rendered,
            "case {}: expected the rendering to be stable, got {:?}",
            row.label,
            definition_of(&read.body["type"])
        );
    }
}
