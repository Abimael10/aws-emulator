// SPDX-License-Identifier: CC0-1.0

//! The OPENAPI documents a cluster's Kubernetes API server publishes, read over the
//! address the cluster itself publishes.
//!
//! The RULE under test: **a Kubernetes client validates a manifest against the
//! schema the API server publishes before it sends one, so the server publishes an
//! OpenAPI document for every group version it serves — the v3 index and its
//! per-group-version documents, and the one Swagger 2.0 document an older client
//! reads — each carrying one schema per kind, tagged with the group, version and
//! kind a client resolves its manifest by, and stating what this server REQUIRES of
//! that kind.** A server that publishes none cannot be driven by a stock client at
//! all: `kubectl apply` stops at "error validating data: failed to download
//! openapi" before it sends anything.
//!
//! The rows differ along what the rule generalises over: the group version (the core
//! API and the two groups beside it), the kind inside it, and which document is read
//! (the v3 index, a v3 group-version document, the v2 document) — plus the two
//! negative rows a client can reach: a group version this server publishes no
//! document for, and a method the documents are not served under.
//!
//! The last suite ties the published schema to the server's own admission: the
//! members the Pod schema declares `required` are exactly the ones a create is
//! refused without, so a manifest that validates is one this server accepts.

use serde_json::{json, Value};
use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_pod_spec::MAX_CONTAINERS_PER_POD;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{account_bearer_token, dial, provisioned_cluster, Kubeconfig};
use crate::wire::emulator;

/// One request to the cluster's published address IN THE CLEAR, stating the
/// encodings the client can read: the published documents are the one surface a
/// client negotiates an encoding for, and the harness every other case here uses
/// states none.
///
/// The address answers both a TLS client and one that verifies nothing (the first
/// byte of the connection says which), so this reaches the same server the TLS
/// cases do. It presents the account's own credential, because an API server
/// authenticates every request before it negotiates anything
/// ([`crate::kubernetes_anonymous_requests`]).
async fn dial_accepting(config: &Kubeconfig, case: &str, path: &str, accept: &str) -> (u16, Value) {
    let authority = config
        .endpoint
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: expected an https endpoint"))
        .to_string();
    let (_, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: the endpoint must carry a port"));
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("case {case:?}: {port:?} must be a port: {error}"));
    let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the address must accept: {error}"));
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: {}\r\nAccept: {accept}\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n",
        account_bearer_token()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request must be written: {error}"));
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the answer must be read: {error}"));
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: expected an HTTP answer, got {text:?}"));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {head:?}"));
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// The group versions this server serves, as the index names each: the path under
/// `/openapi/v3`, and the `(group, version)` a schema of it is tagged with.
///
/// SOURCE: the discovery documents this server serves — `/api` names the core
/// version and `/apis` names the two groups beside it — so the table is the surface
/// a client can address rather than a list written beside it.
const GROUP_VERSIONS: &[(&str, &str, &str)] = &[
    ("api/v1", "", "v1"),
    (
        "apis/rbac.authorization.k8s.io/v1",
        "rbac.authorization.k8s.io",
        "v1",
    ),
    (
        "apis/authentication.k8s.io/v1",
        "authentication.k8s.io",
        "v1",
    ),
];

/// One kind the documents must carry a schema for: the group version it belongs to,
/// the kind, and the schema name both documents key it by.
///
/// SOURCE: the resources the API server serves — the core group's resource list, the
/// RBAC group's and the authentication group's — one row per kind a client sends or
/// reads.
const KINDS: &[(&str, &str, &str)] = &[
    ("api/v1", "Pod", "io.k8s.api.core.v1.Pod"),
    ("api/v1", "Namespace", "io.k8s.api.core.v1.Namespace"),
    ("api/v1", "ConfigMap", "io.k8s.api.core.v1.ConfigMap"),
    (
        "api/v1",
        "ServiceAccount",
        "io.k8s.api.core.v1.ServiceAccount",
    ),
    ("api/v1", "Event", "io.k8s.api.core.v1.Event"),
    ("api/v1", "Node", "io.k8s.api.core.v1.Node"),
    (
        "apis/rbac.authorization.k8s.io/v1",
        "Role",
        "io.k8s.api.rbac.v1.Role",
    ),
    (
        "apis/rbac.authorization.k8s.io/v1",
        "RoleBinding",
        "io.k8s.api.rbac.v1.RoleBinding",
    ),
    (
        "apis/rbac.authorization.k8s.io/v1",
        "ClusterRole",
        "io.k8s.api.rbac.v1.ClusterRole",
    ),
    (
        "apis/rbac.authorization.k8s.io/v1",
        "ClusterRoleBinding",
        "io.k8s.api.rbac.v1.ClusterRoleBinding",
    ),
    (
        "apis/authentication.k8s.io/v1",
        "TokenRequest",
        "io.k8s.api.authentication.v1.TokenRequest",
    ),
    (
        "apis/authentication.k8s.io/v1",
        "SelfSubjectReview",
        "io.k8s.api.authentication.v1.SelfSubjectReview",
    ),
];

