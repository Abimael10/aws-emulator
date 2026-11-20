// SPDX-License-Identifier: CC0-1.0

//! The OpenAPI documents a cluster's Kubernetes API server PUBLISHES, which a
//! client reads before it sends a manifest.
//!
//! A Kubernetes client does not only send objects: it validates what it is about
//! to send against the schema the API server publishes for the kind. `kubectl
//! apply` reads the server's OpenAPI documents first and refuses to send a
//! manifest it cannot validate at all ("error validating data: failed to download
//! openapi"), so an API server that publishes none cannot be driven by a stock
//! client even for the objects it does serve.
//!
//! Two surfaces are published, because a client reads whichever its version
//! knows:
//!
//! * `GET /openapi/v3` is the INDEX of group versions, each with the relative URL
//!   its own document is served at, and `GET /openapi/v3/<group version>` is that
//!   document — an OpenAPI 3.0 document whose `components.schemas` carry one
//!   schema per kind, each tagged with the group, version and kind it is for
//!   (`x-kubernetes-group-version-kind`), which is how a client resolves the kind
//!   its manifest names;
//! * `GET /openapi/v2` is the single Swagger 2.0 document an older client reads,
//!   carrying the SAME schemas under `definitions`.
//!
//! What the schemas STATE is what this API server does with an object, and
//! nothing more: the members it requires of a kind are `required` (a Pod's
//! container list, and the name and image of each container —
//! [`super::kubernetes_pod_spec`] refuses a manifest without them), and every
//! object it stores as it was submitted carries
//! `x-kubernetes-preserve-unknown-fields`, because this server keeps the members
//! it was sent rather than dropping the ones it does not read. So a manifest this
//! server would accept validates, and one it would refuse does not — the document
//! and the admission are one rule.

use super::kubernetes_objects::CORE_API_VERSION;
use super::kubernetes_rbac::{RBAC_API_GROUP, RBAC_GROUP_VERSION};
use super::kubernetes_self_subject_review::{
    AUTHENTICATION_API_GROUP, AUTHENTICATION_GROUP_VERSION,
};
use super::*;

/// The path the OpenAPI v3 INDEX is served at, as the segments of a request path.
pub const OPENAPI_V3_SEGMENT: &str = "openapi";
pub const OPENAPI_V3_VERSION: &str = "v3";

/// The path the single Swagger 2.0 document is served at.
pub const OPENAPI_V2_VERSION: &str = "v2";

/// The version of the OpenAPI specification a v3 document declares, and of the
/// Swagger specification the v2 document declares.
const OPENAPI_VERSION: &str = "3.0.0";
const SWAGGER_VERSION: &str = "2.0";

/// The `info` every document carries: the Kubernetes API, at the version of this
/// cluster's control plane.
fn info(kubernetes_version: &str) -> ::serde_json::Value {
    ::serde_json::json!({ "title": "Kubernetes", "version": format!("v{kubernetes_version}") })
}

/// The group versions this API server publishes a document for, as the index
/// names each: the path under `/openapi/v3`, the API group (empty for the core
/// one) and the version.
///
/// They are exactly the group versions the routing serves
/// ([`super::kubernetes_api`]), so a client that reads the index finds a document
/// for every group version it can address and none for one it cannot.
fn published_group_versions() -> Vec<(String, &'static str, &'static str)> {
    vec![
        (
            format!("api/{CORE_API_VERSION}"),
            "",
            CORE_API_VERSION,
        ),
        (
            format!("apis/{RBAC_API_GROUP}/{RBAC_GROUP_VERSION}"),
            RBAC_API_GROUP,
            RBAC_GROUP_VERSION,
        ),
        (
            format!("apis/{AUTHENTICATION_API_GROUP}/{AUTHENTICATION_GROUP_VERSION}"),
            AUTHENTICATION_API_GROUP,
            AUTHENTICATION_GROUP_VERSION,
        ),
    ]
}

/// `GET /openapi/v3`: the index of the documents this server publishes, each with
/// the relative URL it is served at.
///
/// The `hash` the URL carries is the digest of the document it names, which is
/// what a client caches by: a document that has not changed carries the same hash,
/// so nothing re-reads it, and the value is derived from the document itself
/// rather than minted per call.
#[must_use]
pub fn v3_index(kubernetes_version: &str) -> ::serde_json::Value {
    let mut paths = ::serde_json::Map::new();
    for (path, group, version) in published_group_versions() {
        let document = group_version_document(kubernetes_version, group, version);
        let hash = document_hash(&document);
        paths.insert(
            path.clone(),
            ::serde_json::json!({
                "serverRelativeURL": format!("/{OPENAPI_V3_SEGMENT}/{OPENAPI_V3_VERSION}/{path}?hash={hash}"),
            }),
        );
    }
    ::serde_json::json!({ "paths": ::serde_json::Value::Object(paths) })
}

