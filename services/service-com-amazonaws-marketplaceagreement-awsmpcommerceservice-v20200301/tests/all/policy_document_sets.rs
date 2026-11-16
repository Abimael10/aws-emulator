// SPDX-License-Identifier: CC0-1.0

//! WHICH documents in force on the calling principal decide a call of this service, driven
//! over the real wire path (an awsJson1_0 request through the multi-service router into this
//! service mounted beside IAM and STS on one shared engine), so what is asserted is what a
//! client sees.
//!
//! RULE. IAM takes its decision over EVERY identity-based policy document in force on the
//! principal the request's credentials name — each inline policy the identity carries, under
//! whatever policy name it was written under, and the in-force version of each managed
//! policy attached to it — and combines them by its own two rules: an explicit `Deny` of any
//! action that permits the operation, in ANY of those documents, refuses the call; otherwise
//! an `Allow` of any of those actions in ANY of them serves it; with neither the call falls
//! to IAM's default deny. A refusal is the modelled `AccessDeniedException`
//! (`@httpError(403)`, a service-common error of this model), taken BEFORE the store is
//! read, and its sentence names the principal, the action the operation is performed under,
//! the resource that action takes and which of IAM's rules refused it. So which document
//! carried a statement never changes the answer, a grant one document gives is lost to a
//! deny in another, and taking the one document that denied back off lifts the refusal while
//! the rest of the set still decides the call.
//!
//! The rows span what the rule generalises over: WHERE each document is held (one inline
//! policy of the role, a second inline policy under another name, a customer-managed policy
//! attached to it), HOW MANY are in force at once (one, two, three), which EFFECT each states
//! (an allow of the operation's own action, an allow of the coarse view action, an allow of
//! another service's actions, a deny of either marketplace action) and the OPERATION asked
//! for (one that two actions permit, one that only its own action permits).
//!
//! The harness, the principals and the refusal clause are the ones
//! [`crate::identity_authorization`] states this service's authorization rule with, so the
//! documents this service reads are the ones IAM's own `PutRolePolicy`, `CreatePolicy` +
//! `AttachRolePolicy` and `DeleteRolePolicy` wrote. What this suite adds is the dimension
//! those suites hold fixed at a single document: the SET.

use serde_json::{json, Value};

use crate::identity_authorization::{
    allow, code_of, deny, evaluated_actions, message_of, Emulator, Verdict, ACCESS_DENIED,
    ANY_RESOURCE, SIGNING_NAME,
};
use crate::permitting_actions::{coarse_action, NOT_FOUND};

