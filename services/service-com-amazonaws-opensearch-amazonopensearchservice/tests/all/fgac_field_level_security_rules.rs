// SPDX-License-Identifier: CC0-1.0

//! What a role's FIELD-LEVEL SECURITY rules are worth, driven over the real wire path: an
//! HTTP Basic-authenticated request to the endpoint host the control plane published, into
//! the domain's own REST data plane.
//!
//! Three rules, each over a SET of inputs rather than one recorded call:
//!
//! * the `fls` list of a role's `index_permissions` entry says WHICH fields of the indexes
//!   that entry's patterns name the principals the role binds may read — a bare name
//!   INCLUDES only the fields named, a `~`-prefixed name EXCLUDES the field named and
//!   leaves the rest, each name is the field's dotted path and may carry the `*` wildcard —
//!   and a field the rules hide is answered exactly as one the document does not carry: it
//!   is in no hit's `_source`, in no realtime read's, its value is nowhere in the answer at
//!   all, and it matches no clause of a query, while a principal no rule narrows reads every
//!   field;
//! * a principal nothing binds to any role may do nothing, so once the role mapping that
//!   bound it is deleted its reads are refused `403 security_exception` naming the action
//!   refused and the user it was refused for — and that refusal carries nothing of the
//!   fields the deleted role used to narrow;
//! * the CLUSTER-HEALTH route — the one a client polls before it configures anything — is
//!   decided like every other route of the plane: authenticated against the internal user
//!   database and then judged by what the authenticated user's roles permit at cluster
//!   level.
//!
//! The domain configuration, the Basic-authenticated request and the published endpoint come
//! from [`super::fgac_internal_user_database`] and [`super::domain_rest_data_plane`] rather
//! than a second copy of either.

use serde_json::{json, Value};

use super::domain_lifecycle::{Answer, Emulator};
use super::domain_rest_data_plane::{endpoint_of, error_type};
use super::fgac_internal_user_database::{
    create_body, hit_ids, rest, MASTER, MASTER_PASSWORD,
};

/// The index every case below reads.
const INDEX: &str = "catalog";

/// The one document it holds.
const DOCUMENT: &str = "one";

/// The dotted path of every LEAF field that document carries, with the value held there —
/// each one single-token text, so a `term` clause on it is a question about that one field.
///
/// A row of the table below names the paths it leaves visible; every other path here is one
/// its rules hide.
const LEAVES: [(&str, &str); 5] = [
    ("title", "widget"),
    ("secret", "classified"),
    ("price", "seven"),
    ("audit.actor", "jordan"),
    ("audit.note", "approved"),
];

/// The document those leaves compose, as a client writes it.
fn document() -> Value {
    json!({
        "title": "widget",
        "secret": "classified",
        "price": "seven",
        "audit": { "actor": "jordan", "note": "approved" }
    })
}

/// A JSON pointer to the leaf `path` of a `_source`: the dotted path, one segment per
/// member.
fn pointer(path: &str) -> String {
    format!("/{}", path.replace('.', "/"))
}

/// The same request as the master user, which every case's setup is performed by.
async fn as_master(
    emulator: &Emulator,
    host: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    rest(
        emulator,
        host,
        Some((MASTER, MASTER_PASSWORD)),
        method,
        path,
        body,
    )
    .await
}

/// A domain with fine-grained access control over an internal user database, holding
/// [`INDEX`] with the one [`DOCUMENT`] in it — written by the master user, whom no role
/// narrows.
async fn given_domain(emulator: &Emulator, domain: &str) -> String {
    let created = emulator.create(create_body(domain)).await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain with an internal master user\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    let host = endpoint_of(emulator, domain);
    let index = as_master(emulator, &host, "PUT", &format!("/{INDEX}"), None).await;
    assert!(
        (200..300).contains(&index.status),
        "case \"creating index {INDEX}\": expected a 2xx, got {} with {:?}",
        index.status,
        index.body
    );
    let written = as_master(
        emulator,
        &host,
        "PUT",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        Some(document()),
    )
    .await;
    assert!(
        (200..300).contains(&written.status),
        "case \"writing document {DOCUMENT}\": expected a 2xx, got {} with {:?}",
        written.status,
        written.body
    );
    host
}

