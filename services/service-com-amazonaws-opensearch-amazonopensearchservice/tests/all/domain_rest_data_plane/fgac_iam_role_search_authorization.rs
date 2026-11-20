// SPDX-License-Identifier: CC0-1.0

//! Fine-grained access control over the address a domain PUBLISHES, decided for the
//! principal a client of an estate actually reaches it with: an IAM role assumed through
//! STS, on a domain whose master user is a user of the domain's own internal database — so
//! no IAM identity is the master and every IAM principal's verdict is the role mappings'
//! alone.
//!
//! RULE: on a domain with fine-grained access control switched on, the address the control
//! plane published for it ANSWERS a SigV4-signed request, and the CLUSTER decides it: a
//! principal that is neither the master user nor bound by a role mapping is refused `403`
//! carrying the security plugin's own `security_exception` — the action it refused and the
//! user it refused it for, with the backend roles that user arrived with — and a principal a
//! role mapping DOES bind may do exactly what the role it is bound to permits, on the
//! indexes that role's patterns name, and no more.
//!
//! Three suites, each over a SET of inputs rather than one recorded call:
//!
//! * the refusal, over a table of routes spanning what the boundary covers (a search of one
//!   index and of every index, a count, a document read, a document write, the cluster's own
//!   health) — so the action the sentence names is the request's and not a fixed string, and
//!   the answer is the cluster's document rather than the front door's envelope or the IAM
//!   layer's;
//! * the mapping, which is the ONE change that makes those same requests serve, with nothing
//!   about the request, the credential or the IAM grant moved;
//! * the GRAIN of a mapping: bound to a role that permits one action group on one index
//!   pattern, the same principal is served what that role names and refused everything
//!   outside it — the rows swap which side of the role document each request falls on.
//!
//! The parent suite's harness, session seeding and signed-request helper are reused
//! ([`super`]), the address is read where a client reads it
//! ([`crate::published_domain_address`]), and the master's own Basic-authenticated requests
//! go through the internal-user-database suite's helper
//! ([`crate::fgac_internal_user_database`]); this suite adds no second copy of any of them.

use super::*;

use crate::domain_lifecycle::settled_domain;
// The domain configuration under test, and the master user's own credentials, are the
// internal-user-database suite's: one create body for the one configuration both suites are
// about, rather than a second copy of it here.
use crate::fgac_internal_user_database::{create_body, rest as basic_rest, MASTER, MASTER_PASSWORD};
use crate::published_domain_address::{at_address, published_endpoint, split_address};

/// The index the routes below read and write. A row's path below is a `&'static str`, so it
/// spells this name out rather than interpolating it.
const INDEX: &str = "catalog";

/// A second index, so a role that names one index pattern is observed on both sides of it.
const OTHER_INDEX: &str = "archive";

/// The document the master seeds, which the document read addresses.
const DOCUMENT: &str = "one";

/// The role the master binds the assumed-role principal to in the mapped cases: the security
/// plugin's own all-permitting predefined role.
const ALL_ACCESS: &str = "all_access";

/// The role written by the cases that pin a mapping's GRAIN.
const SEARCHER: &str = "catalog_searcher";

/// Create such a domain, wait until it reports itself up — the condition a client polls for
/// before it dials the data plane — and answer the address `DescribeDomain` published for it.
async fn given_fgac_domain(emulator: &Emulator, domain: &str) -> String {
    let created = emulator.create(create_body(domain)).await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain with fine-grained access control and an internal master user\": \
         expected 200, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(emulator, domain).await;
    published_endpoint(emulator, domain, "DescribeDomain once the domain reports itself up").await
}

