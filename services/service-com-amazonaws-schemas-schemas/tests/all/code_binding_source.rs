// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `GetCodeBindingSource`.
//!
//! RULE under test: the source of a code binding is the PACKAGE of source files the
//! service generated for that binding — a declaration per type the binding's schema
//! version declares, in the language the binding was generated for, laid out as a
//! package of that language, together with what that language needs to BUILD those
//! declarations and to read and write the documents they describe (its build descriptor
//! and its (de)serialization support) — served as one ZIP archive and computed from the
//! STORED document of that version, so it is the same archive however often it is read;
//! and a binding whose generation has produced nothing yet (one no put started, or one
//! still in progress) has no source, which is the service's `NotFoundException`
//! (HTTP 404).
//!
//! The mapping the generated declarations are asserted against is this emulator's, and
//! it is stated once — in the generator, `src/ops/get_code_binding_source.rs` — and
//! pinned here per member: a JSON Schema `string` becomes the language's string type, a
//! `string` with `format: date-time` its date-time type, `integer` and `number` its two
//! number types, `boolean` its boolean, an `array` its list of what the items declare, a
//! `$ref` to a named schema of the document that schema's own declaration, and anything
//! else (an object with no members of its own, a declaration with no `type`) the
//! language's unconstrained type. A member the document's `required` does not name is
//! optional, which each language says in its own way.
//!
//! The lifecycle of the binding itself is the sibling suite
//! [`crate::code_binding_lifecycle`], and which `Language` values reach a handler at all
//! is [`crate::code_bindings`]. Every case here drives the real wire path: a signed
//! `restJson1` request through `MultiServiceRouter::dispatch`, on the harness in
//! [`crate::registry_schema_lifecycle`].

use std::io::{Cursor, Read};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_schemas_schemas::state::{
    code_binding_language_pattern, AWS_MANAGED_REGISTRY, CODE_BINDING_LANGUAGES,
    CODE_BINDING_LANGUAGE_PARAMETER, CODE_GENERATION_DURATION,
};

use crate::registry_schema_lifecycle::{create_registry, schema_uri, Answer, Emulator};

/// The registry every case below creates its schemas in.
const REGISTRY: &str = "code-binding-source-registry";

/// `GetCodeBindingSource` lives one segment below the two other code-binding
/// operations, with the same optional `schemaVersion` query member.
fn source_uri(registry: &str, schema: &str, language: &str, version: Option<&str>) -> String {
    let base = format!("{}/language/{language}/source", schema_uri(registry, schema));
    match version {
        Some(version) => format!("{base}?schemaVersion={version}"),
        None => base,
    }
}

/// `PutCodeBinding` / `DescribeCodeBinding`, which is what brings a binding into
/// existence and reports it.
fn binding_uri(registry: &str, schema: &str, language: &str, version: Option<&str>) -> String {
    let base = format!("{}/language/{language}", schema_uri(registry, schema));
    match version {
        Some(version) => format!("{base}?schemaVersion={version}"),
        None => base,
    }
}

/// The OpenAPI 3 document the main table generates from: an event envelope that
/// requires two of its members, references a nested schema and a list of an aliased
/// scalar, and a nested schema carrying one member of every JSON Schema type the
/// mapping covers.
const ORDER_DOCUMENT: &str = r##"{
  "openapi": "3.0.0",
  "info": {"title": "Order Placed", "version": "1.0.0"},
  "paths": {},
  "components": {"schemas": {
    "AWSEvent": {
      "type": "object",
      "required": ["detail-type", "detail"],
      "properties": {
        "detail-type": {"type": "string"},
        "account": {"type": "string"},
        "time": {"type": "string", "format": "date-time"},
        "detail": {"$ref": "#/components/schemas/Order"},
        "resources": {"type": "array", "items": {"$ref": "#/components/schemas/Resource"}}
      }
    },
    "Order": {
      "type": "object",
      "required": ["id"],
      "properties": {
        "id": {"type": "string"},
        "quantity": {"type": "integer"},
        "price": {"type": "number"},
        "paid": {"type": "boolean"},
        "tags": {"type": "array", "items": {"type": "string"}},
        "metadata": {"type": "object"},
        "placedAt": {"type": "string", "format": "date-time"}
      }
    },
    "Resource": {"type": "string"}
  }}
}"##;

/// The JSON Schema draft-04 document the table also generates from: the document is
/// itself the root schema, `definitions` holds the other one, and a member's type is
/// stated as draft-04's nullable union.
const SHIPMENT_DOCUMENT: &str = r##"{
  "$schema": "http://json-schema.org/draft-04/schema#",
  "type": "object",
  "required": ["shippingAddress"],
  "properties": {
    "shippingAddress": {"$ref": "#/definitions/ShippingAddress"},
    "deliveredAt": {"type": ["string", "null"], "format": "date-time"}
  },
  "definitions": {
    "ShippingAddress": {
      "type": "object",
      "properties": {"postalCode": {"type": "string"}, "floor": {"type": "integer"}}
    }
  }
}"##;

