// SPDX-License-Identifier: CC0-1.0

//! An Amazon EKS cluster's KUBERNETES API: who a request is served as, what the
//! cluster's RBAC objects let that identity do, and the audit record each request
//! leaves behind.
//!
//! RULE: a cluster's Kubernetes API server serves the `rbac.authorization.k8s.io/v1`
//! group — its discovery documents and its four resources — and every request it
//! answers is decided in the API server's own order: the credential names an IAM
//! principal, the cluster's ACCESS ENTRY for that principal is the Kubernetes identity
//! the request is served as (`AccessEntry$username` is "the name of a user that can
//! authenticate to your cluster", `AccessEntry$kubernetesGroups` "a list of Kubernetes
//! groups that the principal is a member of"), the RBAC objects and the access
//! policies associated to that entry decide whether that identity may do what the
//! request addresses, and — while the `audit` control plane log type is enabled
//! (`LogSetup$enabled`) — the request is recorded in the cluster's own log group
//! `/aws/eks/<cluster name>/cluster`, naming that identity and the decision taken.
//!
//! Four services have to agree for that to hold — IAM holds the user the entry names,
//! STS says which principal a credential speaks for, EKS serves the cluster's API and
//! writes the entry, CloudWatch Logs holds the audit records — so this is the suite
//! that can pin it; the EKS crate's own `kubernetes_api.rs` suite pins what the API
//! answers with no other service mounted.
//!
//! The client is this suite's own, and deliberately so: the XML readers and the form
//! encoder come from `query_wire.rs`, but the request driver cannot be `eks_wire.rs`'s
//! `send` — that one is SYNCHRONOUS (it blocks a `tokio::runtime::Runtime` handed to
//! it) and signs every request with one fixed access key id, while these rows are
//! `async` and must sign as the IAM user whose key the access entry maps, one row at a
//! time. The Kubernetes half is a raw HTTP/1.1 exchange on the port the cluster
//! published, so nothing but what the emulator announced is used to reach it.
//!
//! The rows differ along what the rule generalises over: the verb and the resource a
//! request addresses, the namespace it addresses them in, whether the identity's
//! binding names it by group or by user, whether the rule that would admit it is in a
//! `Role` or a `ClusterRole`, and which principal the request's credential speaks for
//! (an IAM user the cluster maps, one it does not, and the cluster's creator).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::query_wire::{element, form_encode};

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The signing names the router picks each service by.
const EKS: &str = "eks";
const LOGS: &str = "logs";

/// The `X-Amz-Target` prefix CloudWatch Logs names an operation under.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// A key id no store holds, which is the account's own published credential — the
/// credential the cluster below is created with, so the cluster creator's access entry
/// is filed under the principal it speaks for.
pub(crate) const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// An identity-based policy allowing every Amazon EKS action, which an IAM user this
/// suite creates needs to call the service at all.
const EKS_ADMIN_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Action":"eks:*","Resource":"*"}]}"#
);

/// How long the suite waits between looks at a condition it is waiting for.
pub(crate) const POLL_WAIT: Duration = Duration::from_millis(100);

/// How many times it looks, which bounds every wait here far above the spans the
/// emulator owes.
pub(crate) const POLL_ATTEMPTS: usize = 150;

/// The four services this rule spans, over ONE engine.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

/// One answer from a cluster's Kubernetes API address.
#[derive(Debug)]
pub(crate) struct ApiAnswer {
    pub(crate) status: u16,
    pub(crate) body: Value,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These rows exercise handler behaviour, not request signing.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_eks_awswesleyfrontend::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        Self { router }
    }
}

/// One AWS call this suite drives, as one value: the service it addresses, the
/// credential it is signed with, and the request itself.
///
/// Carried as a struct rather than as a parameter each, because a call is one thing
/// and the drivers below differ only in a field or two of it.
struct AwsCall<'call> {
    signing_name: &'call str,
    access_key_id: &'call str,
    method: &'call str,
    uri: &'call str,
    content_type: &'call str,
    /// The `X-Amz-Target` an RPC protocol names its operation under, and `None` for
    /// the REST ones.
    target: Option<String>,
    body: String,
}

