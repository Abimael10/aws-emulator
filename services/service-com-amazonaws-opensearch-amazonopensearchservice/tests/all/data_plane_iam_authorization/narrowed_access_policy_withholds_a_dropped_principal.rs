// SPDX-License-Identifier: CC0-1.0

//! Narrowing a domain's access policy so it no longer covers one principal's request, read
//! from the data plane that principal reaches the domain through.
//!
//! RULE: on a domain that CARRIES an access policy, a SigV4-signed data-plane request is
//! served only while a statement of the policy in force covers it — the principal that
//! signed, the `es:ESHttp*` action its method asks for, and the domain path it addresses —
//! so an `AccessPolicies` update that stops covering one principal's write refuses that
//! identical write `403` with AWS's own sentence, stores nothing (a read by a principal the
//! document still covers finds the document absent), and restoring the document serves the
//! same write again; a principal the narrowed document still covers is served throughout, so
//! neither verdict is the one an inert policy or a blanket refusal would give.
//!
//! The rows span WHICH element of the statement stops covering the request — the statement
//! dropped altogether, the `Principal` list it is no longer in, the `Action` narrowed past
//! the one the request's method names, and the `Resource` narrowed to another index's paths —
//! and the method each write is sent with, so the action in the refusal is the request's and
//! not a fixed string. Neither principal carries an identity-based grant of these actions,
//! which is the position a role reaching a domain through its access policy alone is in: the
//! document under test is the whole of what covers the request.
//!
//! The parent suite's harness, create body, principal seeding and policy-update helper are
//! reused ([`super`]), and the address is read where a client reads it
//! ([`crate::published_domain_address`]); this suite adds no second copy of any of them.

use serde_json::{json, Value};

use crate::domain_lifecycle::{Answer, Emulator, REGION};
use crate::published_domain_address::{
    at_address, given_session, refusal_message, role_arn, session_arn,
};

/// The role every policy below keeps covering: the principal that provisioned the domain and
/// stays named in its access policy, which is what each row's verdict for the other principal
/// is read against.
const KEEPER_ROLE: &str = "plane-keeper";
const KEEPER_KEY: &str = "ASIAEXAMPLEPLANEKPR1";

/// The role each narrowing stops covering: the principal whose writes the full document
/// admits and the narrowed one does not.
const DROPPED_ROLE: &str = "plane-dropped";
const DROPPED_KEY: &str = "ASIAEXAMPLEPLANEDRP1";

/// A role named only inside a policy document — the third party a narrowed `Principal` list
/// names instead of the dropped principal. No session speaks for it.
const OTHER_ROLE: &str = "plane-bystander";

/// The index every write of this suite addresses. The keeper's seed write is what creates it,
/// so a read that finds nothing is reporting an absent DOCUMENT rather than an absent index.
const INDEX: &str = "catalog";

/// A second index, so a statement whose `Resource` names one index's paths is read on the
/// other side of it.
const OTHER_INDEX: &str = "archive";

/// The one domain the rows run on, which keeps its identity and its published address across
/// every update.
const DOMAIN: &str = "plane-narrowed";

/// The document the keeper seeds, which creates [`INDEX`].
const SEED_DOCUMENT: &str = "seed";

/// The paths of one index under a domain's ARN, as an access-policy `Resource` names them
/// (`arn:<partition>:es:<region>:<account>:domain/<name>/<index>/*`) — the whole-domain form
/// of the same element is the parent suite's [`super::domain_resource`].
fn index_paths(domain: &str, index: &str) -> String {
    format!(
        "arn:aws:es:{REGION}:{}:domain/{domain}/{index}/*",
        ::engine::emulator_account_id()
    )
}

/// A statement covering `principals` for every data-plane action on every path of `domain`.
fn covering(principals: Value, domain: &str) -> Value {
    json!({
        "Effect": "Allow",
        "Principal": { "AWS": principals },
        "Action": "es:ESHttp*",
        "Resource": super::domain_resource(domain),
    })
}

/// The access policy as the domain is created with it: ONE statement naming both principals,
/// the way a client writes a document that admits the identity it provisions with and the
/// identity its workload signs as.
fn full_policy(domain: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [covering(
            json!([role_arn(KEEPER_ROLE), role_arn(DROPPED_ROLE)]),
            domain,
        )],
    })
    .to_string()
}

