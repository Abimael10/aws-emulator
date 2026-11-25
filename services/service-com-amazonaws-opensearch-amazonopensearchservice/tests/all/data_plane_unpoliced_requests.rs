// SPDX-License-Identifier: CC0-1.0

//! What decides a signed data-plane request that NO policy in force says anything about.
//!
//! The rule under test: **a signed request to the address a domain publishes is refused at
//! the IAM layer only where a policy in force refuses it — an explicit `Deny` on either
//! side, or a domain access policy none of whose statements allows the request. Where the
//! domain carries NO access policy and the signing principal's identity policies say nothing
//! about the action, nothing in force refuses it, so the decision belongs to the CLUSTER the
//! request reached: on a domain with fine-grained access control that is the master user and
//! the role mappings (the master may do everything and a principal no mapping binds is
//! refused `403` with `error.type` `security_exception`), and on a domain without it the
//! cluster's own surface.**
//!
//! That is the configuration a client provisioning a fine-grained-access-control domain
//! actually arrives in: it creates the master as an IAM ROLE, assumes it, and sends index and
//! search requests with that session — having written no domain access policy and no
//! identity policy, because the authorization it asked for is the cluster's. Refusing such a
//! request at the IAM layer hides the whole boundary: the master is never served, and the
//! principal the boundary is about is refused with IAM's `Message` envelope instead of the
//! cluster's `security_exception`.
//!
//! The rows span what the rule generalises over: whether the domain has fine-grained access
//! control at all, which principal signs (the master role's own session, a session of a role
//! no mapping binds, the account's own credential), what it asks for (the cluster's health
//! and banner, an index create, a document write, a count, a search), and what the policies
//! in force state (nothing at all, an explicit identity-side `Deny`, an access policy that
//! allows somebody else, and one that names the caller's whole ACCOUNT — which grants no
//! principal of it and leaves that account's identity policies deciding) — so neither the
//! verdict nor the envelope a refusal carries is one a fixed answer would give.
//!
//! Every case drives the real wire path — an HTTP request through the multi-service router
//! into the mounted service, sent to the address the control plane PUBLISHED for the domain.
//! The control-plane harness comes from [`super::domain_lifecycle`] and the session, policy
//! and published-address helpers from [`super::published_domain_address`]; this suite adds no
//! second copy of them.

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::ingest::{
    self, DocumentWrite,
};

use super::domain_lifecycle::{settled_domain, Answer, Emulator, DEFAULT_ACCESS_KEY_ID, REGION};
use super::published_domain_address::{
    at_address, given_role_policy, given_session, published_endpoint, refusal_message, role_arn,
    session_arn,
};

/// The session name every seeded credential of this suite carries.
const SESSION: &str = "unpoliced-session";

/// The role a domain names as its fine-grained-access-control master user, and the access key
/// id of its session. It carries NO identity policy, and no domain here carries an access
/// policy: what is left to decide is the cluster's own.
const MASTER_ROLE: &str = "unpoliced-master";
const MASTER_KEY: &str = "ASIAEXAMPLEUNPOLMST1";

/// A role of the same account that no role mapping binds and no policy names.
const UNMAPPED_ROLE: &str = "unpoliced-unmapped";
const UNMAPPED_KEY: &str = "ASIAEXAMPLEUNPOLUNM1";

/// A role whose OWN identity policy explicitly denies these actions — the one rule that
/// refuses a request however little else is stated.
const DENIED_ROLE: &str = "unpoliced-denied";
const DENIED_KEY: &str = "ASIAEXAMPLEUNPOLDNY1";

/// The role a PRODUCER outside this service writes as, carrying an identity policy that says
/// nothing about this service's data-plane actions — documents that exist and state nothing,
/// which is a different position from holding none.
const PRODUCER_ROLE: &str = "unpoliced-producer";

/// The index the rows write to, count and search.
const INDEX: &str = "catalog";

/// The `error.type` a refusal the CLUSTER states carries.
fn error_type(answer: &Answer) -> String {
    answer.body["error"]["type"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// An identity-based policy that explicitly denies every data-plane action of this service.
fn identity_deny() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Deny", "Action": "es:ESHttp*", "Resource": "*" }],
    })
}

/// An access policy that allows SOMEBODY ELSE on every path of `domain` — a domain that does
/// carry one, so a request none of its statements allows is refused by it.
fn allows_someone_else(domain: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": role_arn("unpoliced-somebody-else") },
            "Action": "es:ESHttp*",
            "Resource": format!(
                "arn:aws:es:{REGION}:{}:domain/{domain}/*",
                ::engine::emulator_account_id()
            ),
        }],
    })
    .to_string()
}