/// The relative URL the v3 index publishes one group version's document at, read out
/// of the index the way a client does.
fn served_url(index: &Value, path: &str) -> String {
    index["paths"][path]["serverRelativeURL"]
        .as_str()
        .unwrap_or_else(|| panic!("case {path:?}: the index must publish a document, got {index}"))
        .to_string()
}

/// One published document, read over the cluster's own address.
async fn document(config: &Kubeconfig, case: &str, path: &str) -> Value {
    let answered = dial(config, case, "GET", path, None).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: {path} must be served, got {answered:?}"
    );
    answered.body
}

#[tokio::test]
async fn every_group_version_this_server_serves_publishes_an_openapi_document() {
    let router = emulator();
    let config = provisioned_cluster(&router, REGION, "openapi-host").await;

    let index = document(&config, "the v3 index", "/openapi/v3").await;
    for (path, group, version) in GROUP_VERSIONS {
        let case = format!("the v3 document of {path}");
        let url = served_url(&index, path);
        assert!(
            url.starts_with(&format!("/openapi/v3/{path}?")),
            "case {case:?}: the index must publish the document under its own path, got {url:?}"
        );
        let published = document(&config, &case, &url).await;
        assert_eq!(
            published["openapi"], json!("3.0.0"),
            "case {case:?}: a v3 document declares the specification version, got {published}"
        );
        let schemas = &published["components"]["schemas"];
        // Every kind of THIS group version is in it, tagged with the group and the
        // version a client resolves its manifest by.
        for (kind_path, kind, name) in KINDS.iter().filter(|(kind_path, _, _)| kind_path == path) {
            let schema = &schemas[*name];
            assert_eq!(
                schema["type"], json!("object"),
                "case {case:?}: {name} must be an object schema, got {schema}"
            );
            assert_eq!(
                schema["x-kubernetes-group-version-kind"],
                json!([{ "group": group, "version": version, "kind": kind }]),
                "case {case:?}: {name} must be tagged with the kind it is for, got {schema}"
            );
            assert_eq!(
                kind_path, path,
                "case {case:?}: the row belongs to this group version"
            );
        }
    }

    // The ONE Swagger 2.0 document an older client reads carries the same schemas,
    // under the member that document declares them in and referencing them the way
    // that document does.
    let swagger = document(&config, "the v2 document", "/openapi/v2").await;
    assert_eq!(
        swagger["swagger"], json!("2.0"),
        "case the v2 document: it declares the Swagger version, got {swagger}"
    );
    for (_, kind, name) in KINDS {
        let schema = &swagger["definitions"][*name];
        assert_eq!(
            schema["type"], json!("object"),
            "case {name:?}: the v2 document must define every kind, got {schema}"
        );
        assert_eq!(
            schema["x-kubernetes-group-version-kind"][0]["kind"],
            json!(kind),
            "case {name:?}: the definition must name the kind it is for, got {schema}"
        );
    }
    let rendered = swagger.to_string();
    assert!(
        !rendered.contains("#/components/schemas/"),
        "case the v2 document: it must reference its own definitions, got {rendered}"
    );
    assert!(
        rendered.contains("#/definitions/"),
        "case the v2 document: it must reference its definitions, got {rendered}"
    );

    // A group version this server publishes no document for, and a method the
    // documents are not served under: both are refused rather than answered empty.
    let missing = dial(
        &config,
        "a group version no document is published for",
        "GET",
        "/openapi/v3/apis/apps/v1",
        None,
    )
    .await;
    assert_eq!(
        missing.status, 404,
        "case a group version no document is published for: expected the not-found \
         Status, got {missing:?}"
    );
    let posted = dial(&config, "a write to the index", "POST", "/openapi/v3", None).await;
    assert_eq!(
        posted.status, 405,
        "case a write to the index: the documents are read, got {posted:?}"
    );

    // AN ENCODING THIS SERVER DOES NOT WRITE: the documents are the one surface a
    // client negotiates an encoding for, so an `Accept` naming only an encoding this
    // server does not produce is refused as unacceptable rather than answered with a
    // body the client cannot decode — while every `Accept` that admits JSON is served.
    let negotiations: [(&str, &str, u16); 7] = [
        (
            "an Accept naming only another encoding",
            "application/com.github.proto-openapi.spec.v2@v1.0+protobuf",
            406,
        ),
        ("an Accept naming JSON", "application/json", 200),
        (
            "an Accept naming another encoding and JSON",
            "application/com.github.proto-openapi.spec.v2@v1.0+protobuf, application/json",
            200,
        ),
        ("an Accept taking anything", "*/*", 200),
        (
            "an Accept naming JSON with parameters",
            "application/json;q=0.9",
            200,
        ),
        (
            "an Accept that refuses JSON outright",
            "application/json;q=0",
            406,
        ),
        (
            "an Accept that refuses everything else but takes JSON",
            "*/*;q=0, application/json",
            200,
        ),
    ];
    for (case, accept, expected) in negotiations {
        let (status, body) = dial_accepting(&config, case, "/openapi/v2", accept).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected {expected} for {accept:?}, got {status} with {body}"
        );
        if expected == 406 {
            assert_eq!(
                body["message"],
                json!("only the following media types are accepted: application/json"),
                "case {case:?}: the refusal names what is accepted, got {body}"
            );
        } else {
            assert_eq!(
                body["swagger"], json!("2.0"),
                "case {case:?}: the document must be served, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn the_published_pod_schema_requires_what_a_create_is_refused_without() {
    let router = emulator();
    let config = provisioned_cluster(&router, REGION, "openapi-admission").await;

    let index = document(&config, "the v3 index", "/openapi/v3").await;
    let core = document(
        &config,
        "the core document",
        &served_url(&index, "api/v1"),
    )
    .await;
    let schemas = &core["components"]["schemas"];
    assert_eq!(
        schemas["io.k8s.api.core.v1.PodSpec"]["required"],
        json!(["containers"]),
        "case the Pod spec: the schema must require the container list, got {schemas}"
    );
    assert_eq!(
        schemas["io.k8s.api.core.v1.Container"]["required"],
        json!(["name", "image"]),
        "case a container: the schema must require the members a container is \
         refused without, got {schemas}"
    );

    // What the schema requires is what the server requires: each row leaves out one
    // of those members and is refused, while the manifest that carries them all is
    // admitted.
    let rows: [(&str, Value); 3] = [
        (
            "a pod with no container list",
            json!({ "metadata": { "name": "no-containers" }, "spec": {} }),
        ),
        (
            "a container with no name",
            json!({
                "metadata": { "name": "no-container-name" },
                "spec": { "containers": [{ "image": "registry.example.com/app:1" }] },
            }),
        ),
        (
            "a container with no image",
            json!({
                "metadata": { "name": "no-container-image" },
                "spec": { "containers": [{ "name": "app" }] },
            }),
        ),
    ];
    for (case, manifest) in rows {
        let answered = dial(
            &config,
            case,
            "POST",
            "/api/v1/namespaces/default/pods",
            Some(&manifest),
        )
        .await;
        assert_eq!(
            answered.status, 422,
            "case {case:?}: the server must refuse what its schema requires, got {answered:?}"
        );
    }
    let admitted = dial(
        &config,
        "a pod carrying every required member",
        "POST",
        "/api/v1/namespaces/default/pods",
        Some(&json!({
            "metadata": { "name": "validated" },
            "spec": { "containers": [{ "name": "app", "image": "registry.example.com/app:1" }] },
        })),
    )
    .await;
    assert_eq!(
        admitted.status, 201,
        "case a pod carrying every required member: it must be admitted, got {admitted:?}"
    );

    // AND WHAT IT HOLDS A LIST TO: a pod may declare at most as many containers as
    // this server holds a pod to, because every container is one more image its
    // node's kubelet pulls. The bound itself and one past it are both rows.
    let containers = |count: usize| -> Value {
        json!({
            "metadata": { "name": format!("containers-{count}") },
            "spec": {
                "containers": (0..count)
                    .map(|index| json!({
                        "name": format!("app-{index}"),
                        "image": "registry.example.com/app:1",
                    }))
                    .collect::<Vec<Value>>(),
            },
        })
    };
    let at_the_bound = dial(
        &config,
        "a pod at the container bound",
        "POST",
        "/api/v1/namespaces/default/pods",
        Some(&containers(MAX_CONTAINERS_PER_POD)),
    )
    .await;
    assert_eq!(
        at_the_bound.status, 201,
        "case a pod at the container bound: it must be admitted, got {at_the_bound:?}"
    );
    let past_the_bound = dial(
        &config,
        "a pod past the container bound",
        "POST",
        "/api/v1/namespaces/default/pods",
        Some(&containers(MAX_CONTAINERS_PER_POD + 1)),
    )
    .await;
    assert_eq!(
        past_the_bound.status, 422,
        "case a pod past the container bound: it must be refused, got {past_the_bound:?}"
    );
    assert_eq!(
        past_the_bound.body["message"],
        json!(format!(
            "Pod \"containers-{}\" is invalid: spec.containers: Too many: {}: must have at \
             most {MAX_CONTAINERS_PER_POD} items",
            MAX_CONTAINERS_PER_POD + 1,
            MAX_CONTAINERS_PER_POD + 1
        )),
        "case a pod past the container bound: the refusal names the list and the limit, got \
         {past_the_bound:?}"
    );
}
