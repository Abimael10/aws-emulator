// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a cluster's Kubernetes API server refuses an unauthorized request
//! with, over the address the cluster itself publishes.
//!
//! The RULE under test: the `Status` an API server answers a request its authorizer
//! does not admit with carries the authorizer's own fixed sentence, and that sentence
//! NAMES THE OBJECT the request addressed — `<resource> "<name>" is forbidden: User
//! "<username>" cannot <verb> resource "<resource>" in API group "<group>"
//! <scope>` — exactly when the request's path names one; a request that addresses a
//! COLLECTION names no object and carries the shorter `<resource> is forbidden: …`
//! form, and a request that addresses no resource at all carries `forbidden: User
//! "<username>" cannot <verb> path "<path>"`. The leading resource token is the
//! resource qualified by its API group (`<resource>.<group>`) outside the core group,
//! while the `resource "…"` clause is the resource with the SUBRESOURCE the request
//! addressed, and the scope is the namespace the request is in or the cluster scope.
//!
//! The rows differ along what the rule generalises over: whether the path names an
//! object, the verb the method is (update, create, get, list, delete), the API group
//! (the core one and a named one), whether a subresource is addressed, and whether the
//! request is namespaced or cluster-scoped.
//!
//! The client is the raw kubeconfig-built one this crate's `kubernetes_api` suite
//! dials with, so nothing but what the emulator published is used to reach the API.

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    access_policy_arn, ACCESS_SCOPE_TYPE_CLUSTER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, kubeconfig_of, poll_until_active, ApiAnswer, Kubeconfig};
use crate::wire::{call, emulator, encode_label, Answer};

/// The access policy that grants the permissions of the Kubernetes `cluster-admin`
/// role, used to stand the cluster's objects up before the grant is taken away again.
const CLUSTER_ADMIN_POLICY: &str = "AmazonEKSClusterAdminPolicy";

/// The namespace the namespaced rows address.
const NAMESPACE: &str = "sentence-team";

/// A second namespace, which the rows address by name without being granted it.
const OTHER_NAMESPACE: &str = "sentence-elsewhere";

/// The username the cluster's access entry maps its IAM principal to, which is the
/// user the refusal names.
const MAPPED_USERNAME: &str = "sentence-cm-reader";

/// The Kubernetes group that entry puts the principal in, and which the `RoleBinding`
/// below binds the reader role to.
const READERS_GROUP: &str = "sentence-cm-readers";

/// The config map the named rows address, created while the identity still carried
/// the cluster-admin grant.
const CONFIG_MAP: &str = "sentence-settings";

/// Create the cluster and wait for the clock-owed provisioning to be materialised,
/// then read the kubeconfig members back the way a client does.
async fn provisioned_cluster(router: &MultiServiceRouter, name: &str) -> Kubeconfig {
    let body = serde_json::json!({
        "name": name,
        "roleArn": "arn:aws:iam::000000000000:role/eksClusterRole",
        "resourcesVpcConfig": { "subnetIds": ["subnet-aaaa1111", "subnet-bbbb2222"] },
        "accessConfig": {
            "authenticationMode": "API",
            // No bootstrap entry: the credential this suite dials with is mapped by
            // the access entry created below and by nothing else.
            "bootstrapClusterCreatorAdminPermissions": false,
        },
    });
    let created = call(router, REGION, "POST", "/clusters", &body.to_string()).await;
    assert_eq!(
        created.status, 200,
        "setup: expected 200 from CreateCluster({name}), got {created:?}"
    );
    let described = poll_until_active(router, REGION, name).await;
    kubeconfig_of(name, &described)
}

/// `CreateAccessEntry` for `principal_arn`, mapping it to [`MAPPED_USERNAME`] and
/// [`READERS_GROUP`].
async fn create_access_entry(
    router: &MultiServiceRouter,
    cluster: &str,
    principal_arn: &str,
) -> Answer {
    let body = serde_json::json!({
        "principalArn": principal_arn,
        "username": MAPPED_USERNAME,
        "kubernetesGroups": [READERS_GROUP],
    });
    call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/access-entries", encode_label(cluster)),
        &body.to_string(),
    )
    .await
}