/// An identity-based policy that allows every data-plane action of this service — the grant
/// that decides a request where the domain's access policy names the whole ACCOUNT, which
/// grants no principal of it on its own.
fn identity_grant() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "es:ESHttp*", "Resource": "*" }],
    })
}

/// The master user of the domain whose access policy delegates to the account, and the access
/// key id of its session. It carries the identity grant that delegation leaves deciding.
const DELEGATED_MASTER_ROLE: &str = "unpoliced-rooted-master";
const DELEGATED_MASTER_KEY: &str = "ASIAEXAMPLEUNPOLRTM1";

/// A role of the same account that domain's cluster binds to no role, carrying that same
/// identity grant — so IAM admits its requests and what is left to decide is the cluster's.
const DELEGATED_UNMAPPED_ROLE: &str = "unpoliced-rooted-unmapped";
const DELEGATED_UNMAPPED_KEY: &str = "ASIAEXAMPLEUNPOLRTU1";

/// An access policy that allows this service's data-plane actions to the ACCOUNT ROOT — the
/// delegation a client writes when the decision it wants taken is IAM's, naming the account
/// rather than each principal of it.
fn allows_the_account_root(domain: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()) },
            "Action": "es:*",
            "Resource": format!(
                "arn:aws:es:{REGION}:{}:domain/{domain}/*",
                ::engine::emulator_account_id()
            ),
        }],
    })
    .to_string()
}

/// `CreateDomain` for a domain carrying no access policy, with fine-grained access control
/// switched on for `master_user_arn` when one is named and off when none is.
fn create_body(domain: &str, master_user_arn: Option<String>) -> Value {
    let mut body = json!({
        "DomainName": domain,
        "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
    });
    if let Some(master_user_arn) = master_user_arn {
        body["EncryptionAtRestOptions"] = json!({ "Enabled": true });
        body["NodeToNodeEncryptionOptions"] = json!({ "Enabled": true });
        body["DomainEndpointOptions"] = json!({ "EnforceHTTPS": true });
        body["AdvancedSecurityOptions"] = json!({
            "Enabled": true,
            "InternalUserDatabaseEnabled": false,
            "MasterUserOptions": { "MasterUserARN": master_user_arn },
        });
    }
    body
}

/// Seed the three sessions, and the one identity policy any of them carries.
fn given_principals(emulator: &Emulator) {
    for (role, key) in [
        (MASTER_ROLE, MASTER_KEY),
        (UNMAPPED_ROLE, UNMAPPED_KEY),
        (DENIED_ROLE, DENIED_KEY),
    ] {
        given_session(emulator, role, SESSION, key);
    }
    given_role_policy(emulator, DENIED_ROLE, identity_deny());
}

/// Create `domain` and answer the address it publishes once it reports itself settled.
async fn given_domain(emulator: &Emulator, domain: &str, master_user_arn: Option<String>) -> String {
    let created = emulator
        .create(create_body(domain, master_user_arn))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain({domain:?})\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(emulator, domain).await;
    published_endpoint(emulator, domain, domain).await
}

/// The routes a principal is observed on, as `(case, method, path, body)`.
fn routes() -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        (
            "the cluster health",
            "GET",
            "/_cluster/health".to_string(),
            None,
        ),
        ("the cluster banner", "GET", "/".to_string(), None),
        ("an index create", "PUT", format!("/{INDEX}"), None),
        (
            "a document write",
            "PUT",
            format!("/{INDEX}/_doc/one"),
            Some(json!({ "title": "unpoliced" })),
        ),
        ("a count", "GET", format!("/{INDEX}/_count"), None),
        (
            "a search",
            "POST",
            format!("/{INDEX}/_search"),
            Some(json!({})),
        ),
    ]
}

/// RULE: on a domain with fine-grained access control that carries no access policy, the
/// master role's session — holding no identity policy of its own — is served every route, and
/// a session of a role no mapping binds is refused by the CLUSTER with `403
/// security_exception` rather than by the IAM layer.
#[tokio::test]
async fn an_fgac_domain_with_no_policies_is_decided_by_its_cluster() {
    let emulator = Emulator::new();
    given_principals(&emulator);
    let address = given_domain(&emulator, "unpoliced-fgac", Some(role_arn(MASTER_ROLE))).await;

    for (case, method, path, body) in routes() {
        let served = at_address(&emulator, &address, MASTER_KEY, method, &path, body.clone()).await;
        assert!(
            (200..300).contains(&served.status),
            "case {case:?}: expected the master role's session to be served, got {} with {:?}",
            served.status,
            served.body
        );
        let refused = at_address(&emulator, &address, UNMAPPED_KEY, method, &path, body).await;
        assert_eq!(
            (refused.status, error_type(&refused)),
            (403, "security_exception".to_string()),
            "case {case:?}: expected the cluster's own refusal of an unmapped principal, got {} \
             with {:?}",
            refused.status,
            refused.body
        );
    }

    // The document the master wrote is the one the cluster answers its own reads with, so what
    // was served really went through the domain's records.
    let searched = at_address(
        &emulator,
        &address,
        MASTER_KEY,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (
            searched.status,
            searched.body["hits"]["hits"][0]["_source"]["title"].as_str(),
        ),
        (200, Some("unpoliced")),
        "case \"the master reading back what it wrote\": expected its own document, got {} with \
         {:?}",
        searched.status,
        searched.body
    );
}

