// SPDX-License-Identifier: CC0-1.0

//! What `GetIntrospectionSchema` answers with: the service's own CANONICAL rendering of the
//! type system an API holds, and the media type that payload is labelled with.
//!
//! The rules asserted here, each one level above the single call that exposed it:
//!
//! 1. the SDL a read answers with is PRINTED from the type system the API holds rather than
//!    echoed from the document that installed it, in the service's own canonical layout: a
//!    `schema` definition binding the root operation types the system holds, then the type
//!    definitions grouped by kind and in ascending name order within each group, each
//!    definition's members (fields, input fields, enum values, arguments, interfaces, union
//!    members and directives) in ascending NAME order, a body indented two spaces with one
//!    member per line, and an applied directive's argument written with a space before its
//!    colon. So two documents that declare the SAME type system render to the same bytes
//!    however their authors wrote them, and rendering the answer again changes nothing;
//! 2. `includeDirectives` is what DECLINES the directives the schema states: a call that
//!    asks for none is answered with the type system alone, in either representation, and a
//!    call that names no flag is answered with them;
//! 3. the payload is labelled `application/json`, in both representations the `format`
//!    member can ask for — never the opaque-download media type a blob payload defaults to;
//! 4. the `JSON` representation is a whole GraphQL introspection document: beside the
//!    schema's own types it reports the types GraphQL's introspection system defines
//!    (`__Schema`, `__Type`, `__TypeKind`, `__Field`, `__InputValue`, `__EnumValue`,
//!    `__Directive`, `__DirectiveLocation`) with their own descriptions and fields, the
//!    built-in scalars with the descriptions GraphQL gives them, and the directives the
//!    schema supports — each type reported once, in the order the schema reaches it.
//!
//! Every case goes through the router the production emulator mounts, and the rows span what
//! the rules generalise over: the ORDER an author wrote the definitions and members in, the
//! KINDS a document declares, whether it wrote a `schema` definition and whether its roots
//! carry GraphQL's default names, a definition that carries a description, a member that
//! carries a directive, and a document that defines a directive of its own.

use bytes::Bytes;
use serde_json::Value;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::schema_creation::{await_creation, start_creation};
use crate::source_api_association::{create_api, router, REGION};

/// Every value of the `OutputType` enum — the whole set the required `format` member can
/// take.
///
/// Source: `smithy_query(query="shape", shape="OutputType")`.
const OUTPUT_TYPES: &[&str] = &["SDL", "JSON"];

/// The media type AppSync labels an introspection schema with, whichever representation it
/// is rendered in (recorded behaviour).
const SCHEMA_MEDIA_TYPE: &str = "application/json";

/// One answer to `GetIntrospectionSchema`: the status, the media type the payload is
/// labelled with and the raw bytes, because the schema is the response's whole payload
/// rather than a JSON structure.
#[derive(Debug, Clone)]
struct Served {
    status: u16,
    media_type: String,
    payload: Vec<u8>,
}

impl Served {
    /// The payload as the text the schema was rendered as.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }

    /// The payload as the introspection document it carries.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.payload).unwrap_or(Value::Null)
    }
}

/// Read the introspection schema of `api_id`, keeping the response's own media type.
async fn read_schema(router: &MultiServiceRouter, api_id: &str, query: &str) -> Served {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!(
            "https://appsync.{REGION}.amazonaws.com/v1/apis/{api_id}/schema?{query}"
        ))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR/20260101/{REGION}/appsync/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let media_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Served {
        status,
        media_type,
        payload: response.into_body().to_vec(),
    }
}

/// Create one GraphQL API, install `document` on it and wait for the creation the clock
/// owes to have been performed.
async fn api_holding(router: &MultiServiceRouter, case: &str, document: &str) -> String {
    let api = create_api(router, case, state::GRAPHQL_API_TYPE).await;
    let started = start_creation(router, &api.id, document.as_bytes()).await;
    assert_eq!(
        started.status, 200,
        "case {case:?}: expected StartSchemaCreation 200, got {} {:?}",
        started.status, started.body
    );
    await_creation(router, &api.id, case).await;
    api.id
}