/// One request the master makes with its Basic credential — every case's setup, and the
/// control that each route is one the cluster serves at all.
///
/// The published address is read through the ONE split both halves of this suite use
/// ([`split_address`]), the same one `at_address` reads it through for a signed request, so
/// the two cannot drift on how a published value is dialled whichever shape it takes.
async fn as_master(
    emulator: &Emulator,
    address: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let (authority, prefix) = split_address(address);
    basic_rest(
        emulator,
        authority,
        Some((MASTER, MASTER_PASSWORD)),
        method,
        &format!("{prefix}{path}"),
        body,
    )
    .await
}

/// One SigV4-signed request from the assumed-role session, sent to the address the control
/// plane published and dialled exactly as a client dials it.
async fn as_assumed_role(
    emulator: &Emulator,
    address: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    at_address(emulator, address, SESSION_KEY_ID, method, path, body).await
}

/// Seed the session the signed requests speak for, the IAM grant its role carries — so what
/// is left to decide is the cluster's part — and the two indexes with one document, written
/// by the master.
async fn given_principal_and_indexes(emulator: &Emulator, address: &str) {
    given_session(emulator);
    given_data_plane_policy(emulator);
    for index in [INDEX, OTHER_INDEX] {
        let created = as_master(
            emulator,
            address,
            "PUT",
            &format!("/{index}"),
            Some(json!({ "mappings": { "properties": { "title": { "type": "keyword" } } } })),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case \"the master creating {index}\": expected 200, got {} with {:?}",
            created.status, created.body
        );
        let written = as_master(
            emulator,
            address,
            "PUT",
            &format!("/{index}/_doc/{DOCUMENT}"),
            Some(json!({ "title": "seeded" })),
        )
        .await;
        assert_eq!(
            (written.status, written.body["result"].as_str()),
            (201, Some("created")),
            "case \"the master seeding {index}\": expected the engine's own answer, got {} with \
             {:?}",
            written.status,
            written.body
        );
    }
}

/// One request the boundary decides: what it asks for, and the security-plugin action name it
/// asks for it under.
struct Row {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    body: Option<Value>,
    /// The action the plugin names in its refusal. Written as the security plugin's own
    /// action names spell it, which is the source every one of these comes from — the names
    /// its default action groups are defined over.
    action: &'static str,
    /// Where the MASTER's control request for this row goes, where that must NOT be the row's
    /// own path: a WRITE control would otherwise create the very document the refused write
    /// addresses, and the document count taken around the refusals could not tell a refused
    /// write from a served one. `None` leaves the control on the row's own path.
    control_path: Option<&'static str>,
}

/// The table the boundary is read over: a search of one index and of every index, a count, a
/// document read, a document write, and a read of the cluster itself — so both namespaces the
/// plugin judges an action in (`indices:` and `cluster:`) are covered.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a search of one index",
            method: "POST",
            path: "/catalog/_search",
            body: Some(json!({})),
            action: "indices:data/read/search",
            control_path: None,
        },
        Row {
            case: "a search of every index the cluster holds",
            method: "GET",
            path: "/_search",
            body: None,
            action: "indices:data/read/search",
            control_path: None,
        },
        Row {
            case: "a count",
            method: "GET",
            path: "/catalog/_count",
            body: None,
            action: "indices:data/read/count",
            control_path: None,
        },
        Row {
            case: "a document read",
            method: "GET",
            path: "/catalog/_doc/one",
            body: None,
            action: "indices:data/read/get",
            control_path: None,
        },
        Row {
            case: "a document write",
            method: "PUT",
            // An id NOTHING else writes, so a write the cluster served instead of refusing
            // shows up in the document count taken around the refusals.
            path: "/catalog/_doc/written",
            body: Some(json!({ "title": "written" })),
            action: "indices:data/write/index",
            control_path: Some("/catalog/_doc/control"),
        },
        Row {
            case: "the cluster's own health",
            method: "GET",
            path: "/_cluster/health",
            body: None,
            action: "cluster:monitor/health",
            control_path: None,
        },
    ]
}

