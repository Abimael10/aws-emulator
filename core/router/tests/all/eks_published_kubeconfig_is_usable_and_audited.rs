// SPDX-License-Identifier: CC0-1.0

//! The kubeconfig an Amazon EKS cluster PUBLISHES, used the way the API documents
//! it — and the audit trail the requests made over it leave behind.
//!
//! RULE: a value the control plane publishes for a client to DIAL comes with the
//! material that verifies it. `Cluster$endpoint` is "the endpoint for your
//! Kubernetes API server" and `Certificate$data` is "the Base64-encoded certificate
//! data required to communicate with your cluster. Add this to the
//! `certificate-authority-data` section of the kubeconfig file for your cluster"
//! (`smithy_query(query="shape", shape="Certificate")`) — so Base64-decoding that
//! member yields the PEM certificate of the authority that signed the certificate
//! the published endpoint presents: a client that installs it as its ONLY trust
//! anchor completes the handshake, is served the Kubernetes API, and — while the
//! `audit` control plane log type is enabled (`LogSetup$enabled`) — leaves one
//! record per request in the cluster's own log group, naming the username the
//! cluster's access entry maps its IAM principal to and the authorizer's decision,
//! for an admitted and a refused request alike.
//!
//! Four services have to agree for that to hold — EKS issues the authority, serves
//! the endpoint under it and writes the access entry, IAM holds the user the entry
//! names, STS says which principal a credential speaks for, and CloudWatch Logs
//! holds the records — so this is the suite that can pin it. The EKS crate's own
//! `certificate_authority_material.rs` reads what the member decodes to; this one
//! USES it, which is the one thing a decoded certificate either does or does not do.
//!
//! The client is deliberately a raw one: a TCP connection to the port the cluster
//! published, a TLS handshake whose trust anchor is nothing but the published
//! authority, and an HTTP/1.1 request written by hand — so no trust store, no
//! environment and no other certificate of this emulator can stand in for what
//! `DescribeCluster` published. It is synchronous, driven on a blocking thread while
//! the emulator's own listener runs on the test runtime.
//!
//! The rows differ along what the rule generalises over: which cluster published the
//! kubeconfig, the server name the client dials the endpoint by, whose anchor it
//! trusts (its own and another cluster's), the verb and resource a request addresses,
//! and the decision the cluster's RBAC objects take on it.

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, TcpStream};
use std::sync::Arc;

use rustls::pki_types::pem::PemObject as _;
use serde_json::{json, Value};

use crate::eks_kubernetes_api_rbac_audit::{
    bearer_token, ApiAnswer, Emulator, AMBIENT_KEY, POLL_ATTEMPTS, POLL_WAIT,
};

/// What a client reads out of `DescribeCluster` to build a kubeconfig: the address
/// the cluster publishes for its Kubernetes API server, and the certificate
/// authority published beside it — decoded the way a kubeconfig's
/// `certificate-authority-data` is.
#[derive(Debug, Clone)]
struct Kubeconfig {
    endpoint: String,
    authority_pem: String,
}

/// The namespace the admitted rows address.
const NAMESPACE: &str = "tls-team";

/// The Kubernetes group the access entry puts its principal in, and which the
/// namespace's `RoleBinding` binds.
const BOUND_GROUP: &str = "tls-team-writers";

/// The username the access entry maps its IAM principal to, which is what every
/// audit record of that principal's requests must name.
const MAPPED_USERNAME: &str = "tls-team-user";

/// The `Role` the decision rows are judged by: the verbs it names on config maps of
/// its own namespace, and nothing else.
fn configmap_role() -> Value {
    json!({
        "metadata": { "name": "configmap-writer" },
        "rules": [{
            "apiGroups": [""],
            "resources": ["configmaps"],
            "verbs": ["get", "list", "create"],
        }],
    })
}