/// `AssociateAccessPolicy` / `DisassociateAccessPolicy` for the cluster-admin policy
/// over the whole cluster: the grant that lets the identity stand the objects up, and
/// its removal, which leaves only the bound reader role.
async fn associate_policy(
    router: &MultiServiceRouter,
    cluster: &str,
    principal_arn: &str,
) -> Answer {
    let body = serde_json::json!({
        "policyArn": access_policy_arn(CLUSTER_ADMIN_POLICY),
        "accessScope": { "type": ACCESS_SCOPE_TYPE_CLUSTER },
    });
    call(
        router,
        REGION,
        "POST",
        &format!(
            "/clusters/{}/access-entries/{}/access-policies",
            encode_label(cluster),
            encode_label(principal_arn)
        ),
        &body.to_string(),
    )
    .await
}

/// See [`associate_policy`].
async fn disassociate_policy(
    router: &MultiServiceRouter,
    cluster: &str,
    principal_arn: &str,
) -> Answer {
    call(
        router,
        REGION,
        "DELETE",
        &format!(
            "/clusters/{}/access-entries/{}/access-policies/{}",
            encode_label(cluster),
            encode_label(principal_arn),
            encode_label(&access_policy_arn(CLUSTER_ADMIN_POLICY))
        ),
        "",
    )
    .await
}

/// Create one object through the API and assert it was.
async fn must_create(
    config: &Kubeconfig,
    case: &str,
    path: &str,
    manifest: &serde_json::Value,
) -> ApiAnswer {
    let created = dial(config, case, "POST", path, Some(manifest)).await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected 201 Created from {path}, got {created:?}"
    );
    created
}

/// One request the cluster's authorizer does not admit, and what the API server's
/// refusal must therefore say about it.
struct Row {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<serde_json::Value>,
    /// The verb the method is on what the path addresses.
    verb: &'static str,
    /// What the path addresses, and `None` for a path that addresses no resource at
    /// all — a server path rather than an object or a collection.
    addressed: Option<Addressed>,
}

/// The resource one row addresses, as the sentence about it names it.
struct Addressed {
    /// The object the path names, and `None` for a request addressing a COLLECTION —
    /// which is what decides whether the sentence carries the object clause at all.
    name: Option<&'static str>,
    /// The resource token qualified by its API group, which is how the sentence opens.
    qualified: &'static str,
    /// The resource with the subresource the request addressed, if any.
    resource: &'static str,
    api_group: &'static str,
    /// The namespace the request is in, and `None` for a cluster-scoped request.
    namespace: Option<&'static str>,
}

impl Row {
    /// The whole `message` this row's refusal must carry, built from the authorizer's
    /// fixed text and the row's own values.
    fn expected(&self) -> String {
        let Some(addressed) = &self.addressed else {
            return format!(
                "forbidden: User \"{MAPPED_USERNAME}\" cannot {} path \"{}\"",
                self.verb, self.path
            );
        };
        let opens = match addressed.name {
            Some(name) => format!("{} \"{name}\"", addressed.qualified),
            None => addressed.qualified.to_string(),
        };
        let scope = match addressed.namespace {
            Some(namespace) => format!("in the namespace \"{namespace}\""),
            None => "at the cluster scope".to_string(),
        };
        format!(
            "{opens} is forbidden: User \"{MAPPED_USERNAME}\" cannot {} resource \"{}\" \
             in API group \"{}\" {scope}",
            self.verb, addressed.resource, addressed.api_group
        )
    }

    /// The object the path names, which the `Status` details must name too.
    fn names(&self) -> Option<&'static str> {
        self.addressed.as_ref().and_then(|addressed| addressed.name)
    }
}