/// A document that declares no named schema at all — the one the sibling code-binding
/// suites create their schemas with.
const BARE_DOCUMENT: &str = r#"{"openapi": "3.0.0"}"#;

/// One schema the table generates bindings from.
struct SchemaCase {
    name: &'static str,
    document: &'static str,
    schema_type: &'static str,
}

/// The schemas the rows below work on: an OpenAPI 3 document under a dotted name, a
/// draft-04 document under a hyphenated one, and a document with nothing in it.
fn schema_cases() -> Vec<SchemaCase> {
    vec![
        SchemaCase {
            name: "order.placed",
            document: ORDER_DOCUMENT,
            schema_type: "OpenApi3",
        },
        SchemaCase {
            name: "shipment-events",
            document: SHIPMENT_DOCUMENT,
            schema_type: "JSONSchemaDraft4",
        },
        SchemaCase {
            name: "bare-schema",
            document: BARE_DOCUMENT,
            schema_type: "OpenApi3",
        },
    ]
}

/// An emulator with the registry and every schema of [`schema_cases`] created.
async fn emulator_with_schemas() -> Emulator {
    let emulator = Emulator::new();
    create_registry(&emulator, REGISTRY).await;
    for case in schema_cases() {
        let created = emulator
            .call(
                "POST",
                &schema_uri(REGISTRY, case.name),
                Some(json!({ "Content": case.document, "Type": case.schema_type })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected 201 from CreateSchema, got {} {:?}",
            case.name, created.status, created.body
        );
    }
    emulator
}

/// Start a binding and wait out the generation span the service gives it, so there is
/// source to read. Answers what the put reported.
async fn generated(emulator: &Emulator, uri: &str, case: &str) -> Answer {
    let put = emulator.call("POST", uri, None).await;
    assert_eq!(
        put.status, 202,
        "case {case:?}: expected 202 from PutCodeBinding, got {} {:?}",
        put.status, put.body
    );
    // Nothing polls generation to completion: the due instant was fixed at the put, so
    // waiting past it is all that is needed.
    tokio::time::sleep(CODE_GENERATION_DURATION + Duration::from_millis(50)).await;
    put
}

/// The archive one answer carries, as `(path, text)` per entry in the archive's own
/// order.
fn archive_entries(case: &str, answer: &Answer) -> Vec<(String, String)> {
    let mut archive = ::zip::ZipArchive::new(Cursor::new(answer.raw.to_vec())).unwrap_or_else(
        |error| {
            panic!(
                "case {case:?}: the body must be a ZIP archive ({error}), got {} bytes",
                answer.raw.len()
            )
        },
    );
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .unwrap_or_else(|error| panic!("case {case:?}: entry {index} must read ({error})"));
        let path = entry
            .name()
            .unwrap_or_else(|error| panic!("case {case:?}: entry {index} must be named ({error})"))
            .to_string();
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .unwrap_or_else(|error| panic!("case {case:?}: {path} must be UTF-8 text ({error})"));
        entries.push((path, text));
    }
    entries
}

/// Read one binding's source, asserting it was served.
async fn source(emulator: &Emulator, uri: &str, case: &str) -> Answer {
    let answer = emulator.call_payload("GET", uri).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from GetCodeBindingSource, got {} {:?}",
        answer.status, answer.body
    );
    answer
}

/// The package a schema's generated files live in, below the language's source root:
/// the one root every binding shares and the schema name's own alphanumeric runs,
/// lower-cased (`src/ops/get_code_binding_source.rs`, `package_segments`).
pub(crate) fn package_path(schema: &str) -> String {
    let mut segments = vec!["schema".to_string()];
    segments.extend(
        schema
            .split(|character: char| !character.is_alphanumeric())
            .filter(|part| !part.is_empty())
            .map(str::to_lowercase),
    );
    segments.join("/")
}

/// Every entry a generated package carries BESIDE its declarations, for `language` and
/// the schema `schema`: the descriptor(s) the language's own build reads, at the archive
/// root where that build looks for them, and the support source the declared types are
/// read and written through. The generator states this set once
/// (`src/ops/get_code_binding_source.rs`, `build_files` / `support_files`); it is
/// restated here so a case can tell a declaration from the package around it.
pub(crate) fn package_entries(language: &str, schema: &str) -> Vec<String> {
    let package = package_path(schema);
    match language {
        "Java8" => vec![
            "pom.xml".to_string(),
            format!("src/main/java/{package}/Marshaller.java"),
        ],
        "TypeScript3" => vec![
            "package.json".to_string(),
            "tsconfig.json".to_string(),
            format!("src/{package}/index.ts"),
            format!("src/{package}/Marshaller.ts"),
        ],
        "Python36" => {
            let mut entries = vec!["setup.py".to_string(), "requirements.txt".to_string()];
            let segments: Vec<&str> = package.split('/').collect();
            // Every level of the package is a Python package of its own.
            for level in 1..=segments.len() {
                entries.push(format!("src/{}/__init__.py", segments[..level].join("/")));
            }
            entries.push(format!("src/{package}/marshaller.py"));
            entries
        }
        "Go1" => vec!["go.mod".to_string(), format!("{package}/marshaller.go")],
        other => panic!("no package layout known for language {other:?}"),
    }
}

