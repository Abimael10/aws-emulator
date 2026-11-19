// SPDX-License-Identifier: CC0-1.0

//! Fine-grained access control through the INTERNAL USER DATABASE, driven over the real
//! wire path: an HTTP Basic-authenticated request to the endpoint host `DescribeDomain`
//! published, through the multi-service router, into the mounted service.
//!
//! Four rules, each over a SET of inputs rather than one recorded call:
//!
//! * AUTHENTICATION — on a domain with fine-grained access control switched on, the
//!   published endpoint admits a request only as a user the cluster can authenticate it
//!   as: a Basic credential matching a user of the internal database authenticates as that
//!   user, any other Basic credential is refused `401`, and a request presenting no
//!   credential at all is refused `401` with the `WWW-Authenticate` challenge that says
//!   how to present one;
//! * the internal user DATABASE — the users the security plugin's own API writes, reports
//!   and removes, with no password or hash ever reported back, reachable by the master
//!   user alone;
//! * the ACCOUNT route — what the plugin tells each authenticated caller it is;
//! * what a ROLE is worth — a principal its mappings bind may do only what one of its
//!   roles permits, on the indexes that role's patterns name, and sees only the documents
//!   and fields that role's `dls`/`fls` leave it — including through an AGGREGATION, which
//!   is a read of the field it names.
//!
//! The control-plane harness is this crate's own ([`super::domain_lifecycle`]) and the
//! endpoint resolution comes from [`super::domain_rest_data_plane`] rather than a second
//! copy of either.

use bytes::Bytes;
use serde_json::{json, Value};

use super::domain_lifecycle::{Answer, Emulator};
use super::domain_rest_data_plane::{endpoint_of, error_type};

/// The master user every domain below is created with, and its password — the credentials
/// `MasterUserOptions$MasterUserName`/`MasterUserPassword` name.
const MASTER: &str = "os-master";

/// Its password. Within `MasterUserPassword`'s modelled length (8..128).
const MASTER_PASSWORD: &str = "Master-pass-1!";

/// The second user the database comes to hold, which the cases below bind to roles.
const ANALYST: &str = "analyst";

/// Its password, within the same bounds.
const ANALYST_PASSWORD: &str = "Analyst-pass-1!";

/// The challenge header a cluster that admits nobody anonymously answers with.
const CHALLENGE: &str = "www-authenticate";

/// A domain with fine-grained access control switched on, its three prerequisites met, and
/// an INTERNAL master user — the configuration a client that wants its own user database
/// creates.
fn create_body(domain: &str) -> Value {
    json!({
        "DomainName": domain,
        "EncryptionAtRestOptions": { "Enabled": true },
        "NodeToNodeEncryptionOptions": { "Enabled": true },
        "DomainEndpointOptions": { "EnforceHTTPS": true },
        "AdvancedSecurityOptions": {
            "Enabled": true,
            "InternalUserDatabaseEnabled": true,
            "MasterUserOptions": {
                "MasterUserName": MASTER,
                "MasterUserPassword": MASTER_PASSWORD
            }
        }
    })
}

/// Create such a domain and answer the endpoint host it published.
async fn given_fgac_domain(emulator: &Emulator, domain: &str) -> String {
    let created = emulator.create(create_body(domain)).await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain with an internal master user\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    endpoint_of(emulator, domain)
}

