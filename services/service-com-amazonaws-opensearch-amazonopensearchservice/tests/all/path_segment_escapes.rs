// SPDX-License-Identifier: CC0-1.0

//! What a PERCENT-ESCAPE in the request target of a domain's REST data plane stands for,
//! driven over the real wire path: a signed request to a domain's endpoint, through the
//! multi-service router, into the mounted service. What is asserted is what a client sees.
//!
//! The rule, over a SET of inputs rather than one recorded call:
//!
//! > a URI's path is a sequence of percent-encoded segments, so the value a segment carries
//! > is what its escapes STAND FOR and never the escape text — the document id a write,
//! > read, create, update or delete addresses, and the index name beside it. A client whose
//! > id carries a character that would otherwise divide the path (the `/` of a hierarchical
//! > object key) has to escape it, and the cluster stores, reports and addresses the decoded
//! > id.
//!
//! The rows span what the rule generalises over: which character is escaped (a path
//! separator, a space, a plus, a percent, a query and a fragment delimiter, a non-ASCII
//! character, an unreserved one), how many escapes one segment carries, the case of the hex
//! digits, and a segment carrying none at all. Each row writes into an index of its own, so
//! no row can be read as another's, and each is checked against the LITERAL escape text as
//! well: a cluster that stored the escapes would answer the double-escaped address instead.
//!
//! Asserted beside it, because it is the same reading of the same path: the RESOURCE the
//! authorization decision is taken against is built from those same values, so a policy that
//! names a document path refuses the request that addresses it however the client had to
//! escape it.
//!
//! The harness is [`super::domain_lifecycle`]'s and the data-plane request helpers are
//! [`super::domain_rest_data_plane`]'s; the session and policy seeding is
//! [`super::published_domain_address`]'s. This suite adds no second copy of either.

/// The same rule read over the segment's ROLE rather than over the escaped character: every
/// other name this surface takes in a path — an alias, a template, an ingest pipeline — and
/// the naming rules applied to what a segment spells.
///
/// The `#[path]` is what points this child at its own directory: this suite is itself reached
/// through a `#[path]` from `tests/all.rs`, so a bare `mod` would resolve to a SIBLING suite.
#[path = "path_segment_escapes/escaped_name_segments.rs"]
mod escaped_name_segments;

use serde_json::{json, Value};

use super::data_source_registry::created_domain;
use super::domain_lifecycle::{Answer, Emulator, DEFAULT_ACCESS_KEY_ID, REGION};
use super::domain_rest_data_plane::{endpoint_of, error_type, rest, rest_prefixed};
use super::published_domain_address::{given_role_policy, given_session, role_arn};

/// The domain every request below is addressed to.
const DOMAIN: &str = "escape-plane";

/// One row of the escape table: the id as the client has to SEND it, and the id the cluster
/// therefore holds.
struct Escaped {
    case: &'static str,
    /// The id as it travels in the path.
    sent: &'static str,
    /// What that segment's escapes stand for, which is the id the cluster holds.
    held: &'static str,
}

/// The ids the rule is read over.
///
/// The escapes are the ones a client cannot avoid (`/`, `?`, `#`, a space and `%` itself
/// cannot stand in a path segment as themselves) beside ones that are equivalent to the
/// character they encode (an escaped `+`, an escaped unreserved `-`), a non-ASCII character,
/// and a segment that carries no escape at all.
fn rows() -> Vec<Escaped> {
    vec![
        Escaped {
            case: "the separator of a hierarchical object key",
            sent: "docs%2Fsummer-2026.json",
            held: "docs/summer-2026.json",
        },
        Escaped {
            case: "the same octet in lower-case hex",
            sent: "docs%2fwinter-2026.json",
            held: "docs/winter-2026.json",
        },
        Escaped {
            case: "several separators in one segment",
            sent: "a%2Fb%2Fc%2Fd.json",
            held: "a/b/c/d.json",
        },
        Escaped {
            case: "a space",
            sent: "quarterly%20report",
            held: "quarterly report",
        },
        Escaped {
            case: "a plus, which a path segment carries as itself",
            sent: "spring+summer",
            held: "spring+summer",
        },
        Escaped {
            case: "an escaped plus, which stands for the same character",
            sent: "autumn%2Bwinter",
            held: "autumn+winter",
        },
        Escaped {
            case: "the escape character itself",
            sent: "100%25-indexed",
            held: "100%-indexed",
        },
        Escaped {
            case: "the character that would otherwise begin the query string",
            sent: "what%3Fnow",
            held: "what?now",
        },
        Escaped {
            case: "the character that would otherwise begin the fragment",
            sent: "tag%23one",
            held: "tag#one",
        },
        Escaped {
            case: "a non-ASCII character, as two escaped UTF-8 bytes",
            sent: "caf%C3%A9-notes",
            held: "café-notes",
        },
        Escaped {
            case: "an escaped unreserved character, which stands for itself",
            sent: "plain%2Did",
            held: "plain-id",
        },
        Escaped {
            case: "a segment carrying no escape at all",
            sent: "plain-id",
            held: "plain-id",
        },
    ]
}