impl Emulator {
    /// One request to the service a call names, signed with the credential it names —
    /// the key id a service reads the caller's identity from. The signature is never
    /// checked: this suite runs with authentication off.
    async fn dispatch(&self, call: AwsCall<'_>) -> (u16, Value) {
        let AwsCall {
            signing_name,
            access_key_id,
            method,
            uri,
            content_type,
            target,
            body,
        } = call;
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        );
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("authorization", authorization)
            .header("content-type", content_type);
        if let Some(target) = target {
            request = request.header("x-amz-target", target);
        }
        let response = self
            .router
            .dispatch(
                request
                    .body(Bytes::from(body))
                    .expect("a well-formed request"),
            )
            .await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let parsed = if raw.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One EKS call (restJson1) as `access_key_id`.
    pub(crate) async fn eks(
        &self,
        access_key_id: &str,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (u16, Value) {
        self.dispatch(AwsCall {
            signing_name: EKS,
            access_key_id,
            method,
            uri,
            content_type: "application/json",
            target: None,
            body: body.to_string(),
        })
        .await
    }

    /// One CloudWatch Logs call that must succeed.
    pub(crate) async fn logs(&self, operation: &str, body: Value, case: &str) -> Value {
        let (status, parsed) = self
            .dispatch(AwsCall {
                signing_name: LOGS,
                access_key_id: AMBIENT_KEY,
                method: "POST",
                uri: "/",
                content_type: "application/x-amz-json-1.1",
                target: Some(format!("{LOGS_TARGET_PREFIX}.{operation}")),
                body: body.to_string(),
            })
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected logs {operation} to answer 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One awsQuery form body posted to `signing_name` as the published credential.
    async fn query(&self, signing_name: &str, body: String, case: &str) -> String {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization)
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: expected {signing_name} to answer 200, got {status} {text}"
        );
        text
    }