/// How the security plugin states a refusal of `action` for the assumed-role session: the
/// action, and the user DESCRIPTION the parent suite owns — the session's own name and the
/// backend roles its credential speaks for — so this refusal and the `authinfo` the parent
/// asserts cannot drift on who the caller is.
fn security_exception_reason(action: &str) -> String {
    format!(
        "no permissions for [{action}] and {}",
        user_description(&delivery_session_arn(), &[delivery_role_arn()])
    )
}

/// Assert `answered` is the cluster's refusal of `action` — the status, the error type, the
/// sentence, and that it is the CLUSTER's answer rather than the front door's or the IAM
/// layer's.
fn assert_refused(answered: &Answer, case: &str, action: &str) {
    assert_eq!(
        answered.status, 403,
        "case {case:?}: expected the cluster's refusal, got {} with {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        error_type(answered),
        "security_exception",
        "case {case:?}: expected the security plugin's error type, got {:?}",
        answered.body
    );
    assert_eq!(
        answered.body["error"]["reason"].as_str(),
        Some(security_exception_reason(action).as_str()),
        "case {case:?}: expected the plugin's own sentence, got {:?}",
        answered.body
    );
    assert_eq!(
        answered.body["status"].as_i64(),
        Some(403),
        "case {case:?}: every error of this surface repeats its status in the body, got {:?}",
        answered.body
    );
    // The published address is served by the DOMAIN: an answer in the front door's own
    // envelope would mean no service claimed the address a client was told to use.
    assert_eq!(
        answered.body["__type"],
        Value::Null,
        "case {case:?}: the published address must be answered by the domain itself, got {} \
         with {:?}",
        answered.status,
        answered.body
    );
    // And it is the cluster's decision rather than the IAM layer's, whose refusal carries a
    // `Message` sentence and none of the engine's document.
    assert_eq!(
        answered.body["Message"],
        Value::Null,
        "case {case:?}: IAM allows this request, so the refusal must be the cluster's, got \
         {:?}",
        answered.body
    );
}

/// RULE: an assumed IAM role no role mapping binds is refused every route the boundary covers
/// on a fine-grained-access-control domain — `403 security_exception`, naming the action the
/// request asks for and the user it was refused for — whatever its IAM policies allow.
#[tokio::test]
async fn an_unmapped_assumed_role_is_refused_the_published_data_plane() {
    let emulator = Emulator::new();
    let address = given_fgac_domain(&emulator, "fgac-unmapped").await;
    given_principal_and_indexes(&emulator, &address).await;

    // The master is served every one of the routes, so what each row below observes is the
    // CALLER and not a route the cluster does not serve. A control that WRITES goes to an id
    // of its own ([`Row::control_path`]), leaving the id the refused write addresses
    // uncreated.
    for Row {
        case,
        method,
        path,
        body,
        action: _,
        control_path,
    } in rows()
    {
        let served = as_master(
            &emulator,
            &address,
            method,
            control_path.unwrap_or(path),
            body,
        )
        .await;
        assert!(
            (200..300).contains(&served.status),
            "case {case:?}: expected the master to be served, got {} with {:?}",
            served.status,
            served.body
        );
    }
    // What the index holds once that is done, which the refusals must leave alone: the
    // document the seeding wrote and the one the master's own write control added. A write
    // the cluster served instead of refusing would make it three.
    let held = counted_documents(&emulator, &address, "before any refusal").await;
    assert_eq!(
        held,
        Some(2),
        "case \"before any refusal\": expected the seeded document and the master's own write, \
         got {held:?}"
    );

    for Row {
        case,
        method,
        path,
        body,
        action,
        control_path: _,
    } in rows()
    {
        let answered = as_assumed_role(&emulator, &address, method, path, body).await;
        assert_refused(&answered, case, action);
    }

    // Nothing the refused rows asked for happened: the index holds what it held.
    assert_eq!(
        counted_documents(&emulator, &address, "after every refusal").await,
        held,
        "case \"after every refusal\": expected the index to hold what it held before them"
    );
}