/// One REST request to the published host, presenting `credential` as HTTP Basic
/// credentials — or presenting none at all where it is `None`.
///
/// `pub(crate)` because a sibling suite drives a Basic-authenticated request the same way
/// rather than growing a second copy of this.
pub(crate) async fn rest(
    emulator: &Emulator,
    host: &str,
    credential: Option<(&str, &str)>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json");
    if let Some((user, password)) = credential {
        let encoded = ::aws_smithy_types::base64::encode(format!("{user}:{password}"));
        request = request.header("authorization", format!("Basic {encoded}"));
    }
    let request = request
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    emulator.dispatch(request).await
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

/// The same request as the analyst, which every case's subject is.
async fn as_analyst(
    emulator: &Emulator,
    host: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    rest(
        emulator,
        host,
        Some((ANALYST, ANALYST_PASSWORD)),
        method,
        path,
        body,
    )
    .await
}

/// Add the analyst to the database, bound to `role` — the two writes a client makes to put
/// a user of its own behind a role.
async fn given_analyst(emulator: &Emulator, host: &str, role: &str) {
    let created = as_master(
        emulator,
        host,
        "PUT",
        &format!("/_plugins/_security/api/internalusers/{ANALYST}"),
        Some(json!({ "password": ANALYST_PASSWORD })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"the master adding an internal user\": expected 201, got {} with {:?}",
        created.status, created.body
    );
    let mapped = as_master(
        emulator,
        host,
        "PUT",
        &format!("/_plugins/_security/api/rolesmapping/{role}"),
        Some(json!({ "users": [ANALYST] })),
    )
    .await;
    assert_eq!(
        mapped.status, 201,
        "case \"the master binding that user to {role}\": expected 201, got {} with {:?}",
        mapped.status, mapped.body
    );
}

/// Write the role `name` with the one `index_permissions` entry `permission`.
async fn given_role(emulator: &Emulator, host: &str, name: &str, permission: Value) {
    let written = as_master(
        emulator,
        host,
        "PUT",
        &format!("/_plugins/_security/api/roles/{name}"),
        Some(json!({ "index_permissions": [permission] })),
    )
    .await;
    assert_eq!(
        written.status, 201,
        "case \"the master writing role {name}\": expected 201, got {} with {:?}",
        written.status, written.body
    );
}

/// Create the index `index` and write `documents` into it, as the master user.
async fn given_documents(emulator: &Emulator, host: &str, index: &str, documents: &[(&str, Value)]) {
    let created = as_master(emulator, host, "PUT", &format!("/{index}"), None).await;
    assert!(
        (200..300).contains(&created.status),
        "case \"creating index {index}\": expected a 2xx, got {} with {:?}",
        created.status,
        created.body
    );
    for (id, source) in documents {
        let written = as_master(
            emulator,
            host,
            "PUT",
            &format!("/{index}/_doc/{id}"),
            Some(source.clone()),
        )
        .await;
        assert!(
            (200..300).contains(&written.status),
            "case \"writing document {id} into {index}\": expected a 2xx, got {} with {:?}",
            written.status,
            written.body
        );
    }
}

/// The `_id`s a search answered, in the order it answered them.
fn hit_ids(answer: &Answer) -> Vec<String> {
    answer.body["hits"]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|hit| hit["_id"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: on a domain with fine-grained access control switched on, the published endpoint
/// admits a request only as a user the cluster authenticates it as — a Basic credential of
/// a user the internal database holds, at that user's own password. Every other credential
/// is refused `401`, and a request presenting none at all is refused `401` carrying the
/// Basic challenge.
#[tokio::test]
async fn the_endpoint_admits_only_a_credential_the_internal_database_authenticates() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-authn").await;

    /// One credential presented, and what the cluster makes of it.
    struct Row {
        case: &'static str,
        credential: Option<(&'static str, &'static str)>,
        status: u16,
        /// Whether the answer must tell the client HOW to present a credential.
        challenged: bool,
    }
    let rows = [
        Row {
            case: "the master user's own credential",
            credential: Some((MASTER, MASTER_PASSWORD)),
            status: 200,
            challenged: false,
        },
        Row {
            case: "the master user with a wrong password",
            credential: Some((MASTER, "Master-pass-2!")),
            status: 401,
            challenged: false,
        },
        Row {
            case: "the master user with an empty password",
            credential: Some((MASTER, "")),
            status: 401,
            challenged: false,
        },
        Row {
            case: "a user the database does not hold",
            credential: Some(("nobody", "Nobody-pass-1!")),
            status: 401,
            challenged: false,
        },
        Row {
            case: "a name that is the master's password and a password that is its name",
            credential: Some((MASTER_PASSWORD, MASTER)),
            status: 401,
            challenged: false,
        },
        Row {
            case: "no credential at all",
            credential: None,
            status: 401,
            challenged: true,
        },
    ];
    for Row {
        case,
        credential,
        status,
        challenged,
    } in rows
    {
        let answered = rest(&emulator, &host, credential, "GET", "/", None).await;
        assert_eq!(
            answered.status, status,
            "case {case:?}: expected HTTP {status}, got {} with {:?}",
            answered.status, answered.body
        );
        let challenge = answered.headers.get(CHALLENGE).cloned();
        assert_eq!(
            challenge.is_some(),
            challenged,
            "case {case:?}: expected the Basic challenge to be {}, got {challenge:?}",
            if challenged { "present" } else { "absent" }
        );
        if let Some(challenge) = &challenge {
            assert!(
                challenge.starts_with("Basic realm="),
                "case {case:?}: expected a Basic realm challenge, got {challenge:?}"
            );
        }
        // No refusal of an unauthenticated request says anything about the cluster.
        if status == 401 {
            let said = answered.body.to_string();
            assert!(
                !said.contains("cluster_name") && !said.contains("fgac-authn"),
                "case {case:?}: expected the refusal to name nothing of the cluster, got \
                 {said:?}"
            );
        }
    }

    // The master's own 200 is the cluster banner, computed from the domain record.
    let banner = as_master(&emulator, &host, "GET", "/", None).await;
    assert_eq!(
        banner.body["version"]["distribution"].as_str(),
        Some("opensearch"),
        "case \"the banner the master is served\": expected the engine's distribution, got \
         {:?}",
        banner.body
    );

    // A domain the boundary is NOT switched on for runs no security plugin, so it
    // authenticates nobody and refuses nobody for presenting no credential.
    super::data_source_registry::created_domain(&emulator, "open-authn").await;
    let open_host = endpoint_of(&emulator, "open-authn");
    let served = rest(&emulator, &open_host, None, "GET", "/", None).await;
    assert_eq!(
        served.status, 200,
        "case \"a domain without fine-grained access control, asked anonymously\": expected \
         200, got {} with {:?}",
        served.status, served.body
    );
}

/// RULE: the credential verification the emulator performs for the requests it names
/// operations for does not stand between a Basic credential and the cluster — a
/// Basic-authenticated data-plane request is served the same on a router that verifies
/// what it is handed as on one that does not, because the cluster, not the front door, is
/// what authenticates it.
#[tokio::test]
async fn a_basic_credential_is_served_where_the_emulator_verifies_credentials() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-verified").await;
    // A second router over the SAME shared engine, with the verification the deployed
    // emulator performs switched ON: the domain the control plane above created is the one
    // this router's services read.
    let mut verifying = smithy_server_runtime::MultiServiceRouter::new();
    verifying.set_request_authentication(true);
    verifying.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(
            std::sync::Arc::clone(emulator.engine()),
        ),
    );

    let encoded = ::aws_smithy_types::base64::encode(format!("{MASTER}:{MASTER_PASSWORD}"));
    let request = http::Request::builder()
        .method("GET")
        .uri("/")
        .header("host", &host)
        .header("authorization", format!("Basic {encoded}"))
        .body(Bytes::new())
        .expect("the test request is well formed");
    let answered = verifying.dispatch(request).await;
    assert_eq!(
        answered.status().as_u16(),
        200,
        "case \"the master's Basic credential on a verifying router\": expected 200, got {} \
         with {:?}",
        answered.status(),
        String::from_utf8_lossy(answered.body())
    );
}

/// RULE: the internal user database is the master user's alone to read and write, it
/// reports back everything a write named BUT the credential, and the master user's own
/// record is read-only there.
#[tokio::test]
async fn the_internal_user_database_round_trips_a_user_without_its_credential() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-users").await;
    let path = format!("/_plugins/_security/api/internalusers/{ANALYST}");

    // A user the database does not hold yet needs a credential it can be authenticated
    // against: one naming neither is refused, and so is one naming a pre-hashed credential
    // this cluster cannot compare a presented password against — a record stored for
    // either would be a user nothing could ever authenticate as.
    for (case, body) in [
        (
            "adding a user with neither password nor hash",
            json!({ "backend_roles": ["readers"] }),
        ),
        (
            "adding a user with a pre-hashed credential",
            json!({ "hash": "$2y$12$00000000000000000000000000000000000000000000000000000" }),
        ),
    ] {
        let refused = as_master(&emulator, &host, "PUT", &path, Some(body)).await;
        assert_eq!(
            (refused.status, refused.body["status"].as_str()),
            (400, Some("error")),
            "case {case:?}: expected the write to be refused, got {} with {:?}",
            refused.status,
            refused.body
        );
        let absent = as_master(&emulator, &host, "GET", &path, None).await;
        assert_eq!(
            absent.status, 404,
            "case {case:?}: expected the refused user to be held by nothing, got {} with {:?}",
            absent.status, absent.body
        );
    }

    let created = as_master(
        &emulator,
        &host,
        "PUT",
        &path,
        Some(json!({
            "password": ANALYST_PASSWORD,
            "backend_roles": ["readers"],
            "opendistro_security_roles": ["ledger_reader"],
            "attributes": { "team": "finance" }
        })),
    )
    .await;
    assert_eq!(
        (created.status, created.body["status"].as_str()),
        (201, Some("CREATED")),
        "case \"adding a user\": expected a created answer, got {} with {:?}",
        created.status,
        created.body
    );

    let read = as_master(&emulator, &host, "GET", &path, None).await;
    assert_eq!(
        read.status, 200,
        "case \"reading the user back\": expected 200, got {} with {:?}",
        read.status, read.body
    );
    for (member, expected) in [
        ("backend_roles", json!(["readers"])),
        ("opendistro_security_roles", json!(["ledger_reader"])),
        ("attributes", json!({ "team": "finance" })),
    ] {
        assert_eq!(
            read.body[ANALYST][member], expected,
            "case \"the reported {member}\": expected {expected:?}, got {:?}",
            read.body
        );
    }
    let reported = read.body.to_string();
    assert!(
        !reported.contains(ANALYST_PASSWORD) && !reported.contains("hash"),
        "case \"the reported user\": expected no credential of any form in it, got \
         {reported:?}"
    );

    // The listing holds the master user too: the master IS a user of the database on a
    // domain that keeps one.
    let listed = as_master(
        &emulator,
        &host,
        "GET",
        "/_plugins/_security/api/internalusers",
        None,
    )
    .await;
    for name in [MASTER, ANALYST] {
        assert!(
            listed.body.get(name).is_some(),
            "case \"the listing holding {name}\": expected it to be reported, got {:?}",
            listed.body
        );
    }

    // A second write that names no credential leaves the one the user has, so the user can
    // still be authenticated — which it proves by being refused as UNMAPPED rather than as
    // unauthenticated.
    let updated = as_master(
        &emulator,
        &host,
        "PUT",
        &path,
        Some(json!({ "backend_roles": ["readers", "auditors"] })),
    )
    .await;
    assert_eq!(
        (updated.status, updated.body["status"].as_str()),
        (200, Some("OK")),
        "case \"updating the user\": expected an updated answer, got {} with {:?}",
        updated.status,
        updated.body
    );
    let still_authenticated = as_analyst(&emulator, &host, "GET", "/", None).await;
    assert_eq!(
        (still_authenticated.status, error_type(&still_authenticated)),
        (403, "security_exception".to_string()),
        "case \"the updated user asking for the banner\": expected the cluster's refusal of \
         an unmapped user, got {} with {:?}",
        still_authenticated.status,
        still_authenticated.body
    );

    // The database is the master's alone.
    let probed = as_analyst(&emulator, &host, "GET", &path, None).await;
    assert_eq!(
        (probed.status, error_type(&probed)),
        (403, "security_exception".to_string()),
        "case \"a non-master reading the database\": expected a security refusal, got {} \
         with {:?}",
        probed.status,
        probed.body
    );

    // The master user's own record is the domain's configuration, not this API's.
    for method in ["PUT", "DELETE"] {
        let refused = as_master(
            &emulator,
            &host,
            method,
            &format!("/_plugins/_security/api/internalusers/{MASTER}"),
            Some(json!({ "password": "Another-pass-1!" })),
        )
        .await;
        assert_eq!(
            (refused.status, refused.body["status"].as_str()),
            (403, Some("FORBIDDEN")),
            "case \"{method} on the master user's record\": expected it to be read-only, got \
             {} with {:?}",
            refused.status,
            refused.body
        );
    }

    // Removed, and then held by nothing.
    let removed = as_master(&emulator, &host, "DELETE", &path, None).await;
    assert_eq!(
        (removed.status, removed.body["status"].as_str()),
        (200, Some("OK")),
        "case \"removing the user\": expected an OK answer, got {} with {:?}",
        removed.status,
        removed.body
    );
    let gone = as_master(&emulator, &host, "GET", &path, None).await;
    assert_eq!(
        (gone.status, gone.body["status"].as_str()),
        (404, Some("NOT_FOUND")),
        "case \"reading the removed user\": expected a not-found answer, got {} with {:?}",
        gone.status,
        gone.body
    );
}