/// One row of the canonical-rendering table: a document, and the SDL the type system it
/// declares is printed as.
#[derive(Debug)]
struct Case {
    label: &'static str,
    /// The document the creation carries.
    document: &'static str,
    /// Another document declaring the SAME type system, written differently — reordered
    /// definitions, reordered members, with or without a `schema` definition — which must
    /// render to the same bytes.
    written_differently: &'static str,
    /// The canonical SDL both of them are printed as.
    canonical: &'static str,
}

/// The rows: one type system whose author wrote nothing in name order, and one that
/// declares every kind SDL can declare so the order the kinds are written in is pinned.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "members-out-of-name-order",
            document: concat!(
                "type Note @aws_api_key @aws_iam {\n",
                "  id: ID!\n",
                "  title: String\n",
                "  legacy: String @deprecated(reason: \"use title\")\n",
                "}\n\n",
                "enum Colour {\n  RED\n  GREEN\n}\n\n",
                "type Query {\n  getNote(id: ID!): Note @aws_iam\n  colours: [Colour]\n}\n",
            ),
            // The same type system, with the definitions in another order, the members in
            // another order, the directives on `Note` the other way round and an explicit
            // `schema` definition the first document leaves out.
            written_differently: concat!(
                "schema {\n  query: Query\n}\n\n",
                "type Query {\n  colours: [Colour]\n  getNote(id: ID!): Note @aws_iam\n}\n\n",
                "type Note @aws_iam @aws_api_key {\n",
                "  legacy: String @deprecated(reason: \"use title\")\n",
                "  title: String\n",
                "  id: ID!\n",
                "}\n\n",
                "enum Colour {\n  GREEN\n  RED\n}\n",
            ),
            canonical: concat!(
                "schema {\n  query: Query\n}\n\n",
                "type Note @aws_api_key @aws_iam {\n",
                "  id: ID!\n",
                "  legacy: String @deprecated(reason : \"use title\")\n",
                "  title: String\n",
                "}\n\n",
                "type Query {\n  colours: [Colour]\n  getNote(id: ID!): Note @aws_iam\n}\n\n",
                "enum Colour {\n  GREEN\n  RED\n}\n",
            ),
        },
        Case {
            label: "every-kind",
            document: concat!(
                "schema {\n  mutation: Mutation\n  query: Query\n}\n\n",
                "type Query {\n  posts: [Post]\n  node(id: ID!): Node\n}\n\n",
                "type Mutation {\n  addPost(input: PostInput!): Post\n}\n\n",
                "interface Node {\n  id: ID!\n}\n\n",
                "type Post implements Node {\n  title: String\n  id: ID!\n}\n\n",
                "union Author = Person | Organisation\n\n",
                "type Person {\n  name: String\n}\n\n",
                "type Organisation {\n  name: String\n}\n\n",
                "enum Status {\n  PUBLISHED\n  DRAFT\n}\n\n",
                "input PostInput {\n  title: String!\n  status: Status\n}\n\n",
                "scalar Money\n",
            ),
            written_differently: concat!(
                "scalar Money\n\n",
                "input PostInput {\n  status: Status\n  title: String!\n}\n\n",
                "enum Status {\n  DRAFT\n  PUBLISHED\n}\n\n",
                "union Author = Organisation | Person\n\n",
                "type Organisation {\n  name: String\n}\n\n",
                "type Person {\n  name: String\n}\n\n",
                "type Post implements Node {\n  id: ID!\n  title: String\n}\n\n",
                "interface Node {\n  id: ID!\n}\n\n",
                "type Mutation {\n  addPost(input: PostInput!): Post\n}\n\n",
                "type Query {\n  node(id: ID!): Node\n  posts: [Post]\n}\n",
            ),
            canonical: concat!(
                "schema {\n  query: Query\n  mutation: Mutation\n}\n\n",
                "interface Node {\n  id: ID!\n}\n\n",
                "union Author = Organisation | Person\n\n",
                "type Mutation {\n  addPost(input: PostInput!): Post\n}\n\n",
                "type Organisation {\n  name: String\n}\n\n",
                "type Person {\n  name: String\n}\n\n",
                "type Post implements Node {\n  id: ID!\n  title: String\n}\n\n",
                "type Query {\n  node(id: ID!): Node\n  posts: [Post]\n}\n\n",
                "enum Status {\n  DRAFT\n  PUBLISHED\n}\n\n",
                "scalar Money\n\n",
                "input PostInput {\n  status: Status\n  title: String!\n}\n",
            ),
        },
    ]
}