/// How many documents [`INDEX`] holds, counted as the master — the cluster's own accounting
/// of what the writes above left.
async fn counted_documents(emulator: &Emulator, address: &str, case: &str) -> Option<i64> {
    let counted = as_master(emulator, address, "GET", &format!("/{INDEX}/_count"), None).await;
    assert_eq!(
        counted.status, 200,
        "case {case:?}: expected the master's count to be served, got {} with {:?}",
        counted.status, counted.body
    );
    counted.body["count"].as_i64()
}

/// RULE (the other half): binding the role a session speaks for to a role mapping is the ONE
/// change that makes those same signed requests serve — the credential, the IAM grant and
/// the requests are the ones that were refused.
#[tokio::test]
async fn a_role_mapping_is_what_serves_the_same_signed_requests() {
    let emulator = Emulator::new();
    let address = given_fgac_domain(&emulator, "fgac-mapped").await;
    given_principal_and_indexes(&emulator, &address).await;

    // Refused before the mapping, on the row the issue's own client sends.
    let before = as_assumed_role(&emulator, &address, "POST", "/catalog/_search", Some(json!({})))
        .await;
    assert_refused(
        &before,
        "a search before any mapping binds the role",
        "indices:data/read/search",
    );

    // The security API is the master's alone — the parent suite owns that half of the rule —
    // so the mapping below is written by the master.
    //
    // The master binds the ROLE the session speaks for: the identity a mapping is written
    // with, since that is the identity the client created.
    let mapped = as_master(
        &emulator,
        &address,
        "PUT",
        &format!("/_plugins/_security/api/rolesmapping/{ALL_ACCESS}"),
        Some(json!({ "backend_roles": [delivery_role_arn()], "users": [], "hosts": [] })),
    )
    .await;
    assert_eq!(
        mapped.status, 201,
        "case \"the master binding the role\": got {} with {:?}",
        mapped.status, mapped.body
    );

    for Row {
        case,
        method,
        path,
        body,
        action: _,
        control_path: _,
    } in rows()
    {
        let answered = as_assumed_role(&emulator, &address, method, path, body).await;
        assert!(
            (200..300).contains(&answered.status),
            "case {case:?} mapped: expected the request to serve, got {} with {:?}",
            answered.status,
            answered.body
        );
    }

    // And what it was served is the cluster's own data: every document the index holds — the
    // one the seeding wrote and the one the mapped principal's own write row above added —
    // the seeded one among them.
    let searched =
        as_assumed_role(&emulator, &address, "POST", "/catalog/_search", Some(json!({}))).await;
    let total = searched.body["hits"]["total"]["value"].as_i64();
    assert_eq!(
        total,
        Some(2),
        "case \"the mapped principal's search\": expected the seeded document and the one this \
         test's own write row added, got {} with {:?}",
        searched.status,
        searched.body
    );
    assert_eq!(
        total,
        counted_documents(&emulator, &address, "the mapped principal's search").await,
        "case \"the mapped principal's search\": expected the search to answer what the \
         cluster's own count holds, got {:?}",
        searched.body
    );
    let ids: Vec<&str> = searched.body["hits"]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .filter_map(|hit| hit["_id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        ids.contains(&DOCUMENT),
        "case \"the mapped principal's search\": expected the seeded document among the hits, \
         got {ids:?}"
    );
}

/// One request decided by the ROLE a mapping binds the principal to: whether that role
/// permits it, and the action it is judged under where it does not.
struct GrainRow {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    body: Option<Value>,
    /// `None` where the role permits the request, and the action the refusal names where it
    /// does not.
    refused_action: Option<&'static str>,
}

/// RULE: a mapping binds a principal to a ROLE, and the role document says what holding it is
/// worth — the action groups it permits on the index patterns it names — so the same
/// principal is served inside that role and refused outside it.
///
/// The mapping names the role's ARN by a WILDCARD pattern, which is the form a client writes
/// one in to cover a family of roles, so what binds the principal is the pattern and not an
/// exact string.
#[tokio::test]
async fn a_mapped_principal_may_do_only_what_its_role_permits() {
    let emulator = Emulator::new();
    let address = given_fgac_domain(&emulator, "fgac-grained").await;
    given_principal_and_indexes(&emulator, &address).await;

    // The role: the `search` default action group on one index pattern, written in the
    // security plugin's own role-document form (`index_permissions` entries carrying
    // `index_patterns` and `allowed_actions`).
    let role = as_master(
        &emulator,
        &address,
        "PUT",
        &format!("/_plugins/_security/api/roles/{SEARCHER}"),
        Some(json!({
            "cluster_permissions": [],
            "index_permissions": [{
                "index_patterns": [format!("{INDEX}*")],
                "allowed_actions": ["search"],
            }],
        })),
    )
    .await;
    assert_eq!(
        role.status, 201,
        "case \"the master writing the role\": got {} with {:?}",
        role.status, role.body
    );
    // The mapping names the role by a PATTERN covering a family of roles, derived from the
    // role's own ARN with its last character replaced by the wildcard — so a rename of the
    // role cannot silently move which side of the mapping the principal falls on.
    let mut pattern = delivery_role_arn();
    pattern.pop();
    pattern.push('*');
    let mapped = as_master(
        &emulator,
        &address,
        "PUT",
        &format!("/_plugins/_security/api/rolesmapping/{SEARCHER}"),
        Some(json!({ "backend_roles": [pattern] })),
    )
    .await;
    assert_eq!(
        mapped.status, 201,
        "case \"the master binding the role by a wildcard pattern\": got {} with {:?}",
        mapped.status, mapped.body
    );

    let grain: Vec<GrainRow> = vec![
        GrainRow {
            case: "a search of the index the role's pattern names",
            method: "POST",
            path: "/catalog/_search",
            body: Some(json!({})),
            refused_action: None,
        },
        GrainRow {
            case: "a search of an index no pattern of the role names",
            method: "POST",
            path: "/archive/_search",
            body: Some(json!({})),
            refused_action: Some("indices:data/read/search"),
        },
        GrainRow {
            case: "a count the role's action group does not carry",
            method: "GET",
            path: "/catalog/_count",
            body: None,
            refused_action: Some("indices:data/read/count"),
        },
        GrainRow {
            case: "a write the role's action group does not carry",
            method: "PUT",
            path: "/catalog/_doc/two",
            body: Some(json!({ "title": "written" })),
            refused_action: Some("indices:data/write/index"),
        },
        GrainRow {
            case: "a cluster read the role permits no cluster action for",
            method: "GET",
            path: "/_cluster/health",
            body: None,
            refused_action: Some("cluster:monitor/health"),
        },
    ];

    for GrainRow {
        case,
        method,
        path,
        body,
        refused_action,
    } in grain
    {
        let answered = as_assumed_role(&emulator, &address, method, path, body).await;
        match refused_action {
            None => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: expected the role to serve it, got {} with {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.body["hits"]["total"]["value"].as_i64(),
                    Some(1),
                    "case {case:?}: expected the document the master seeded, got {:?}",
                    answered.body
                );
            }
            Some(action) => assert_refused(&answered, case, action),
        }
    }

    // The master is served every one of them on the same domain, so each refusal above is
    // the ROLE's narrowing and not a route or an index the cluster lacks.
    for (case, method, path) in [
        ("the master's search of the other index", "POST", "/archive/_search"),
        ("the master's count", "GET", "/catalog/_count"),
        ("the master's cluster read", "GET", "/_cluster/health"),
    ] {
        let served = as_master(&emulator, &address, method, path, None).await;
        assert!(
            (200..300).contains(&served.status),
            "case {case:?}: expected the master to be served, got {} with {:?}",
            served.status,
            served.body
        );
    }
}
