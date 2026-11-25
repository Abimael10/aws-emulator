// SPDX-License-Identifier: CC0-1.0

//! What decides a signed data-plane request on a domain whose access policy names an ACCOUNT
//! rather than a principal.
//!
//! The rule under test: **naming an account in a resource-based policy's `Principal` is not
//! naming a principal — such a statement delegates the decision to that account's own
//! identity-based policies instead of granting any principal of it — so a signed request to
//! the address a domain publishes is served exactly while the policies in force on the SIGNING
//! principal allow the one `es:ESHttp<Method>` action its HTTP method asks for, is refused
//! `403` with AWS's own sentence naming that action otherwise, and a refused write leaves the
//! index unmodified.** A statement that names the principal itself still grants it, which is
//! what separates the delegation from a refusal of everything.
//!
//! The rows differ along what the rule generalises over: WHICH spelling the access policy names
//! the account by (the account's root ARN and its bare account id, beside a control document
//! that names the principals themselves), WHAT the identity policy in force on the signer says
//! (the write action alone, the read action alone, the wildcard every one of them matches, a
//! document about another service entirely, and the wildcard beside an explicit deny of the
//! write), and WHICH action the request asks for (one per HTTP method the request carries).
//! Every row asserts the status, the sentence a refusal carries, and — for a write — whether
//! the DOCUMENT is there afterwards, read back through the domain's own data plane by a
//! principal the same document admits, so a write that answered without storing anything and a
//! refusal that stored something both fail here.
//!
//! Every case drives the real wire path — an HTTP request through the multi-service router into
//! the mounted service, sent to the address the control plane PUBLISHED for the domain — and
//! every wait is for the CONDITION a domain reports, never for a span. The control-plane
//! harness comes from [`super::domain_lifecycle`] and the session, policy and published-address
//! helpers from [`super::published_domain_address`]; this suite adds no second copy of them.

use serde_json::{json, Value};

use super::domain_lifecycle::{settled_domain, Answer, Emulator, REGION};
use super::published_domain_address::{
    at_address, given_role_policy, given_session, published_endpoint, refusal_message, role_arn,
    session_arn,
};

/// The session name every seeded credential of this suite carries.
const SESSION: &str = "delegated-session";

/// The index every request below addresses.
const INDEX: &str = "catalog";

/// The document id each domain is seeded with, which the read rows ask for: a read the policies
/// admit answers it, so a refused read is told apart from a read of nothing.
const SEEDED_ID: &str = "seeded";

/// The engine release every domain here runs.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// One principal the rows sign as: the role its session speaks for, that session's access key
/// id, and the identity-based document IAM holds for the role.
struct Signer {
    role: &'static str,
    access_key_id: &'static str,
    /// The role's inline policy — what the delegation below leaves deciding.
    document: fn() -> Value,
}

/// An identity document whose `Statement` list is `statements`.
fn identity(statements: Value) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The `es:ESHttp*` actions a request's HTTP method asks for, as the IAM Service Authorization
/// Reference publishes them under "Actions defined by Amazon OpenSearch Service": one per
/// method, which is where each row's action comes from.
const WRITE_ACTION: &str = "es:ESHttpPut";
const READ_ACTION: &str = "es:ESHttpGet";

/// A principal whose identity policy allows the WRITE action alone.
const WRITER: Signer = Signer {
    role: "delegated-writer",
    access_key_id: "ASIAEXAMPLEDELEGWRT1",
    document: || {
        identity(json!([{ "Effect": "Allow", "Action": WRITE_ACTION, "Resource": "*" }]))
    },
};

/// A principal whose identity policy allows the READ action alone.
const READER: Signer = Signer {
    role: "delegated-reader",
    access_key_id: "ASIAEXAMPLEDELEGRDR1",
    document: || identity(json!([{ "Effect": "Allow", "Action": READ_ACTION, "Resource": "*" }])),
};

/// A principal whose identity policy allows the wildcard every data-plane action matches.
const EVERY: Signer = Signer {
    role: "delegated-every",
    access_key_id: "ASIAEXAMPLEDELEGEVY1",
    document: || identity(json!([{ "Effect": "Allow", "Action": "es:ESHttp*", "Resource": "*" }])),
};