/// The `RoleBinding` that binds that role to the group the access entry names.
fn group_binding() -> Value {
    json!({
        "metadata": { "name": "configmap-writers" },
        "roleRef": {
            "apiGroup": "rbac.authorization.k8s.io",
            "kind": "Role",
            "name": "configmap-writer",
        },
        "subjects": [{ "kind": "Group", "name": BOUND_GROUP }],
    })
}

/// Create the cluster `cluster` with the `audit` control plane log type enabled,
/// wait for the clock-owed `ACTIVE` move, and answer the kubeconfig it published:
/// the endpoint, and the authority beside it Base64-decoded to its PEM.
async fn published_kubeconfig(emulator: &Emulator, cluster: &str, case: &str) -> Kubeconfig {
    let body = json!({
        "name": cluster,
        "roleArn": format!("arn:aws:iam::{}:role/eksClusterRole", engine::emulator_account_id()),
        "resourcesVpcConfig": { "subnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"] },
        "accessConfig": { "authenticationMode": "API_AND_CONFIG_MAP" },
        "logging": { "clusterLogging": [{ "types": ["audit"], "enabled": true }] },
    });
    let (status, parsed) = emulator.eks(AMBIENT_KEY, "POST", "/clusters", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the cluster must be created, got {status} with {parsed}"
    );
    for _ in 0..POLL_ATTEMPTS {
        let (status, parsed) = emulator
            .eks(AMBIENT_KEY, "GET", &format!("/clusters/{cluster}"), json!({}))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the cluster must be readable, got {status} with {parsed}"
        );
        let described = &parsed["cluster"];
        if described["status"] == json!("ACTIVE") {
            if let Some(endpoint) = described["endpoint"].as_str() {
                let data = described["certificateAuthority"]["data"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the cluster must publish its certificate authority, \
                             got {described:?}"
                        )
                    });
                return Kubeconfig {
                    endpoint: endpoint.to_string(),
                    authority_pem: authority_pem(case, data),
                };
            }
        }
        tokio::time::sleep(POLL_WAIT).await;
    }
    panic!("case {case:?}: the cluster never published an endpoint while ACTIVE");
}

/// `certificateAuthority.data` read the way a kubeconfig's
/// `certificate-authority-data` is: Base64-decoded to the PEM a client installs.
fn authority_pem(case: &str, data: &str) -> String {
    let decoded = aws_smithy_types::base64::decode(data).unwrap_or_else(|error| {
        panic!("case {case:?}: the published authority must be Base64, got {data:?} ({error})")
    });
    let pem = String::from_utf8(decoded).unwrap_or_else(|error| {
        panic!("case {case:?}: the decoded certificate data must be text ({error})")
    });
    assert!(
        pem.starts_with("-----BEGIN CERTIFICATE-----"),
        "case {case:?}: the decoded data must be a PEM certificate block, got {pem:?}"
    );
    pem
}

/// The host and the port a published `https://` endpoint is dialled at.
fn address(case: &str, endpoint: &str) -> (String, u16) {
    let authority = endpoint.strip_prefix("https://").unwrap_or_else(|| {
        panic!("case {case:?}: the endpoint must be an https URL, got {endpoint:?}")
    });
    let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: the endpoint must carry a port, got {authority:?}")
    });
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("case {case:?}: {port:?} must be a port: {error}"));
    (host.to_string(), port)
}