/// RULE 1: the SDL is the canonical printing of the type system the API holds, so the
/// document an author uploaded does not show through it — and two documents declaring one
/// type system are answered with one document.
#[tokio::test]
async fn the_sdl_is_the_canonical_printing_of_the_type_system() {
    let router = router();
    for case in cases() {
        let api_id = api_holding(&router, case.label, case.document).await;
        let served = read_schema(&router, &api_id, "format=SDL").await;
        assert_eq!(
            served.status, 200,
            "case {:?}: expected HTTP 200, got {} with {:?}",
            case.label,
            served.status,
            served.text()
        );
        assert_eq!(
            served.text(),
            case.canonical,
            "case {:?}: expected the canonical schema {:?}, got {:?}",
            case.label,
            case.canonical,
            served.text()
        );

        // The same type system written differently renders to the same bytes...
        let other = api_holding(
            &router,
            &format!("{}-rewritten", case.label),
            case.written_differently,
        )
        .await;
        let rewritten = read_schema(&router, &other, "format=SDL").await;
        assert_eq!(
            rewritten.text(),
            case.canonical,
            "case {:?} rewritten: expected the same canonical schema {:?}, got {:?}",
            case.label,
            case.canonical,
            rewritten.text()
        );

        // ...and the answer is itself a schema document that installs the same type system,
        // so printing it again changes nothing.
        let round_trip = api_holding(
            &router,
            &format!("{}-round-trip", case.label),
            case.canonical,
        )
        .await;
        let again = read_schema(&router, &round_trip, "format=SDL").await;
        assert_eq!(
            again.text(),
            case.canonical,
            "case {:?} round trip: expected the rendering to be stable, got {:?}",
            case.label,
            again.text()
        );
    }
}

/// RULES 1 and 2, over a description and a directive definition: a described definition
/// carries its description, the schema's own directive definition is part of the document,
/// and a call that DECLINES the schema's directives is answered with the type system alone.
#[tokio::test]
async fn the_canonical_printing_states_descriptions_and_directive_definitions() {
    let router = router();
    let document = concat!(
        "directive @auth(level: String! = \"owner\") repeatable on FIELD_DEFINITION\n\n",
        "\"The queries this API serves.\"\n",
        "type Query {\n",
        "  \"The greeting.\"\n",
        "  hello: String @auth(level: \"anyone\")\n",
        "}\n",
    );
    let case = "described-and-directive-defining";
    let api_id = api_holding(&router, case, document).await;

    // The directives are part of the schema a call that does not decline them reads — the
    // definition the document writes and the directive written on the field — and a call
    // that names no flag is such a call.
    let with_directives = concat!(
        "schema {\n  query: Query\n}\n\n",
        "directive @auth(level: String! = \"owner\") repeatable on FIELD_DEFINITION\n\n",
        "\"The queries this API serves.\"\n",
        "type Query {\n",
        "  \"The greeting.\"\n",
        "  hello: String @auth(level : \"anyone\")\n",
        "}\n",
    );
    for query in ["format=SDL", "format=SDL&includeDirectives=true"] {
        let asked = read_schema(&router, &api_id, query).await;
        assert_eq!(
            asked.text(),
            with_directives,
            "case {case:?} {query:?}: expected {with_directives:?}, got {:?}",
            asked.text()
        );
    }

    // A call that DECLINES them is answered with the type system alone: no definition, and
    // no directive on any member — while the descriptions, which are no directive, stay.
    let declined = read_schema(&router, &api_id, "format=SDL&includeDirectives=false").await;
    let without_directives = concat!(
        "schema {\n  query: Query\n}\n\n",
        "\"The queries this API serves.\"\n",
        "type Query {\n",
        "  \"The greeting.\"\n",
        "  hello: String\n",
        "}\n",
    );
    assert_eq!(
        declined.text(),
        without_directives,
        "case {case:?} declining directives: expected {without_directives:?}, got {:?}",
        declined.text()
    );
}