/// The index row `position` writes into: one index per row, so no row's document can be read
/// as another's.
fn index_of(position: usize) -> String {
    format!("escapes-{position}")
}

/// The source every row stores, which names the id it is held under so a `_source` read
/// cannot be confused with another row's.
fn source_of(row: &Escaped) -> Value {
    json!({ "key": row.held, "note": "indexed by its object key" })
}

/// The same target with every escape ESCAPED AGAIN, which addresses the literal escape text
/// — the id a cluster that did not decode the segment would have stored.
fn double_escaped(sent: &str) -> String {
    sent.replace('%', "%25")
}

/// One request on the domain's data plane, under the prefixed path addressing.
async fn plane(emulator: &Emulator, method: &str, path: &str, body: Option<Value>) -> Answer {
    rest_prefixed(emulator, DOMAIN, method, path, body).await
}

/// The ids the hits of a search carry.
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

/// RULE: the document id a write addresses is the DECODED value of its path segment, and
/// that is the id the write answers with, the read reports, the search hit carries and the
/// literal escape text does not address.
#[tokio::test]
async fn an_escaped_id_segment_is_stored_and_reported_as_the_character_it_stands_for() {
    let emulator = Emulator::new();
    created_domain(&emulator, DOMAIN).await;

    for (position, row) in rows().iter().enumerate() {
        let index = index_of(position);
        let case = row.case;
        let source = source_of(row);

        // The write: the id it reports is the decoded one, in the index the first segment
        // names.
        let written = plane(
            &emulator,
            "PUT",
            &format!("/{index}/_doc/{}?refresh=true", row.sent),
            Some(source.clone()),
        )
        .await;
        assert_eq!(
            (written.status, written.body["_id"].as_str()),
            (201, Some(row.held)),
            "case {case:?}: expected 201 with _id {:?}, got {} with {:?}",
            row.held,
            written.status,
            written.body
        );
        assert_eq!(
            written.body["_index"].as_str(),
            Some(index.as_str()),
            "case {case:?}: expected the write to land in {index:?}, got {:?}",
            written.body
        );

        // The read of the same address answers that document, decoded id and all.
        let read = plane(&emulator, "GET", &format!("/{index}/_doc/{}", row.sent), None).await;
        assert_eq!(
            (
                read.status,
                read.body["found"].as_bool(),
                read.body["_id"].as_str()
            ),
            (200, Some(true), Some(row.held)),
            "case {case:?}: expected the document to be found under {:?}, got {} with {:?}",
            row.held,
            read.status,
            read.body
        );
        assert_eq!(
            read.body["_source"], source,
            "case {case:?}: expected the source as sent, got {:?}",
            read.body
        );

        // A search over the index reports the same id, so a client can correlate a hit with
        // the key it indexed under.
        let searched = plane(
            &emulator,
            "POST",
            &format!("/{index}/_search"),
            Some(json!({ "query": { "match_all": {} } })),
        )
        .await;
        assert_eq!(
            hit_ids(&searched),
            vec![row.held.to_string()],
            "case {case:?}: expected one hit with _id {:?}, got {} with {:?}",
            row.held,
            searched.status,
            searched.body
        );

        // The LITERAL escape text is another id, which the index does not hold: a cluster
        // that stored the escapes would answer this address instead of the one above.
        if row.sent != row.held {
            let literal = double_escaped(row.sent);
            let absent = plane(
                &emulator,
                "GET",
                &format!("/{index}/_doc/{literal}"),
                None,
            )
            .await;
            assert_eq!(
                (absent.status, absent.body["found"].as_bool()),
                (404, Some(false)),
                "case {case:?}: expected the escape text {:?} to address no document, got {} \
                 with {:?}",
                row.sent,
                absent.status,
                absent.body
            );
        }
    }
}

