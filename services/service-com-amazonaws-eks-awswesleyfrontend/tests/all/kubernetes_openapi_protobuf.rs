// SPDX-License-Identifier: CC0-1.0

//! The ENCODING a published OpenAPI document is written in, and what the published
//! schemas therefore have to declare.
//!
//! The RULE under test: **a published document is published in every encoding this
//! API server writes it in — the schema document in JSON and in the Protocol Buffers
//! encoding the Go client a stock Kubernetes command line is built on asks for it in
//! — the request's `Accept` chooses which one an answer is written in, and both
//! writings carry ONE document; and because a client validates a whole manifest
//! against that document before it sends it, the schema of a kind declares the
//! members that kind HAS rather than only the ones this server reads.** A server
//! that publishes the document in JSON alone is answered-and-refused by that client:
//! it reports that the schema could not be downloaded and sends no manifest at all,
//! and one whose schemas declare less than the kind has makes it refuse a manifest
//! this server would have accepted.
//!
//! The rows differ along what the rule generalises over: the encoding a client asks
//! for (named alone, named beside the other, at a lower quality, refused outright,
//! left to the server), the document asked for (the Swagger document, which is
//! published in both encodings, and the v3 index, which is published in JSON only),
//! and the members a manifest carries (what the server reads, what it only keeps,
//! nested lists, metadata, and a misspelling no schema declares).

use prost::Message as _;
use serde_json::{json, Value};
use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_openapi_protobuf::{
    self as openapi_protobuf, Document, Schema,
};

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, provisioned_cluster};
use crate::kubernetes_openapi_documents::{dial_accepting_bytes, document, KINDS};
use crate::wire::emulator;

/// How a Swagger 2.0 document references one of its own definitions.
const DEFINITION_REFERENCE: &str = "#/definitions/";

/// The definition `name` of the decoded document.
fn definition<'a>(case: &str, decoded: &'a Document, name: &str) -> &'a Schema {
    let definitions = decoded
        .definitions
        .as_ref()
        .unwrap_or_else(|| panic!("case {case:?}: the document must carry its definitions"));
    definitions
        .additional_properties
        .iter()
        .find(|named| named.name == name)
        .and_then(|named| named.value.as_deref())
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the document must define {name}, got {:?}",
                definitions
                    .additional_properties
                    .iter()
                    .map(|named| named.name.as_str())
                    .collect::<Vec<&str>>()
            )
        })
}

/// The member `member` a decoded schema declares.
fn property<'a>(case: &str, schema: &'a Schema, member: &str) -> &'a Schema {
    schema
        .properties
        .as_deref()
        .and_then(|properties| {
            properties
                .additional_properties
                .iter()
                .find(|named| named.name == member)
        })
        .and_then(|named| named.value.as_deref())
        .unwrap_or_else(|| panic!("case {case:?}: the schema must declare {member}, got {schema:?}"))
}

