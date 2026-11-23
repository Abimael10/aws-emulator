// SPDX-License-Identifier: CC0-1.0

//! The ENCODING a client writes a write body to the served cluster API in.
//!
//! The RULE under test: the Kubernetes API server a cluster publishes reads a
//! write body in each media type it advertises as readable — the JSON document,
//! the YAML document that is the same object written as YAML, and the Kubernetes
//! protobuf encoding, in which the object travels as the bytes of its own
//! generated message behind the envelope naming its kind — whatever media-type
//! parameters the client states beside it and whatever the case, and it answers
//! `415` only for a media type it reads none of, naming the set it does read. The
//! object a create stores is the same object whichever encoding it arrived in, so
//! a client reads back what it sent.
//!
//! The rows differ along what the rule generalises over: the ENCODING the body is
//! written in, the media-type parameters and the letter case the client states it
//! with, whether it states one at all, and the RESOURCE the body is written to (a
//! namespace, a service account, a ConfigMap, a pod and an RBAC role — every
//! resource a client writes through this address).
//!
//! The client is the raw, kubeconfig-built one `kubernetes_api.rs` dials with: a
//! TCP connection to the published address, TLS verified against nothing but the
//! published certificate authority, and one HTTP/1.1 exchange carrying the body as
//! the BYTES the client encoded it to. A body in the protobuf encoding is built
//! here the way a client builds one — the object encoded against the generated
//! Kubernetes schema inside the `runtime.Unknown` envelope — so nothing of this
//! emulator's own reading is used to write it.

use k8s_pb::api::core::v1 as core_v1;
use k8s_pb::api::rbac::v1 as rbac_v1;
use k8s_pb::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_pb::apimachinery::pkg::runtime::{TypeMeta, Unknown};
use k8s_pb::apimachinery::pkg::util::intstr::IntOrString;
use prost::Message as _;
use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_api::READ_MEDIA_TYPES;

use crate::kubernetes_api::{account_bearer_token, dial, dial_bytes, ApiAnswer, Kubeconfig};
use crate::kubernetes_cluster_resources::published_cluster;

/// The media type a client states when it wrote the body in the Kubernetes
/// protobuf encoding, as the API server's own set of readable media types names it
/// (`READ_MEDIA_TYPES`).
const PROTOBUF: &str = "application/vnd.kubernetes.protobuf";

/// The `apiVersion` of the core group and of the RBAC group, which the envelope of
/// a body in the protobuf encoding names the object's kind under.
const CORE_API_VERSION: &str = "v1";
const RBAC_API_VERSION: &str = "rbac.authorization.k8s.io/v1";

/// One object as the BYTES a client writing the Kubernetes protobuf encoding
/// sends: the four magic bytes of the encoding, then the envelope naming the
/// object's `apiVersion` and `kind` and carrying the object's own encoded message.
fn protobuf_body(api_version: &str, kind: &str, object: &impl prost::Message) -> Vec<u8> {
    let envelope = Unknown {
        type_meta: Some(TypeMeta {
            api_version: Some(api_version.to_string()),
            kind: Some(kind.to_string()),
        }),
        raw: Some(object.encode_to_vec()),
        ..Default::default()
    };
    let mut body = b"k8s\x00".to_vec();
    envelope
        .encode(&mut body)
        .expect("setup: the envelope encodes into a growable buffer");
    body
}

/// The `metadata` of an object a row writes: its name, and the labels the client
/// expects to read back on it.
fn metadata(name: &str, labels: &[(&str, &str)]) -> ObjectMeta {
    ObjectMeta {
        name: Some(name.to_string()),
        labels: labels
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        ..Default::default()
    }
}

/// One `POST` of `payload` to `path`, with the client stating `content_type` as the
/// media type it wrote the body in.
async fn post(
    config: &Kubeconfig,
    case: &str,
    path: &str,
    content_type: &str,
    payload: &[u8],
) -> ApiAnswer {
    dial_bytes(
        config,
        case,
        "POST",
        path,
        content_type,
        Some(&account_bearer_token()),
        payload,
    )
    .await
}