/// RULE: the same decoding holds for the INDEX name segment — an escape in it stands for the
/// character it encodes, so the request addresses the index that character spells.
#[tokio::test]
async fn an_escaped_index_segment_addresses_the_index_its_escapes_spell() {
    let emulator = Emulator::new();
    created_domain(&emulator, DOMAIN).await;

    let written = plane(
        &emulator,
        "PUT",
        "/orders/_doc/1?refresh=true",
        Some(json!({ "key": "1" })),
    )
    .await;
    assert_eq!(
        written.status, 201,
        "case \"the write that creates the index\": expected 201, got {} with {:?}",
        written.status, written.body
    );

    // `%6F` is the letter `o`: escaping an unreserved character names the same index.
    let read = plane(&emulator, "GET", "/%6Frders/_doc/1", None).await;
    assert_eq!(
        (
            read.status,
            read.body["found"].as_bool(),
            read.body["_index"].as_str()
        ),
        (200, Some(true), Some("orders")),
        "case \"an escaped letter in the index segment\": expected the document of index \
         \"orders\", got {} with {:?}",
        read.status,
        read.body
    );

    // And the same index is searched through that address.
    let searched = plane(
        &emulator,
        "POST",
        "/%6Frders/_search",
        Some(json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        hit_ids(&searched),
        vec!["1".to_string()],
        "case \"an escaped letter in the index segment of a search\": expected the one hit, \
         got {} with {:?}",
        searched.status,
        searched.body
    );
}

/// RULE: every route that addresses one document by its id reads that id the same way — the
/// create, the partial update and the delete address the decoded id, and so does the refusal
/// each of them words.
#[tokio::test]
async fn the_create_update_and_delete_routes_address_the_decoded_id() {
    let emulator = Emulator::new();
    created_domain(&emulator, DOMAIN).await;

    let sent = "docs%2Fledger.json";
    let held = "docs/ledger.json";
    let literal = double_escaped(sent);

    // `_create` stores the decoded id.
    let created = plane(
        &emulator,
        "PUT",
        &format!("/books/_create/{sent}?refresh=true"),
        Some(json!({ "status": "pending", "owner": "x" })),
    )
    .await;
    assert_eq!(
        (created.status, created.body["_id"].as_str()),
        (201, Some(held)),
        "case \"_create of an escaped id\": expected 201 with _id {held:?}, got {} with {:?}",
        created.status,
        created.body
    );

    // A second `_create` of the SAME address is the version conflict of a document already
    // there, which quotes the decoded id.
    let again = plane(
        &emulator,
        "PUT",
        &format!("/books/_create/{sent}"),
        Some(json!({ "status": "again" })),
    )
    .await;
    assert_eq!(
        (again.status, error_type(&again).as_str()),
        (409, "version_conflict_engine_exception"),
        "case \"_create of an id already held\": expected the version conflict, got {} with \
         {:?}",
        again.status,
        again.body
    );
    assert!(
        again.body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains(held),
        "case \"_create of an id already held\": expected the reason to quote {held:?}, got \
         {:?}",
        again.body
    );

    // A `_create` of the LITERAL escape text is a different id, so it is taken.
    let other = plane(
        &emulator,
        "PUT",
        &format!("/books/_create/{literal}?refresh=true"),
        Some(json!({ "status": "literal" })),
    )
    .await;
    assert_eq!(
        (other.status, other.body["_id"].as_str()),
        (201, Some(sent)),
        "case \"_create of the escape text\": expected 201 with _id {sent:?}, got {} with \
         {:?}",
        other.status,
        other.body
    );

    // The partial update merges onto the document the decoded id names, and leaves the one
    // the escape text names standing.
    let updated = plane(
        &emulator,
        "POST",
        &format!("/books/_update/{sent}?refresh=true"),
        Some(json!({ "doc": { "status": "done" } })),
    )
    .await;
    assert_eq!(
        (updated.status, updated.body["result"].as_str()),
        (200, Some("updated")),
        "case \"_update of an escaped id\": expected the merge to land, got {} with {:?}",
        updated.status,
        updated.body
    );
    let merged = plane(&emulator, "GET", &format!("/books/_doc/{sent}"), None).await;
    assert_eq!(
        merged.body["_source"],
        json!({ "status": "done", "owner": "x" }),
        "case \"_update of an escaped id\": expected the merged document, got {:?}",
        merged.body
    );
    let untouched = plane(&emulator, "GET", &format!("/books/_doc/{literal}"), None).await;
    assert_eq!(
        untouched.body["_source"],
        json!({ "status": "literal" }),
        "case \"_update of an escaped id\": expected the document the escape text names to \
         be untouched, got {:?}",
        untouched.body
    );

    // An `_update` of an id nothing holds is the engine's missing-document refusal, quoting
    // the decoded id.
    let missing = plane(
        &emulator,
        "POST",
        "/books/_update/docs%2Fabsent.json",
        Some(json!({ "doc": { "status": "done" } })),
    )
    .await;
    assert_eq!(
        (missing.status, error_type(&missing).as_str()),
        (404, "document_missing_exception"),
        "case \"_update of an id nothing holds\": expected the missing-document refusal, got \
         {} with {:?}",
        missing.status,
        missing.body
    );
    assert!(
        missing.body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("docs/absent.json"),
        "case \"_update of an id nothing holds\": expected the reason to quote the decoded \
         id, got {:?}",
        missing.body
    );

    // The delete addresses the decoded id too: it removes that document and reports it,
    // and the one the escape text names stays.
    let deleted = plane(
        &emulator,
        "DELETE",
        &format!("/books/_doc/{sent}?refresh=true"),
        None,
    )
    .await;
    assert_eq!(
        (
            deleted.status,
            deleted.body["result"].as_str(),
            deleted.body["_id"].as_str()
        ),
        (200, Some("deleted"), Some(held)),
        "case \"a delete of an escaped id\": expected the document to be deleted, got {} \
         with {:?}",
        deleted.status,
        deleted.body
    );
    let gone = plane(&emulator, "GET", &format!("/books/_doc/{sent}"), None).await;
    assert_eq!(
        (gone.status, gone.body["found"].as_bool()),
        (404, Some(false)),
        "case \"a delete of an escaped id\": expected the document to be gone, got {} with \
         {:?}",
        gone.status,
        gone.body
    );
    let kept = plane(&emulator, "GET", &format!("/books/_doc/{literal}"), None).await;
    assert_eq!(
        (kept.status, kept.body["_id"].as_str()),
        (200, Some(sent)),
        "case \"a delete of an escaped id\": expected the document the escape text names to \
         stay, got {} with {:?}",
        kept.status,
        kept.body
    );
}

/// RULE: the decoding is the SURFACE's, not one addressing's — the host a domain publishes
/// and the prefixed path serve the same escaped target as the same document.
#[tokio::test]
async fn both_addressings_read_an_escaped_id_as_the_same_document() {
    let emulator = Emulator::new();
    created_domain(&emulator, DOMAIN).await;
    let host = endpoint_of(&emulator, DOMAIN);

    let sent = "docs%2Fpublished.json";
    let held = "docs/published.json";

    // Written through the endpoint the control plane publishes, the way a client of the
    // domain writes.
    let written = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "PUT",
        &format!("/catalog/_doc/{sent}?refresh=true"),
        Some(json!({ "key": held })),
    )
    .await;
    assert_eq!(
        (written.status, written.body["_id"].as_str()),
        (201, Some(held)),
        "case \"the published endpoint\": expected 201 with _id {held:?}, got {} with {:?}",
        written.status,
        written.body
    );

    // Read back through both addressings, which answer the same document.
    for (case, answer) in [
        (
            "the published endpoint",
            rest(
                &emulator,
                DEFAULT_ACCESS_KEY_ID,
                &host,
                "GET",
                &format!("/catalog/_doc/{sent}"),
                None,
            )
            .await,
        ),
        (
            "the prefixed path",
            plane(&emulator, "GET", &format!("/catalog/_doc/{sent}"), None).await,
        ),
    ] {
        assert_eq!(
            (
                answer.status,
                answer.body["found"].as_bool(),
                answer.body["_id"].as_str()
            ),
            (200, Some(true), Some(held)),
            "case {case:?}: expected the document under {held:?}, got {} with {:?}",
            answer.status,
            answer.body
        );
    }
}