/// The names of the members a decoded schema declares.
fn property_names(schema: &Schema) -> Vec<&str> {
    schema
        .properties
        .as_deref()
        .map(|properties| {
            properties
                .additional_properties
                .iter()
                .map(|named| named.name.as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// The value of the vendor extension `name` a decoded schema carries, read the way a
/// client reads one: the text the extension travels as, parsed.
fn extension(case: &str, schema: &Schema, name: &str) -> Value {
    let carried = schema
        .vendor_extension
        .iter()
        .find(|named| named.name == name)
        .and_then(|named| named.value.as_ref())
        .unwrap_or_else(|| panic!("case {case:?}: the schema must carry {name}, got {schema:?}"));
    serde_json::from_str(&carried.yaml).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: {name} must carry a value a client can parse, got {:?}: {error}",
            carried.yaml
        )
    })
}

#[tokio::test]
async fn the_schema_document_is_published_in_both_encodings_and_carries_one_document() {
    let router = emulator();
    let config = provisioned_cluster(&router, REGION, "openapi-encodings").await;

    let case = "the schema document in the Protocol Buffers encoding";
    let (status, content_type, bytes) = dial_accepting_bytes(
        &config,
        case,
        "/openapi/v2",
        openapi_protobuf::PROTOBUF_MEDIA_TYPE,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the document must be served in the encoding the client asked for, \
         got {status} with {:?}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(
        content_type,
        openapi_protobuf::PROTOBUF_CONTENT_TYPE,
        "case {case:?}: the answer must state the content type this encoding is written \
         with, got {content_type:?}"
    );
    let decoded = Document::decode(bytes.as_slice())
        .unwrap_or_else(|error| panic!("case {case:?}: the body must decode: {error}"));

    // ONE document, two writings of it: the same version, the same definitions in the
    // same order, and the same statement about every kind.
    let published = document(&config, "the schema document in JSON", "/openapi/v2").await;
    assert_eq!(
        json!(decoded.swagger),
        published["swagger"],
        "case {case:?}: both writings declare one specification version, got {decoded:?}"
    );
    let info = decoded
        .info
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the document must say what it is about"));
    assert_eq!(
        json!(info.title),
        published["info"]["title"],
        "case {case:?}: both writings name one API, got {info:?}"
    );
    assert_eq!(
        json!(info.version),
        published["info"]["version"],
        "case {case:?}: both writings name the cluster's own version, got {info:?}"
    );
    let names: Vec<&str> = decoded
        .definitions
        .as_ref()
        .map(|definitions| {
            definitions
                .additional_properties
                .iter()
                .map(|named| named.name.as_str())
                .collect()
        })
        .unwrap_or_default();
    let published_names: Vec<&str> = published["definitions"]
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: the JSON writing must carry definitions"))
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        names, published_names,
        "case {case:?}: both writings carry the same definitions"
    );

    for (_, kind, name) in KINDS {
        let case = format!("the definition of {kind}");
        let schema = definition(&case, &decoded, name);
        let in_json = &published["definitions"][*name];
        assert_eq!(
            schema.r#type.clone().unwrap_or_default().value,
            vec!["object".to_string()],
            "case {case:?}: a kind is an object schema, got {schema:?}"
        );
        assert_eq!(
            extension(&case, schema, "x-kubernetes-group-version-kind"),
            in_json["x-kubernetes-group-version-kind"],
            "case {case:?}: both writings tag the schema with the kind it is for"
        );
        assert_eq!(
            json!(schema.required),
            in_json
                .get("required")
                .cloned()
                .unwrap_or_else(|| json!([])),
            "case {case:?}: both writings require the same members"
        );
        let declared: Vec<String> = property_names(schema)
            .into_iter()
            .map(str::to_string)
            .collect();
        let declared_in_json: Vec<String> = in_json["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: the JSON writing must declare members"))
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            declared, declared_in_json,
            "case {case:?}: both writings declare the same members"
        );
    }

    // The structure a client walks to validate a manifest: the kind refers to the
    // nested schema, which requires what this server requires, and its list refers to
    // the schema of its entries.
    let pod = definition(case, &decoded, "io.k8s.api.core.v1.Pod");
    assert_eq!(
        property(case, pod, "spec").reference,
        format!("{DEFINITION_REFERENCE}io.k8s.api.core.v1.PodSpec"),
        "case {case:?}: the kind must refer to the nested schema, got {pod:?}"
    );
    let pod_spec = definition(case, &decoded, "io.k8s.api.core.v1.PodSpec");
    assert_eq!(
        pod_spec.required,
        vec!["containers".to_string()],
        "case {case:?}: the specification must require the container list, got {pod_spec:?}"
    );
    let containers = property(case, pod_spec, "containers");
    let entries = containers
        .items
        .as_deref()
        .unwrap_or_else(|| panic!("case {case:?}: the list must say what its entries are"));
    assert_eq!(
        entries
            .schema
            .first()
            .map(|entry| entry.reference.clone())
            .unwrap_or_default(),
        format!("{DEFINITION_REFERENCE}io.k8s.api.core.v1.Container"),
        "case {case:?}: the list must refer to the schema of its entries, got {entries:?}"
    );
    let container = definition(case, &decoded, "io.k8s.api.core.v1.Container");
    assert_eq!(
        container.required,
        vec!["name".to_string(), "image".to_string()],
        "case {case:?}: a container must require what this server requires, got {container:?}"
    );
}

