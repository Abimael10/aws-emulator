// SPDX-License-Identifier: CC0-1.0

//! The PROTOCOL BUFFERS encoding the Swagger 2.0 schema document is published in,
//! beside the JSON one ([`super::kubernetes_openapi`]).
//!
//! The schema document is the one surface of a Kubernetes API server a client
//! NEGOTIATES an encoding for, and the Go client every stock Kubernetes command
//! line is built on asks for it in this encoding ALONE: its discovery client sends
//! `GET /openapi/v2` with an `Accept` that names the Protocol Buffers media type
//! and nothing else, and decodes the answer as the schema document's own generated
//! message. A server that writes only JSON therefore answers that client with a
//! refusal it cannot get past — it reports that the schema could not be downloaded
//! and sends no manifest at all — even though the very same document is sitting
//! behind the same path in JSON. So a document this server publishes is published
//! in BOTH encodings, and which one an answer is written in is the client's choice.
//!
//! # Where the message set comes from
//!
//! The messages below are the OpenAPI v2 Protocol Buffers schema's
//! (`OpenAPIv2.proto`, the schema the Kubernetes API server publishes its Swagger
//! 2.0 document through), each member declared at the field number that schema
//! gives it, and `prost` is the codec — so the wire format is the schema's rather
//! than this emulator's. They are declared here rather than taken from a crate
//! because no published crate carries that schema's Rust bindings; only the members
//! the published document uses are declared, and a member left out is a member the
//! document does not carry.
//!
//! What is encoded is the document [`super::kubernetes_openapi::v2_document`]
//! already builds, read member by member rather than restated: the two encodings of
//! one document cannot disagree about a schema, a required member or a kind, because
//! there is one document and two writings of it.
//!
//! A member the JSON document carries EMPTY — a schema that declares no properties,
//! a kind that requires nothing — is left absent here rather than encoded as an
//! empty message, which says the same thing: nothing is declared, so a client reads
//! the object as one whose members this server does not constrain.

use ::prost::Message as _;
use ::serde_json::Value;

/// The media type the schema document is published in here, which is what a client
/// that reads this encoding names on `Accept`.
pub const PROTOBUF_MEDIA_TYPE: &str = "application/com.github.proto-openapi.spec.v2@v1.0+protobuf";

/// The content type the document is WRITTEN with when it is written in this
/// encoding.
///
/// It is not the media type above, and cannot be: that name carries a character a
/// media-type parser does not admit in a subtype (`@`), so a client that parses the
/// content type of what it was handed — the Go client does, before it looks at the
/// body — fails on the header rather than reading the document. The published API
/// server states the generic binary content type on this document for exactly that
/// reason, and the client reads the body as the encoding it asked for.
pub const PROTOBUF_CONTENT_TYPE: &str = "application/octet-stream";

/// The prefix a member of a published schema carries when it is a VENDOR EXTENSION
/// rather than a schema keyword — `x-kubernetes-group-version-kind`, which is how a
/// client resolves the kind its manifest names, and
/// `x-kubernetes-preserve-unknown-fields`. The schema document carries each as a
/// named extension of the schema it was written on.
pub const EXTENSION_PREFIX: &str = "x-";

/// The schema keywords this writing carries, which is every keyword the published
/// document uses ([`super::kubernetes_openapi`]) — the arms of [`schema`] are this
/// list.
///
/// A keyword the published document started using without being written here would be
/// LOST in this encoding while the JSON writing carried it, so the suite that reads
/// both writings holds the document to this list rather than to what either writing
/// happens to carry.
pub const WRITTEN_KEYWORDS: &[&str] = &[
    "$ref",
    "format",
    "description",
    "default",
    "maximum",
    "minimum",
    "type",
    "required",
    "items",
    "properties",
    "additionalProperties",
];

/// `document`, the published Swagger 2.0 document, as the bytes of the schema
/// document's own message.
#[must_use]
pub fn encoded(document: &Value) -> Vec<u8> {
    let info = &document["info"];
    Document {
        swagger: text(document, "swagger"),
        info: Some(Info {
            title: text(info, "title"),
            version: text(info, "version"),
        }),
        // The document's `paths`, carrying what it carries in JSON: this server
        // serves its resources under the paths its routing answers, and the schemas
        // are what a client validates a manifest against.
        paths: Some(Paths::default()),
        definitions: Some(Definitions {
            additional_properties: named_schemas(&document["definitions"]),
        }),
    }
    .encode_to_vec()
}

/// The member `name` of `value` as the text it carries, and the empty string for a
/// member it does not carry.
fn text(value: &Value, name: &str) -> String {
    value[name].as_str().unwrap_or_default().to_string()
}

/// Every text of `value`, which is what a schema's list-of-strings members carry: a
/// required-member list, and the type of a schema.
fn texts(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Array(entries) => entries
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// The schemas `value` holds, in the order the document names them: one named schema
/// per member of a JSON object of schemas, which is both how the document keys its
/// definitions and how a schema keys its properties.
fn named_schemas(value: &Value) -> Vec<NamedSchema> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, schema)| NamedSchema {
            name: name.clone(),
            value: Some(Box::new(self::schema(schema))),
        })
        .collect()
}