/// The role the policy below names, whose session every request of the authorization case
/// is signed with.
const WRITER_ROLE: &str = "escape-writer";

/// The session name that role's seeded credential carries.
const WRITER_SESSION: &str = "escape-session";

/// The access key id of that session's credentials.
const WRITER_KEY: &str = "ASIAEXAMPLEESCAPES01";

/// The identity-based grant that role carries: every data-plane action of this service, so
/// what is left to decide is the domain's own access policy.
fn data_plane_grant() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "es:ESHttp*", "Resource": "*" }],
    })
}

/// The access policy the authorization case deploys: the role allowed on the domain, with
/// two `Deny` statements naming document paths — one written with the literal separator the
/// ids carry, and one written with the escape TEXT a client has to send it as.
fn document_path_policy(domain: &str) -> String {
    let base = format!(
        "arn:aws:es:{REGION}:{}:domain/{domain}",
        ::engine::emulator_account_id()
    );
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": { "AWS": role_arn(WRITER_ROLE) },
                "Action": "es:ESHttp*",
                "Resource": format!("{base}/*"),
            },
            {
                "Effect": "Deny",
                "Principal": { "AWS": role_arn(WRITER_ROLE) },
                "Action": "es:ESHttp*",
                "Resource": format!("{base}/*/_doc/docs/deny-*"),
            },
            {
                "Effect": "Deny",
                "Principal": { "AWS": role_arn(WRITER_ROLE) },
                "Action": "es:ESHttp*",
                "Resource": format!("{base}/*/_doc/docs%2Fescaped-*"),
            },
        ],
    })
    .to_string()
}