/// The entries of `paths` that are DECLARATIONS: everything the package itself is not.
pub(crate) fn declaration_paths(language: &str, schema: &str, paths: &[String]) -> Vec<String> {
    let package = package_entries(language, schema);
    paths
        .iter()
        .filter(|path| !package.contains(path))
        .cloned()
        .collect()
}

/// One (schema, language) row of the main table: the files the archive must hold, and
/// the declarations those files must carry.
struct SourceCase {
    case: &'static str,
    schema: &'static str,
    language: &'static str,
    /// Every entry the archive holds, by path — the package layout of that language.
    files: &'static [&'static str],
    /// Fragments the archive's text must carry: one per member of the document, as the
    /// language declares it. See the module docs for the mapping they pin.
    declarations: &'static [&'static str],
}

/// The table: the two documents above, each generated in every language the service
/// generates for, so the rows differ along the language, the document's own language
/// (OpenAPI 3 against draft-04), the shape of the schema name, the JSON type of each
/// member, whether the document requires it, and whether it is a scalar, a list, a
/// reference or free-form.
fn source_cases() -> Vec<SourceCase> {
    vec![
        SourceCase {
            case: "openapi-java",
            schema: "order.placed",
            language: "Java8",
            files: &[
                "pom.xml",
                "src/main/java/schema/order/placed/AWSEvent.java",
                "src/main/java/schema/order/placed/Order.java",
                "src/main/java/schema/order/placed/Marshaller.java",
            ],
            declarations: &[
                "package schema.order.placed;",
                "public class AWSEvent {",
                "private String detailType;",
                "private String account;",
                "private java.time.OffsetDateTime time;",
                "private Order detail;",
                "private java.util.List<String> resources;",
                "public Order getDetail() {",
                "public void setDetail(final Order detail) {",
                "public class Order {",
                "private String id;",
                "private Long quantity;",
                "private Double price;",
                "private Boolean paid;",
                "private java.util.List<String> tags;",
                "private Object metadata;",
                "private java.time.OffsetDateTime placedAt;",
            ],
        },
        SourceCase {
            case: "openapi-typescript",
            schema: "order.placed",
            language: "TypeScript3",
            files: &[
                "package.json",
                "tsconfig.json",
                "src/schema/order/placed/AWSEvent.ts",
                "src/schema/order/placed/Order.ts",
                "src/schema/order/placed/index.ts",
                "src/schema/order/placed/Marshaller.ts",
            ],
            declarations: &[
                "import { Order } from \"./Order\";",
                "export interface AWSEvent {",
                "\"detail-type\": string;",
                "account?: string;",
                "time?: Date;",
                "detail: Order;",
                "resources?: Array<string>;",
                "export interface Order {",
                "id: string;",
                "quantity?: number;",
                "price?: number;",
                "paid?: boolean;",
                "tags?: Array<string>;",
                "metadata?: any;",
                "placedAt?: Date;",
            ],
        },
        SourceCase {
            case: "openapi-python",
            schema: "order.placed",
            language: "Python36",
            files: &[
                "setup.py",
                "requirements.txt",
                "src/schema/order/placed/awsevent.py",
                "src/schema/order/placed/order.py",
                "src/schema/__init__.py",
                "src/schema/order/__init__.py",
                "src/schema/order/placed/__init__.py",
                "src/schema/order/placed/marshaller.py",
            ],
            declarations: &[
                "from datetime import datetime",
                "from typing import List, Optional",
                "from .order import Order",
                "class AWSEvent:",
                "detail_type: str",
                "account: Optional[str]",
                "time: Optional[datetime]",
                "detail: Order",
                "resources: Optional[List[str]]",
                "class Order:",
                "id: str",
                "quantity: Optional[int]",
                "price: Optional[float]",
                "paid: Optional[bool]",
                "tags: Optional[List[str]]",
                "metadata: Optional[Any]",
                "placed_at: Optional[datetime]",
            ],
        },
        SourceCase {
            case: "openapi-go",
            schema: "order.placed",
            language: "Go1",
            files: &[
                "go.mod",
                "schema/order/placed/awsevent.go",
                "schema/order/placed/order.go",
                "schema/order/placed/marshaller.go",
            ],
            declarations: &[
                "package placed",
                "import \"time\"",
                "type AWSEvent struct {",
                "DetailType string `json:\"detail-type\"`",
                "Account string `json:\"account,omitempty\"`",
                "Time time.Time `json:\"time,omitempty\"`",
                "Detail Order `json:\"detail\"`",
                "Resources []string `json:\"resources,omitempty\"`",
                "type Order struct {",
                "Id string `json:\"id\"`",
                "Quantity int64 `json:\"quantity,omitempty\"`",
                "Price float64 `json:\"price,omitempty\"`",
                "Paid bool `json:\"paid,omitempty\"`",
                "Tags []string `json:\"tags,omitempty\"`",
                "Metadata interface{} `json:\"metadata,omitempty\"`",
                "PlacedAt time.Time `json:\"placedAt,omitempty\"`",
            ],
        },
        SourceCase {
            case: "draft4-java",
            schema: "shipment-events",
            language: "Java8",
            files: &[
                "pom.xml",
                "src/main/java/schema/shipment/events/ShipmentEvents.java",
                "src/main/java/schema/shipment/events/ShippingAddress.java",
                "src/main/java/schema/shipment/events/Marshaller.java",
            ],
            declarations: &[
                "package schema.shipment.events;",
                "public class ShipmentEvents {",
                "private ShippingAddress shippingAddress;",
                "private java.time.OffsetDateTime deliveredAt;",
                "public class ShippingAddress {",
                "private String postalCode;",
                "private Long floor;",
            ],
        },
        SourceCase {
            case: "draft4-typescript",
            schema: "shipment-events",
            language: "TypeScript3",
            files: &[
                "package.json",
                "tsconfig.json",
                "src/schema/shipment/events/ShipmentEvents.ts",
                "src/schema/shipment/events/ShippingAddress.ts",
                "src/schema/shipment/events/index.ts",
                "src/schema/shipment/events/Marshaller.ts",
            ],
            declarations: &[
                "import { ShippingAddress } from \"./ShippingAddress\";",
                "export interface ShipmentEvents {",
                "shippingAddress: ShippingAddress;",
                "deliveredAt?: Date;",
                "export interface ShippingAddress {",
                "postalCode?: string;",
                "floor?: number;",
            ],
        },
        SourceCase {
            case: "draft4-python",
            schema: "shipment-events",
            language: "Python36",
            files: &[
                "setup.py",
                "requirements.txt",
                "src/schema/shipment/events/shipment_events.py",
                "src/schema/shipment/events/shipping_address.py",
                "src/schema/__init__.py",
                "src/schema/shipment/__init__.py",
                "src/schema/shipment/events/__init__.py",
                "src/schema/shipment/events/marshaller.py",
            ],
            declarations: &[
                "from .shipping_address import ShippingAddress",
                "class ShipmentEvents:",
                "shipping_address: ShippingAddress",
                "delivered_at: Optional[datetime]",
                "class ShippingAddress:",
                "postal_code: Optional[str]",
                "floor: Optional[int]",
            ],
        },
        SourceCase {
            case: "draft4-go",
            schema: "shipment-events",
            language: "Go1",
            files: &[
                "go.mod",
                "schema/shipment/events/shipment_events.go",
                "schema/shipment/events/shipping_address.go",
                "schema/shipment/events/marshaller.go",
            ],
            declarations: &[
                "package events",
                "type ShipmentEvents struct {",
                "ShippingAddress ShippingAddress `json:\"shippingAddress\"`",
                "DeliveredAt time.Time `json:\"deliveredAt,omitempty\"`",
                "type ShippingAddress struct {",
                "PostalCode string `json:\"postalCode,omitempty\"`",
                "Floor int64 `json:\"floor,omitempty\"`",
            ],
        },
    ]
}