/// RULE 3: the payload is labelled with the media type AppSync labels a schema with,
/// whichever representation the call asks for.
#[tokio::test]
async fn the_schema_payload_is_labelled_as_json() {
    let router = router();
    let api_id = api_holding(
        &router,
        "media-type",
        "type Query {\n  hello: String\n}\n",
    )
    .await;
    for format in OUTPUT_TYPES {
        for flag in ["", "&includeDirectives=true"] {
            let served = read_schema(&router, &api_id, &format!("format={format}{flag}")).await;
            assert_eq!(
                (served.status, served.media_type.as_str()),
                (200, SCHEMA_MEDIA_TYPE),
                "case \"{format}{flag}\": expected 200 labelled {SCHEMA_MEDIA_TYPE:?}, got {} \
                 labelled {:?}",
                served.status,
                served.media_type
            );
        }
    }
}

/// RULE 4: the `JSON` representation is a whole introspection document — the schema's own
/// types, the types the introspection system defines, the built-in scalars it references
/// with the descriptions GraphQL gives them, and the directives the schema supports.
#[tokio::test]
async fn the_json_representation_is_a_whole_introspection_document() {
    let router = router();
    let case = "introspection-document";
    let api_id = api_holding(&router, case, cases()[0].document).await;
    let served = read_schema(&router, &api_id, "format=JSON&includeDirectives=true").await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: expected HTTP 200, got {} with {:?}",
        served.status,
        served.text()
    );
    let document = served.json();
    let schema = &document["data"]["__schema"];
    let types = schema["types"].as_array().cloned().unwrap_or_default();

    // Every type the schema has is reported once, in the order the schema reaches it: from
    // its root operation type outwards, then the introspection system's own `__Schema` and
    // everything that names.
    let reported: Vec<&str> = types
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    let expected = vec![
        "Query",
        "Note",
        "ID",
        "String",
        "Colour",
        "__Schema",
        "__Type",
        "__TypeKind",
        "__Field",
        "__InputValue",
        "Boolean",
        "__EnumValue",
        "__Directive",
        "__DirectiveLocation",
    ];
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the types {expected:?}, got {reported:?}"
    );

    // The built-in scalars carry the descriptions GraphQL gives them, and a type of the
    // schema carries the description its own definition states (none here).
    for (name, described) in [
        ("ID", Some("Built-in ID")),
        ("String", Some("Built-in String")),
        ("Boolean", Some("Built-in Boolean")),
        ("Note", None),
    ] {
        let entry = named(&types, name);
        assert_eq!(
            entry["description"].as_str(),
            described,
            "case {case:?}: expected {name:?} described {described:?}, got {entry}"
        );
        assert_eq!(
            entry["kind"].as_str(),
            Some(if name == "Note" { "OBJECT" } else { "SCALAR" }),
            "case {case:?}: expected the kind of {name:?}, got {entry}"
        );
    }

    // The introspection system's own types are reported as GraphQL defines them: `__Schema`
    // with its description and its five fields, and `__TypeKind` with the kinds it names.
    let meta = named(&types, "__Schema");
    assert_eq!(
        meta["description"].as_str(),
        Some(
            "A GraphQL Introspection defines the capabilities of a GraphQL server. It exposes \
             all available types and directives on the server, the entry points for query, \
             mutation, and subscription operations."
        ),
        "case {case:?}: expected the introspection system's description of __Schema, got {meta}"
    );
    let members: Vec<&str> = meta["fields"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| field["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        members,
        vec![
            "types",
            "queryType",
            "mutationType",
            "directives",
            "subscriptionType"
        ],
        "case {case:?}: expected __Schema's own fields, got {members:?}"
    );
    assert_eq!(
        meta["fields"][0]["type"]["kind"].as_str(),
        Some("NON_NULL"),
        "case {case:?}: expected __Schema.types to be a non-null list, got {}",
        meta["fields"][0]["type"]
    );
    assert_eq!(
        meta["fields"][0]["type"]["ofType"]["ofType"]["ofType"]["name"].as_str(),
        Some("__Type"),
        "case {case:?}: expected __Schema.types to resolve to __Type, got {}",
        meta["fields"][0]["type"]
    );
    let kinds = named(&types, "__TypeKind");
    let named_kinds: Vec<&str> = kinds["enumValues"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        named_kinds,
        vec![
            "SCALAR",
            "OBJECT",
            "INTERFACE",
            "UNION",
            "ENUM",
            "INPUT_OBJECT",
            "LIST",
            "NON_NULL"
        ],
        "case {case:?}: expected the kinds __TypeKind names, got {named_kinds:?}"
    );

    // The directives the schema supports are reported: GraphQL's own, which every server
    // provides, beside any the document defines — the fixture's own schema defines none.
    let directives: Vec<&str> = schema["directives"]
        .as_array()
        .map(|reported| {
            reported
                .iter()
                .filter_map(|directive| directive["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    for supported in ["skip", "include", "deprecated"] {
        assert!(
            directives.contains(&supported),
            "case {case:?}: expected the directive {supported:?} to be reported, got \
             {directives:?}"
        );
    }
    let deprecated = schema["directives"]
        .as_array()
        .and_then(|reported| {
            reported
                .iter()
                .find(|directive| directive["name"].as_str() == Some("deprecated"))
        })
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        (
            deprecated["locations"][0].as_str(),
            deprecated["args"][0]["name"].as_str(),
            deprecated["args"][0]["defaultValue"].as_str(),
        ),
        (
            Some("FIELD_DEFINITION"),
            Some("reason"),
            Some("\"No longer supported\""),
        ),
        "case {case:?}: expected @deprecated as GraphQL declares it, got {deprecated}"
    );

    // A schema that defines a directive of its own reports that one too, as its own
    // definition declares it.
    let defining = api_holding(
        &router,
        "defines-a-directive",
        "directive @auth(level: String!) on FIELD_DEFINITION\n\ntype Query {\n  secret: String\n}\n",
    )
    .await;
    let own = read_schema(&router, &defining, "format=JSON&includeDirectives=true").await;
    let answered = own.json();
    let reported: Vec<&str> = answered["data"]["__schema"]["directives"]
        .as_array()
        .map(|directives| {
            directives
                .iter()
                .filter_map(|directive| directive["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        reported.contains(&"auth") && reported.contains(&"skip"),
        "case \"defines-a-directive\": expected the schema's own directive beside the ones \
         every schema holds, got {reported:?}"
    );

    // A call that DECLINES the schema's directives is answered with none.
    let declined = read_schema(&router, &api_id, "format=JSON&includeDirectives=false").await;
    assert_eq!(
        declined.json()["data"]["__schema"]["directives"],
        Value::Null,
        "case {case:?} declining directives: expected no directives, got {}",
        declined.json()["data"]["__schema"]["directives"]
    );
}

/// The entry of `types` that reports the type named `name`, or `Value::Null` where none
/// does.
fn named(types: &[Value], name: &str) -> Value {
    types
        .iter()
        .find(|entry| entry["name"].as_str() == Some(name))
        .cloned()
        .unwrap_or(Value::Null)
}