/// RULE: a namespace create is read in every media type this server advertises as
/// readable, however the client states that media type, and the namespace it
/// stores is the one the body named.
#[tokio::test]
async fn a_create_is_read_in_every_media_type_the_server_advertises() {
    let (_router, _engine, config) = published_cluster("encodings-cluster").await;

    // The JSON and the YAML form of ONE namespace manifest, written as a client
    // writes each: the YAML row is indented YAML rather than JSON text, so a row
    // that claims YAML is read by a YAML reading rather than by the JSON one.
    let json_body = |name: &str, label: &str| {
        serde_json::json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": { "name": name, "labels": { "team": label } },
        })
        .to_string()
        .into_bytes()
    };
    let yaml_body = |name: &str, label: &str| {
        format!(
            "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {name}\n  labels:\n    team: \
             {label}\n"
        )
        .into_bytes()
    };
    let protobuf = |name: &str, label: &str| {
        protobuf_body(
            CORE_API_VERSION,
            "Namespace",
            &core_v1::Namespace {
                metadata: Some(metadata(name, &[("team", label)])),
                ..Default::default()
            },
        )
    };

    // (the case, the media type the client states, the namespace it creates, the
    // body as that client encoded it)
    let rows: Vec<(&str, String, &str, Vec<u8>)> = vec![
        (
            "a JSON document",
            "application/json".to_string(),
            "encoded-json",
            json_body("encoded-json", "json"),
        ),
        (
            "a JSON document with its charset stated",
            "application/json; charset=utf-8".to_string(),
            "encoded-json-charset",
            json_body("encoded-json-charset", "charset"),
        ),
        (
            "a JSON document whose media type is stated in another case",
            "Application/JSON".to_string(),
            "encoded-json-case",
            json_body("encoded-json-case", "case"),
        ),
        (
            "a client that states no media type at all",
            String::new(),
            "encoded-json-unstated",
            json_body("encoded-json-unstated", "unstated"),
        ),
        (
            "a YAML document",
            "application/yaml".to_string(),
            "encoded-yaml",
            yaml_body("encoded-yaml", "yaml"),
        ),
        (
            "a YAML document with its charset stated",
            "application/yaml; charset=utf-8".to_string(),
            "encoded-yaml-charset",
            yaml_body("encoded-yaml-charset", "yamlcharset"),
        ),
        (
            "the Kubernetes protobuf encoding",
            PROTOBUF.to_string(),
            "encoded-protobuf",
            protobuf("encoded-protobuf", "protobuf"),
        ),
        (
            "the Kubernetes protobuf encoding with a parameter stated",
            format!("{PROTOBUF}; charset=utf-8"),
            "encoded-protobuf-parameter",
            protobuf("encoded-protobuf-parameter", "parameter"),
        ),
    ];

    for (case, content_type, name, payload) in &rows {
        let created = post(&config, case, "/api/v1/namespaces", content_type, payload).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201 from the namespace create, got {created:?}"
        );
        assert_eq!(
            created.body["metadata"]["name"],
            serde_json::json!(name),
            "case {case:?}: the create answers the namespace the body named, got {created:?}"
        );
        assert_eq!(
            created.body["kind"],
            serde_json::json!("Namespace"),
            "case {case:?}: the create answers a Namespace, got {created:?}"
        );
        // Read back through the implemented reader: what the create stored is what
        // a later caller sees, whichever encoding the body arrived in.
        let read = dial(
            &config,
            case,
            "GET",
            &format!("/api/v1/namespaces/{name}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the created namespace must be readable, got {read:?}"
        );
        assert_eq!(
            read.body["metadata"]["labels"],
            created.body["metadata"]["labels"],
            "case {case:?}: the stored labels are the ones the body carried, got {read:?}"
        );
        assert!(
            read.body["metadata"]["labels"]["team"].is_string(),
            "case {case:?}: the label the body carried is read back, got {read:?}"
        );
    }
}