/// Write the role `role` permitting reads of [`INDEX`] under the field-level security rules
/// `rules`, add the internal user `user` at `password`, and bind that user to the role —
/// the three writes a client makes to put a narrowed reader behind a role.
async fn given_narrowed_reader(
    emulator: &Emulator,
    host: &str,
    role: &str,
    rules: &[&str],
    user: &str,
    password: &str,
) {
    let mut permission = json!({
        "index_patterns": [INDEX],
        "allowed_actions": ["read", "search"]
    });
    // A role that imposes no field-level security carries no `fls` member at all, which is
    // the shape a client writes when it narrows nothing.
    if !rules.is_empty() {
        permission["fls"] = json!(rules);
    }
    let written = as_master(
        emulator,
        host,
        "PUT",
        &format!("/_plugins/_security/api/roles/{role}"),
        Some(json!({ "index_permissions": [permission] })),
    )
    .await;
    assert_eq!(
        written.status, 201,
        "case \"the master writing role {role}\": expected 201, got {} with {:?}",
        written.status, written.body
    );
    let added = as_master(
        emulator,
        host,
        "PUT",
        &format!("/_plugins/_security/api/internalusers/{user}"),
        Some(json!({ "password": password })),
    )
    .await;
    assert_eq!(
        added.status, 201,
        "case \"the master adding internal user {user}\": expected 201, got {} with {:?}",
        added.status, added.body
    );
    let mapped = as_master(
        emulator,
        host,
        "PUT",
        &format!("/_plugins/_security/api/rolesmapping/{role}"),
        Some(json!({ "users": [user] })),
    )
    .await;
    assert_eq!(
        mapped.status, 201,
        "case \"the master binding {user} to {role}\": expected 201, got {} with {:?}",
        mapped.status, mapped.body
    );
}

/// RULE: a role's `fls` rules decide which fields of the indexes its `index_permissions`
/// name the principals it binds may read — bare names include only what they name,
/// `~`-prefixed names exclude what they name, both may carry the `*` wildcard over a dotted
/// path — and a hidden field is answered exactly as one the document does not hold: absent
/// from a hit, absent from a realtime read, nowhere in the answer, and matching no query
/// clause.
#[tokio::test]
async fn field_level_security_rules_decide_which_fields_a_mapped_user_reads() {
    let emulator = Emulator::new();
    let host = given_domain(&emulator, "fgac-fls-rules").await;

    // The password every narrowed reader below authenticates with — within
    // `MasterUserPassword`'s modelled length (8..128), the bound the user API holds a
    // credential to.
    let password = "Reader-pass-1!";
    // (case, the role's `fls` rules, the leaf paths those rules leave VISIBLE)
    //
    // The rule forms are the security plugin's own `fls` syntax: an include list, an
    // exclude list written with `~`, dotted paths into a nested field, and the `*` wildcard
    // over either — one row per form, and one row for a role that narrows nothing.
    let rows: [(&str, &[&str], &[&str]); 8] = [
        (
            "a role with no fls at all",
            &[],
            &["title", "secret", "price", "audit.actor", "audit.note"],
        ),
        (
            "one excluded top-level field",
            &["~secret"],
            &["title", "price", "audit.actor", "audit.note"],
        ),
        (
            "two excluded fields, one of them nested",
            &["~secret", "~audit.actor"],
            &["title", "price", "audit.note"],
        ),
        (
            "an excluded family named by a wildcard",
            &["~audit.*"],
            &["title", "secret", "price"],
        ),
        ("one included top-level field", &["title"], &["title"]),
        (
            "an included top-level field beside an included nested one",
            &["title", "audit.note"],
            &["title", "audit.note"],
        ),
        (
            "an included family named by a wildcard",
            &["pri*", "audit.*"],
            &["price", "audit.actor", "audit.note"],
        ),
        (
            "an include narrowed by an exclude under it",
            &["audit.*", "~audit.actor"],
            &["audit.note"],
        ),
    ];

    for (row, (case, rules, visible)) in rows.into_iter().enumerate() {
        let user = format!("reader-{row}");
        let role = format!("role-{user}");
        given_narrowed_reader(&emulator, &host, &role, rules, &user, password).await;
        let credential = Some((user.as_str(), password));

        // What the narrowed reader is answered for a search over the whole index, and for
        // the realtime read of the same document: the same projection, because a field a
        // role hides is not part of the document that reader sees at all.
        let searched = rest(
            &emulator,
            &host,
            credential,
            "POST",
            &format!("/{INDEX}/_search"),
            Some(json!({ "size": 10 })),
        )
        .await;
        assert_eq!(
            (searched.status, hit_ids(&searched)),
            (200, vec![DOCUMENT.to_string()]),
            "case {case:?}: expected the one document to be hit, got {} with {:?}",
            searched.status,
            searched.body
        );
        let read = rest(
            &emulator,
            &host,
            credential,
            "GET",
            &format!("/{INDEX}/_doc/{DOCUMENT}"),
            None,
        )
        .await;
        assert_eq!(
            (read.status, read.body["found"].clone()),
            (200, json!(true)),
            "case {case:?}: expected the document to be found, got {} with {:?}",
            read.status,
            read.body
        );
        let answered = searched.body.to_string();
        for (path, held) in LEAVES {
            let expected = if visible.contains(&path) {
                json!(held)
            } else {
                Value::Null
            };
            for (reader, source) in [
                ("hit", searched.body["hits"]["hits"][0]["_source"].clone()),
                ("read", read.body["_source"].clone()),
            ] {
                let answered = source.pointer(&pointer(path)).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    answered, expected,
                    "case {case:?}: expected the {reader}'s {path} to read {expected:?}, got \
                     {answered:?}"
                );
            }
            // A hidden field's VALUE is nowhere in what the reader is answered, not only
            // absent from the projected source.
            if expected.is_null() {
                assert!(
                    !answered.contains(held),
                    "case {case:?}: expected nothing of the hidden {path} in the answer, got \
                     {answered:?}"
                );
            }
        }

        // A hidden field matches no clause either: the reader is asked about a document
        // that does not carry it.
        for (path, held) in LEAVES {
            let expected: Vec<String> = if visible.contains(&path) {
                vec![DOCUMENT.to_string()]
            } else {
                Vec::new()
            };
            let asked = rest(
                &emulator,
                &host,
                credential,
                "POST",
                &format!("/{INDEX}/_search"),
                Some(json!({ "query": { "term": { path: held } } })),
            )
            .await;
            assert_eq!(
                (asked.status, hit_ids(&asked)),
                (200, expected.clone()),
                "case {case:?}: expected a term query on {path} to hit {expected:?}, got {} \
                 with {:?}",
                asked.status,
                asked.body
            );
        }
    }

    // The master user, whom no role narrows, reads every field of the same document on the
    // same index — so what the readers above did not see is the rules' doing and not the
    // index's.
    let unnarrowed = as_master(
        &emulator,
        &host,
        "GET",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        None,
    )
    .await;
    for (path, held) in LEAVES {
        let answered = unnarrowed.body["_source"]
            .pointer(&pointer(path))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            answered,
            json!(held),
            "case \"the master reading {path}\": expected {held:?}, got {answered:?}"
        );
    }
}