/// A principal whose identity policy exists and is about ANOTHER service, so the documents in
/// force on it state nothing about these requests.
const NOTHING: Signer = Signer {
    role: "delegated-none",
    access_key_id: "ASIAEXAMPLEDELEGNON1",
    document: || identity(json!([{ "Effect": "Allow", "Action": "s3:GetObject", "Resource": "*" }])),
};

/// A principal whose identity policy allows every data-plane action and explicitly denies the
/// write — the rule that wins over every allow, whichever side states it.
const DENIED: Signer = Signer {
    role: "delegated-denied",
    access_key_id: "ASIAEXAMPLEDELEGDNY1",
    document: || {
        identity(json!([
            { "Effect": "Allow", "Action": "es:ESHttp*", "Resource": "*" },
            { "Effect": "Deny", "Action": WRITE_ACTION, "Resource": "*" },
        ]))
    },
};

/// Every principal the rows sign as, which is also the seeding order.
const SIGNERS: &[&Signer] = &[&WRITER, &READER, &EVERY, &NOTHING, &DENIED];

/// Which principals the domain's access policy NAMES.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// The account's root ARN, which names the whole account.
    AccountRoot,
    /// The bare account id, the other spelling of the same account.
    AccountId,
    /// Each signing principal itself — the control, where the document grants the request
    /// whatever the identity side says.
    ThePrincipals,
}

impl Named {
    /// The domain carrying this document.
    fn domain(self) -> &'static str {
        match self {
            Self::AccountRoot => "delegated-root",
            Self::AccountId => "delegated-account",
            Self::ThePrincipals => "delegated-named",
        }
    }

    /// The `Principal` element's `AWS` value for this document.
    fn principal(self) -> Value {
        let account = ::engine::emulator_account_id();
        match self {
            Self::AccountRoot => json!(format!("arn:aws:iam::{account}:root")),
            Self::AccountId => json!(account),
            Self::ThePrincipals => {
                json!(SIGNERS.iter().map(|signer| role_arn(signer.role)).collect::<Vec<_>>())
            }
        }
    }

    /// The whole access policy the domain is created with: ONE `Allow` of every data-plane
    /// action on every path of the domain, naming the principals above and nothing else.
    fn access_policy(self) -> String {
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "AWS": self.principal() },
                "Action": "es:ESHttp*",
                "Resource": format!(
                    "arn:aws:es:{REGION}:{}:domain/{}/*",
                    ::engine::emulator_account_id(),
                    self.domain()
                ),
            }],
        })
        .to_string()
    }
}

/// What the domain must answer one row's request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The request is admitted and the cluster answers it.
    Served,
    /// IAM refuses it, with the clause naming which of its rules did.
    Refused(Clause),
}

/// Which of IAM's rules a refusal names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    /// Nothing in force allows the action: the access policy granted the ACCOUNT, which is a
    /// delegation rather than a grant, so what would have had to allow it is the principal's
    /// own identity policy.
    NoIdentityGrant,
    /// A statement of the principal's own identity policy explicitly denies it.
    ExplicitIdentityDeny,
}

impl Clause {
    /// The clause the refusal's sentence ends with, for a request asking for `action`.
    fn text(self, action: &str) -> String {
        match self {
            Self::NoIdentityGrant => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitIdentityDeny => {
                "with an explicit deny in an identity-based policy".to_string()
            }
        }
    }
}

/// One row: the document in force on the domain, the principal that signs, the request it
/// makes, and what the domain must answer.
struct Row {
    case: &'static str,
    policy: Named,
    signer: &'static Signer,
    method: &'static str,
    /// The `es:ESHttp*` action that method asks for, which a refusal's sentence names.
    action: &'static str,
    expected: Expected,
}