/// RULE: the plugin's account route tells each AUTHENTICATED caller who it is — its own
/// name, whether the internal database holds it, and the roles this domain's configuration
/// binds it to — whatever permissions that caller holds.
#[tokio::test]
async fn the_account_route_reports_the_authenticated_user_and_its_roles() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-account").await;
    given_role(
        &emulator,
        &host,
        "ledger_reader",
        json!({ "index_patterns": ["ledger-*"], "allowed_actions": ["read"] }),
    )
    .await;
    given_analyst(&emulator, &host, "ledger_reader").await;

    /// Who asked, and what the plugin tells it.
    struct Row {
        case: &'static str,
        credential: (&'static str, &'static str),
        roles: Value,
        reserved: bool,
    }
    let rows = [
        Row {
            case: "the master user",
            credential: (MASTER, MASTER_PASSWORD),
            roles: json!(["all_access"]),
            reserved: true,
        },
        Row {
            case: "a user a role mapping binds",
            credential: (ANALYST, ANALYST_PASSWORD),
            roles: json!(["ledger_reader"]),
            reserved: false,
        },
    ];
    for Row {
        case,
        credential,
        roles,
        reserved,
    } in rows
    {
        let answered = rest(
            &emulator,
            &host,
            Some(credential),
            "GET",
            "/_plugins/_security/api/account",
            None,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the plugin to answer, got {} with {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.body["user_name"].as_str(),
            Some(credential.0),
            "case {case:?}: expected the user {:?}, got {:?}",
            credential.0,
            answered.body
        );
        assert_eq!(
            answered.body["is_internal_user"],
            json!(true),
            "case {case:?}: expected the database to be reported as holding it, got {:?}",
            answered.body
        );
        assert_eq!(
            answered.body["is_reserved"],
            json!(reserved),
            "case {case:?}: expected is_reserved {reserved}, got {:?}",
            answered.body
        );
        assert_eq!(
            answered.body["roles"], roles,
            "case {case:?}: expected the roles {roles:?}, got {:?}",
            answered.body
        );
    }
}