/// RULE: the archive holds one source file per named schema the document declares, in
/// the package layout of the language asked for, and each file declares that schema's
/// own members with the type the mapping gives them — beside the files that package
/// is built and (de)serialized through, which [`the_package_builds_and_reads_every_declared_type`]
/// is about.
#[tokio::test]
async fn the_source_is_the_package_generated_from_the_stored_document() {
    let emulator = emulator_with_schemas().await;

    for case in source_cases() {
        let label = case.case;
        let uri = source_uri(REGISTRY, case.schema, case.language, None);
        generated(
            &emulator,
            &binding_uri(REGISTRY, case.schema, case.language, None),
            label,
        )
        .await;

        let answer = source(&emulator, &uri, label).await;
        let entries = archive_entries(label, &answer);
        let paths: Vec<&str> = entries.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths, case.files,
            "case {label:?}: expected the package's files, got {paths:?}"
        );
        let text: String = entries
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        for declaration in case.declarations {
            assert!(
                text.contains(declaration),
                "case {label:?}: the generated source must declare {declaration:?}, got {text}"
            );
        }
    }
}

/// One language's package around its declarations: the descriptor its own build reads,
/// the entry its (de)serialization lives in, and how that entry names the reader of one
/// declared type. `{artifact}`, `{package}` and `{type}` stand for the package's
/// artifact name, the schema's package path and a declared type.
struct PackageCase {
    language: &'static str,
    descriptor: &'static str,
    names_artifact: &'static str,
    marshaller: &'static str,
    reads_type: &'static str,
}