/// The table: the two spellings an account is named by against every identity document above,
/// over both methods — so neither the verdict nor the action a refusal names is a fixed answer
/// — plus the control rows whose document names the principals themselves.
const ROWS: &[Row] = &[
    Row {
        // The recorded form: a role granted the read action alone, writing.
        case: "the root ARN, a role granted only the read action, writing",
        policy: Named::AccountRoot,
        signer: &READER,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Refused(Clause::NoIdentityGrant),
    },
    Row {
        case: "the root ARN, a role granted the write action, writing",
        policy: Named::AccountRoot,
        signer: &WRITER,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Served,
    },
    Row {
        case: "the root ARN, a role granted only the write action, reading",
        policy: Named::AccountRoot,
        signer: &WRITER,
        method: "GET",
        action: READ_ACTION,
        expected: Expected::Refused(Clause::NoIdentityGrant),
    },
    Row {
        case: "the root ARN, a role granted the read action, reading",
        policy: Named::AccountRoot,
        signer: &READER,
        method: "GET",
        action: READ_ACTION,
        expected: Expected::Served,
    },
    Row {
        case: "the root ARN, a role granted the wildcard, writing",
        policy: Named::AccountRoot,
        signer: &EVERY,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Served,
    },
    Row {
        case: "the root ARN, a role granted the wildcard, reading",
        policy: Named::AccountRoot,
        signer: &EVERY,
        method: "GET",
        action: READ_ACTION,
        expected: Expected::Served,
    },
    Row {
        case: "the root ARN, a role whose document is about another service, writing",
        policy: Named::AccountRoot,
        signer: &NOTHING,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Refused(Clause::NoIdentityGrant),
    },
    Row {
        case: "the root ARN, a role whose document is about another service, reading",
        policy: Named::AccountRoot,
        signer: &NOTHING,
        method: "GET",
        action: READ_ACTION,
        expected: Expected::Refused(Clause::NoIdentityGrant),
    },
    Row {
        case: "the root ARN, a role whose own document denies the write, writing",
        policy: Named::AccountRoot,
        signer: &DENIED,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Refused(Clause::ExplicitIdentityDeny),
    },
    Row {
        case: "the root ARN, a role whose own document denies the write alone, reading",
        policy: Named::AccountRoot,
        signer: &DENIED,
        method: "GET",
        action: READ_ACTION,
        expected: Expected::Served,
    },
    Row {
        case: "the bare account id, a role granted only the read action, writing",
        policy: Named::AccountId,
        signer: &READER,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Refused(Clause::NoIdentityGrant),
    },
    Row {
        case: "the bare account id, a role granted the write action, writing",
        policy: Named::AccountId,
        signer: &WRITER,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Served,
    },
    Row {
        case: "the bare account id, a role whose document is about another service, reading",
        policy: Named::AccountId,
        signer: &NOTHING,
        method: "GET",
        action: READ_ACTION,
        expected: Expected::Refused(Clause::NoIdentityGrant),
    },
    Row {
        case: "the principals themselves, a role whose document is about another service, writing",
        policy: Named::ThePrincipals,
        signer: &NOTHING,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Served,
    },
    Row {
        case: "the principals themselves, a role granted only the read action, writing",
        policy: Named::ThePrincipals,
        signer: &READER,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Served,
    },
    Row {
        case: "the principals themselves, a role whose own document denies the write, writing",
        policy: Named::ThePrincipals,
        signer: &DENIED,
        method: "PUT",
        action: WRITE_ACTION,
        expected: Expected::Refused(Clause::ExplicitIdentityDeny),
    },
];