/// Where one identity-based document a row puts in force is held.
#[derive(Debug, Clone, Copy)]
enum Held {
    /// An inline policy of the role, under the policy name given.
    Inline(&'static str),
    /// A customer-managed policy created under that name and attached to the role.
    Attached(&'static str),
}

/// One document a row puts in force on the calling role: where it is held, and what it says.
struct Document {
    held: Held,
    /// Built from the action the operation under test is performed under, so one row decides
    /// every operation.
    statements: fn(&str) -> Value,
}

/// One document set the rule is asserted over, with the verdict it reaches for an operation
/// the coarse view action also permits and for one only its own action permits.
struct Row {
    case: &'static str,
    documents: Vec<Document>,
    /// The verdict for an operation the coarse view action also permits.
    viewable: Verdict,
    /// The verdict for an operation only its own action permits.
    own_only: Verdict,
}

/// Every document set the rule spans: one, two and three documents, held inline and as an
/// attachment, stating a grant of the operation's own action, a grant of the coarse view
/// action, a grant of another service entirely, and a deny of either marketplace action.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an allow and a deny of the same action, in two inline policies",
            documents: vec![
                Document {
                    held: Held::Inline("grant-operation"),
                    statements: |own| allow(json!(own)),
                },
                Document {
                    held: Held::Inline("scope-down"),
                    statements: |own| deny(json!(own)),
                },
            ],
            viewable: Verdict::ExplicitDeny,
            own_only: Verdict::ExplicitDeny,
        },
        Row {
            case: "an allow inline, the deny in an attached managed policy",
            documents: vec![
                Document {
                    held: Held::Inline("grant-operation"),
                    statements: |own| allow(json!(own)),
                },
                Document {
                    held: Held::Attached("scope-down"),
                    statements: |own| deny(json!(own)),
                },
            ],
            viewable: Verdict::ExplicitDeny,
            own_only: Verdict::ExplicitDeny,
        },
        Row {
            case: "the only grant in an attached managed policy",
            documents: vec![Document {
                held: Held::Attached("grant-operation"),
                statements: |own| allow(json!(own)),
            }],
            viewable: Verdict::Served,
            own_only: Verdict::Served,
        },
        Row {
            case: "a deny of the coarse view action, beside two grants, across three documents",
            documents: vec![
                Document {
                    held: Held::Inline("grant-operation"),
                    statements: |own| allow(json!(own)),
                },
                Document {
                    held: Held::Attached("grant-view"),
                    statements: |_| allow(json!(coarse_action())),
                },
                Document {
                    held: Held::Inline("scope-down-view"),
                    statements: |_| deny(json!(coarse_action())),
                },
            ],
            // The coarse action permits the first operation, so a deny of it refuses that
            // call; the decision about an operation only its own action permits is not taken
            // over the coarse action at all, so the grant of its own action still serves it.
            viewable: Verdict::ExplicitDeny,
            own_only: Verdict::Served,
        },
        Row {
            case: "a deny in the only document, with no grant anywhere",
            documents: vec![Document {
                held: Held::Inline("scope-down"),
                statements: |own| deny(json!(own)),
            }],
            viewable: Verdict::ExplicitDeny,
            own_only: Verdict::ExplicitDeny,
        },
        Row {
            case: "two documents, neither about this service",
            documents: vec![
                Document {
                    held: Held::Inline("other-service"),
                    statements: |_| allow(json!("s3:*")),
                },
                Document {
                    held: Held::Attached("another-service"),
                    statements: |_| allow(json!("sqs:*")),
                },
            ],
            viewable: Verdict::DefaultDeny,
            own_only: Verdict::DefaultDeny,
        },
        Row {
            case: "the grant in one of three documents, the others about other services",
            documents: vec![
                Document {
                    held: Held::Inline("other-service"),
                    statements: |_| allow(json!("s3:*")),
                },
                Document {
                    held: Held::Inline("grant-operation"),
                    statements: |own| allow(json!(own)),
                },
                Document {
                    held: Held::Attached("another-service"),
                    statements: |_| allow(json!("sqs:*")),
                },
            ],
            viewable: Verdict::Served,
            own_only: Verdict::Served,
        },
        Row {
            case: "the coarse grant attached, beside an inline policy about another service",
            documents: vec![
                Document {
                    held: Held::Attached("grant-view"),
                    statements: |_| allow(json!(coarse_action())),
                },
                Document {
                    held: Held::Inline("other-service"),
                    statements: |_| allow(json!("s3:*")),
                },
            ],
            viewable: Verdict::Served,
            own_only: Verdict::DefaultDeny,
        },
    ]
}

/// One operation the rule is asserted over: the call that reaches it, and whether the coarse
/// view action also permits it.
struct Operation {
    case: &'static str,
    name: &'static str,
    /// Built from the fixture agreement's id, so a served row really answers from the store.
    body: fn(&str) -> Value,
    /// Whether `aws-marketplace:ViewSubscriptions` also permits this operation.
    viewable: bool,
    /// Whether a served answer REPORTS the agreement the call named, under `agreementId` —
    /// the member `DescribeAgreementOutput` carries
    /// (`smithy_query(query="operation",operation="DescribeAgreement")`). A listing answers
    /// `items` instead, so only its status is asserted.
    reports_agreement_id: bool,
}