    /// Create a cluster with the `audit` control plane log type enabled, wait for the
    /// clock-owed `ACTIVE` move, and answer the address it published for its
    /// Kubernetes API server.
    async fn provisioned_cluster(&self, cluster: &str, case: &str) -> String {
        let body = json!({
            "name": cluster,
            "roleArn": format!("arn:aws:iam::{}:role/eksClusterRole", engine::emulator_account_id()),
            "resourcesVpcConfig": { "subnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"] },
            "accessConfig": { "authenticationMode": "API_AND_CONFIG_MAP" },
            "logging": { "clusterLogging": [{ "types": ["audit"], "enabled": true }] },
        });
        let (status, parsed) = self.eks(AMBIENT_KEY, "POST", "/clusters", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the cluster must be created, got {status} with {parsed}"
        );
        for _ in 0..POLL_ATTEMPTS {
            let (status, parsed) = self
                .eks(AMBIENT_KEY, "GET", &format!("/clusters/{cluster}"), json!({}))
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the cluster must be readable, got {status} with {parsed}"
            );
            if parsed["cluster"]["status"] == json!("ACTIVE") {
                if let Some(endpoint) = parsed["cluster"]["endpoint"].as_str() {
                    return endpoint.to_string();
                }
            }
            tokio::time::sleep(POLL_WAIT).await;
        }
        panic!("case {case:?}: the cluster never published an endpoint while ACTIVE");
    }

    /// One request to the cluster's published Kubernetes API address, as a client
    /// built from its kubeconfig makes it: one HTTP/1.1 exchange on the port the
    /// cluster published, carrying the bearer token the row presents — and the
    /// ACCOUNT's own credential when the row names none, which is the principal
    /// that created the cluster and administers it. An API server authenticates
    /// every request, so a client of one always presents a token.
    pub(crate) async fn k8s(
        &self,
        endpoint: &str,
        case: &str,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> ApiAnswer {
        let authority = endpoint
            .strip_prefix("https://")
            .unwrap_or_else(|| panic!("case {case:?}: expected an https endpoint, got {endpoint:?}"));
        let (_, port) = authority
            .rsplit_once(':')
            .unwrap_or_else(|| panic!("case {case:?}: the endpoint must carry a port, got {authority:?}"));
        let port: u16 = port
            .parse()
            .unwrap_or_else(|error| panic!("case {case:?}: {port:?} must be a port: {error}"));
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the published address must accept: {error}")
            });
        let payload = body.map(|body| body.to_string()).unwrap_or_default();
        let presented = token
            .map(str::to_string)
            .unwrap_or_else(crate::eks_wire::account_kubernetes_bearer_token);
        let authorization = format!("Authorization: Bearer {presented}\r\n");
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
             {authorization}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
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

    /// Every message the cluster's audit stream holds, as a client reads them.
    pub(crate) async fn audit_records(&self, cluster: &str, case: &str) -> Vec<Value> {
        let group = format!("/aws/eks/{cluster}/cluster");
        let streams = self
            .logs(
                "DescribeLogStreams",
                json!({ "logGroupName": &group }),
                case,
            )
            .await;
        let audit_streams: Vec<String> = streams["logStreams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_owned))
            .filter(|name| name.starts_with("kube-apiserver-audit-"))
            .collect();
        let mut records = Vec::new();
        for stream in audit_streams {
            let answer = self
                .logs(
                    "GetLogEvents",
                    json!({ "logGroupName": &group, "logStreamName": stream }),
                    case,
                )
                .await;
            for event in answer["events"].as_array().cloned().unwrap_or_default() {
                if let Some(message) = event["message"].as_str() {
                    records.push(serde_json::from_str(message).unwrap_or(Value::Null));
                }
            }
        }
        records
    }

    /// Create an IAM user carrying [`EKS_ADMIN_POLICY`] and one long-term access key
    /// of its own; answers `(user ARN, access key id)`.
    pub(crate) async fn user_with_key(
        &self,
        user_name: &str,
        case: &str,
    ) -> (String, String) {
        let created = self
            .query(
                "iam",
                format!("Action=CreateUser&Version=2010-05-08&UserName={user_name}"),
                case,
            )
            .await;
        let arn = element(&created, "Arn");
        self.query(
            "iam",
            format!(
                "Action=PutUserPolicy&Version=2010-05-08&UserName={user_name}&PolicyName=eks\
                 &PolicyDocument={}",
                form_encode(EKS_ADMIN_POLICY)
            ),
            case,
        )
        .await;
        let key = self
            .query(
                "iam",
                format!("Action=CreateAccessKey&Version=2010-05-08&UserName={user_name}"),
                case,
            )
            .await;
        let access_key_id = element(&key, "AccessKeyId");
        (arn, access_key_id)
    }

    /// `CreateAccessEntry`: map `principal_arn` to `username` and `groups`.
    pub(crate) async fn access_entry(
        &self,
        cluster: &str,
        principal_arn: &str,
        username: &str,
        groups: &[&str],
        case: &str,
    ) {
        let (status, parsed) = self
            .eks(
                AMBIENT_KEY,
                "POST",
                &format!("/clusters/{cluster}/access-entries"),
                json!({
                    "principalArn": principal_arn,
                    "username": username,
                    "kubernetesGroups": groups,
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAccessEntry must answer 200, got {status} with {parsed}"
        );
    }
}

/// The bearer token a kubeconfig's credential plugin builds for the credential
/// `access_key_id`, from the one builder every suite that dials a published cluster
/// endpoint shares ([`crate::eks_wire::kubernetes_bearer_token`]).
pub(crate) fn bearer_token(access_key_id: &str) -> String {
    crate::eks_wire::kubernetes_bearer_token(access_key_id)
}

/// A token of the documented form carrying `url`, for the rows that present one whose
/// URL is not a credential this server can read.
fn bearer_token_for_url(url: &str) -> String {
    format!("k8s-aws-v1.{}", crate::eks_wire::base64url(url))
}

/// The namespace the rows address, created through the API by the cluster's creator.
const NAMESPACE: &str = "team-a";

/// A second namespace, which the rows' binding does not reach.
const OTHER_NAMESPACE: &str = "team-b";

/// The Kubernetes group the access entry puts its principal in, and which the
/// namespace's `RoleBinding` binds.
const WRITERS_GROUP: &str = "team-a-writers";

/// The username the access entry maps its IAM principal to.
const MAPPED_USERNAME: &str = "team-a-user";

/// The `Role` the rows are decided by: the verbs it names on config maps of its own
/// namespace, and nothing else.
fn configmap_reader_role() -> Value {
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
fn writers_binding() -> Value {
    json!({
        "metadata": { "name": "configmap-writers" },
        "roleRef": {
            "apiGroup": "rbac.authorization.k8s.io",
            "kind": "Role",
            "name": "configmap-writer",
        },
        "subjects": [{ "kind": "Group", "name": WRITERS_GROUP }],
    })
}

/// The path of a namespaced RBAC collection.
fn rbac_path(namespace: &str, resource: &str) -> String {
    format!("/apis/rbac.authorization.k8s.io/v1/namespaces/{namespace}/{resource}")
}

// ---------------------------------------------------------------------------
// RULE: the `rbac.authorization.k8s.io/v1` group is served — discovered, created,
// read back, listed and given up — on the address the cluster publishes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_cluster_api_serves_the_rbac_group_it_discovers() {
    let emulator = Emulator::new();
    let case = "the RBAC group of a cluster's API server";
    let endpoint = emulator.provisioned_cluster("rbac-served", case).await;

    // Discovery: the group a client finds at `/apis` is the group it can then address,
    // and the version document names the four resources it serves.
    let groups = emulator
        .k8s(&endpoint, case, None, "GET", "/apis", None)
        .await;
    assert_eq!(
        groups.status, 200,
        "case {case:?}: expected the APIGroupList, got {groups:?}"
    );
    let served: Vec<String> = groups.body["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|group| group["name"].as_str().map(str::to_owned))
        .collect();
    assert!(
        served.contains(&"rbac.authorization.k8s.io".to_string()),
        "case {case:?}: the RBAC group must be discovered, got {served:?}"
    );
    let version = emulator
        .k8s(
            &endpoint,
            case,
            None,
            "GET",
            "/apis/rbac.authorization.k8s.io/v1",
            None,
        )
        .await;
    assert_eq!(
        version.status, 200,
        "case {case:?}: expected the APIResourceList, got {version:?}"
    );
    let resources: Vec<String> = version.body["resources"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|resource| resource["name"].as_str().map(str::to_owned))
        .collect();
    for expected in ["roles", "rolebindings", "clusterroles", "clusterrolebindings"] {
        assert!(
            resources.contains(&expected.to_string()),
            "case {case:?}: {expected:?} must be served, got {resources:?}"
        );
    }

    // The rows differ along the resource and its scope: a namespaced one and a
    // cluster-scoped one, a role and a binding.
    let created = emulator
        .k8s(
            &endpoint,
            case,
            None,
            "POST",
            "/api/v1/namespaces",
            Some(json!({ "metadata": { "name": NAMESPACE } })),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: the namespace must be created, got {created:?}"
    );

    let rows: Vec<(&str, String, Value, &str)> = vec![
        (
            "a namespaced Role",
            rbac_path(NAMESPACE, "roles"),
            configmap_reader_role(),
            "Role",
        ),
        (
            "a namespaced RoleBinding",
            rbac_path(NAMESPACE, "rolebindings"),
            writers_binding(),
            "RoleBinding",
        ),
        (
            "a cluster-scoped ClusterRole",
            "/apis/rbac.authorization.k8s.io/v1/clusterroles".to_string(),
            json!({
                "metadata": { "name": "namespace-reader" },
                "rules": [{ "apiGroups": [""], "resources": ["namespaces"], "verbs": ["list"] }],
            }),
            "ClusterRole",
        ),
        (
            "a cluster-scoped ClusterRoleBinding",
            "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings".to_string(),
            json!({
                "metadata": { "name": "namespace-readers" },
                "roleRef": {
                    "apiGroup": "rbac.authorization.k8s.io",
                    "kind": "ClusterRole",
                    "name": "namespace-reader",
                },
                "subjects": [{ "kind": "User", "name": MAPPED_USERNAME }],
            }),
            "ClusterRoleBinding",
        ),
    ];

    for (row, path, manifest, kind) in &rows {
        let name = manifest["metadata"]["name"]
            .as_str()
            .expect("the row names its object")
            .to_string();
        let created = emulator
            .k8s(&endpoint, row, None, "POST", path, Some(manifest.clone()))
            .await;
        assert_eq!(
            created.status, 201,
            "case {row:?}: expected 201 Created, got {created:?}"
        );
        assert_eq!(
            created.body["kind"],
            json!(kind),
            "case {row:?}: expected a {kind}, got {:?}",
            created.body
        );
        assert_eq!(
            created.body["apiVersion"],
            json!("rbac.authorization.k8s.io/v1"),
            "case {row:?}: expected the group's apiVersion, got {:?}",
            created.body
        );
        assert!(
            created.body["metadata"]["uid"].is_string(),
            "case {row:?}: the create must mint a uid, got {:?}",
            created.body
        );

        // Round trip: the object reads back as it was stored, and the collection
        // lists it.
        let read = emulator
            .k8s(&endpoint, row, None, "GET", &format!("{path}/{name}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {row:?}: the object must read back, got {read:?}"
        );
        assert_eq!(
            read.body, created.body,
            "case {row:?}: the read must answer what the create stored, got {:?}",
            read.body
        );
        let listed = emulator.k8s(&endpoint, row, None, "GET", path, None).await;
        assert_eq!(
            listed.status, 200,
            "case {row:?}: the collection must be listed, got {listed:?}"
        );
        let names: Vec<String> = listed.body["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item["metadata"]["name"].as_str().map(str::to_owned))
            .collect();
        assert!(
            names.contains(&name),
            "case {row:?}: the listing must hold {name:?}, got {names:?}"
        );

        // A second create of the same name is the conflict it is.
        let again = emulator
            .k8s(&endpoint, row, None, "POST", path, Some(manifest.clone()))
            .await;
        assert_eq!(
            again.status, 409,
            "case {row:?}: a second create of one name must conflict, got {again:?}"
        );
    }

    // An object the cluster does not hold is the Kubernetes not-found Status, and a
    // deleted one stops being held.
    let absent = emulator
        .k8s(
            &endpoint,
            case,
            None,
            "GET",
            &format!("{}/absent", rbac_path(NAMESPACE, "roles")),
            None,
        )
        .await;
    assert_eq!(
        absent.status, 404,
        "case {case:?}: a Role nothing created must be 404, got {absent:?}"
    );
    let deleted = emulator
        .k8s(
            &endpoint,
            case,
            None,
            "DELETE",
            &format!("{}/configmap-writer", rbac_path(NAMESPACE, "roles")),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: the delete must be served, got {deleted:?}"
    );
    let gone = emulator
        .k8s(
            &endpoint,
            case,
            None,
            "GET",
            &format!("{}/configmap-writer", rbac_path(NAMESPACE, "roles")),
            None,
        )
        .await;
    assert_eq!(
        gone.status, 404,
        "case {case:?}: a deleted Role must be 404, got {gone:?}"
    );
}

/// One row of the decision table: a request the mapped identity makes, and the status
/// the cluster's own RBAC must answer it with.
struct DecisionRow {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
}

// ---------------------------------------------------------------------------
// RULE: what an identity may do is decided by the RBAC objects bound to the username
// and groups the cluster's access entry maps its IAM principal to — and a principal no
// access entry maps authenticates as nobody.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_rbac_objects_decide_what_the_mapped_identity_may_do() {
    let emulator = Emulator::new();
    let case = "a mapped IAM principal";
    let cluster = "rbac-decides";
    let endpoint = emulator.provisioned_cluster(cluster, case).await;

    // The cluster's creator — whose access entry carries the cluster-admin access
    // policy — sets the cluster up: two namespaces, the Role, and the binding that
    // names the group the access entry puts its principal in.
    for namespace in [NAMESPACE, OTHER_NAMESPACE] {
        let created = emulator
            .k8s(
                &endpoint,
                case,
                None,
                "POST",
                "/api/v1/namespaces",
                Some(json!({ "metadata": { "name": namespace } })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "setup: the namespace {namespace:?} must be created, got {created:?}"
        );
    }
    for (path, manifest) in [
        (rbac_path(NAMESPACE, "roles"), configmap_reader_role()),
        (rbac_path(NAMESPACE, "rolebindings"), writers_binding()),
    ] {
        let created = emulator
            .k8s(&endpoint, case, None, "POST", &path, Some(manifest))
            .await;
        assert_eq!(
            created.status, 201,
            "setup: {path} must be created, got {created:?}"
        );
    }

    // The IAM principal the cluster maps, and one it does not.
    let (mapped_arn, mapped_key) = emulator.user_with_key("rbac-mapped", case).await;
    let (_unmapped_arn, unmapped_key) = emulator.user_with_key("rbac-unmapped", case).await;
    emulator
        .access_entry(
            cluster,
            &mapped_arn,
            MAPPED_USERNAME,
            &[WRITERS_GROUP],
            case,
        )
        .await;
    let token = bearer_token(&mapped_key);

    let rows = vec![
        DecisionRow {
            case: "a verb and resource the bound Role names",
            method: "GET",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
            body: None,
            status: 200,
        },
        DecisionRow {
            case: "a create the bound Role names",
            method: "POST",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
            body: Some(json!({
                "metadata": { "name": "allowed" },
                "data": { "key": "value" },
            })),
            status: 201,
        },
        DecisionRow {
            case: "a read of one object the Role names",
            method: "GET",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps/allowed"),
            body: None,
            status: 200,
        },
        DecisionRow {
            case: "a verb the Role does not name",
            method: "DELETE",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps/allowed"),
            body: None,
            status: 403,
        },
        DecisionRow {
            case: "a resource the Role does not name",
            method: "GET",
            path: format!("/api/v1/namespaces/{NAMESPACE}/secrets"),
            body: None,
            status: 403,
        },
        DecisionRow {
            case: "the same resource in a namespace the binding does not reach",
            method: "GET",
            path: format!("/api/v1/namespaces/{OTHER_NAMESPACE}/configmaps"),
            body: None,
            status: 403,
        },
        DecisionRow {
            case: "a cluster-scoped read no binding grants",
            method: "GET",
            path: "/api/v1/namespaces".to_string(),
            body: None,
            status: 403,
        },
        DecisionRow {
            case: "an RBAC object of the namespace, which the Role does not name",
            method: "POST",
            path: rbac_path(NAMESPACE, "roles"),
            body: Some(json!({ "metadata": { "name": "escalation" }, "rules": [] })),
            status: 403,
        },
        DecisionRow {
            case: "the version document every authenticated client reads",
            method: "GET",
            path: "/version".to_string(),
            body: None,
            status: 200,
        },
    ];

    for row in &rows {
        let answered = emulator
            .k8s(
                &endpoint,
                row.case,
                Some(&token),
                row.method,
                &row.path,
                row.body.clone(),
            )
            .await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {answered:?}",
            row.case, row.status
        );
        if row.status == 403 {
            assert_eq!(
                answered.body["reason"],
                json!("Forbidden"),
                "case {:?}: a refusal is the Forbidden Status, got {:?}",
                row.case,
                answered.body
            );
            let message = answered.body["message"].as_str().unwrap_or_default();
            assert!(
                message.contains(MAPPED_USERNAME),
                "case {:?}: the refusal must name the user it was taken for, got {message:?}",
                row.case
            );
        }
    }

    // A principal the cluster maps to nobody authenticates as nobody, whatever the
    // RBAC objects say — and so does a presented token this server cannot read a
    // credential out of at all.
    for (case, presented) in [
        (
            "an IAM principal no access entry names",
            bearer_token(&unmapped_key),
        ),
        (
            "a bearer token of another scheme",
            "not-a-k8s-aws-token".to_string(),
        ),
        (
            "an EKS token whose segment is not base64url",
            "k8s-aws-v1.$$$".to_string(),
        ),
        (
            "an EKS token whose URL carries no credential scope",
            bearer_token_for_url("https://sts.amazonaws.com/?Action=GetCallerIdentity"),
        ),
    ] {
        let unmapped = emulator
            .k8s(
                &endpoint,
                case,
                Some(&presented),
                "GET",
                &format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
                None,
            )
            .await;
        assert_eq!(
            unmapped.status, 401,
            "case {case:?}: expected 401 Unauthorized, got {unmapped:?}"
        );
        assert_eq!(
            unmapped.body["reason"],
            json!("Unauthorized"),
            "case {case:?}: a refusal is the Unauthorized Status, got {:?}",
            unmapped.body
        );
    }

    // A binding named by USER rather than by group reaches the same identity, and a
    // `ClusterRoleBinding` reaches it in every namespace — which is what the two rows
    // the binding above did not admit now become.
    for (row, path, manifest) in [
        (
            "a ClusterRole bound to the username by a ClusterRoleBinding",
            "/apis/rbac.authorization.k8s.io/v1/clusterroles",
            json!({
                "metadata": { "name": "configmap-reader-everywhere" },
                "rules": [{
                    "apiGroups": [""],
                    "resources": ["configmaps"],
                    "verbs": ["get", "list"],
                }],
            }),
        ),
        (
            "the binding that names the username itself",
            "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings",
            json!({
                "metadata": { "name": "configmap-readers-everywhere" },
                "roleRef": {
                    "apiGroup": "rbac.authorization.k8s.io",
                    "kind": "ClusterRole",
                    "name": "configmap-reader-everywhere",
                },
                "subjects": [{ "kind": "User", "name": MAPPED_USERNAME }],
            }),
        ),
    ] {
        let created = emulator
            .k8s(&endpoint, row, None, "POST", path, Some(manifest))
            .await;
        assert_eq!(
            created.status, 201,
            "case {row:?}: expected 201 Created, got {created:?}"
        );
    }
    let elsewhere = emulator
        .k8s(
            &endpoint,
            "the namespace the cluster-wide binding now reaches",
            Some(&token),
            "GET",
            &format!("/api/v1/namespaces/{OTHER_NAMESPACE}/configmaps"),
            None,
        )
        .await;
    assert_eq!(
        elsewhere.status, 200,
        "case a cluster-wide binding: the same read must now be admitted, got {elsewhere:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE: with the `audit` log type enabled, every request the API server answers is
// recorded in the cluster's own log group, naming the identity the access entry mapped
// the IAM principal to and the decision the authorizer took — for the admitted request
// and for the refused one alike.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_request_leaves_an_audit_record_naming_the_access_entry_username() {
    let emulator = Emulator::new();
    let case = "the audit log of a cluster that enabled it";
    let cluster = "rbac-audited";
    let endpoint = emulator.provisioned_cluster(cluster, case).await;

    let created = emulator
        .k8s(
            &endpoint,
            case,
            None,
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
        (rbac_path(NAMESPACE, "roles"), configmap_reader_role()),
        (rbac_path(NAMESPACE, "rolebindings"), writers_binding()),
    ] {
        let created = emulator
            .k8s(&endpoint, case, None, "POST", &path, Some(manifest))
            .await;
        assert_eq!(
            created.status, 201,
            "setup: {path} must be created, got {created:?}"
        );
    }
    let (mapped_arn, mapped_key) = emulator.user_with_key("audit-mapped", case).await;
    emulator
        .access_entry(
            cluster,
            &mapped_arn,
            MAPPED_USERNAME,
            &[WRITERS_GROUP],
            case,
        )
        .await;
    let token = bearer_token(&mapped_key);

    // One request the Role admits and one it refuses, as the same identity.
    let allowed = emulator
        .k8s(
            &endpoint,
            "the admitted request",
            Some(&token),
            "POST",
            &format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
            Some(json!({ "metadata": { "name": "audited" }, "data": { "key": "value" } })),
        )
        .await;
    assert_eq!(
        allowed.status, 201,
        "case the admitted request: expected 201, got {allowed:?}"
    );
    let refused = emulator
        .k8s(
            &endpoint,
            "the refused request",
            Some(&token),
            "DELETE",
            &format!("/api/v1/namespaces/{NAMESPACE}/configmaps/audited"),
            None,
        )
        .await;
    assert_eq!(
        refused.status, 403,
        "case the refused request: expected 403, got {refused:?}"
    );

    // The two records, each read back the way an operator auditing the cluster reads
    // them. The wait is for the CONDITION — the delivery is a call to another service
    // — re-read until it holds or a generous deadline passes.
    let mut records = Vec::new();
    for _ in 0..POLL_ATTEMPTS {
        records = emulator.audit_records(cluster, case).await;
        let decisions: Vec<&Value> = records
            .iter()
            .filter(|record| {
                record["user"]["username"] == json!(MAPPED_USERNAME)
                    && record["objectRef"]["resource"] == json!("configmaps")
            })
            .collect();
        if decisions.len() >= 2 {
            break;
        }
        tokio::time::sleep(POLL_WAIT).await;
    }

    for (row, verb, status, decision) in [
        ("the admitted request", "create", 201, "allow"),
        ("the refused request", "delete", 403, "forbid"),
    ] {
        let found: Vec<&Value> = records
            .iter()
            .filter(|record| {
                record["verb"] == json!(verb)
                    && record["objectRef"]["resource"] == json!("configmaps")
                    && record["objectRef"]["namespace"] == json!(NAMESPACE)
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "case {row:?}: expected exactly one audit record of {verb:?} on configmaps, got \
             {records:?}"
        );
        let record = found[0];
        assert_eq!(
            record["user"]["username"],
            json!(MAPPED_USERNAME),
            "case {row:?}: the record must name the access entry's username, got {record:?}"
        );
        let groups: Vec<String> = record["user"]["groups"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|group| group.as_str().map(str::to_owned))
            .collect();
        assert!(
            groups.contains(&WRITERS_GROUP.to_string()),
            "case {row:?}: the record must name the entry's Kubernetes groups, got {groups:?}"
        );
        assert_eq!(
            record["responseStatus"]["code"],
            json!(status),
            "case {row:?}: the record must name the status the request was answered with, \
             got {record:?}"
        );
        assert_eq!(
            record["annotations"]["authorization.k8s.io/decision"],
            json!(decision),
            "case {row:?}: the record must name the authorizer's decision, got {record:?}"
        );
        assert_eq!(
            record["objectRef"]["name"],
            json!("audited"),
            "case {row:?}: the record must name the object the request acted on, got \
             {record:?}"
        );
        assert_eq!(
            record["apiVersion"],
            json!("audit.k8s.io/v1"),
            "case {row:?}: the record is a Kubernetes audit event, got {record:?}"
        );
        assert!(
            record["requestURI"]
                .as_str()
                .is_some_and(|uri| uri.contains(NAMESPACE)),
            "case {row:?}: the record must name the request it audits, got {record:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: a cluster that does not enable the `audit` log type records nothing, which is
// what makes a disabled type observably different from an enabled one.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cluster_without_the_audit_log_type_records_nothing() {
    let emulator = Emulator::new();
    let case = "a cluster that enables no log type";
    let cluster = "rbac-unaudited";
    let body = json!({
        "name": cluster,
        "roleArn": format!("arn:aws:iam::{}:role/eksClusterRole", engine::emulator_account_id()),
        "resourcesVpcConfig": { "subnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"] },
        "accessConfig": { "authenticationMode": "API_AND_CONFIG_MAP" },
    });
    let (status, parsed) = emulator.eks(AMBIENT_KEY, "POST", "/clusters", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the cluster must be created, got {status} with {parsed}"
    );
    let mut endpoint = None;
    for _ in 0..POLL_ATTEMPTS {
        let (status, parsed) = emulator
            .eks(AMBIENT_KEY, "GET", &format!("/clusters/{cluster}"), json!({}))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the cluster must be readable, got {status} with {parsed}"
        );
        if parsed["cluster"]["status"] == json!("ACTIVE") {
            if let Some(published) = parsed["cluster"]["endpoint"].as_str() {
                endpoint = Some(published.to_string());
                break;
            }
        }
        tokio::time::sleep(POLL_WAIT).await;
    }
    let endpoint =
        endpoint.unwrap_or_else(|| panic!("case {case:?}: the cluster published no endpoint"));

    let served = emulator
        .k8s(&endpoint, case, None, "GET", "/api/v1/namespaces", None)
        .await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: the request is served either way, got {served:?}"
    );

    // No log group at all: nothing created it, because no type exports.
    let group = format!("/aws/eks/{cluster}/cluster");
    let described = emulator
        .logs(
            "DescribeLogGroups",
            json!({ "logGroupNamePrefix": &group }),
            case,
        )
        .await;
    let held: Vec<String> = described["logGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|held| held["logGroupName"].as_str().map(str::to_owned))
        .collect();
    assert!(
        !held.contains(&group),
        "case {case:?}: a cluster that enables no log type must have no log group, got {held:?}"
    );
}