/// RULE: the cluster-health route is decided like every other route of the plane — the
/// Basic credential of a user the internal database holds is authenticated and then judged
/// by what its roles permit at CLUSTER level, so the master user is answered the health
/// computed from its own domain's record while a reader a role binds to index permissions
/// alone is refused `403 security_exception` naming the cluster action, and a request
/// presenting no credential is refused `401` with the challenge that says how to present
/// one.
///
/// It is the route a client polls before it configures anything, which is why it is asserted
/// for each kind of caller rather than for the master alone.
#[tokio::test]
async fn the_cluster_health_route_is_decided_for_the_basic_credential_it_was_asked_with() {
    let emulator = Emulator::new();
    let domain = "fgac-fls-health";
    let host = given_domain(&emulator, domain).await;
    let user = "health-reader";
    let password = "Reader-pass-1!";
    given_narrowed_reader(
        &emulator,
        &host,
        "index_reader",
        &["~secret"],
        user,
        password,
    )
    .await;

    // The master's own credential: served, and what it is served is this domain's record —
    // the cluster a domain's health names is `<account>:<domain>`.
    let served = as_master(&emulator, &host, "GET", "/_cluster/health", None).await;
    let named = format!("{}:{domain}", ::engine::emulator_account_id());
    assert_eq!(
        (served.status, served.body["cluster_name"].clone()),
        (200, json!(named)),
        "case \"the master asking the cluster health\": expected 200 for its own domain, got \
         {} with {:?}",
        served.status,
        served.body
    );

    // A reader whose role names index permissions only holds no cluster action, so the same
    // route is refused for it — the index reads it does hold are served (asserted above), so
    // this is the role's own boundary and not the credential's.
    let refused = rest(
        &emulator,
        &host,
        Some((user, password)),
        "GET",
        "/_cluster/health",
        None,
    )
    .await;
    assert_eq!(
        (refused.status, error_type(&refused).as_str()),
        (403, "security_exception"),
        "case \"an index-only reader asking the cluster health\": expected a 403 \
         security_exception, got {} with {:?}",
        refused.status,
        refused.body
    );
    let stated = refused.body["error"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        stated.contains("no permissions for [cluster:monitor/health]"),
        "case \"an index-only reader asking the cluster health\": expected the refusal to \
         name the cluster action, got {stated:?}"
    );

    // And the same route presenting no credential at all: refused before any permission is
    // consulted, and told how to present one.
    let anonymous = rest(&emulator, &host, None, "GET", "/_cluster/health", None).await;
    assert_eq!(
        anonymous.status, 401,
        "case \"the cluster health asked anonymously\": expected 401, got {} with {:?}",
        anonymous.status, anonymous.body
    );
    let challenge = anonymous.headers.get("www-authenticate").cloned();
    assert!(
        challenge
            .as_deref()
            .is_some_and(|challenge| challenge.starts_with("Basic realm=")),
        "case \"the cluster health asked anonymously\": expected a Basic realm challenge, got \
         {challenge:?}"
    );
}