/// One row of the authorization table: the id as the client sends it, and whether the
/// deployed policy refuses the write of it.
struct Decided {
    case: &'static str,
    sent: &'static str,
    /// The status the write is answered with: the policy's refusal, or the created document.
    status: u16,
}

/// RULE: the resource an authorization decision is taken against is built from the same
/// values the path's segments carry, so a policy statement naming a document path decides
/// the request that addresses that document — whatever escapes the client had to send it
/// with — and a statement naming the escape text names no resource the request addresses.
#[tokio::test]
async fn an_access_policy_decides_the_document_path_the_segments_spell() {
    let emulator = Emulator::new();
    let domain = "escape-acl";
    given_session(&emulator, WRITER_ROLE, WRITER_SESSION, WRITER_KEY);
    given_role_policy(&emulator, WRITER_ROLE, data_plane_grant());
    let created = emulator
        .create(json!({
            "DomainName": domain,
            "AccessPolicies": document_path_policy(domain),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain with the document-path policy\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    let host = endpoint_of(&emulator, domain);

    let rows: &[Decided] = &[
        Decided {
            case: "an id the policy denies, sent escaped",
            sent: "docs%2Fdeny-me.json",
            status: 403,
        },
        Decided {
            case: "an id of the same prefix the policy says nothing about",
            sent: "docs%2Fkeep-me.json",
            status: 201,
        },
        Decided {
            case: "an id a statement names by its escape text, which addresses nothing",
            sent: "docs%2Fescaped-ok.json",
            status: 201,
        },
        Decided {
            case: "an id carrying no separator at all, outside every deny",
            sent: "deny-me.json",
            status: 201,
        },
    ];

    for row in rows {
        let case = row.case;
        let answer = rest(
            &emulator,
            WRITER_KEY,
            &host,
            "PUT",
            &format!("/documents/_doc/{}", row.sent),
            Some(json!({ "key": row.sent })),
        )
        .await;
        assert_eq!(
            answer.status, row.status,
            "case {case:?}: expected {}, got {} with {:?}",
            row.status, answer.status, answer.body
        );
    }
}