/// The two classes of operation a document set decides: one the coarse view action also
/// permits, so its decision is taken over two actions, and one only its own action permits.
fn operations() -> Vec<Operation> {
    vec![
        Operation {
            case: "the report of one agreement",
            name: "DescribeAgreement",
            body: |agreement| json!({"agreementId": agreement}),
            viewable: true,
            reports_agreement_id: true,
        },
        Operation {
            case: "a listing of an agreement's charges",
            name: "ListAgreementCharges",
            body: |agreement| json!({"agreementId": agreement}),
            viewable: false,
            reports_agreement_id: false,
        },
    ]
}

/// Put one row's documents in force on `role`, each where the row holds it.
async fn put_in_force(emulator: &Emulator, role: &str, documents: &[Document], own: &str) {
    for document in documents {
        let policy = (document.statements)(own);
        match document.held {
            Held::Inline(name) => emulator.put_named_role_policy(role, name, &policy).await,
            Held::Attached(name) => {
                emulator
                    .attach_managed_policy(role, &format!("{role}-{name}"), &policy)
                    .await
            }
        }
    }
}

/// RULE: the decision is taken over every identity-based document in force on the calling
/// principal, wherever it is held — a deny in any of them refuses the call, an allow in any
/// of them (with no deny) serves it, and with neither it falls to the default deny.
#[tokio::test]
async fn every_document_in_force_on_the_principal_decides_the_call() {
    for (index, row) in rows().into_iter().enumerate() {
        for operation in operations() {
            let case = format!("{} / {}", row.case, operation.case);
            let emulator = Emulator::new();
            let agreement = emulator.given_agreement(&format!("ap-docset{index}")).await;
            let own = format!("{SIGNING_NAME}:{}", operation.name);
            let role = format!("docset-{index}-{}", operation.name);
            // The role is assumed with no document at all, then every document of the row is
            // put in force on it: a session reads the policies as they stand when it CALLS,
            // which is what makes a set written after the assume decide the call.
            let session = emulator.assumed_role(&role, None).await;
            put_in_force(&emulator, &role, &row.documents, &own).await;

            let body = (operation.body)(&agreement);
            let (status, answered) = emulator
                .call(&session.access_key_id, operation.name, &body)
                .await;
            let expected = if operation.viewable {
                row.viewable
            } else {
                row.own_only
            };
            if expected == Verdict::Served {
                // A served call reaches the operation and answers from the store: the
                // success status, and — for the report of one agreement — the id the call
                // named, so a served row proves the set of documents let the read through
                // rather than merely not carrying the 403.
                assert_eq!(
                    status, 200,
                    "case {case:?}: a set of documents that allows the call must be served by \
                     the operation, got {status} with {answered}"
                );
                if operation.reports_agreement_id {
                    assert_eq!(
                        answered["agreementId"].as_str().unwrap_or_default(),
                        agreement.as_str(),
                        "case {case:?}: the served report must answer the agreement the call \
                         named, got {answered}"
                    );
                }
                continue;
            }
            assert_eq!(
                (status, code_of(&answered)),
                ACCESS_DENIED,
                "case {case:?}: expected the modelled refusal, got {status} with {answered}"
            );
            let sentence = format!(
                "User: {} is not authorized to perform: {own} on resource: {ANY_RESOURCE} {}",
                session.arn,
                expected.reason(&evaluated_actions(operation.name))
            );
            assert_eq!(
                message_of(&answered),
                sentence,
                "case {case:?}: expected IAM's wording, got {answered}"
            );
        }
    }
}

/// The inline policy the grant is written under, kept for the whole of the lifecycle test.
const GRANT_POLICY: &str = "grant-describe-agreement";
/// The inline policy the deny is written under, put in force beside the grant and taken off
/// again — a SECOND document, so the grant is never rewritten.
const DENY_POLICY: &str = "deny-describe-agreement";