/// The same document with the dropped principal's coverage narrowed away by `row`, the keeper
/// left covered in full: the whole policy in force for that row.
fn narrowed_policy(domain: &str, row: &Row) -> String {
    let mut statements = vec![covering(json!(role_arn(KEEPER_ROLE)), domain)];
    statements.extend((row.narrowed)(domain));
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// One form a narrowing takes, and the write it is read through.
struct Row {
    case: &'static str,
    /// The method the write is sent with, which is where its action comes from.
    method: &'static str,
    /// The `es:ESHttp*` action that method names, as the refusal spells it.
    action: &'static str,
    /// The stem of the document ids this row writes, so each row's legs are its own.
    document: &'static str,
    /// The statement the narrowed document carries about the dropped principal BESIDE the
    /// keeper's own, or `None` where the narrowing is that there is no such statement left.
    narrowed: fn(&str) -> Option<Value>,
}

/// The table: the statement dropped, and each of the three elements that decide whether a
/// statement covers a request — `Principal`, `Action`, `Resource` — narrowed so it no longer
/// does, over two of the methods this surface reads an action from.
const ROWS: &[Row] = &[
    Row {
        case: "the dropped principal's statement removed, the keeper's alone left",
        method: "PUT",
        action: "es:ESHttpPut",
        document: "statement-dropped",
        narrowed: |_domain| None,
    },
    Row {
        case: "a Principal list the dropped principal is no longer in",
        method: "POST",
        action: "es:ESHttpPost",
        document: "principal-narrowed",
        narrowed: |domain| {
            Some(covering(
                json!([role_arn(KEEPER_ROLE), role_arn(OTHER_ROLE)]),
                domain,
            ))
        },
    },
    Row {
        case: "an Action narrowed past the one the write's method names",
        method: "PUT",
        action: "es:ESHttpPut",
        document: "action-narrowed",
        narrowed: |domain| {
            Some(json!({
                "Effect": "Allow",
                "Principal": { "AWS": role_arn(DROPPED_ROLE) },
                "Action": "es:ESHttpGet",
                "Resource": super::domain_resource(domain),
            }))
        },
    },
    Row {
        case: "a Resource naming another index's paths",
        method: "POST",
        action: "es:ESHttpPost",
        document: "resource-narrowed",
        narrowed: |domain| {
            Some(json!({
                "Effect": "Allow",
                "Principal": { "AWS": role_arn(DROPPED_ROLE) },
                "Action": "es:ESHttp*",
                "Resource": index_paths(domain, OTHER_INDEX),
            }))
        },
    },
];

/// Seed the two sessions. Neither role is given an identity-based policy, so the domain's own
/// access policy is the whole of what covers their requests.
fn given_principals(emulator: &Emulator) {
    for (role, key) in [(KEEPER_ROLE, KEEPER_KEY), (DROPPED_ROLE, DROPPED_KEY)] {
        given_session(emulator, role, super::SESSION, key);
    }
}

/// One document write to the published address, sent as `access_key_id`'s session.
async fn write_document(
    emulator: &Emulator,
    address: &str,
    access_key_id: &str,
    method: &str,
    document: &str,
    body: Value,
) -> Answer {
    at_address(
        emulator,
        address,
        access_key_id,
        method,
        &format!("/{INDEX}/_doc/{document}"),
        Some(body),
    )
    .await
}

/// One document read of the published address, sent as the keeper's session — the principal
/// every policy in force covers, so what it reports is the state of the index and never a
/// verdict about the reader.
async fn read_document(emulator: &Emulator, address: &str, document: &str) -> Answer {
    at_address(
        emulator,
        address,
        KEEPER_KEY,
        "GET",
        &format!("/{INDEX}/_doc/{document}"),
        None,
    )
    .await
}

/// The body each write carries, so a document read back is the one that was written.
fn document_body(case: &str) -> Value {
    json!({ "title": case })
}

/// The sentence IAM refuses the dropped principal's request with when no statement of the
/// policy in force covers it: the session that signed, the action its method asked for, and
/// the rule that refused — the default deny, which for a domain carrying an access policy is
/// that document not allowing the action.
fn default_deny_sentence(action: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} because no resource-based policy \
         allows the {action} action",
        session_arn(DROPPED_ROLE, super::SESSION)
    )
}