/// One row per language the service generates for: what a package of that language is
/// built and (de)serialized by (`src/ops/get_code_binding_source.rs`, `build_files` /
/// `support_files`).
fn package_cases() -> Vec<PackageCase> {
    vec![
        PackageCase {
            language: "Java8",
            descriptor: "pom.xml",
            names_artifact: "<artifactId>{artifact}</artifactId>",
            marshaller: "src/main/java/{package}/Marshaller.java",
            reads_type: "public static {type} unmarshal{type}(final String document)",
        },
        PackageCase {
            language: "TypeScript3",
            descriptor: "package.json",
            names_artifact: "\"name\": \"{artifact}\"",
            marshaller: "src/{package}/Marshaller.ts",
            reads_type: "export function unmarshal{type}(document: string): {type}",
        },
        PackageCase {
            language: "Python36",
            descriptor: "setup.py",
            names_artifact: "name=\"{artifact}\"",
            marshaller: "src/{package}/marshaller.py",
            reads_type: "    {type}: {",
        },
        PackageCase {
            language: "Go1",
            descriptor: "go.mod",
            names_artifact: "module {artifact}",
            marshaller: "{package}/marshaller.go",
            reads_type: "func Unmarshal{type}(document []byte) ({type}, error)",
        },
    ]
}

/// The artifact name a schema's generated package is published as: the schema name's
/// own alphanumeric runs, lower-cased and hyphen-joined
/// (`src/ops/get_code_binding_source.rs`, `artifact_name`).
fn artifact_name(schema: &str) -> String {
    schema
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<String>>()
        .join("-")
}

/// RULE: a code binding's source is a PACKAGE, not a bag of declarations — so beside a
/// declaration per implied type the archive carries what that language needs to build
/// the declarations and to read and write the documents they describe, generated from
/// the same document: the build descriptor at the archive root, naming the schema's own
/// artifact, and the (de)serialization support, which knows every declared type.
///
/// The rows differ along the language, the document's own language (OpenAPI 3 against
/// draft-04), the shape of the schema name and the number and names of the types the
/// document declares.
#[tokio::test]
async fn the_package_builds_and_reads_every_declared_type() {
    let emulator = emulator_with_schemas().await;

    // The two documents of the main table, with the types each declares.
    for (schema, declared) in [
        ("order.placed", ["AWSEvent", "Order"]),
        ("shipment-events", ["ShipmentEvents", "ShippingAddress"]),
    ] {
        for case in package_cases() {
            let label = format!("{schema}/{}", case.language);
            let artifact = artifact_name(schema);
            let package = package_path(schema);
            generated(
                &emulator,
                &binding_uri(REGISTRY, schema, case.language, None),
                &label,
            )
            .await;
            let answer = source(
                &emulator,
                &source_uri(REGISTRY, schema, case.language, None),
                &label,
            )
            .await;
            let entries = archive_entries(&label, &answer);
            let paths: Vec<String> = entries.iter().map(|(path, _)| path.clone()).collect();

            // Every file of the package is there, beside the declarations.
            for entry in package_entries(case.language, schema) {
                assert!(
                    paths.contains(&entry),
                    "case {label:?}: the package must carry {entry:?}, got {paths:?}"
                );
            }
            let declarations = declaration_paths(case.language, schema, &paths);
            assert_eq!(
                declarations.len(),
                declared.len(),
                "case {label:?}: one declaration per declared type {declared:?}, got \
                 {declarations:?}"
            );
            assert!(
                paths.len() > declarations.len(),
                "case {label:?}: a package is more than its declarations, got {paths:?}"
            );

            let text_of = |wanted: &str| -> String {
                entries
                    .iter()
                    .find(|(path, _)| path == wanted)
                    .map(|(_, text)| text.clone())
                    .unwrap_or_else(|| {
                        panic!("case {label:?}: {wanted:?} must be an entry, got {paths:?}")
                    })
            };

            // The build descriptor sits at the archive root, which is where that
            // language's build looks for it, and names the schema's own artifact.
            assert!(
                !case.descriptor.contains('/'),
                "case {label:?}: the build descriptor must sit at the archive root, got {:?}",
                case.descriptor
            );
            let descriptor = text_of(case.descriptor);
            let names_artifact = case.names_artifact.replace("{artifact}", &artifact);
            assert!(
                descriptor.contains(&names_artifact),
                "case {label:?}: the build descriptor must name the artifact \
                 {names_artifact:?}, got {descriptor}"
            );

            // The (de)serialization support knows every type the package declares.
            let marshaller = text_of(&case.marshaller.replace("{package}", &package));
            for type_name in declared {
                let reads = case.reads_type.replace("{type}", type_name);
                assert!(
                    marshaller.contains(&reads),
                    "case {label:?}: the package's (de)serialization must read \
                     {type_name:?} ({reads:?}), got {marshaller}"
                );
            }
        }
    }
}