/// RULE: a principal nothing binds to any role may do nothing, so deleting the role mapping
/// that bound a narrowed reader leaves its reads refused `403 security_exception` naming the
/// action refused and the user it was refused for — and the refusal carries nothing of the
/// fields that role narrowed.
#[tokio::test]
async fn a_reader_whose_role_mapping_is_deleted_is_refused_and_told_nothing_of_the_document() {
    let emulator = Emulator::new();
    let host = given_domain(&emulator, "fgac-fls-unmapped").await;
    let user = "unmapped-reader";
    let password = "Reader-pass-1!";
    let role = "public_reader";
    given_narrowed_reader(
        &emulator,
        &host,
        role,
        &["~secret"],
        user,
        password,
    )
    .await;
    let credential = Some((user, password));
    let search = format!("/{INDEX}/_search");
    let read = format!("/{INDEX}/_doc/{DOCUMENT}");

    // While the mapping stands, the reader is served — narrowed by the role's own rules.
    let served = rest(
        &emulator,
        &host,
        credential,
        "POST",
        &search,
        Some(json!({ "size": 10 })),
    )
    .await;
    assert_eq!(
        (served.status, hit_ids(&served)),
        (200, vec![DOCUMENT.to_string()]),
        "case \"the mapped reader searching\": expected the one document, got {} with {:?}",
        served.status,
        served.body
    );

    let deleted = as_master(
        &emulator,
        &host,
        "DELETE",
        &format!("/_plugins/_security/api/rolesmapping/{role}"),
        None,
    )
    .await;
    assert_eq!(
        (deleted.status, deleted.body["message"].clone()),
        (200, json!(format!("'{role}' deleted."))),
        "case \"the master deleting the role mapping\": expected 200, got {} with {:?}",
        deleted.status,
        deleted.body
    );

    // (case, the method, the path, the body, the action the plugin names in its refusal)
    let rows: [(&str, &str, &str, Option<Value>, &str); 3] = [
        (
            "the unmapped reader searching",
            "POST",
            &search,
            Some(json!({ "size": 10 })),
            "indices:data/read/search",
        ),
        (
            "the unmapped reader asking a term query on the field the role had hidden",
            "POST",
            &search,
            Some(json!({ "query": { "term": { "secret": "classified" } } })),
            "indices:data/read/search",
        ),
        (
            "the unmapped reader reading the document",
            "GET",
            &read,
            None,
            "indices:data/read/get",
        ),
    ];
    for (case, method, path, body, action) in rows {
        let refused = rest(&emulator, &host, credential, method, path, body).await;
        assert_eq!(
            (refused.status, error_type(&refused).as_str()),
            (403, "security_exception"),
            "case {case:?}: expected a 403 security_exception, got {} with {:?}",
            refused.status,
            refused.body
        );
        let stated = refused.body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for part in [
            format!("no permissions for [{action}]"),
            format!("name={user}"),
        ] {
            assert!(
                stated.contains(&part),
                "case {case:?}: expected the refusal to carry {part:?}, got {stated:?}"
            );
        }
        // The refusal tells the caller nothing of the document it was refused.
        let answered = refused.body.to_string();
        for (path, held) in LEAVES {
            assert!(
                !answered.contains(held),
                "case {case:?}: expected nothing of {path} in the refusal, got {answered:?}"
            );
        }
    }
}