/// Assert the dropped principal's write is SERVED while the full document is in force, and
/// that what it wrote is readable.
async fn assert_write_served(emulator: &Emulator, address: &str, row: &Row, document: &str) {
    let case = row.case;
    let written = write_document(
        emulator,
        address,
        DROPPED_KEY,
        row.method,
        document,
        document_body(case),
    )
    .await;
    assert_eq!(
        (written.status, written.body["result"].as_str()),
        (201, Some("created")),
        "case {case:?}: expected the covered principal's write to be the engine's own \
         created answer, got {} with {:?}",
        written.status,
        written.body
    );
    let read = read_document(emulator, address, document).await;
    assert_eq!(
        (read.status, read.body["_source"].clone()),
        (200, document_body(case)),
        "case {case:?}: expected the written document to be readable, got {} with {:?}",
        read.status,
        read.body
    );
}

/// RULE: on a domain carrying an access policy, a signed data-plane request is served only
/// while a statement of the policy in force covers it — so narrowing that document away from
/// one principal refuses its identical write `403` with AWS's own sentence and writes
/// nothing, restoring the document serves the same write again, and the principal the
/// narrowed document still covers is served throughout.
#[tokio::test]
async fn a_narrowed_access_policy_refuses_the_write_it_no_longer_covers() {
    let emulator = Emulator::new();
    given_principals(&emulator);
    let address = super::given_domain(
        &emulator,
        DOMAIN,
        &full_policy(DOMAIN),
        "the domain both principals are named on",
    )
    .await;

    // The keeper's own write creates the index every row addresses, so a read that finds
    // nothing below is about the DOCUMENT and not about a missing index.
    let seeded = write_document(
        &emulator,
        &address,
        KEEPER_KEY,
        "PUT",
        SEED_DOCUMENT,
        document_body("the seeded index"),
    )
    .await;
    assert_eq!(
        seeded.status, 201,
        "case \"the seeded index\": expected the keeper's write to create the index, got {} \
         with {:?}",
        seeded.status, seeded.body
    );

    for row in ROWS {
        let case = row.case;
        let granted = format!("{}-covered", row.document);
        let withheld = format!("{}-withheld", row.document);
        let keeper = format!("{}-keeper", row.document);

        // While both principals are named, the dropped principal's write is served.
        super::given_access_policy(&emulator, DOMAIN, &full_policy(DOMAIN), case).await;
        assert_write_served(&emulator, &address, row, &granted).await;

        // The one change: the policy narrowed so it no longer covers that write.
        super::given_access_policy(&emulator, DOMAIN, &narrowed_policy(DOMAIN, row), case).await;
        let refused = write_document(
            &emulator,
            &address,
            DROPPED_KEY,
            row.method,
            &withheld,
            document_body(case),
        )
        .await;
        assert_eq!(
            (refused.status, refusal_message(&refused)),
            (403, default_deny_sentence(row.action)),
            "case {case:?}: expected the default-deny refusal of {}, got {} with {:?}",
            row.action,
            refused.status,
            refused.body
        );
        // And the refused write stored nothing: the document is absent to a principal the
        // narrowed document still covers.
        let absent = read_document(&emulator, &address, &withheld).await;
        assert_eq!(
            (absent.status, absent.body["found"].as_bool()),
            (404, Some(false)),
            "case {case:?}: expected the refused write to have stored nothing, got {} with \
             {:?}",
            absent.status,
            absent.body
        );
        // The narrowed document is neither inert nor a blanket refusal: the principal it
        // still covers writes the same shape of request and is served.
        let keeper_write = write_document(
            &emulator,
            &address,
            KEEPER_KEY,
            row.method,
            &keeper,
            document_body(case),
        )
        .await;
        assert_eq!(
            (keeper_write.status, keeper_write.body["result"].as_str()),
            (201, Some("created")),
            "case {case:?}: expected the still-covered principal's write to be served, got \
             {} with {:?}",
            keeper_write.status,
            keeper_write.body
        );

        // Restoring the document serves the identical write it refused.
        super::given_access_policy(&emulator, DOMAIN, &full_policy(DOMAIN), case).await;
        assert_write_served(&emulator, &address, row, &withheld).await;
    }
}