/// `GET /openapi/v3/<group version>`: the OpenAPI 3.0 document of ONE group
/// version, and `None` for a group version this server publishes none for.
///
/// `path` is the path under `/openapi/v3`, as the index names it.
#[must_use]
pub fn v3_document(kubernetes_version: &str, path: &str) -> Option<::serde_json::Value> {
    let (_, group, version) = published_group_versions()
        .into_iter()
        .find(|(published, _, _)| published == path)?;
    Some(group_version_document(kubernetes_version, group, version))
}

/// `GET /openapi/v2`: the single Swagger 2.0 document, carrying the schemas of
/// every group version this server publishes under `definitions`.
#[must_use]
pub fn v2_document(kubernetes_version: &str) -> ::serde_json::Value {
    let mut definitions = ::serde_json::Map::new();
    for (_, group, version) in published_group_versions() {
        for (name, schema) in schemas_of(group, version) {
            definitions.insert(name, swagger_references(schema));
        }
    }
    ::serde_json::json!({
        "swagger": SWAGGER_VERSION,
        "info": info(kubernetes_version),
        // This server serves its resources under the paths the routing answers;
        // the document a client validates a manifest against is the SCHEMAS, and
        // the paths of a Swagger document are not what it reads for that.
        "paths": ::serde_json::Map::new(),
        "definitions": ::serde_json::Value::Object(definitions),
    })
}

/// The OpenAPI 3.0 document of one group version.
fn group_version_document(
    kubernetes_version: &str,
    group: &str,
    version: &str,
) -> ::serde_json::Value {
    let schemas: ::serde_json::Map<String, ::serde_json::Value> =
        schemas_of(group, version).into_iter().collect();
    ::serde_json::json!({
        "openapi": OPENAPI_VERSION,
        "info": info(kubernetes_version),
        "paths": ::serde_json::Map::new(),
        "components": { "schemas": ::serde_json::Value::Object(schemas) },
    })
}

/// The digest of one published document, which is what a client caches it by.
fn document_hash(document: &::serde_json::Value) -> String {
    hex::encode(Sha256::digest(document.to_string().as_bytes()))
}

/// Every `#/components/$ref` of a v3 schema, rewritten to the `#/definitions/`
/// form a Swagger 2.0 document references its definitions by.
fn swagger_references(schema: ::serde_json::Value) -> ::serde_json::Value {
    match schema {
        ::serde_json::Value::String(value) => {
            ::serde_json::Value::String(value.replace(V3_SCHEMA_REF, SWAGGER_SCHEMA_REF))
        }
        ::serde_json::Value::Array(entries) => ::serde_json::Value::Array(
            entries.into_iter().map(swagger_references).collect(),
        ),
        ::serde_json::Value::Object(members) => ::serde_json::Value::Object(
            members
                .into_iter()
                .map(|(name, value)| (name, swagger_references(value)))
                .collect(),
        ),
        other => other,
    }
}

/// How a v3 document and a Swagger 2.0 document each reference a schema.
const V3_SCHEMA_REF: &str = "#/components/schemas/";
const SWAGGER_SCHEMA_REF: &str = "#/definitions/";

/// The schema name of one Kubernetes kind, as both documents key it: the Go
/// package path of the type, which is how a client looks a kind's schema up.
fn schema_name(group: &str, version: &str, kind: &str) -> String {
    match group {
        "" => format!("io.k8s.api.core.{version}.{kind}"),
        RBAC_API_GROUP => format!("io.k8s.api.rbac.{version}.{kind}"),
        AUTHENTICATION_API_GROUP => format!("io.k8s.api.authentication.{version}.{kind}"),
        other => format!("io.k8s.api.{other}.{version}.{kind}"),
    }
}

/// The schema name of one `apimachinery` type, which every group version's
/// objects reference.
fn meta_schema_name(kind: &str) -> String {
    format!("io.k8s.apimachinery.pkg.apis.meta.v1.{kind}")
}

/// A reference to the schema `name`.
fn reference(name: &str) -> ::serde_json::Value {
    ::serde_json::json!({ "$ref": format!("{V3_SCHEMA_REF}{name}") })
}

/// An object schema whose members this server KEEPS as it was sent them: the
/// properties it reads are declared, the ones it does not are preserved.
fn object(
    properties: ::serde_json::Value,
    required: &[&str],
) -> ::serde_json::Value {
    let mut schema = ::serde_json::json!({
        "type": "object",
        "properties": properties,
        // This server stores an object as it was submitted — nothing it does not
        // read is dropped — so the schema it publishes preserves those members
        // rather than declaring the object closed.
        "x-kubernetes-preserve-unknown-fields": true,
    });
    if !required.is_empty() {
        schema["required"] = ::serde_json::json!(required);
    }
    schema
}