/// RULE: what the package's (de)serialization says about a member is read from the
/// document — the name the member carries ON THE WIRE, which is not the name the
/// language declares it under, and whether the document declares it as an instant, as
/// another declared type or as a list of values.
///
/// The rows are the languages whose JSON reader needs to be told: one per kind of
/// per-member knowledge the document is the only source of.
#[tokio::test]
async fn the_marshalling_carries_what_the_document_says_about_each_member() {
    let emulator = emulator_with_schemas().await;

    for (schema, language, marshaller, fragments) in [
        (
            "order.placed",
            "Python36",
            "src/schema/order/placed/marshaller.py",
            &[
                // A member whose wire name is no identifier of the language.
                "\"detail_type\": (\"detail-type\", None, False),",
                // A member holding another declared type.
                "\"detail\": (\"detail\", Order, False),",
                // A member holding a list.
                "\"tags\": (\"tags\", None, True),",
                // A member the document declares as an instant.
                "\"placed_at\": (\"placedAt\", datetime, False),",
            ][..],
        ),
        (
            "order.placed",
            "TypeScript3",
            "src/schema/order/placed/Marshaller.ts",
            &["const instants: ReadonlyArray<string> = [\"time\", \"placedAt\"];"][..],
        ),
        (
            "shipment-events",
            "Python36",
            "src/schema/shipment/events/marshaller.py",
            &[
                "\"shipping_address\": (\"shippingAddress\", ShippingAddress, False),",
                "\"delivered_at\": (\"deliveredAt\", datetime, False),",
            ][..],
        ),
        (
            "shipment-events",
            "Java8",
            "src/main/java/schema/shipment/events/ShipmentEvents.java",
            &[
                // Java reads a member by the name the document gives it.
                "@JsonProperty(\"shippingAddress\")",
                "@JsonProperty(\"deliveredAt\")",
            ][..],
        ),
    ] {
        let label = format!("{schema}/{language}");
        generated(
            &emulator,
            &binding_uri(REGISTRY, schema, language, None),
            &label,
        )
        .await;
        let answer = source(
            &emulator,
            &source_uri(REGISTRY, schema, language, None),
            &label,
        )
        .await;
        let entries = archive_entries(&label, &answer);
        let text = entries
            .iter()
            .find(|(path, _)| path == marshaller)
            .map(|(_, text)| text.clone())
            .unwrap_or_else(|| {
                panic!(
                    "case {label:?}: {marshaller:?} must be an entry, got {:?}",
                    entries.iter().map(|(path, _)| path).collect::<Vec<_>>()
                )
            });
        for fragment in fragments {
            assert!(
                text.contains(fragment),
                "case {label:?}: {marshaller} must carry {fragment:?}, got {text}"
            );
        }
    }
}

/// RULE: the document is what decides the archive — a document that declares no named
/// schema generates no source file, so the archive holds none.
#[tokio::test]
async fn a_document_that_declares_no_schema_generates_no_file() {
    let emulator = emulator_with_schemas().await;

    for language in CODE_BINDING_LANGUAGES {
        let label = format!("bare-schema/{language}");
        generated(
            &emulator,
            &binding_uri(REGISTRY, "bare-schema", language, None),
            &label,
        )
        .await;
        let answer = source(
            &emulator,
            &source_uri(REGISTRY, "bare-schema", language, None),
            &label,
        )
        .await;
        let entries = archive_entries(&label, &answer);
        assert!(
            entries.is_empty(),
            "case {label:?}: a document declaring no schema generates no file, got {:?}",
            entries.iter().map(|(path, _)| path).collect::<Vec<_>>()
        );
    }
}

/// RULE: the source is the generation's own product, so it exists only once generation
/// has produced it — and a read never changes what a later read sees.
#[tokio::test]
async fn the_source_appears_only_once_generation_has_completed() {
    let emulator = emulator_with_schemas().await;

    for language in CODE_BINDING_LANGUAGES {
        let label = format!("order.placed/{language}");
        let read = source_uri(REGISTRY, "order.placed", language, None);
        let binding = binding_uri(REGISTRY, "order.placed", language, None);

        // Before the put there is no binding at all, so there is no source.
        let before = emulator.call_payload("GET", &read).await;
        assert_eq!(
            before.status, 404,
            "case {label:?}: nothing is generated until a put asks for it, got {} {:?}",
            before.status, before.body
        );
        assert_eq!(
            before.error_code(),
            "NotFoundException",
            "case {label:?}: expected NotFoundException, got {:?}",
            before.body
        );

        let started = Instant::now();
        let put = emulator.call("POST", &binding, None).await;
        assert_eq!(
            put.status, 202,
            "case {label:?}: expected 202 from PutCodeBinding, got {} {:?}",
            put.status, put.body
        );
        // Generation completes on the clock, so the in-progress answer is only pinned
        // while the span has provably not elapsed yet.
        let in_progress = emulator.call_payload("GET", &read).await;
        if started.elapsed() < CODE_GENERATION_DURATION {
            assert_eq!(
                in_progress.status, 404,
                "case {label:?}: a generation still in progress has produced no source, \
                 got {} {:?}",
                in_progress.status, in_progress.body
            );
        }

        tokio::time::sleep(CODE_GENERATION_DURATION + Duration::from_millis(50)).await;

        let first = source(&emulator, &read, &label).await;
        let second = source(&emulator, &read, &label).await;
        assert_eq!(
            first.raw, second.raw,
            "case {label:?}: two reads of one binding must answer the same archive"
        );
        // And reading the source left the binding exactly as it was: the same creation
        // date the put reported, still reported by the describe.
        let described = emulator.call("GET", &binding, None).await;
        assert_eq!(
            described.status, 200,
            "case {label:?}: the binding must survive the reads, got {} {:?}",
            described.status, described.body
        );
        assert_eq!(
            described.value_at("CreationDate"),
            put.value_at("CreationDate"),
            "case {label:?}: reading the source must not move the creation date, got {:?}",
            described.body
        );
    }
}