/// RULE (the other side of the same reading): on a domain that carries no access policy and
/// no fine-grained access control, a principal no policy states anything about reaches the
/// cluster, which has no boundary of its own to refuse it — the position the account's own
/// credential is already in, now read for a role session too.
#[tokio::test]
async fn a_domain_with_no_policies_at_all_serves_every_principal_it_can_resolve() {
    let emulator = Emulator::new();
    given_principals(&emulator);
    let address = given_domain(&emulator, "unpoliced-open", None).await;

    for key in [MASTER_KEY, UNMAPPED_KEY, DEFAULT_ACCESS_KEY_ID] {
        let served = at_address(&emulator, &address, key, "GET", "/_cluster/health", None).await;
        assert_eq!(
            served.status, 200,
            "case \"the cluster health for {key}\": expected the cluster to answer a request no \
             policy refuses, got {} with {:?}",
            served.status, served.body
        );
    }
}

/// RULE (the same reading where the domain DOES carry an access policy, and that policy names
/// the caller's whole ACCOUNT): naming an account is not naming a principal, so such a
/// statement grants no principal of it by itself and delegates the decision to that account's
/// own identity-based policies — a session whose role holds the grant is served, after which
/// the CLUSTER is what decides and refuses a role no mapping binds with `security_exception`,
/// while a session the delegation leaves with no grant at all is refused by IAM with the
/// sentence naming the side that would have had to allow it.
#[tokio::test]
async fn an_access_policy_about_the_account_root_delegates_to_the_identity_policies() {
    let emulator = Emulator::new();
    given_principals(&emulator);
    for (role, key) in [
        (DELEGATED_MASTER_ROLE, DELEGATED_MASTER_KEY),
        (DELEGATED_UNMAPPED_ROLE, DELEGATED_UNMAPPED_KEY),
    ] {
        given_session(&emulator, role, SESSION, key);
        given_role_policy(&emulator, role, identity_grant());
    }
    let domain = "unpoliced-rooted";
    let created = emulator
        .create({
            let mut body = create_body(domain, Some(role_arn(DELEGATED_MASTER_ROLE)));
            body["AccessPolicies"] = Value::String(allows_the_account_root(domain));
            body
        })
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain({domain:?})\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(&emulator, domain).await;
    let address = published_endpoint(&emulator, domain, domain).await;

    let served = at_address(
        &emulator,
        &address,
        DELEGATED_MASTER_KEY,
        "GET",
        "/_cluster/health",
        None,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case \"the master role's session, whose identity policy allows the action\": expected \
         the cluster health, got {} with {:?}",
        served.status, served.body
    );
    let refused = at_address(
        &emulator,
        &address,
        DELEGATED_UNMAPPED_KEY,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (refused.status, error_type(&refused)),
        (403, "security_exception".to_string()),
        "case \"a session of a role the identity side admits and no mapping binds\": expected \
         the cluster's own refusal, got {} with {:?}",
        refused.status,
        refused.body
    );
    let delegated = at_address(
        &emulator,
        &address,
        UNMAPPED_KEY,
        "GET",
        "/_cluster/health",
        None,
    )
    .await;
    assert_eq!(
        (delegated.status, refusal_message(&delegated)),
        (
            403,
            format!(
                "User: {} is not authorized to perform: es:ESHttpGet because no identity-based \
                 policy allows the es:ESHttpGet action",
                session_arn(UNMAPPED_ROLE, SESSION)
            )
        ),
        "case \"a session the account-wide statement grants nothing\": expected IAM's own \
         refusal, got {} with {:?}",
        delegated.status,
        delegated.body
    );
}

/// One request the IAM layer must still refuse, and the clause its sentence ends with.
struct RefusedRow {
    case: &'static str,
    /// The domain the row is read on: the one carrying no policy at all, or the one whose
    /// access policy is about somebody else.
    policed_domain: bool,
    access_key_id: &'static str,
    role: &'static str,
    /// The clause naming which of IAM's rules refused.
    clause: &'static str,
}