/// The object schema of one KIND: an object as above, carrying the group, version
/// and kind a client resolves its manifest against, and the three members every
/// Kubernetes object has.
fn kind_schema(
    group: &str,
    version: &str,
    kind: &str,
    mut properties: ::serde_json::Map<String, ::serde_json::Value>,
    required: &[&str],
) -> ::serde_json::Value {
    properties.insert("apiVersion".to_string(), ::serde_json::json!({ "type": "string" }));
    properties.insert("kind".to_string(), ::serde_json::json!({ "type": "string" }));
    properties.insert(
        "metadata".to_string(),
        reference(&meta_schema_name("ObjectMeta")),
    );
    let mut schema = object(::serde_json::Value::Object(properties), required);
    schema["x-kubernetes-group-version-kind"] = ::serde_json::json!([{
        "group": group,
        "version": version,
        "kind": kind,
    }]);
    schema
}

/// One schema entry, as the documents key it.
fn entry(
    group: &str,
    version: &str,
    kind: &str,
    properties: &[(&str, ::serde_json::Value)],
    required: &[&str],
) -> (String, ::serde_json::Value) {
    let members: ::serde_json::Map<String, ::serde_json::Value> = properties
        .iter()
        .map(|(name, schema)| ((*name).to_string(), schema.clone()))
        .collect();
    (
        schema_name(group, version, kind),
        kind_schema(group, version, kind, members, required),
    )
}

/// A schema of an arbitrary object this server keeps as it was sent: a `status`
/// it computes, a `data` map, a nested configuration.
fn open_object() -> ::serde_json::Value {
    object(::serde_json::json!({}), &[])
}

/// A map of strings, as a label set, an annotation set and a ConfigMap's data
/// are.
fn string_map() -> ::serde_json::Value {
    ::serde_json::json!({
        "type": "object",
        "additionalProperties": { "type": "string" },
    })
}

/// A list of the schema `name`.
fn list_of(name: &str) -> ::serde_json::Value {
    ::serde_json::json!({ "type": "array", "items": reference(name) })
}

/// The media type the published documents are written in, and the `Status` the API
/// server refuses an `Accept` it cannot satisfy with.
///
/// The OpenAPI documents are the one surface of this API a client NEGOTIATES an
/// encoding for — the Swagger 2.0 document is published in a Protocol Buffers
/// encoding as well as in JSON, and a client that asks for that encoding cannot read
/// the other one. This server writes JSON, so an `Accept` naming only an encoding it
/// does not produce is refused as unacceptable rather than answered `200` with a body
/// the client cannot decode.
pub const DOCUMENT_MEDIA_TYPE: &str = "application/json";

/// Whether `accept` admits the encoding these documents are written in: a request
/// that states no preference, one that takes anything, one that takes any
/// `application/*`, and one that names JSON itself.
///
/// A media type offered with `q=0` is one the client states it does NOT accept ("a
/// quality value of 0 means 'not acceptable'", RFC 9110), so it admits nothing.
#[must_use]
pub fn admits_json(accept: Option<&str>) -> bool {
    let Some(accept) = accept.map(str::trim).filter(|accept| !accept.is_empty()) else {
        return true;
    };
    accept.split(',').any(|offered| {
        let mut parameters = offered.split(';');
        let media_type = parameters
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let refused = parameters.any(|parameter| {
            let (name, value) = parameter.split_once('=').unwrap_or_default();
            name.trim().eq_ignore_ascii_case("q")
                && value
                    .trim()
                    .parse::<f32>()
                    .is_ok_and(|quality| quality <= 0.0)
        });
        !refused
            && matches!(
                media_type.as_str(),
                "*/*" | "application/*" | DOCUMENT_MEDIA_TYPE
            )
    })
}

/// The sentence the API server refuses an unacceptable `Accept` with, naming the
/// media types it does accept.
#[must_use]
pub fn unacceptable_message() -> String {
    format!("only the following media types are accepted: {DOCUMENT_MEDIA_TYPE}")
}