/// RULE: a principal a role mapping binds may do only what one of its roles permits, on
/// the indexes that role's own `index_patterns` name; everything else is refused `403` with
/// a `security_exception` naming the action it refused.
#[tokio::test]
async fn a_role_permits_only_the_actions_and_the_indexes_its_document_names() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-roles").await;
    given_role(
        &emulator,
        &host,
        "ledger_reader",
        json!({ "index_patterns": ["ledger-*"], "allowed_actions": ["read"] }),
    )
    .await;
    given_analyst(&emulator, &host, "ledger_reader").await;
    for index in ["ledger-2024", "audit-2024"] {
        given_documents(&emulator, &host, index, &[("one", json!({ "title": "a" }))]).await;
    }

    /// One request the analyst makes, and whether its role is worth it.
    struct Row {
        case: &'static str,
        method: &'static str,
        path: &'static str,
        body: Option<Value>,
        /// The action the cluster names where it refuses, and `None` where it serves.
        refused: Option<&'static str>,
    }
    let rows = [
        Row {
            case: "a search of an index the role's pattern names",
            method: "POST",
            path: "/ledger-2024/_search",
            body: Some(json!({})),
            refused: None,
        },
        Row {
            case: "a read of one document of that index",
            method: "GET",
            path: "/ledger-2024/_doc/one",
            body: None,
            refused: None,
        },
        Row {
            case: "a count of that index",
            method: "GET",
            path: "/ledger-2024/_count",
            body: None,
            refused: None,
        },
        Row {
            case: "a search of an index outside the role's pattern",
            method: "POST",
            path: "/audit-2024/_search",
            body: Some(json!({})),
            refused: Some("indices:data/read/search"),
        },
        Row {
            case: "a read of one document outside it",
            method: "GET",
            path: "/audit-2024/_doc/one",
            body: None,
            refused: Some("indices:data/read/get"),
        },
        Row {
            case: "a write to the index the role may READ",
            method: "PUT",
            path: "/ledger-2024/_doc/two",
            body: Some(json!({ "title": "b" })),
            refused: Some("indices:data/write/index"),
        },
        Row {
            case: "an index create the role does not permit",
            method: "PUT",
            path: "/ledger-2025",
            body: None,
            refused: Some("indices:admin/create"),
        },
    ];
    for Row {
        case,
        method,
        path,
        body,
        refused,
    } in rows
    {
        let answered = as_analyst(&emulator, &host, method, path, body).await;
        match refused {
            None => assert!(
                (200..300).contains(&answered.status),
                "case {case:?}: expected the role to be worth it, got {} with {:?}",
                answered.status,
                answered.body
            ),
            Some(action) => {
                assert_eq!(
                    (answered.status, error_type(&answered)),
                    (403, "security_exception".to_string()),
                    "case {case:?}: expected the cluster's refusal, got {} with {:?}",
                    answered.status,
                    answered.body
                );
                let reason = answered.body["error"]["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    reason.contains(&format!("no permissions for [{action}]")),
                    "case {case:?}: expected the refusal to name the action {action:?}, got \
                     {reason:?}"
                );
            }
        }
    }

    // The master user is narrowed by none of it.
    let master_read = as_master(&emulator, &host, "POST", "/audit-2024/_search", Some(json!({})))
        .await;
    assert_eq!(
        master_read.status, 200,
        "case \"the master searching the same index\": expected 200, got {} with {:?}",
        master_read.status, master_read.body
    );
}

