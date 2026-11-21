// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS an Amazon EKS cluster publishes for its Kubernetes API server, and what a
//! client that follows it observes there.
//!
//! RULE: `Cluster$endpoint` is "the endpoint for your Kubernetes API server", so every
//! read that publishes it publishes an address THIS emulator answers that cluster's
//! Kubernetes API on — never a host in AWS's own zone, which resolves to AWS production
//! or to nothing on the machine the emulator runs on. A client that dials the published
//! address is served as the Kubernetes identity the cluster's access entry maps its IAM
//! principal to (`AccessEntry$username` is "the name of a user that can authenticate to
//! your cluster"), the access policies associated to that entry decide what that identity
//! may do, and — while the `audit` control plane log type is enabled (`LogSetup$enabled`:
//! "If a log type is enabled, that log type exports its control plane logs to CloudWatch
//! Logs") — each decision is recorded in the cluster's own log group
//! `/aws/eks/<cluster name>/cluster`, naming that username and the decision taken.
//!
//! Four services have to agree for that to hold — IAM holds the user the entry names, STS
//! says which principal a credential speaks for, EKS publishes the address and serves the
//! API, CloudWatch Logs holds the records — so this is a cross-service suite, and it
//! drives the harness `eks_kubernetes_api_rbac_audit.rs` already has rather than a second
//! one of its own.
//!
//! The rows differ along what the rule generalises over: the `authenticationMode` the
//! cluster was created with (both of the two modes that serve the access-entry API), the
//! username the entry maps to, and — per cluster — the two requests a decision is taken
//! on, which address a CLUSTER-SCOPED resource through the published address: one the
//! associated access policy admits and one it refuses.

use serde_json::{json, Value};

use crate::eks_kubernetes_api_rbac_audit::{
    bearer_token, Emulator, AMBIENT_KEY, POLL_ATTEMPTS, POLL_WAIT,
};

/// The hosts this emulator answers on, which is what a published address has to name: the
/// loopback interface the core binds a per-resource listener to.
const OWN_HOSTS: [&str; 4] = ["127.0.0.1", "localhost", "[::1]", "::1"];

/// The published access policy the rows associate: the read-only one, which grants
/// `get`/`list`/`watch` on a cluster's namespaces and changes nothing — so one of the two
/// requests below is admitted by it and the other is not.
const VIEW_POLICY_ARN: &str = "arn:aws:eks::aws:cluster-access-policy/AmazonEKSViewPolicy";

/// The Kubernetes group the access entry puts its principal in. NOTHING binds it on these
/// clusters, so what the identity may do comes from the associated access policy alone.
const UNBOUND_GROUP: &str = "endpoint-audit-readers";

/// The group Kubernetes puts every authenticated request in, which a served identity
/// therefore carries beside the groups its access entry names.
const AUTHENTICATED_GROUP: &str = "system:authenticated";

/// The namespace Kubernetes creates with every cluster, which the refused request below
/// addresses: it exists on each row's cluster without the client creating anything.
const DEFAULT_NAMESPACE: &str = "default";

/// One cluster of the table: the case it stands for, the mode it authenticates its
/// Kubernetes callers under, and the identities filed against it.
struct ClusterCase {
    label: &'static str,
    cluster: &'static str,
    /// An `AuthenticationMode` member that serves the access-entry API.
    authentication_mode: &'static str,
    /// The IAM user whose credential the client presents.
    user_name: &'static str,
    /// The username the access entry maps that user to.
    username: &'static str,
}

/// One request a decision is taken on, and what the audit record of it says.
struct DecisionCase {
    row: &'static str,
    method: &'static str,
    path: String,
    /// The status the API server answers it with.
    status: u16,
    /// The verb the request addresses, as an audit record names it: a read of a
    /// collection is `list`, a delete of one object is `delete`.
    verb: &'static str,
    /// `objectRef.namespace`, which a request addressing no namespace carries none of.
    namespace: Option<&'static str>,
    /// `objectRef.name`, which a request naming no object carries none of.
    name: Option<&'static str>,
    /// The authorizer's decision, as the annotation records it.
    decision: &'static str,
}