/// The schemas of one group version: one per kind this server serves there, plus
/// the nested types they reference.
fn schemas_of(group: &str, version: &str) -> Vec<(String, ::serde_json::Value)> {
    let mut schemas = vec![(
        meta_schema_name("ObjectMeta"),
        object(
            ::serde_json::json!({
                "name": { "type": "string" },
                "generateName": { "type": "string" },
                "namespace": { "type": "string" },
                "uid": { "type": "string" },
                "resourceVersion": { "type": "string" },
                "creationTimestamp": { "type": "string" },
                "labels": string_map(),
                "annotations": string_map(),
            }),
            &[],
        ),
    )];
    match (group, version) {
        ("", _) => {
            let container = schema_name(group, version, "Container");
            schemas.push((
                container.clone(),
                object(
                    ::serde_json::json!({
                        "name": { "type": "string" },
                        "image": { "type": "string" },
                    }),
                    // The two members this server requires of a container: a pod
                    // whose container names neither has no workload to run
                    // ([`super::kubernetes_pod_spec`]).
                    &["name", "image"],
                ),
            ));
            let pod_spec = schema_name(group, version, "PodSpec");
            schemas.push((
                pod_spec.clone(),
                object(
                    ::serde_json::json!({
                        "containers": list_of(&container),
                        "serviceAccountName": { "type": "string" },
                        "serviceAccount": { "type": "string" },
                        "nodeName": { "type": "string" },
                    }),
                    // "There must be at least one container in a Pod".
                    &["containers"],
                ),
            ));
            schemas.push(entry(
                group,
                version,
                "Pod",
                &[
                    ("spec", reference(&pod_spec)),
                    ("status", open_object()),
                ],
                &["spec"],
            ));
            schemas.push(entry(group, version, "Namespace", &[("spec", open_object()), ("status", open_object())], &[]));
            schemas.push(entry(
                group,
                version,
                "ConfigMap",
                &[("data", string_map()), ("binaryData", string_map())],
                &[],
            ));
            schemas.push(entry(group, version, "ServiceAccount", &[], &[]));
            schemas.push(entry(
                group,
                version,
                "Event",
                &[
                    ("involvedObject", open_object()),
                    ("reason", ::serde_json::json!({ "type": "string" })),
                    ("message", ::serde_json::json!({ "type": "string" })),
                    ("type", ::serde_json::json!({ "type": "string" })),
                    ("source", open_object()),
                    ("count", ::serde_json::json!({ "type": "integer" })),
                    ("firstTimestamp", ::serde_json::json!({ "type": "string" })),
                    ("lastTimestamp", ::serde_json::json!({ "type": "string" })),
                ],
                &[],
            ));
            schemas.push(entry(
                group,
                version,
                "Node",
                &[("spec", open_object()), ("status", open_object())],
                &[],
            ));
        }
        (RBAC_API_GROUP, _) => {
            let rule = schema_name(group, version, "PolicyRule");
            let string_list = ::serde_json::json!({
                "type": "array",
                "items": { "type": "string" },
            });
            schemas.push((
                rule.clone(),
                object(
                    ::serde_json::json!({
                        "apiGroups": string_list,
                        "resources": string_list,
                        "resourceNames": string_list,
                        "verbs": string_list,
                        "nonResourceURLs": string_list,
                    }),
                    // "Verbs is a list of Verbs that apply to ALL the ResourceKinds
                    // contained in this rule", the one member a rule states nothing
                    // without.
                    &["verbs"],
                ),
            ));
            let subject = schema_name(group, version, "Subject");
            schemas.push((
                subject.clone(),
                object(
                    ::serde_json::json!({
                        "kind": { "type": "string" },
                        "apiGroup": { "type": "string" },
                        "name": { "type": "string" },
                        "namespace": { "type": "string" },
                    }),
                    &["kind", "name"],
                ),
            ));
            let role_ref = schema_name(group, version, "RoleRef");
            schemas.push((
                role_ref.clone(),
                object(
                    ::serde_json::json!({
                        "apiGroup": { "type": "string" },
                        "kind": { "type": "string" },
                        "name": { "type": "string" },
                    }),
                    &["apiGroup", "kind", "name"],
                ),
            ));
            for kind in ["Role", "ClusterRole"] {
                schemas.push(entry(group, version, kind, &[("rules", list_of(&rule))], &[]));
            }
            for kind in ["RoleBinding", "ClusterRoleBinding"] {
                schemas.push(entry(
                    group,
                    version,
                    kind,
                    &[
                        ("subjects", list_of(&subject)),
                        ("roleRef", reference(&role_ref)),
                    ],
                    &["roleRef"],
                ));
            }
        }
        (AUTHENTICATION_API_GROUP, _) => {
            schemas.push(entry(
                group,
                version,
                "TokenRequest",
                &[("spec", open_object()), ("status", open_object())],
                &[],
            ));
            schemas.push(entry(
                group,
                version,
                "SelfSubjectReview",
                &[("spec", open_object()), ("status", open_object())],
                &[],
            ));
        }
        _ => {}
    }
    schemas
}