/// RULE: a role the security configuration holds no document for is one of the plugin's
/// PREDEFINED roles, judged by the document it is — so a mapping to the read-only predefined
/// role permits reads of every index and no write, without any role having been written —
/// while a mapping to a role that is NEITHER names permissions this cluster cannot read, so
/// it is narrowed no further.
///
/// `readall` is an entry of that catalogue the issue this suite was written for named
/// nothing about, which is why it is the one asserted here.
#[tokio::test]
async fn a_predefined_read_only_role_needs_no_role_document() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-predefined").await;
    given_analyst(&emulator, &host, "readall").await;
    given_documents(
        &emulator,
        &host,
        "ledger-2024",
        &[("one", json!({ "title": "a" }))],
    )
    .await;

    // What the predefined role is worth, and what it is not.
    struct Row {
        case: &'static str,
        method: &'static str,
        path: &'static str,
        body: Option<Value>,
        /// The action the cluster names where it refuses, and `None` where it serves.
        refused: Option<&'static str>,
    }
    let rows = [
        Row {
            case: "a search of an index no pattern of the mapping named",
            method: "POST",
            path: "/ledger-2024/_search",
            body: Some(json!({})),
            refused: None,
        },
        Row {
            case: "a read of one of its documents",
            method: "GET",
            path: "/ledger-2024/_doc/one",
            body: None,
            refused: None,
        },
        Row {
            case: "a write the read-only role does not permit",
            method: "PUT",
            path: "/ledger-2024/_doc/two",
            body: Some(json!({ "title": "b" })),
            refused: Some("indices:data/write/index"),
        },
    ];
    for Row {
        case,
        method,
        path,
        body,
        refused,
    } in rows
    {
        let answered = as_analyst(&emulator, &host, method, path, body).await;
        match refused {
            None => assert!(
                (200..300).contains(&answered.status),
                "case {case:?}: expected the predefined role to be worth it, got {} with {:?}",
                answered.status,
                answered.body
            ),
            Some(action) => {
                let reason = answered.body["error"]["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert_eq!(
                    (answered.status, error_type(&answered)),
                    (403, "security_exception".to_string()),
                    "case {case:?}: expected the cluster's refusal, got {} with {:?}",
                    answered.status,
                    answered.body
                );
                assert!(
                    reason.contains(&format!("no permissions for [{action}]")),
                    "case {case:?}: expected the refusal to name {action:?}, got {reason:?}"
                );
            }
        }
    }

    // A mapping to a role this configuration holds no document for, and that is none of the
    // predefined ones, names permissions this cluster cannot read: the principal is bound,
    // and nothing here narrows what it may do.
    let mapped = as_master(
        &emulator,
        &host,
        "PUT",
        "/_plugins/_security/api/rolesmapping/written_elsewhere",
        Some(json!({ "users": [ANALYST] })),
    )
    .await;
    assert_eq!(
        mapped.status, 201,
        "case \"binding the user to a role the configuration holds no document for\": \
         expected 201, got {} with {:?}",
        mapped.status, mapped.body
    );
    let written = as_analyst(
        &emulator,
        &host,
        "PUT",
        "/ledger-2024/_doc/three",
        Some(json!({ "title": "c" })),
    )
    .await;
    assert!(
        (200..300).contains(&written.status),
        "case \"a role whose permissions cannot be read\": expected the request to be \
         narrowed no further, got {} with {:?}",
        written.status,
        written.body
    );
}