/// RULE: the protobuf encoding is read for EVERY resource a client writes through
/// this address, and the object it stores carries what the message carried.
#[tokio::test]
async fn every_resource_a_client_writes_is_read_in_the_protobuf_encoding() {
    let (_router, _engine, config) = published_cluster("protobuf-resources-cluster").await;
    let namespace = "protobuf-team";

    let created = post(
        &config,
        "a namespace",
        "/api/v1/namespaces",
        PROTOBUF,
        &protobuf_body(
            CORE_API_VERSION,
            "Namespace",
            &core_v1::Namespace {
                metadata: Some(metadata(namespace, &[("owner", "protobuf")])),
                ..Default::default()
            },
        ),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"a namespace\": expected HTTP 201, got {created:?}"
    );

    // A service account, which a pod of the namespace then runs as.
    let account = post(
        &config,
        "a service account",
        &format!("/api/v1/namespaces/{namespace}/serviceaccounts"),
        PROTOBUF,
        &protobuf_body(
            CORE_API_VERSION,
            "ServiceAccount",
            &core_v1::ServiceAccount {
                metadata: Some(metadata("protobuf-runner", &[])),
                ..Default::default()
            },
        ),
    )
    .await;
    assert_eq!(
        account.status, 201,
        "case \"a service account\": expected HTTP 201, got {account:?}"
    );

    // A ConfigMap, whose `data` entries are the ones the message carried.
    let config_map = post(
        &config,
        "a ConfigMap",
        &format!("/api/v1/namespaces/{namespace}/configmaps"),
        PROTOBUF,
        &protobuf_body(
            CORE_API_VERSION,
            "ConfigMap",
            &core_v1::ConfigMap {
                metadata: Some(metadata("protobuf-settings", &[])),
                data: [
                    ("greeting".to_string(), "hello".to_string()),
                    ("tier".to_string(), "silver".to_string()),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        ),
    )
    .await;
    assert_eq!(
        config_map.status, 201,
        "case \"a ConfigMap\": expected HTTP 201, got {config_map:?}"
    );
    assert_eq!(
        config_map.body["data"],
        serde_json::json!({ "greeting": "hello", "tier": "silver" }),
        "case \"a ConfigMap\": the stored data is what the message carried, got {config_map:?}"
    );

    // A pod, whose containers and their environment are the ones the message
    // declared — the deepest projection of the encoding this server reads.
    let pod = post(
        &config,
        "a pod",
        &format!("/api/v1/namespaces/{namespace}/pods"),
        PROTOBUF,
        &protobuf_body(
            CORE_API_VERSION,
            "Pod",
            &core_v1::Pod {
                metadata: Some(metadata("protobuf-pod", &[("app", "protobuf")])),
                spec: Some(core_v1::PodSpec {
                    service_account_name: Some("protobuf-runner".to_string()),
                    restart_policy: Some("Never".to_string()),
                    containers: vec![core_v1::Container {
                        name: Some("app".to_string()),
                        image: Some("nginx:1.27".to_string()),
                        command: vec!["/bin/sh".to_string(), "-c".to_string()],
                        env: vec![core_v1::EnvVar {
                            name: Some("TIER".to_string()),
                            value: Some("silver".to_string()),
                            ..Default::default()
                        }],
                        ports: vec![core_v1::ContainerPort {
                            name: Some("http".to_string()),
                            container_port: Some(8080),
                            ..Default::default()
                        }],
                        // The two forms a probe addresses a port in: the number
                        // of one, and the NAME of a container port.
                        readiness_probe: Some(core_v1::Probe {
                            handler: Some(core_v1::ProbeHandler {
                                http_get: Some(core_v1::HttpGetAction {
                                    path: Some("/healthz".to_string()),
                                    port: Some(IntOrString {
                                        int_val: Some(8080),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            initial_delay_seconds: Some(5),
                            period_seconds: Some(10),
                            ..Default::default()
                        }),
                        startup_probe: Some(core_v1::Probe {
                            handler: Some(core_v1::ProbeHandler {
                                tcp_socket: Some(core_v1::TcpSocketAction {
                                    port: Some(IntOrString {
                                        str_val: Some("http".to_string()),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            failure_threshold: Some(3),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    )
    .await;
    assert_eq!(
        pod.status, 201,
        "case \"a pod\": expected HTTP 201, got {pod:?}"
    );
    assert_eq!(
        pod.body["spec"]["containers"][0]["image"],
        serde_json::json!("nginx:1.27"),
        "case \"a pod\": the container's image is the declared one, got {pod:?}"
    );
    assert_eq!(
        pod.body["spec"]["containers"][0]["command"],
        serde_json::json!(["/bin/sh", "-c"]),
        "case \"a pod\": the container's command is the declared one, got {pod:?}"
    );
    assert_eq!(
        pod.body["spec"]["containers"][0]["env"][0],
        serde_json::json!({ "name": "TIER", "value": "silver" }),
        "case \"a pod\": the container's environment is the declared one, got {pod:?}"
    );
    assert_eq!(
        pod.body["spec"]["serviceAccountName"],
        serde_json::json!("protobuf-runner"),
        "case \"a pod\": the pod runs as the account the message named, got {pod:?}"
    );
    assert_eq!(
        pod.body["spec"]["restartPolicy"],
        serde_json::json!("Never"),
        "case \"a pod\": the pod keeps the restart policy the message named, got {pod:?}"
    );
    // The probes the container declared, with the action flattened onto the probe
    // the way a client writing JSON writes it, and each port in the form the
    // message carried it.
    assert_eq!(
        pod.body["spec"]["containers"][0]["readinessProbe"],
        serde_json::json!({
            "httpGet": { "path": "/healthz", "port": 8080 },
            "initialDelaySeconds": 5,
            "periodSeconds": 10,
        }),
        "case \"a pod\": the readiness probe is the declared one, got {pod:?}"
    );
    assert_eq!(
        pod.body["spec"]["containers"][0]["startupProbe"],
        serde_json::json!({
            "tcpSocket": { "port": "http" },
            "failureThreshold": 3,
        }),
        "case \"a pod\": the startup probe addresses the port by name, got {pod:?}"
    );
    assert_eq!(
        pod.body["spec"]["containers"][0]["ports"][0],
        serde_json::json!({ "name": "http", "containerPort": 8080 }),
        "case \"a pod\": the container's port is the declared one, got {pod:?}"
    );

    // An RBAC role, whose rules are the ones the message carried — a resource of
    // another API group, read out of the same encoding.
    let role = post(
        &config,
        "an RBAC role",
        &format!("/apis/{RBAC_API_VERSION}/namespaces/{namespace}/roles"),
        PROTOBUF,
        &protobuf_body(
            RBAC_API_VERSION,
            "Role",
            &rbac_v1::Role {
                metadata: Some(metadata("protobuf-reader", &[])),
                rules: vec![rbac_v1::PolicyRule {
                    verbs: vec!["get".to_string(), "list".to_string()],
                    api_groups: vec![String::new()],
                    resources: vec!["pods".to_string()],
                    ..Default::default()
                }],
            },
        ),
    )
    .await;
    assert_eq!(
        role.status, 201,
        "case \"an RBAC role\": expected HTTP 201, got {role:?}"
    );
    assert_eq!(
        role.body["rules"][0]["verbs"],
        serde_json::json!(["get", "list"]),
        "case \"an RBAC role\": the stored rule's verbs are the message's, got {role:?}"
    );
    assert_eq!(
        role.body["rules"][0]["resources"],
        serde_json::json!(["pods"]),
        "case \"an RBAC role\": the stored rule's resources are the message's, got {role:?}"
    );

    // Every one of them is readable afterwards: a create in this encoding stores
    // the object, it does not merely answer for it.
    // (the case, the path the object is read at, the name it was created under)
    let reads = [
        ("the namespace", format!("/api/v1/namespaces/{namespace}"), "protobuf-team"),
        (
            "the service account",
            format!("/api/v1/namespaces/{namespace}/serviceaccounts/protobuf-runner"),
            "protobuf-runner",
        ),
        (
            "the ConfigMap",
            format!("/api/v1/namespaces/{namespace}/configmaps/protobuf-settings"),
            "protobuf-settings",
        ),
        (
            "the pod",
            format!("/api/v1/namespaces/{namespace}/pods/protobuf-pod"),
            "protobuf-pod",
        ),
        (
            "the role",
            format!("/apis/{RBAC_API_VERSION}/namespaces/{namespace}/roles/protobuf-reader"),
            "protobuf-reader",
        ),
    ];
    for (case, path, name) in &reads {
        let read = dial(&config, case, "GET", path, None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the object the protobuf create stored must be readable, got {read:?}"
        );
        assert_eq!(
            read.body["metadata"]["name"],
            serde_json::json!(name),
            "case {case:?}: expected the object named {name:?}, got {read:?}"
        );
    }
}

/// RULE: a body this server cannot decode an object out of — one that claims the
/// protobuf encoding and is not in it, and one whose kind the server has no object
/// for — is the `400` of a request it could not read, not a refusal of the media
/// type it was written in.
#[tokio::test]
async fn a_body_the_server_cannot_decode_is_refused_as_one_it_could_not_read() {
    let (_router, _engine, config) = published_cluster("undecodable-cluster").await;

    // (the case, the media type stated, the body, the sentence the refusal names)
    let rows: Vec<(&str, &str, Vec<u8>, &str)> = vec![
        (
            "a body that claims the protobuf encoding and is a JSON document",
            PROTOBUF,
            serde_json::json!({ "metadata": { "name": "not-protobuf" } })
                .to_string()
                .into_bytes(),
            "protobuf parse error",
        ),
        (
            "a protobuf body whose kind this server has no object for",
            PROTOBUF,
            protobuf_body(
                CORE_API_VERSION,
                "Secret",
                &core_v1::Namespace {
                    metadata: Some(metadata("unregistered", &[])),
                    ..Default::default()
                },
            ),
            "no kind \"Secret\" is registered for version \"v1\"",
        ),
        (
            "a YAML body that is a sequence rather than an object",
            "application/yaml",
            b"- one\n- two\n".to_vec(),
            "yaml parse error",
        ),
    ];
    for (case, content_type, payload, named) in &rows {
        let refused = post(&config, case, "/api/v1/namespaces", content_type, payload).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {refused:?}"
        );
        assert_eq!(
            refused.body["reason"],
            serde_json::json!("BadRequest"),
            "case {case:?}: a body the server could not read is a BadRequest, got {refused:?}"
        );
        let message = refused.body["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains(named),
            "case {case:?}: expected the refusal to name {named:?}, got {message:?}"
        );
    }

    // A media type this server reads NONE of is the refusal of the media type
    // itself, and the set it advertises there is the set it really reads — every
    // media type in it is accepted by the rows above.
    let unread = post(
        &config,
        "a media type nothing writes a Kubernetes object in",
        "/api/v1/namespaces",
        "application/octet-stream",
        b"\x01\x02\x03",
    )
    .await;
    assert_eq!(
        unread.status, 415,
        "case \"an unread media type\": expected HTTP 415, got {unread:?}"
    );
    assert_eq!(
        unread.body["reason"],
        serde_json::json!("UnsupportedMediaType"),
        "case \"an unread media type\": the refusal names the body's format, got {unread:?}"
    );
    let advertised = unread.body["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    for media_type in READ_MEDIA_TYPES {
        assert!(
            advertised.contains(media_type),
            "case \"an unread media type\": the refusal must advertise {media_type:?}, got \
             {advertised:?}"
        );
    }
}