#[tokio::test]
async fn the_encoding_an_answer_is_written_in_is_the_clients_choice() {
    let router = emulator();
    let config = provisioned_cluster(&router, REGION, "openapi-negotiation").await;
    let protobuf = openapi_protobuf::PROTOBUF_MEDIA_TYPE;
    let json_media_type = "application/json";

    // Each row: the document asked for, what the client states it can read, and the
    // status and content type it must be answered with.
    let rows: [(&str, &str, String, u16, &str); 10] = [
        (
            "the Protocol Buffers encoding alone",
            "/openapi/v2",
            protobuf.to_string(),
            200,
            openapi_protobuf::PROTOBUF_CONTENT_TYPE,
        ),
        (
            "the Protocol Buffers encoding first, JSON after it",
            "/openapi/v2",
            format!("{protobuf}, {json_media_type}"),
            200,
            openapi_protobuf::PROTOBUF_CONTENT_TYPE,
        ),
        (
            "JSON first, the Protocol Buffers encoding after it",
            "/openapi/v2",
            format!("{json_media_type}, {protobuf}"),
            200,
            json_media_type,
        ),
        (
            "the Protocol Buffers encoding at a lower quality than JSON",
            "/openapi/v2",
            format!("{protobuf};q=0.5, {json_media_type};q=0.9"),
            200,
            json_media_type,
        ),
        (
            "JSON refused outright, the Protocol Buffers encoding taken",
            "/openapi/v2",
            format!("{json_media_type};q=0, {protobuf}"),
            200,
            openapi_protobuf::PROTOBUF_CONTENT_TYPE,
        ),
        (
            "anything, leaving the choice to the server",
            "/openapi/v2",
            "*/*".to_string(),
            200,
            json_media_type,
        ),
        (
            "no preference stated at all",
            "/openapi/v2",
            String::new(),
            200,
            json_media_type,
        ),
        (
            "an encoding this document is not published in",
            "/openapi/v2",
            "application/x-tar".to_string(),
            406,
            json_media_type,
        ),
        (
            "the Protocol Buffers encoding of a document published in JSON only",
            "/openapi/v3",
            protobuf.to_string(),
            406,
            json_media_type,
        ),
        (
            "JSON of a document published in JSON only",
            "/openapi/v3",
            json_media_type.to_string(),
            200,
            json_media_type,
        ),
    ];
    for (case, path, accept, expected_status, expected_content_type) in rows {
        let (status, content_type, bytes) =
            dial_accepting_bytes(&config, case, path, &accept).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status} for {accept:?} at {path}, got {status} \
             with {:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(
            content_type, expected_content_type,
            "case {case:?}: expected the answer to be written with {expected_content_type:?}, \
             got {content_type:?}"
        );
        if content_type == openapi_protobuf::PROTOBUF_CONTENT_TYPE {
            let decoded = Document::decode(bytes.as_slice())
                .unwrap_or_else(|error| panic!("case {case:?}: the body must decode: {error}"));
            assert_eq!(
                decoded.swagger, "2.0",
                "case {case:?}: the document must be the schema document, got {decoded:?}"
            );
            continue;
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the body must be the JSON a client reads, got {:?}: {error}",
                String::from_utf8_lossy(&bytes)
            )
        });
        if expected_status == 406 {
            // The refusal names the encodings THAT document is published in, which is
            // what tells a client what to ask for instead.
            let published = if path == "/openapi/v2" {
                format!("only the following media types are accepted: {json_media_type}, {protobuf}")
            } else {
                format!("only the following media types are accepted: {json_media_type}")
            };
            assert_eq!(
                body["message"],
                json!(published),
                "case {case:?}: the refusal names what is accepted, got {body}"
            );
        } else {
            assert!(
                body.get("swagger").is_some() || body.get("paths").is_some(),
                "case {case:?}: the document must be served, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn the_published_document_uses_only_the_keywords_both_writings_carry() {
    let router = emulator();
    let config = provisioned_cluster(&router, REGION, "openapi-keywords").await;
    let published = document(&config, "the schema document", "/openapi/v2").await;

    // ONE document, two writings of it: a schema keyword the JSON writing used and the
    // Protocol Buffers writing does not carry would be lost from that writing without
    // a word, so every member of every published schema is either a keyword that
    // writing carries or a vendor extension, which it carries whatever its name.
    let mut unwritten: Vec<String> = Vec::new();
    let mut pending: Vec<(String, Value)> = published["definitions"]
        .as_object()
        .unwrap_or_else(|| panic!("the document must carry definitions, got {published}"))
        .iter()
        .map(|(name, schema)| (name.clone(), schema.clone()))
        .collect();
    assert!(
        !pending.is_empty(),
        "the document must carry definitions, got {published}"
    );
    while let Some((path, schema)) = pending.pop() {
        let Some(members) = schema.as_object() else {
            continue;
        };
        for (name, member) in members {
            if name.starts_with(openapi_protobuf::EXTENSION_PREFIX) {
                continue;
            }
            if !openapi_protobuf::WRITTEN_KEYWORDS.contains(&name.as_str()) {
                unwritten.push(format!("{path}.{name}"));
                continue;
            }
            // The keywords whose value is a schema, or schemas, of its own.
            match name.as_str() {
                "items" | "additionalProperties" => {
                    pending.push((format!("{path}.{name}"), member.clone()));
                }
                "properties" => {
                    for (property, nested) in member.as_object().into_iter().flatten() {
                        pending.push((format!("{path}.{property}"), nested.clone()));
                    }
                }
                _ => {}
            }
        }
    }
    unwritten.sort();
    assert_eq!(
        unwritten,
        Vec::<String>::new(),
        "case the published schemas: every keyword the document uses must be one both \
         writings carry, got {unwritten:?} outside {:?}",
        openapi_protobuf::WRITTEN_KEYWORDS
    );
}

/// Every member of `carried` the published schemas do NOT declare, as the path a
/// client names it by — exactly the check a validating client makes before it sends a
/// manifest: it walks the manifest against the schema of its kind, resolving each
/// reference against the document's own definitions, and reports a member no schema
/// declares.
fn undeclared(definitions: &Value, schema: &Value, carried: &Value, path: &str) -> Vec<String> {
    if let Some(reference) = schema["$ref"].as_str() {
        let name = reference.trim_start_matches(DEFINITION_REFERENCE);
        return undeclared(definitions, &definitions[name], carried, path);
    }
    match carried {
        Value::Object(members) => {
            let declared = schema["properties"].as_object();
            // A schema that declares no member holds the object to nothing: every
            // member of it is one this server keeps as it was sent.
            let Some(declared) = declared.filter(|declared| !declared.is_empty()) else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for (name, member) in members {
                let path = format!("{path}.{name}");
                match declared.get(name) {
                    Some(member_schema) => {
                        found.extend(undeclared(definitions, member_schema, member, &path));
                    }
                    None => found.push(path),
                }
            }
            found
        }
        Value::Array(entries) => entries
            .iter()
            .enumerate()
            .flat_map(|(index, entry)| {
                undeclared(
                    definitions,
                    &schema["items"],
                    entry,
                    &format!("{path}[{index}]"),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn the_published_schema_declares_every_member_a_manifest_this_server_accepts_carries() {
    let router = emulator();
    let config = provisioned_cluster(&router, REGION, "openapi-members").await;
    let published = document(&config, "the schema document", "/openapi/v2").await;
    let definitions = published["definitions"].clone();
    let pod = json!({ "$ref": format!("{DEFINITION_REFERENCE}io.k8s.api.core.v1.Pod") });

    // Each row is a manifest a client would send: the members this server itself
    // reads, the ones it only keeps, a nested list, and the metadata every object
    // carries. Every one of them must be declared by the published schema — a client
    // sends nothing it cannot validate — and admitted by the server.
    let rows: [(&str, Value); 5] = [
        (
            "a pod whose container states the command it runs",
            json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": "commanded" },
                "spec": {
                    "restartPolicy": "Never",
                    "containers": [{
                        "name": "app",
                        "image": "registry.example.com/app:1",
                        "command": ["/bin/sh", "-c"],
                        "args": ["echo hello"],
                    }],
                },
            }),
        ),
        (
            "a pod stating where and as whom it runs",
            json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": "scheduled" },
                "spec": {
                    "serviceAccountName": "default",
                    "nodeSelector": { "kubernetes.io/os": "linux" },
                    "tolerations": [{ "key": "dedicated", "operator": "Exists" }],
                    "terminationGracePeriodSeconds": 30,
                    "hostNetwork": false,
                    "containers": [{
                        "name": "app",
                        "image": "registry.example.com/app:1",
                        "env": [{ "name": "LEVEL", "value": "debug" }],
                        "resources": { "limits": { "cpu": "100m", "memory": "64Mi" } },
                        "imagePullPolicy": "IfNotPresent",
                        "tty": false,
                    }],
                },
            }),
        ),
        (
            "a pod with an init container and a pull secret",
            json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": "initialised" },
                "spec": {
                    "imagePullSecrets": [{ "name": "registry" }],
                    "initContainers": [{
                        "name": "setup",
                        "image": "registry.example.com/setup:1",
                        "command": ["/bin/true"],
                    }],
                    "containers": [{ "name": "app", "image": "registry.example.com/app:1" }],
                },
            }),
        ),
        (
            "a pod whose metadata carries labels, annotations and finalizers",
            json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {
                    "name": "labelled",
                    "labels": { "app": "probe" },
                    "annotations": { "owner": "team" },
                    "finalizers": ["example.com/drain"],
                    "creationTimestamp": null,
                },
                "spec": { "containers": [{ "name": "app", "image": "registry.example.com/app:1" }] },
            }),
        ),
        (
            "a pod whose container declares probes and a security context",
            json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": "probed" },
                "spec": {
                    "securityContext": { "runAsNonRoot": true },
                    "containers": [{
                        "name": "app",
                        "image": "registry.example.com/app:1",
                        "livenessProbe": { "httpGet": { "path": "/healthz", "port": 8080 } },
                        "volumeMounts": [{ "name": "data", "mountPath": "/data" }],
                        "securityContext": { "allowPrivilegeEscalation": false },
                    }],
                    "volumes": [{ "name": "data", "emptyDir": {} }],
                },
            }),
        ),
    ];
    for (case, manifest) in rows {
        let missing = undeclared(&definitions, &pod, &manifest, "Pod");
        assert!(
            missing.is_empty(),
            "case {case:?}: the published schema must declare every member of a manifest this \
             server accepts, got {missing:?} undeclared"
        );
        let answered = dial(
            &config,
            case,
            "POST",
            "/api/v1/namespaces/default/pods",
            Some(&manifest),
        )
        .await;
        assert_eq!(
            answered.status, 201,
            "case {case:?}: the server must accept what its own schema admits, got {answered:?}"
        );
    }

    // AND WHAT NO SCHEMA DECLARES: a misspelled member is a member of no kind, so the
    // published schema does not declare it and a validating client says which one.
    let case = "a pod whose specification misspells a member";
    let mut missing = undeclared(
        &definitions,
        &pod,
        &json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": { "name": "misspelled" },
            "spec": {
                "restartPolicyy": "Never",
                "containers": [{
                    "name": "app",
                    "image": "registry.example.com/app:1",
                    "imagee": "registry.example.com/app:2",
                }],
            },
        }),
        "Pod",
    );
    missing.sort();
    assert_eq!(
        missing,
        vec![
            "Pod.spec.containers[0].imagee".to_string(),
            "Pod.spec.restartPolicyy".to_string(),
        ],
        "case {case:?}: the published schema must declare no member of no kind"
    );
}