// ---------------------------------------------------------------------------
// RULE: the authorizer's refusal names the object the request addressed —
// `<resource> "<name>" is forbidden: …` — exactly when the path names one, and the
// shorter form only for a request that addresses a collection or no resource at all.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_rbac_refusal_names_the_object_the_request_addressed() {
    let router = emulator();
    let cluster = "forbidden-sentences";
    let config = provisioned_cluster(&router, cluster).await;

    // The IAM principal the dialling credential speaks for, read from the one
    // definition of it rather than written out here.
    let principal_arn = service_common::caller_principal::ambient_user_arn();
    let entry = create_access_entry(&router, cluster, &principal_arn).await;
    assert_eq!(
        entry.status, 200,
        "setup: expected 200 from CreateAccessEntry, got {entry:?}"
    );
    let associated = associate_policy(&router, cluster, &principal_arn).await;
    assert_eq!(
        associated.status, 200,
        "setup: expected 200 from AssociateAccessPolicy, got {associated:?}"
    );

    // With the cluster-admin policy associated, the identity stands the cluster up:
    // the namespaces, the config map the named rows address, and the Role and binding
    // that leave it able to READ config maps of one namespace and nothing else.
    for namespace in [NAMESPACE, OTHER_NAMESPACE] {
        must_create(
            &config,
            "a namespace the rows address",
            "/api/v1/namespaces",
            &serde_json::json!({ "metadata": { "name": namespace } }),
        )
        .await;
    }
    must_create(
        &config,
        "the config map the named rows address",
        &format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
        &serde_json::json!({
            "metadata": { "name": CONFIG_MAP },
            "data": { "color": "blue" },
        }),
    )
    .await;
    must_create(
        &config,
        "the Role the rows are decided by",
        &format!("/apis/rbac.authorization.k8s.io/v1/namespaces/{NAMESPACE}/roles"),
        &serde_json::json!({
            "metadata": { "name": "sentence-configmap-reader" },
            "rules": [{ "apiGroups": [""], "resources": ["configmaps"], "verbs": ["get"] }],
        }),
    )
    .await;
    must_create(
        &config,
        "the binding that names the entry's group",
        &format!("/apis/rbac.authorization.k8s.io/v1/namespaces/{NAMESPACE}/rolebindings"),
        &serde_json::json!({
            "metadata": { "name": "sentence-configmap-readers" },
            "roleRef": {
                "apiGroup": "rbac.authorization.k8s.io",
                "kind": "Role",
                "name": "sentence-configmap-reader",
            },
            "subjects": [{ "kind": "Group", "name": READERS_GROUP }],
        }),
    )
    .await;

    let revoked = disassociate_policy(&router, cluster, &principal_arn).await;
    assert!(
        revoked.status == 200 || revoked.status == 204,
        "setup: expected DisassociateAccessPolicy to be served, got {revoked:?}"
    );

    // The one request the bound Role DOES grant: a read of one config map by name,
    // which is what makes every row below a refusal of authorization rather than of
    // authentication.
    let granted = dial(
        &config,
        "the read the bound Role grants",
        "GET",
        &format!("/api/v1/namespaces/{NAMESPACE}/configmaps/{CONFIG_MAP}"),
        None,
    )
    .await;
    assert_eq!(
        granted.status, 200,
        "case the read the bound Role grants: expected 200, got {granted:?}"
    );

    let rows = vec![
        Row {
            case: "a named object, the write of the config map the Role may only read",
            method: "PUT",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps/{CONFIG_MAP}"),
            body: Some(serde_json::json!({
                "metadata": { "name": CONFIG_MAP },
                "data": { "color": "green" },
            })),
            verb: "update",
            addressed: Some(Addressed {
                name: Some(CONFIG_MAP),
                qualified: "configmaps",
                resource: "configmaps",
                api_group: "",
                namespace: Some(NAMESPACE),
            }),
        },
        Row {
            case: "the same resource addressed as a COLLECTION, which names no object",
            method: "POST",
            path: format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
            body: Some(serde_json::json!({
                "metadata": { "name": "sentence-second" },
                "data": { "color": "red" },
            })),
            verb: "create",
            addressed: Some(Addressed {
                name: None,
                qualified: "configmaps",
                resource: "configmaps",
                api_group: "",
                namespace: Some(NAMESPACE),
            }),
        },
        Row {
            case: "a named object of a resource the Role does not name",
            method: "DELETE",
            path: format!("/api/v1/namespaces/{NAMESPACE}/secrets/sentence-db"),
            body: None,
            verb: "delete",
            addressed: Some(Addressed {
                name: Some("sentence-db"),
                qualified: "secrets",
                resource: "secrets",
                api_group: "",
                namespace: Some(NAMESPACE),
            }),
        },
        Row {
            case: "that resource addressed as a collection",
            method: "GET",
            path: format!("/api/v1/namespaces/{NAMESPACE}/secrets"),
            body: None,
            verb: "list",
            addressed: Some(Addressed {
                name: None,
                qualified: "secrets",
                resource: "secrets",
                api_group: "",
                namespace: Some(NAMESPACE),
            }),
        },
        Row {
            case: "a named object of a NAMED API group, whose token is qualified by it",
            method: "PUT",
            path: format!("/apis/apps/v1/namespaces/{NAMESPACE}/deployments/sentence-web"),
            body: Some(serde_json::json!({ "metadata": { "name": "sentence-web" } })),
            verb: "update",
            addressed: Some(Addressed {
                name: Some("sentence-web"),
                qualified: "deployments.apps",
                resource: "deployments",
                api_group: "apps",
                namespace: Some(NAMESPACE),
            }),
        },
        Row {
            case: "a SUBRESOURCE of a named object, which the object clause does not carry",
            method: "GET",
            path: format!("/api/v1/namespaces/{NAMESPACE}/pods/sentence-web-0/log"),
            body: None,
            verb: "get",
            addressed: Some(Addressed {
                name: Some("sentence-web-0"),
                qualified: "pods",
                resource: "pods/log",
                api_group: "",
                namespace: Some(NAMESPACE),
            }),
        },
        Row {
            case: "the same resource in a namespace the binding does not reach",
            method: "GET",
            path: format!("/api/v1/namespaces/{OTHER_NAMESPACE}/configmaps/{CONFIG_MAP}"),
            body: None,
            verb: "get",
            addressed: Some(Addressed {
                name: Some(CONFIG_MAP),
                qualified: "configmaps",
                resource: "configmaps",
                api_group: "",
                namespace: Some(OTHER_NAMESPACE),
            }),
        },
        Row {
            case: "a namespace addressed BY NAME, which is the namespace it is in",
            method: "GET",
            path: format!("/api/v1/namespaces/{OTHER_NAMESPACE}"),
            body: None,
            verb: "get",
            addressed: Some(Addressed {
                name: Some(OTHER_NAMESPACE),
                qualified: "namespaces",
                resource: "namespaces",
                api_group: "",
                namespace: Some(OTHER_NAMESPACE),
            }),
        },
        Row {
            case: "a cluster-scoped collection, which names no object",
            method: "POST",
            path: "/api/v1/namespaces".to_string(),
            body: Some(serde_json::json!({ "metadata": { "name": "sentence-nowhere" } })),
            verb: "create",
            addressed: Some(Addressed {
                name: None,
                qualified: "namespaces",
                resource: "namespaces",
                api_group: "",
                namespace: None,
            }),
        },
        Row {
            case: "a named cluster-scoped object of a named API group",
            method: "GET",
            path: "/apis/rbac.authorization.k8s.io/v1/clusterroles/sentence-view".to_string(),
            body: None,
            verb: "get",
            addressed: Some(Addressed {
                name: Some("sentence-view"),
                qualified: "clusterroles.rbac.authorization.k8s.io",
                resource: "clusterroles",
                api_group: "rbac.authorization.k8s.io",
                namespace: None,
            }),
        },
        // A path that addresses no resource at all names no object and no resource:
        // the authorizer's sentence for one is about the PATH.
        Row {
            case: "a path that addresses no resource",
            method: "GET",
            path: "/logs".to_string(),
            body: None,
            verb: "get",
            addressed: None,
        },
    ];

    for row in &rows {
        let answered = dial(&config, row.case, row.method, &row.path, row.body.as_ref()).await;
        assert_eq!(
            answered.status, 403,
            "case {:?}: expected HTTP 403, got {answered:?}",
            row.case
        );
        assert_eq!(
            answered.body["reason"],
            serde_json::json!("Forbidden"),
            "case {:?}: expected reason Forbidden, got {:?}",
            row.case,
            answered.body
        );
        let expected = row.expected();
        let message = answered.body["message"].as_str().unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {:?}: expected message {expected:?}, got {message:?}",
            row.case
        );
        // The object the sentence names is the object the `Status` details name, so a
        // client that parses either is told about the same object.
        if let Some(name) = row.names() {
            assert_eq!(
                answered.body["details"]["name"],
                serde_json::json!(name),
                "case {:?}: expected details.name {name:?}, got {:?}",
                row.case,
                answered.body
            );
        }
    }
}