/// RULE: a key no put created has no source, whichever part of the key — registry,
/// schema, version or language — is the part nothing created.
#[tokio::test]
async fn the_source_of_a_key_no_put_created_is_not_found() {
    let emulator = emulator_with_schemas().await;
    // One binding does exist, so the rows below are absent for their own reason rather
    // than because nothing was ever generated.
    generated(
        &emulator,
        &binding_uri(REGISTRY, "order.placed", "Java8", None),
        "generated",
    )
    .await;

    // Rows: each part of the key in turn is one nothing created, including a
    // `schemaVersion` that is no version number at all.
    for (case, registry, schema, language, version) in [
        (
            "registry-never-created",
            "no-such-registry",
            "order.placed",
            "Java8",
            None,
        ),
        (
            "schema-never-created",
            REGISTRY,
            "no-such-schema",
            "Java8",
            None,
        ),
        (
            "version-the-schema-does-not-hold",
            REGISTRY,
            "order.placed",
            "Java8",
            Some("2"),
        ),
        (
            "version-that-is-not-a-number",
            REGISTRY,
            "order.placed",
            "Java8",
            Some("v1"),
        ),
        (
            "no-binding-for-this-language",
            REGISTRY,
            "order.placed",
            "Go1",
            None,
        ),
        (
            "no-binding-for-this-schema",
            REGISTRY,
            "shipment-events",
            "Java8",
            Some("1"),
        ),
    ] {
        let answer = emulator
            .call_payload("GET", &source_uri(registry, schema, language, version))
            .await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404 from GetCodeBindingSource, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "NotFoundException",
            "case {case:?}: expected NotFoundException, got {:?}",
            answer.body
        );
    }

    // And the one binding that WAS generated still serves its source.
    source(
        &emulator,
        &source_uri(REGISTRY, "order.placed", "Java8", None),
        "generated",
    )
    .await;
}

/// RULE, as for the two sibling code-binding operations: a `Language` outside the set
/// the service generates for is refused HTTP 400 `BadRequestException`, naming the
/// parameter and the pattern an acceptable value matches — before any lookup, so it does
/// not depend on the registry, the schema or the binding existing.
#[tokio::test]
async fn a_language_outside_the_catalog_is_refused() {
    let emulator = emulator_with_schemas().await;

    // Rows: a language with no bindings at all, a bare family without its version
    // suffix, a supported language at an unsupported version, and the wrong case of a
    // supported one.
    for language in ["Cobol85", "Rust1", "Java", "Java11", "java8", "GO1"] {
        for (target, uri) in [
            (
                "existing schema",
                source_uri(REGISTRY, "order.placed", language, None),
            ),
            (
                "absent schema",
                source_uri(REGISTRY, "ghost-schema", language, None),
            ),
            (
                "absent registry",
                source_uri("ghost-registry", "order.placed", language, None),
            ),
        ] {
            let case = format!("{language:?} / {target}");
            let answer = emulator.call_payload("GET", &uri).await;
            assert_eq!(
                answer.status, 400,
                "case {case}: an unsupported Language is a 400, got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                "BadRequestException",
                "case {case}: expected BadRequestException, got {:?}",
                answer.body
            );
            let message = answer
                .body
                .get("Message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                message,
                format!(
                    "Parameter {CODE_BINDING_LANGUAGE_PARAMETER} doesn't match pattern {}",
                    code_binding_language_pattern()
                ),
                "case {case}: the refusal must name the parameter and the pattern it must \
                 match, got {message:?}"
            );
        }
    }
}