/// RULE: a role's `dls` narrows WHICH documents of the indexes it names the principals it
/// binds may read — a document it does not select is reported by no search and read back by
/// no `GET`, exactly as one the index does not hold — while a principal the role does not
/// narrow reads every one of them.
#[tokio::test]
async fn document_level_security_hides_a_document_from_a_search_and_from_a_read() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-dls").await;
    given_role(
        &emulator,
        &host,
        "blue_reader",
        json!({
            "index_patterns": ["ledger-*"],
            "allowed_actions": ["read"],
            "dls": "{\"term\": {\"tenant\": \"blue\"}}"
        }),
    )
    .await;
    given_analyst(&emulator, &host, "blue_reader").await;
    given_documents(
        &emulator,
        &host,
        "ledger-2024",
        &[
            ("blue-1", json!({ "tenant": "blue", "amount": 10 })),
            ("red-1", json!({ "tenant": "red", "amount": 20 })),
            ("blue-2", json!({ "tenant": "blue", "amount": 30 })),
        ],
    )
    .await;

    // Who searched, and which documents the cluster reports to it.
    let rows: [(&str, (&str, &str), Vec<&str>); 2] = [
        (
            "the narrowed user",
            (ANALYST, ANALYST_PASSWORD),
            vec!["blue-1", "blue-2"],
        ),
        (
            "the master user",
            (MASTER, MASTER_PASSWORD),
            vec!["blue-1", "blue-2", "red-1"],
        ),
    ];
    for (case, credential, expected) in rows {
        let answered = rest(
            &emulator,
            &host,
            Some(credential),
            "POST",
            "/ledger-2024/_search",
            Some(json!({ "size": 10 })),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the search to serve, got {} with {:?}",
            answered.status, answered.body
        );
        let mut ids = hit_ids(&answered);
        ids.sort();
        assert_eq!(
            ids, expected,
            "case {case:?}: expected the hits {expected:?}, got {ids:?}"
        );
        assert_eq!(
            answered.body["hits"]["total"]["value"],
            json!(expected.len()),
            "case {case:?}: expected a total of {}, got {:?}",
            expected.len(),
            answered.body
        );
    }

    // A realtime read is narrowed the same way: the hidden document is reported exactly as
    // one the index does not hold, and the admitted one is served.
    let rows: [(&str, &str, u16, bool); 3] = [
        ("a document the role admits", "blue-1", 200, true),
        ("a document the role hides", "red-1", 404, false),
        ("a document the index does not hold", "green-1", 404, false),
    ];
    for (case, id, status, found) in rows {
        let answered = as_analyst(
            &emulator,
            &host,
            "GET",
            &format!("/ledger-2024/_doc/{id}"),
            None,
        )
        .await;
        assert_eq!(
            (answered.status, answered.body["found"].clone()),
            (status, json!(found)),
            "case {case:?}: expected HTTP {status} and found {found}, got {} with {:?}",
            answered.status,
            answered.body
        );
    }
    let seen = as_master(&emulator, &host, "GET", "/ledger-2024/_doc/red-1", None).await;
    assert_eq!(
        (seen.status, seen.body["found"].clone()),
        (200, json!(true)),
        "case \"the master reading the hidden document\": expected it to be served, got {} \
         with {:?}",
        seen.status,
        seen.body
    );
}