/// Percent-encode one `@httpLabel` segment, the way an AWS SDK does: keep the unreserved
/// alphanumerics and escape every other byte, so the `:` and `/` of a principal ARN are
/// not read as segment separators.
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// `CreateCluster` for one row: the `audit` control plane log type enabled, and the
/// authentication mode the row names. A cluster has no control plane yet when the create
/// answers, so it publishes NO address — never one a client could be pointed at.
async fn create_cluster(emulator: &Emulator, case: &ClusterCase) {
    let body = json!({
        "name": case.cluster,
        "roleArn": format!("arn:aws:iam::{}:role/eksClusterRole", engine::emulator_account_id()),
        "resourcesVpcConfig": { "subnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"] },
        "accessConfig": { "authenticationMode": case.authentication_mode },
        "logging": { "clusterLogging": [{ "types": ["audit"], "enabled": true }] },
    });
    let (status, parsed) = emulator.eks(AMBIENT_KEY, "POST", "/clusters", body).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateCluster must answer 200, got {status} with {parsed}",
        case.label
    );
    assert!(
        parsed["cluster"]["endpoint"].is_null(),
        "case {:?}: a cluster with no control plane yet publishes no endpoint, got {:?}",
        case.label,
        parsed["cluster"]["endpoint"]
    );
}

/// `DescribeCluster` until the cluster reports `ACTIVE` with its API server address
/// published, or the poll budget runs out. The wait is for the CONDITION, so a loaded host
/// only makes it slower rather than failing it.
async fn published_endpoint(emulator: &Emulator, case: &ClusterCase) -> String {
    for _ in 0..POLL_ATTEMPTS {
        let (status, parsed) = emulator
            .eks(
                AMBIENT_KEY,
                "GET",
                &format!("/clusters/{}", case.cluster),
                json!({}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {:?}: DescribeCluster must answer 200, got {status} with {parsed}",
            case.label
        );
        if parsed["cluster"]["status"] == json!("ACTIVE") {
            if let Some(endpoint) = parsed["cluster"]["endpoint"].as_str() {
                return endpoint.to_string();
            }
        }
        tokio::time::sleep(POLL_WAIT).await;
    }
    panic!(
        "case {:?}: the clock owes the cluster ACTIVE with a published endpoint",
        case.label
    );
}

/// The `host` and `port` of a published endpoint, asserted to be an address of this
/// emulator's own: an `https` URL on one of [`OWN_HOSTS`] carrying the port its listener
/// answers on, and never a name under AWS's own zone.
fn own_address(label: &str, endpoint: &str) -> (String, u16) {
    let authority = endpoint.strip_prefix("https://").unwrap_or_else(|| {
        panic!("case {label:?}: the endpoint must be an https URL, got {endpoint:?}")
    });
    let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {label:?}: the endpoint must carry a port, got {endpoint:?}")
    });
    assert!(
        !host.ends_with("amazonaws.com"),
        "case {label:?}: the endpoint must be an address this emulator serves, not a name under \
         AWS's own zone, got {endpoint:?}"
    );
    assert!(
        OWN_HOSTS.contains(&host),
        "case {label:?}: expected one of this emulator's own hosts {OWN_HOSTS:?}, got {host:?} in \
         {endpoint:?}"
    );
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("case {label:?}: {port:?} must be a port: {error}"));
    assert_ne!(
        port, 0,
        "case {label:?}: the endpoint must name the port its listener answers on, got {endpoint:?}"
    );
    (host.to_string(), port)
}