/// One exchange on the cluster's published port, over a TLS connection whose ONLY
/// trust anchor is `authority_pem` and whose server name is `server_name`.
///
/// `request` is `None` for a row that asks nothing but whether the handshake is
/// accepted. Synchronous: it is run on a blocking thread so the emulator's listener,
/// which lives on the test runtime, is free to answer it.
fn tls_exchange(
    server_name: String,
    port: u16,
    authority_pem: String,
    request: Option<String>,
) -> Result<String, String> {
    let mut roots = rustls::RootCertStore::empty();
    let certificate = rustls::pki_types::CertificateDer::from_pem_slice(authority_pem.as_bytes())
        .map_err(|error| format!("the published authority must be a PEM certificate: {error}"))?;
    roots
        .add(certificate)
        .map_err(|error| format!("the published authority must load as a trust anchor: {error}"))?;
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| format!("the TLS client must configure: {error}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let dialled = rustls::pki_types::ServerName::try_from(server_name.clone())
        .map_err(|error| format!("{server_name:?} must be a server name: {error}"))?;
    let mut connection = rustls::ClientConnection::new(Arc::new(config), dialled)
        .map_err(|error| format!("the TLS client must connect: {error}"))?;
    // The emulator's OWN listener, on the port it published. A loopback literal, so
    // this suite dials nothing but this machine.
    let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .map_err(|error| format!("the published address must accept: {error}"))?;
    connection
        .complete_io(&mut socket)
        .map_err(|error| format!("the handshake must complete: {error}"))?;
    let Some(request) = request else {
        return Ok(String::new());
    };
    let mut stream = rustls::StreamOwned::new(connection, socket);
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("the request must be written: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("the request must be flushed: {error}"))?;
    let mut raw = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => raw.extend_from_slice(&buffer[..read]),
            // A server that closes without a TLS close notification has still sent
            // its whole answer, which is what this read is after.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                break;
            }
            Err(error) => return Err(format!("the answer must be read: {error}")),
        }
    }
    Ok(String::from_utf8_lossy(&raw).to_string())
}

/// Whether a client trusting nothing but `authority_pem` accepts what the cluster's
/// published endpoint presents, dialling it by `server_name`.
async fn handshake(
    kubeconfig: &Kubeconfig,
    case: &str,
    server_name: &str,
    authority_pem: &str,
) -> Result<(), String> {
    let (_, port) = address(case, &kubeconfig.endpoint);
    let server_name = server_name.to_string();
    let authority_pem = authority_pem.to_string();
    tokio::task::spawn_blocking(move || tls_exchange(server_name, port, authority_pem, None))
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the TLS client must run: {error}"))
        .map(|_| ())
}

/// One request to the cluster's published Kubernetes API address, as a client built
/// from the published kubeconfig makes it: over TLS verified by the published
/// authority alone, carrying the bearer token the row presents.
async fn k8s(
    kubeconfig: &Kubeconfig,
    case: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> ApiAnswer {
    let (host, port) = address(case, &kubeconfig.endpoint);
    let payload = body.map(|body| body.to_string()).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Authorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let authority_pem = kubeconfig.authority_pem.clone();
    let server_name = host.clone();
    let text = tokio::task::spawn_blocking(move || {
        tls_exchange(server_name, port, authority_pem, Some(request))
    })
    .await
    .unwrap_or_else(|error| panic!("case {case:?}: the TLS client must run: {error}"))
    .unwrap_or_else(|error| panic!("case {case:?}: the published kubeconfig must work: {error}"));
    let (head, answered) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: expected an HTTP answer, got {text:?}"));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {head:?}"));
    ApiAnswer {
        status,
        body: serde_json::from_str(answered).unwrap_or(Value::Null),
    }
}

/// The path of a namespaced RBAC collection.
fn rbac_path(namespace: &str, resource: &str) -> String {
    format!("/apis/rbac.authorization.k8s.io/v1/namespaces/{namespace}/{resource}")
}

/// One row of the decision table: a request the mapped identity makes over the
/// published kubeconfig, the status the cluster's own RBAC must answer it with, and
/// what its audit record must say about it.
struct DecisionRow {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
    /// The Kubernetes verb the request addresses, as the record names it.
    verb: &'static str,
    /// The resource it addresses, as the record names it.
    resource: &'static str,
    /// The authorizer's decision the record must carry.
    decision: &'static str,
}