/// RULE: a role's `fls` narrows WHICH fields of the indexes it names the principals it
/// binds may read — a field it hides is in no hit's `_source` and in no read's, and matches
/// no clause of a query either, because it is not part of the document that principal sees.
#[tokio::test]
async fn field_level_security_hides_a_field_from_a_hit_and_from_a_query() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-fls").await;
    given_role(
        &emulator,
        &host,
        "public_reader",
        json!({
            "index_patterns": ["catalog"],
            "allowed_actions": ["read"],
            "fls": ["~secret", "~audit.actor"]
        }),
    )
    .await;
    given_analyst(&emulator, &host, "public_reader").await;
    given_documents(
        &emulator,
        &host,
        "catalog",
        &[(
            "one",
            json!({
                "title": "a widget",
                "secret": "classified",
                "audit": { "actor": "root", "at": "2024-01-01" }
            }),
        )],
    )
    .await;

    // The fields the narrowed user sees of the hit, and the ones it does not.
    let hit = as_analyst(
        &emulator,
        &host,
        "POST",
        "/catalog/_search",
        Some(json!({ "size": 10 })),
    )
    .await;
    assert_eq!(
        hit.status, 200,
        "case \"the narrowed user searching\": expected 200, got {} with {:?}",
        hit.status, hit.body
    );
    let source = hit.body["hits"]["hits"][0]["_source"].clone();
    for (case, pointer, expected) in [
        ("a field no rule hides", "/title", json!("a widget")),
        ("a field a rule hides", "/secret", Value::Null),
        ("a nested field a rule hides", "/audit/actor", Value::Null),
        (
            "a nested field beside it",
            "/audit/at",
            json!("2024-01-01"),
        ),
    ] {
        let held = source.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            held, expected,
            "case {case:?}: expected {pointer} to read {expected:?}, got {held:?}"
        );
    }

    // A hidden field matches no clause either: the document the narrowed user is asked
    // about does not carry it at all.
    let rows: [(&str, Value, usize, usize); 3] = [
        (
            "a query on a field no rule hides",
            json!({ "match": { "title": "widget" } }),
            1,
            1,
        ),
        (
            "a query on a field a rule hides",
            json!({ "term": { "secret": "classified" } }),
            0,
            1,
        ),
        (
            "a query on a nested field a rule hides",
            json!({ "term": { "audit.actor": "root" } }),
            0,
            1,
        ),
    ];
    for (case, query, narrowed, master) in rows {
        let asked = as_analyst(
            &emulator,
            &host,
            "POST",
            "/catalog/_search",
            Some(json!({ "query": query.clone() })),
        )
        .await;
        assert_eq!(
            (asked.status, hit_ids(&asked).len()),
            (200, narrowed),
            "case {case:?}: expected {narrowed} hit(s) for the narrowed user, got {} with \
             {:?}",
            asked.status,
            asked.body
        );
        let unnarrowed = as_master(
            &emulator,
            &host,
            "POST",
            "/catalog/_search",
            Some(json!({ "query": query })),
        )
        .await;
        assert_eq!(
            (unnarrowed.status, hit_ids(&unnarrowed).len()),
            (200, master),
            "case {case:?}: expected {master} hit(s) for the master, got {} with {:?}",
            unnarrowed.status,
            unnarrowed.body
        );
    }

    // A realtime read is narrowed the same way the hit is.
    let read = as_analyst(&emulator, &host, "GET", "/catalog/_doc/one", None).await;
    assert_eq!(
        (read.status, read.body["_source"]["secret"].clone()),
        (200, Value::Null),
        "case \"the narrowed user reading the document\": expected the hidden field to be \
         absent, got {} with {:?}",
        read.status,
        read.body
    );
    let unread = as_master(&emulator, &host, "GET", "/catalog/_doc/one", None).await;
    assert_eq!(
        unread.body["_source"]["secret"].as_str(),
        Some("classified"),
        "case \"the master reading the document\": expected the field to be served, got {:?}",
        unread.body
    );
}