/// `AssociateAccessPolicy`: grant `VIEW_POLICY_ARN` to `principal_arn`'s entry over the
/// whole cluster, which is the scope a cluster-scoped resource is decided in.
async fn associate_view_policy(emulator: &Emulator, case: &ClusterCase, principal_arn: &str) {
    let (status, parsed) = emulator
        .eks(
            AMBIENT_KEY,
            "POST",
            &format!(
                "/clusters/{}/access-entries/{}/access-policies",
                case.cluster,
                encoded(principal_arn)
            ),
            json!({
                "policyArn": VIEW_POLICY_ARN,
                "accessScope": { "type": "cluster" },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {:?}: AssociateAccessPolicy must answer 200, got {status} with {parsed}",
        case.label
    );
}

/// The audit records of `cluster` naming `username` on the `namespaces` resource, looked
/// at again until at least `wanted` of them are held or the poll budget runs out — the
/// export is a call to another service, so the wait is for the CONDITION.
async fn recorded_decisions(
    emulator: &Emulator,
    case: &ClusterCase,
    username: &str,
    wanted: usize,
) -> Vec<Value> {
    let mut records = Vec::new();
    for _ in 0..POLL_ATTEMPTS {
        records = emulator
            .audit_records(case.cluster, case.label)
            .await
            .into_iter()
            .filter(|record| {
                record["user"]["username"] == json!(username)
                    && record["objectRef"]["resource"] == json!("namespaces")
            })
            .collect();
        if records.len() >= wanted {
            break;
        }
        tokio::time::sleep(POLL_WAIT).await;
    }
    records
}

// ---------------------------------------------------------------------------
// RULE: the address a cluster publishes for its Kubernetes API server is one this
// emulator answers that cluster's API on, the access policies the cluster's access entry
// associates decide what a client dialling it may do, and each decision is recorded in
// the cluster's own audit stream under the username the entry maps.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_published_endpoint_serves_decisions_the_audit_log_names_the_username_of() {
    let emulator = Emulator::new();
    // The rows differ along the authentication mode the cluster serves its access-entry
    // API under: the API-only one, and the one that keeps the `aws-auth` ConfigMap beside
    // it. Both map a principal through an access entry, so both decide and audit a
    // request the same way.
    let cases = [
        ClusterCase {
            label: "a cluster relying only on access entries",
            cluster: "endpoint-audited-api",
            authentication_mode: "API",
            user_name: "endpoint-audited-api-user",
            username: "audited-api-user",
        },
        ClusterCase {
            label: "a cluster keeping the aws-auth ConfigMap beside them",
            cluster: "endpoint-audited-both",
            authentication_mode: "API_AND_CONFIG_MAP",
            user_name: "endpoint-audited-both-user",
            username: "audited-both-user",
        },
    ];

    // Create every cluster FIRST and only then wait for any of them: the move to ACTIVE
    // is one the clock owes each cluster from its own create, so the waits overlap
    // instead of adding up.
    for case in &cases {
        create_cluster(&emulator, case).await;
    }

    let mut published: Vec<String> = Vec::new();
    for case in &cases {
        let label = case.label;
        let endpoint = published_endpoint(&emulator, case).await;
        let (_host, _port) = own_address(label, &endpoint);

        // The identity: an IAM user, the cluster's access entry mapping it to a username
        // and a group nothing binds, and the read-only access policy over the whole
        // cluster — so every decision below is the policy's.
        let (user_arn, access_key_id) = emulator.user_with_key(case.user_name, label).await;
        emulator
            .access_entry(case.cluster, &user_arn, case.username, &[UNBOUND_GROUP], label)
            .await;
        associate_view_policy(&emulator, case, &user_arn).await;
        let token = bearer_token(&access_key_id);

        // One request the policy admits and one it refuses, both addressing the
        // cluster-scoped `namespaces` resource through the published address.
        let decisions = [
            DecisionCase {
                row: "a read of the cluster's namespaces, which the policy admits",
                method: "GET",
                path: "/api/v1/namespaces".to_string(),
                status: 200,
                verb: "list",
                namespace: None,
                name: None,
                decision: "allow",
            },
            DecisionCase {
                row: "a delete of a namespace, which the read-only policy refuses",
                method: "DELETE",
                path: format!("/api/v1/namespaces/{DEFAULT_NAMESPACE}"),
                status: 403,
                verb: "delete",
                namespace: Some(DEFAULT_NAMESPACE),
                name: Some(DEFAULT_NAMESPACE),
                decision: "forbid",
            },
        ];

        for decision in &decisions {
            let row = decision.row;
            let answered = emulator
                .k8s(
                    &endpoint,
                    row,
                    Some(&token),
                    decision.method,
                    &decision.path,
                    None,
                )
                .await;
            assert_eq!(
                answered.status, decision.status,
                "case {label:?} / {row:?}: expected {} from the published address, got {answered:?}",
                decision.status
            );
            if decision.status == 403 {
                assert_eq!(
                    answered.body["reason"],
                    json!("Forbidden"),
                    "case {label:?} / {row:?}: expected the Kubernetes forbidden Status, got {:?}",
                    answered.body
                );
                let message = answered.body["message"].as_str().unwrap_or_default();
                assert!(
                    message.contains(case.username),
                    "case {label:?} / {row:?}: the refusal names the identity it was taken for, \
                     got {message:?}"
                );
            }
        }

        let records = recorded_decisions(&emulator, case, case.username, decisions.len()).await;
        let mut audit_ids: Vec<String> = Vec::new();
        for decision in &decisions {
            let row = decision.row;
            let found: Vec<&Value> = records
                .iter()
                .filter(|record| record["verb"] == json!(decision.verb))
                .collect();
            assert_eq!(
                found.len(),
                1,
                "case {label:?} / {row:?}: expected exactly one audit record of {:?} on \
                 namespaces, got {records:?}",
                decision.verb
            );
            let record = found[0];
            assert_eq!(
                record["apiVersion"],
                json!("audit.k8s.io/v1"),
                "case {label:?} / {row:?}: the record is a Kubernetes audit event, got {record:?}"
            );
            assert_eq!(
                record["user"]["username"],
                json!(case.username),
                "case {label:?} / {row:?}: the record must name the access entry's username, got \
                 {record:?}"
            );
            let groups: Vec<String> = record["user"]["groups"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|group| group.as_str().map(str::to_owned))
                .collect();
            for expected in [UNBOUND_GROUP, AUTHENTICATED_GROUP] {
                assert!(
                    groups.contains(&expected.to_string()),
                    "case {label:?} / {row:?}: the record must name the groups the identity \
                     carries, expected {expected:?} among {groups:?}"
                );
            }
            assert_eq!(
                record["responseStatus"]["code"],
                json!(decision.status),
                "case {label:?} / {row:?}: the record must name the status the request was \
                 answered with, got {record:?}"
            );
            assert_eq!(
                record["annotations"]["authorization.k8s.io/decision"],
                json!(decision.decision),
                "case {label:?} / {row:?}: the record must name the authorizer's decision, got \
                 {record:?}"
            );
            assert_eq!(
                record["requestURI"],
                json!(decision.path),
                "case {label:?} / {row:?}: the record must name the request it audits, got \
                 {record:?}"
            );
            match decision.namespace {
                Some(namespace) => assert_eq!(
                    record["objectRef"]["namespace"],
                    json!(namespace),
                    "case {label:?} / {row:?}: the record must name the namespace the request \
                     addressed, got {record:?}"
                ),
                None => assert!(
                    record["objectRef"]["namespace"].is_null(),
                    "case {label:?} / {row:?}: a request addressing no namespace records none, \
                     got {record:?}"
                ),
            }
            match decision.name {
                Some(name) => assert_eq!(
                    record["objectRef"]["name"],
                    json!(name),
                    "case {label:?} / {row:?}: the record must name the object the request acted \
                     on, got {record:?}"
                ),
                None => assert!(
                    record["objectRef"]["name"].is_null(),
                    "case {label:?} / {row:?}: a request naming no object records none, got \
                     {record:?}"
                ),
            }
            let audit_id = record["auditID"].as_str().unwrap_or_default().to_string();
            assert!(
                !audit_id.is_empty(),
                "case {label:?} / {row:?}: every audited request carries an audit id, got \
                 {record:?}"
            );
            audit_ids.push(audit_id);
        }
        audit_ids.sort();
        audit_ids.dedup();
        assert_eq!(
            audit_ids.len(),
            decisions.len(),
            "case {label:?}: two served requests are two audited events, got {audit_ids:?}"
        );

        published.push(endpoint);
    }

    // Each cluster is reached at an address of its OWN: the value is the cluster's, not
    // the service's.
    let mut distinct = published.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        published.len(),
        "each cluster publishes an address of its own, got {published:?}"
    );
}