/// `CreateDomain` for the domain carrying `policy`'s document, answering the address it
/// publishes once it reports itself settled.
async fn given_domain(emulator: &Emulator, policy: Named) -> String {
    let domain = policy.domain();
    let created = emulator
        .create(json!({
            "DomainName": domain,
            "EngineVersion": ENGINE_VERSION,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "AccessPolicies": policy.access_policy(),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain({domain:?})\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(emulator, domain).await;
    published_endpoint(emulator, domain, domain).await
}

/// The path one document of the index is addressed at.
fn document_path(id: &str) -> String {
    format!("/{INDEX}/_doc/{id}")
}

/// The document a row writes, which is also what a read of it must answer.
fn document(case: &str) -> Value {
    json!({ "title": case })
}

/// Send one row's request as the principal it signs as.
async fn requested(emulator: &Emulator, address: &str, row: &Row, id: &str) -> Answer {
    let body = (row.method == "PUT").then(|| document(row.case));
    at_address(
        emulator,
        address,
        row.signer.access_key_id,
        row.method,
        &document_path(id),
        body,
    )
    .await
}

/// RULE: a statement of a domain's access policy naming the caller's ACCOUNT grants no
/// principal of that account by itself — it delegates the decision to the account's own
/// identity-based policies — so each request is served exactly while the documents in force on
/// the signing principal allow the `es:ESHttp<Method>` action its method asks for, every other
/// is refused `403` with the sentence naming that action, and a refused write stores nothing.
#[tokio::test]
async fn an_access_policy_naming_the_account_leaves_the_identity_policies_deciding() {
    let emulator = Emulator::new();
    for signer in SIGNERS {
        given_session(&emulator, signer.role, SESSION, signer.access_key_id);
        given_role_policy(&emulator, signer.role, (signer.document)());
    }

    // One domain per document in force, each seeded with the document the read rows ask for.
    // The seeding is done by the principal every document admits — the wildcard-granted role on
    // the delegating domains, which is also one of the principals the control document names —
    // so a read row that is refused is refused with the document THERE.
    let mut addresses: Vec<(Named, String)> = Vec::new();
    for policy in [Named::AccountRoot, Named::AccountId, Named::ThePrincipals] {
        let address = given_domain(&emulator, policy).await;
        let seeded = at_address(
            &emulator,
            &address,
            EVERY.access_key_id,
            "PUT",
            &document_path(SEEDED_ID),
            Some(document("the seeded document")),
        )
        .await;
        assert!(
            (200..300).contains(&seeded.status),
            "case \"seeding {}\": expected the write every document admits to be served, got {} \
             with {:?}",
            policy.domain(),
            seeded.status,
            seeded.body
        );
        addresses.push((policy, address));
    }

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let address = &addresses
            .iter()
            .find(|(policy, _)| *policy == row.policy)
            .expect("every row's document is one of the three created above")
            .1;
        // A write addresses an id of its own, so what a row stored (or did not) is its own; a
        // read addresses the id every domain was seeded with.
        let id = if row.method == "PUT" {
            format!("written-{index}")
        } else {
            SEEDED_ID.to_string()
        };
        let answer = requested(&emulator, address, row, &id).await;

        match row.expected {
            Expected::Served => assert!(
                (200..300).contains(&answer.status),
                "case {case:?}: expected the request the identity side allows to be served, got \
                 {} with {:?}",
                answer.status,
                answer.body
            ),
            Expected::Refused(clause) => {
                assert_eq!(
                    (answer.status, refusal_message(&answer)),
                    (
                        403,
                        format!(
                            "User: {} is not authorized to perform: {} {}",
                            session_arn(row.signer.role, SESSION),
                            row.action,
                            clause.text(row.action)
                        )
                    ),
                    "case {case:?}: expected IAM's own refusal sentence, got {} with {:?}",
                    answer.status,
                    answer.body
                );
                // The IAM layer refuses before the cluster is asked anything, so the answer
                // carries AWS's own envelope and none of the cluster's error document.
                assert_eq!(
                    answer.body["error"],
                    Value::Null,
                    "case {case:?}: expected no cluster error document beside IAM's refusal, got \
                     {:?}",
                    answer.body
                );
            }
        }

        if row.method != "PUT" {
            continue;
        }
        // What the row DID to the index, read back through the domain's own data plane by a
        // principal every document admits: a served write stored its document and a refused one
        // stored nothing at all.
        let read_back = at_address(
            &emulator,
            address,
            EVERY.access_key_id,
            "GET",
            &document_path(&id),
            None,
        )
        .await;
        let expected_status = if row.expected == Expected::Served {
            200
        } else {
            404
        };
        assert_eq!(
            read_back.status, expected_status,
            "case {case:?}: expected reading the id the write addressed to answer \
             {expected_status}, got {} with {:?}",
            read_back.status, read_back.body
        );
        if row.expected == Expected::Served {
            assert_eq!(
                read_back.body["_source"],
                document(case),
                "case {case:?}: expected the document the write carried, got {:?}",
                read_back.body
            );
        }
    }
}