/// RULE: the version the binding was generated from is the version the source is
/// generated from — so an update that adds a version leaves the earlier binding's
/// source as it was, and a binding on the new version is generated from the new
/// document.
#[tokio::test]
async fn the_source_follows_the_version_the_binding_was_generated_from() {
    let emulator = emulator_with_schemas().await;
    let schema = "order.placed";
    let case = "version-pinned-source";

    generated(
        &emulator,
        &binding_uri(REGISTRY, schema, "Java8", Some("1")),
        case,
    )
    .await;
    let first = source(
        &emulator,
        &source_uri(REGISTRY, schema, "Java8", Some("1")),
        case,
    )
    .await;

    // A second version of the same schema, declaring one schema of a different name.
    let second_document = json!({
        "openapi": "3.0.0",
        "info": {"title": "Order Placed", "version": "2.0.0"},
        "paths": {},
        "components": {"schemas": {
            "Shipment": {"type": "object", "properties": {"carrier": {"type": "string"}}}
        }}
    })
    .to_string();
    let updated = emulator
        .call(
            "PUT",
            &schema_uri(REGISTRY, schema),
            Some(json!({ "Content": second_document, "Type": "OpenApi3" })),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected 200 from UpdateSchema, got {} {:?}",
        updated.status, updated.body
    );
    assert_eq!(
        updated.str_at("SchemaVersion"),
        "2",
        "case {case:?}: the update must mint a second version, got {:?}",
        updated.body
    );

    // The binding on version 1 is unchanged: the same archive, from the version 1
    // document.
    let again = source(
        &emulator,
        &source_uri(REGISTRY, schema, "Java8", Some("1")),
        case,
    )
    .await;
    assert_eq!(
        again.raw, first.raw,
        "case {case:?}: a later version must not change the source of an earlier binding"
    );

    // And a binding on version 2 is generated from the version 2 document.
    generated(
        &emulator,
        &binding_uri(REGISTRY, schema, "Java8", Some("2")),
        case,
    )
    .await;
    let second = source(
        &emulator,
        &source_uri(REGISTRY, schema, "Java8", Some("2")),
        case,
    )
    .await;
    let paths: Vec<String> = archive_entries(case, &second)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        paths,
        vec![
            "pom.xml".to_string(),
            "src/main/java/schema/order/placed/Shipment.java".to_string(),
            "src/main/java/schema/order/placed/Marshaller.java".to_string(),
        ],
        "case {case:?}: the second version's own schema must be what is generated, got {paths:?}"
    );
}

/// RULE: the source is generated from whatever document the version holds — including
/// one the SERVICE wrote, as the published AWS event catalogue's documents are — so a
/// binding on a service-owned schema serves the package generated from the document
/// `DescribeSchema` serves for it.
#[tokio::test]
async fn a_service_owned_schema_generates_its_own_package() {
    let emulator = Emulator::new();
    let schema = "aws.s3@ObjectCreated";
    let case = "managed-schema-source";

    let described = emulator
        .call("GET", &schema_uri(AWS_MANAGED_REGISTRY, schema), None)
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected 200 from DescribeSchema, got {} {:?}",
        described.status, described.body
    );
    let document: Value = serde_json::from_str(&described.str_at("Content"))
        .expect("the published document is JSON");
    let declared: Vec<String> = document["components"]["schemas"]
        .as_object()
        .expect("the published document declares schemas")
        .iter()
        .filter(|(_, schema)| schema.get("properties").is_some())
        .map(|(name, _)| name.clone())
        .collect();
    assert!(
        declared.len() > 1,
        "case {case:?}: this row needs a document declaring more than one schema, got {declared:?}"
    );

    generated(
        &emulator,
        &binding_uri(AWS_MANAGED_REGISTRY, schema, "Python36", None),
        case,
    )
    .await;
    let answer = source(
        &emulator,
        &source_uri(AWS_MANAGED_REGISTRY, schema, "Python36", None),
        case,
    )
    .await;
    let entries = archive_entries(case, &answer);
    let paths: Vec<String> = entries.iter().map(|(path, _)| path.clone()).collect();
    let declarations = declaration_paths("Python36", schema, &paths);
    assert_eq!(
        declarations.len(),
        declared.len(),
        "case {case:?}: one declaration per declared schema of the published document, got \
         {declarations:?} for {declared:?}"
    );
    for entry in package_entries("Python36", schema) {
        assert!(
            paths.contains(&entry),
            "case {case:?}: the package must carry {entry:?}, got {paths:?}"
        );
    }
    for name in &declared {
        assert!(
            entries
                .iter()
                .any(|(path, text)| path.starts_with("src/schema/aws/s3/objectcreated/")
                    && text.contains(&format!("class {name}:"))),
            "case {case:?}: the package must declare {name:?} in the schema's own package, \
             got {:?}",
            entries.iter().map(|(path, _)| path).collect::<Vec<_>>()
        );
    }

    // The catalogue is untouched by having been generated from.
    let after = emulator
        .call("GET", &schema_uri(AWS_MANAGED_REGISTRY, schema), None)
        .await;
    assert_eq!(
        after.body, described.body,
        "case {case:?}: generating source must leave the published schema alone, got {:?}",
        after.body
    );
}