/// One published schema, read member by member off the JSON the document carries.
///
/// The keywords read here are [`WRITTEN_KEYWORDS`]; a member outside them that is not
/// a vendor extension is one the published document does not use.
fn schema(value: &Value) -> Schema {
    let mut encoded = Schema::default();
    let Some(members) = value.as_object() else {
        return encoded;
    };
    for (name, member) in members {
        match name.as_str() {
            "$ref" => encoded.reference = member.as_str().unwrap_or_default().to_string(),
            "format" => encoded.format = member.as_str().unwrap_or_default().to_string(),
            "description" => encoded.description = member.as_str().unwrap_or_default().to_string(),
            // The value this server puts in force for a member a manifest leaves
            // out, which travels as the TEXT of that value like an extension's.
            "default" => {
                encoded.default = Some(Any {
                    yaml: member.to_string(),
                });
            }
            "maximum" => encoded.maximum = member.as_f64(),
            "minimum" => encoded.minimum = member.as_f64(),
            "type" => encoded.r#type = Some(TypeItem { value: texts(member) }),
            "required" => encoded.required = texts(member),
            "items" => {
                encoded.items = Some(Box::new(ItemsItem {
                    schema: vec![self::schema(member)],
                }));
            }
            "properties" => {
                let properties = named_schemas(member);
                if !properties.is_empty() {
                    encoded.properties = Some(Box::new(Properties {
                        additional_properties: properties,
                    }));
                }
            }
            "additionalProperties" => {
                encoded.additional_properties = Some(Box::new(match member {
                    Value::Bool(admitted) => AdditionalPropertiesItem {
                        schema: None,
                        boolean: *admitted,
                    },
                    schema => AdditionalPropertiesItem {
                        schema: Some(Box::new(self::schema(schema))),
                        boolean: false,
                    },
                }));
            }
            extension if extension.starts_with(EXTENSION_PREFIX) => {
                encoded.vendor_extension.push(NamedAny {
                    name: name.clone(),
                    value: Some(Any {
                        // An extension travels as the TEXT of its value, which is
                        // what a client parses it back out of; the JSON the document
                        // carries is that text.
                        yaml: member.to_string(),
                    }),
                });
            }
            _ => {}
        }
    }
    encoded
}

/// The schema document itself.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Document {
    #[prost(string, tag = "1")]
    pub swagger: String,
    #[prost(message, optional, tag = "2")]
    pub info: Option<Info>,
    #[prost(message, optional, tag = "8")]
    pub paths: Option<Paths>,
    #[prost(message, optional, tag = "9")]
    pub definitions: Option<Definitions>,
}

/// What the document is about: the API it describes, at the version of the control
/// plane serving it.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Info {
    #[prost(string, tag = "1")]
    pub title: String,
    #[prost(string, tag = "2")]
    pub version: String,
}

/// The paths the document describes.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Paths {}

/// The document's schemas, keyed by the name a client looks a kind's schema up by.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Definitions {
    #[prost(message, repeated, tag = "1")]
    pub additional_properties: Vec<NamedSchema>,
}

/// The properties of one schema, keyed by member name.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Properties {
    #[prost(message, repeated, tag = "1")]
    pub additional_properties: Vec<NamedSchema>,
}

/// One schema under the name the document keys it by.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NamedSchema {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(message, optional, boxed, tag = "2")]
    pub value: Option<Box<Schema>>,
}

/// The type of a schema, which the document states as one name or as several.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TypeItem {
    #[prost(string, repeated, tag = "1")]
    pub value: Vec<String>,
}

/// What the entries of an array schema are.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ItemsItem {
    #[prost(message, repeated, tag = "1")]
    pub schema: Vec<Schema>,
}

/// What a schema says about the members it does not declare: a schema every one of
/// them is held to, or whether they are admitted at all. The document states one or
/// the other, which is why each is a member of its own at the number the schema
/// gives it.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AdditionalPropertiesItem {
    #[prost(message, optional, boxed, tag = "1")]
    pub schema: Option<Box<Schema>>,
    #[prost(bool, tag = "2")]
    pub boolean: bool,
}

/// One vendor extension of a schema, under the name it was written on.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NamedAny {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(message, optional, tag = "2")]
    pub value: Option<Any>,
}

/// The value of a vendor extension, which travels as the text it was written as.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Any {
    #[prost(string, tag = "2")]
    pub yaml: String,
}

/// One schema of the document: the members it declares, what it requires of an
/// object, and the extensions it carries.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Schema {
    /// The schema this one refers to, as the document references its own
    /// definitions.
    #[prost(string, tag = "1")]
    pub reference: String,
    #[prost(string, tag = "2")]
    pub format: String,
    #[prost(string, tag = "4")]
    pub description: String,
    #[prost(message, optional, tag = "5")]
    pub default: Option<Any>,
    #[prost(double, optional, tag = "7")]
    pub maximum: Option<f64>,
    #[prost(double, optional, tag = "9")]
    pub minimum: Option<f64>,
    #[prost(string, repeated, tag = "19")]
    pub required: Vec<String>,
    #[prost(message, optional, boxed, tag = "21")]
    pub additional_properties: Option<Box<AdditionalPropertiesItem>>,
    #[prost(message, optional, tag = "22")]
    pub r#type: Option<TypeItem>,
    #[prost(message, optional, boxed, tag = "23")]
    pub items: Option<Box<ItemsItem>>,
    #[prost(message, optional, boxed, tag = "25")]
    pub properties: Option<Box<Properties>>,
    #[prost(message, repeated, tag = "31")]
    pub vendor_extension: Vec<NamedAny>,
}