/// RULE: a document decides the call only while it is in force, and the decision precedes the
/// store — so adding a deny of the action as its own document refuses the identical read 403
/// whether the agreement it names exists or not, and taking that one document off (leaving
/// the untouched grant) returns both reads to the answers the store gives.
#[tokio::test]
async fn the_document_that_denied_decides_only_while_it_is_in_force() {
    let emulator = Emulator::new();
    // The proposal identifier's modelled pattern is `^(at-|ap-)[A-Za-z0-9]+$`
    // (`CreateAgreementRequest$agreementProposalIdentifier`), so the prefix is followed by
    // alphanumerics alone.
    let agreement = emulator.given_agreement("ap-docsetlifecycle").await;
    let role = "docset-lifecycle";
    let own = format!("{SIGNING_NAME}:DescribeAgreement");
    let session = emulator.assumed_role(role, None).await;
    emulator
        .put_named_role_policy(role, GRANT_POLICY, &allow(json!(own)))
        .await;

    // (case, the agreementId the identical read names, the code the store answers it with
    // once the caller is allowed — the empty string for the report of the agreement itself)
    let reads: Vec<(&str, String, &str)> = vec![
        ("the agreement the store holds", agreement.clone(), ""),
        (
            "an id nothing answers to",
            "no-such-agreement".to_string(),
            NOT_FOUND.0,
        ),
    ];

    for (case, named, code) in &reads {
        let (status, answered) = emulator
            .call(
                &session.access_key_id,
                "DescribeAgreement",
                &json!({"agreementId": named}),
            )
            .await;
        assert_eq!(
            code_of(&answered),
            *code,
            "case {case:?} (the grant alone): expected {code:?}, got {status} with {answered}"
        );
    }

    // The deny arrives as its OWN document beside the grant, under its own policy name.
    emulator
        .put_named_role_policy(role, DENY_POLICY, &deny(json!(own)))
        .await;
    let refusal = format!(
        "User: {} is not authorized to perform: {own} on resource: {ANY_RESOURCE} {}",
        session.arn,
        Verdict::ExplicitDeny.reason(&evaluated_actions("DescribeAgreement"))
    );
    for (case, named, _) in &reads {
        let (status, answered) = emulator
            .call(
                &session.access_key_id,
                "DescribeAgreement",
                &json!({"agreementId": named}),
            )
            .await;
        assert_eq!(
            (status, code_of(&answered)),
            ACCESS_DENIED,
            "case {case:?} (the deny in force): expected the modelled refusal before the \
             store is read, got {status} with {answered}"
        );
        assert_eq!(
            message_of(&answered),
            refusal,
            "case {case:?} (the deny in force): expected IAM's wording, got {answered}"
        );
    }

    // Only the denying document is taken off; the grant was never rewritten.
    emulator.delete_named_role_policy(role, DENY_POLICY).await;
    for (case, named, code) in &reads {
        let (status, answered) = emulator
            .call(
                &session.access_key_id,
                "DescribeAgreement",
                &json!({"agreementId": named}),
            )
            .await;
        assert_eq!(
            code_of(&answered),
            *code,
            "case {case:?} (the deny taken off): expected {code:?}, got {status} with \
             {answered}"
        );
        if !code.is_empty() {
            assert_eq!(
                message_of(&answered),
                NOT_FOUND.1,
                "case {case:?} (the deny taken off): expected the not-found refusal, got \
                 {status} with {answered}"
            );
        }
    }

    // And the agreement the surviving grant reads back is the one the fixture accepted, so
    // the refusals in between reported nothing about it and changed nothing.
    let (status, described) = emulator
        .call(
            &session.access_key_id,
            "DescribeAgreement",
            &json!({"agreementId": agreement}),
        )
        .await;
    assert_eq!(
        (status, described["agreementId"].as_str().unwrap_or_default()),
        (200, agreement.as_str()),
        "case \"the surviving grant reads the agreement back\": expected 200 with the \
         fixture agreement, got {status} with {described}"
    );
}