/// The table: what is left after the change — the two refusals a policy in force states.
const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "a principal its own identity policy explicitly denies",
        policed_domain: false,
        access_key_id: DENIED_KEY,
        role: DENIED_ROLE,
        clause: "with an explicit deny in an identity-based policy",
    },
    RefusedRow {
        case: "a principal the domain's own access policy names nowhere",
        policed_domain: true,
        access_key_id: UNMAPPED_KEY,
        role: UNMAPPED_ROLE,
        clause: "because no resource-based policy allows the es:ESHttpGet action",
    },
    RefusedRow {
        case: "an explicitly denied principal where the access policy is about somebody else",
        policed_domain: true,
        access_key_id: DENIED_KEY,
        role: DENIED_ROLE,
        clause: "with an explicit deny in an identity-based policy",
    },
];

/// RULE: a policy in force still refuses — an explicit `Deny` in the principal's identity
/// policies, and a domain access policy none of whose statements allows the request — and that
/// refusal is AWS's own `403 Message` naming the principal, the action the method asks for and
/// the rule that refused, with none of the cluster's error document.
#[tokio::test]
async fn a_policy_in_force_still_refuses_with_aws_own_sentence() {
    let emulator = Emulator::new();
    given_principals(&emulator);
    let unpoliced = given_domain(&emulator, "unpoliced-plain", None).await;
    let policed_name = "unpoliced-policed";
    let created = emulator
        .create({
            let mut body = create_body(policed_name, None);
            body["AccessPolicies"] = Value::String(allows_someone_else(policed_name));
            body
        })
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain({policed_name:?})\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(&emulator, policed_name).await;
    let policed = published_endpoint(&emulator, policed_name, policed_name).await;

    for row in REFUSED_ROWS {
        let case = row.case;
        let address = if row.policed_domain {
            &policed
        } else {
            &unpoliced
        };
        let answered = at_address(
            &emulator,
            address,
            row.access_key_id,
            "GET",
            "/_cluster/health",
            None,
        )
        .await;
        assert_eq!(
            (answered.status, refusal_message(&answered)),
            (
                403,
                format!(
                    "User: {} is not authorized to perform: es:ESHttpGet {}",
                    session_arn(row.role, SESSION),
                    row.clause
                )
            ),
            "case {case:?}: expected IAM's own refusal sentence, got {} with {:?}",
            answered.status,
            answered.body
        );
        assert_eq!(
            answered.body["error"],
            Value::Null,
            "case {case:?}: the IAM layer refuses before the cluster, so its answer carries no \
             cluster error document, got {:?}",
            answered.body
        );
    }
}

/// RULE, on the OTHER path this one decision serves: a PRODUCER outside this service writing a
/// document as the role it was configured with takes the same decision
/// (`ingest::authorize`), so a write no policy in force says anything about is left to the
/// cluster there too — it lands on a domain that has no boundary of its own, and the cluster
/// refuses it on a fine-grained-access-control domain that binds the producer's role to no
/// role.
///
/// The producer's role carries an identity policy that is about ANOTHER service, so the
/// documents in force on it exist and state nothing about this request — the case the
/// data-plane rows above read through a signed request, read here through the write path.
#[tokio::test]
async fn a_producers_write_no_policy_states_anything_about_is_left_to_the_cluster() {
    let emulator = Emulator::new();
    given_role_policy(
        &emulator,
        PRODUCER_ROLE,
        json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": "s3:GetObject", "Resource": "*" }],
        }),
    );
    let open_domain = "unpoliced-producer-open";
    let guarded_domain = "unpoliced-producer-guarded";
    let open_address = given_domain(&emulator, open_domain, None).await;
    let guarded_address =
        given_domain(&emulator, guarded_domain, Some(role_arn(MASTER_ROLE))).await;

    let delivered = |address: &str| {
        ingest::index_document(
            &**emulator.engine(),
            &DocumentWrite {
                domain: address,
                index_name: INDEX,
                document_id: Some("delivered"),
                document: br#"{"title":"delivered"}"#,
                principal_arn: &role_arn(PRODUCER_ROLE),
                now: SystemTime::now(),
            },
        )
    };

    let landed = delivered(&open_address);
    assert!(
        landed.is_ok(),
        "case \"a producer writing to a domain no policy bounds\": expected the write to land, \
         got {landed:?}"
    );
    let refused = delivered(&guarded_address);
    assert!(
        matches!(
            &refused,
            Err(ingest::WriteRefusal::Rejected { error_type, .. })
                if *error_type == ingest::SECURITY_EXCEPTION
        ),
        "case \"a producer writing to a domain whose cluster binds its role to no role\": \
         expected the cluster's own refusal, got {refused:?}"
    );
}