// ---------------------------------------------------------------------------
// RULE: the authority a cluster publishes beside its endpoint is the one that
// verifies THAT endpoint — a client trusting nothing else completes the handshake
// and is served, and a client trusting another cluster's authority is not.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_published_authority_is_the_one_that_verifies_the_published_endpoint() {
    let emulator = Emulator::new();

    // Two clusters, each publishing a kubeconfig of its own, so the pair is shown to
    // be per cluster rather than per emulator.
    let first = published_kubeconfig(&emulator, "kubeconfig-tls-one", "the first cluster").await;
    let second = published_kubeconfig(&emulator, "kubeconfig-tls-two", "the second cluster").await;
    assert_ne!(
        first.authority_pem, second.authority_pem,
        "every cluster publishes a certificate authority of its own, got one twice: {:?}",
        first.authority_pem
    );

    // The names a client dials this machine's endpoint by: the host the cluster
    // published, and the loopback name a client on the same machine resolves it to.
    for (case, kubeconfig) in [
        ("the first cluster", &first),
        ("the second cluster", &second),
    ] {
        let (published_host, _) = address(case, &kubeconfig.endpoint);
        for server_name in [published_host.as_str(), "localhost"] {
            let accepted = handshake(kubeconfig, case, server_name, &kubeconfig.authority_pem).await;
            assert!(
                accepted.is_ok(),
                "case {case:?} dialled as {server_name:?}: a client trusting only the published \
                 authority must complete the handshake, got {accepted:?}"
            );
        }

        // The connection that handshake opened serves the Kubernetes API, which is
        // what the published pair is for.
        let served = k8s(
            kubeconfig,
            case,
            &crate::eks_wire::account_kubernetes_bearer_token(),
            "GET",
            "/version",
            None,
        )
        .await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: the verified connection must serve the API, got {served:?}"
        );
        assert!(
            served.body["gitVersion"].is_string(),
            "case {case:?}: the version document must name the served version, got {:?}",
            served.body
        );
    }

    // The anchor is specific: one cluster's authority does not verify the other's
    // endpoint, so what each publishes is its OWN signer rather than one certificate
    // every cluster hands out.
    for (case, kubeconfig, other) in [
        ("the first cluster's endpoint under the second's anchor", &first, &second),
        ("the second cluster's endpoint under the first's anchor", &second, &first),
    ] {
        let (published_host, _) = address(case, &kubeconfig.endpoint);
        let refused = handshake(kubeconfig, case, &published_host, &other.authority_pem).await;
        assert!(
            refused.is_err(),
            "case {case:?}: another cluster's authority must not verify this endpoint, got \
             {refused:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: every request made over the published kubeconfig is recorded in the
// cluster's own audit log under the username its access entry maps the IAM
// principal to, with the decision the authorizer took — admitted or refused alike.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn requests_over_the_published_kubeconfig_are_audited_under_the_entry_username() {
    let emulator = Emulator::new();
    let case = "a cluster reached over its published kubeconfig";
    let cluster = "kubeconfig-tls-audited";
    let kubeconfig = published_kubeconfig(&emulator, cluster, case).await;
    let creator = crate::eks_wire::account_kubernetes_bearer_token();

    // The cluster's creator sets it up over the same verified connection: the
    // namespace, the Role, and the binding that names the group the access entry
    // puts its principal in.
    let created = k8s(
        &kubeconfig,
        case,
        &creator,
        "POST",
        "/api/v1/namespaces",
        Some(json!({ "metadata": { "name": NAMESPACE } })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the namespace must be created, got {created:?}"
    );
    for (path, manifest) in [
        (rbac_path(NAMESPACE, "roles"), configmap_role()),
        (rbac_path(NAMESPACE, "rolebindings"), group_binding()),
    ] {
        let created = k8s(&kubeconfig, case, &creator, "POST", &path, Some(manifest)).await;
        assert_eq!(
            created.status, 201,
            "setup: {path} must be created, got {created:?}"
        );
    }

    // The IAM principal the cluster maps, and the Kubernetes identity it is served as.
    let (principal_arn, access_key) = emulator.user_with_key("kubeconfig-tls-user", case).await;
    emulator
        .access_entry(cluster, &principal_arn, MAPPED_USERNAME, &[BOUND_GROUP], case)
        .await;
    let token = bearer_token(&access_key);

    // The rows: the verb and resource a request addresses, and the decision the
    // bound Role takes on it — two the Role names and two it does not.
    let rows = vec![
        DecisionRow {
            case: "a create the bound Role names",
            method: "POST",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
            body: Some(json!({ "metadata": { "name": "audited" }, "data": { "key": "value" } })),
            status: 201,
            verb: "create",
            resource: "configmaps",
            decision: "allow",
        },
        DecisionRow {
            case: "a read of one object the Role names",
            method: "GET",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps/audited"),
            body: None,
            status: 200,
            verb: "get",
            resource: "configmaps",
            decision: "allow",
        },
        DecisionRow {
            case: "a verb the Role does not name",
            method: "DELETE",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps/audited"),
            body: None,
            status: 403,
            verb: "delete",
            resource: "configmaps",
            decision: "forbid",
        },
        DecisionRow {
            case: "a resource the Role does not name",
            method: "GET",
            path: format!("/api/v1/namespaces/{NAMESPACE}/secrets"),
            body: None,
            status: 403,
            verb: "list",
            resource: "secrets",
            decision: "forbid",
        },
    ];

    for row in &rows {
        let case = row.case;
        let answered = k8s(
            &kubeconfig,
            case,
            &token,
            row.method,
            &row.path,
            row.body.clone(),
        )
        .await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {}, got {answered:?}",
            row.status
        );
        if row.status == 403 {
            let message = answered.body["message"].as_str().unwrap_or_default();
            assert!(
                message.contains(MAPPED_USERNAME),
                "case {case:?}: the refusal must name the identity it was taken for, got \
                 {message:?}"
            );
        }
    }

    // The records, read back the way an operator auditing the cluster reads them. The
    // delivery is a call to another service, so the wait is for the CONDITION: look
    // again until every row's record is there or a generous deadline passes.
    let mut records = Vec::new();
    for _ in 0..POLL_ATTEMPTS {
        records = emulator.audit_records(cluster, case).await;
        let delivered = rows
            .iter()
            .filter(|row| {
                records.iter().any(|record| {
                    record["verb"] == json!(row.verb)
                        && record["objectRef"]["resource"] == json!(row.resource)
                })
            })
            .count();
        if delivered == rows.len() {
            break;
        }
        tokio::time::sleep(POLL_WAIT).await;
    }

    for row in &rows {
        let case = row.case;
        let found: Vec<&Value> = records
            .iter()
            .filter(|record| {
                record["verb"] == json!(row.verb)
                    && record["objectRef"]["resource"] == json!(row.resource)
                    && record["objectRef"]["namespace"] == json!(NAMESPACE)
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: expected exactly one audit record of {:?} on {:?}, got {records:?}",
            row.verb,
            row.resource
        );
        let record = found[0];
        assert_eq!(
            record["user"]["username"],
            json!(MAPPED_USERNAME),
            "case {case:?}: the record must name the access entry's username, got {record:?}"
        );
        let groups: Vec<String> = record["user"]["groups"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|group| group.as_str().map(str::to_owned))
            .collect();
        assert!(
            groups.contains(&BOUND_GROUP.to_string()),
            "case {case:?}: the record must name the entry's Kubernetes groups, got {groups:?}"
        );
        assert_eq!(
            record["annotations"]["authorization.k8s.io/decision"],
            json!(row.decision),
            "case {case:?}: the record must name the authorizer's decision, got {record:?}"
        );
        assert_eq!(
            record["responseStatus"]["code"],
            json!(row.status),
            "case {case:?}: the record must name the status the request was answered with, got \
             {record:?}"
        );
        assert_eq!(
            record["apiVersion"],
            json!("audit.k8s.io/v1"),
            "case {case:?}: the record is a Kubernetes audit event, got {record:?}"
        );
    }
}