/// RULE: an aggregation is a READ of the field it names, so it reads exactly the documents
/// and fields the caller's own roles leave it: a field a role's `fls` hides contributes to no
/// bucket and to no metric for the principals that role binds, and a document its `dls` does
/// not admit is counted in none of them — while the master user, whom nothing narrows, is
/// answered the whole summary.
#[tokio::test]
async fn an_aggregation_reads_only_the_documents_and_fields_the_caller_may_see() {
    let emulator = Emulator::new();
    let host = given_fgac_domain(&emulator, "fgac-aggs").await;
    given_role(
        &emulator,
        &host,
        "payroll_reader",
        json!({
            "index_patterns": ["payroll"],
            "allowed_actions": ["read"],
            // The salary is hidden from this role, and only the blue team's records are
            // admitted to it: one rule per narrowing, so the two effects are told apart.
            "fls": ["~salary"],
            "dls": "{\"term\": {\"team\": \"blue\"}}"
        }),
    )
    .await;
    given_analyst(&emulator, &host, "payroll_reader").await;
    given_documents(
        &emulator,
        &host,
        "payroll",
        &[
            ("one", json!({ "team": "blue", "salary": 100 })),
            ("two", json!({ "team": "blue", "salary": 100 })),
            ("three", json!({ "team": "red", "salary": 200 })),
            ("four", json!({ "team": "red", "salary": 300 })),
        ],
    )
    .await;

    // (case, the aggregations asked, what the narrowed user is answered, what the master is)
    let rows: [(&str, Value, Value, Value); 3] = [
        (
            "a terms aggregation over a field the role hides",
            json!({ "by_salary": { "terms": { "field": "salary" } } }),
            json!({ "by_salary": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [],
            } }),
            json!({ "by_salary": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [
                    { "key": 100, "doc_count": 2 },
                    { "key": 200, "doc_count": 1 },
                    { "key": 300, "doc_count": 1 },
                ],
            } }),
        ),
        (
            "the metrics over that same hidden field",
            json!({
                "paid": { "value_count": { "field": "salary" } },
                "lowest": { "min": { "field": "salary" } },
                "highest": { "max": { "field": "salary" } },
                "total": { "sum": { "field": "salary" } },
                "mean": { "avg": { "field": "salary" } },
            }),
            json!({
                "paid": { "value": 0 },
                "lowest": { "value": Value::Null },
                "highest": { "value": Value::Null },
                "total": { "value": 0.0 },
                "mean": { "value": Value::Null },
            }),
            json!({
                "paid": { "value": 4 },
                "lowest": { "value": 100.0 },
                "highest": { "value": 300.0 },
                "total": { "value": 700.0 },
                "mean": { "value": 175.0 },
            }),
        ),
        (
            "a terms aggregation over a field the role leaves, bucketing the documents it \
             admits",
            json!({ "by_team": { "terms": { "field": "team" } } }),
            json!({ "by_team": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [{ "key": "blue", "doc_count": 2 }],
            } }),
            json!({ "by_team": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [
                    { "key": "blue", "doc_count": 2 },
                    { "key": "red", "doc_count": 2 },
                ],
            } }),
        ),
    ];

    for (case, aggs, narrowed, unnarrowed) in rows {
        let body = json!({ "size": 0, "aggs": aggs });
        let asked = as_analyst(
            &emulator,
            &host,
            "POST",
            "/payroll/_search",
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            asked.status, 200,
            "case {case:?}: expected the narrowed user to be served, got {} with {:?}",
            asked.status, asked.body
        );
        assert_eq!(
            asked.body["aggregations"], narrowed,
            "case {case:?}: expected the narrowed user {narrowed:?}, got {:?}",
            asked.body["aggregations"]
        );
        let master = as_master(&emulator, &host, "POST", "/payroll/_search", Some(body)).await;
        assert_eq!(
            master.body["aggregations"], unnarrowed,
            "case {case:?}: expected the master {unnarrowed:?}, got {:?}",
            master.body["aggregations"]
        );
    }
}
