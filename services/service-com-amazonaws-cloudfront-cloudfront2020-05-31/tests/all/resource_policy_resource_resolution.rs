// SPDX-License-Identifier: CC0-1.0

//! How CloudFront's resource-policy surface resolves the resource its `ResourceArn` names,
//! end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule under test: each of the three operations of the surface resolves its
//! `ResourceArn` against the STORE, by the ARN a create really minted, so an ARN of the
//! accepted resource type that the store holds no resource under is reported ABSENT — HTTP 404
//! `EntityNotFound` with [`NO_SUCH_RESOURCE_MESSAGE`], the sentence CloudFront answers for
//! that class — and only an ARN that does name a held resource reaches the policy itself.
//!
//! [`crate::resource_policy_absent_resource`] is the table over the FORM an unaccepted
//! `ResourceArn` can take. What this suite adds is the resolution itself, over the two
//! dimensions that table does not vary:
//!
//! * a NEAR MISS of an ARN the store does hold — the same ARN with its resource id folded in
//!   case, lengthened, shortened, re-labelled or its body replaced — names no resource, so it
//!   is the 404 rather than the policy of the resource it resembles: the lookup is an exact
//!   match against the stored ARN, never a prefix or a case-insensitive one;
//! * the DOCUMENT the write carries: the absent answer stands for every document this surface
//!   does attach — the same document is accepted under the ARN of a resource that exists — so
//!   what the 404 reports is the resource rather than anything about the policy.
//!
//! Beside each refusal the suite asserts what it left behind: the policy of the resource a
//! near miss resembles is still readable, in the form the write persisted it, so no refused
//! call wrote, moved or removed anything.
//!
//! The wire harness (the router, the request helpers and the element scanner) is the one
//! [`crate::distribution_lifecycle`] builds, and the surface's request helpers are
//! [`crate::resource_policy_lifecycle`]'s and [`crate::resource_policy_delete`]'s.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    EMPTY_RESOURCE_POLICY_DOCUMENT, NO_SUCH_RESOURCE_MESSAGE, VPC_ORIGIN_ID_PREFIX,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, Answer};
use crate::resource_policy_delete::delete_resource_policy;
use crate::resource_policy_lifecycle::{
    created_vpc_origin_arn, get_resource_policy, granted_principal, message, policy,
    put_resource_policy, reported_document, stored,
};

/// The width of the body a VPC origin id carries after its namespace label, read off the ids
/// this service mints: `vo_` ([`VPC_ORIGIN_ID_PREFIX`]) followed by thirteen characters of the
/// uppercase alphanumeric alphabet every entity token of this service is drawn from.
const ID_BODY_LEN: usize = 13;

/// Ask all three operations of the surface about `arn`, in the order a client that attaches,
/// reads and removes a policy would, each answer beside the operation that gave it.
///
/// The document the write carries NAMES `arn` and grants to an identity that exists, so the
/// only thing left for the surface to refuse is the resource the ARN names.
async fn ask_each_operation(router: &MultiServiceRouter, arn: &str) -> Vec<(&'static str, Answer)> {
    vec![
        (
            "PutResourcePolicy",
            put_resource_policy(router, arn, &policy(arn, "resolution-suite")).await,
        ),
        ("GetResourcePolicy", get_resource_policy(router, arn).await),
        (
            "DeleteResourcePolicy",
            delete_resource_policy(router, arn).await,
        ),
    ]
}

/// Assert that every operation of the surface reports `arn` ABSENT.
async fn assert_absent(router: &MultiServiceRouter, case: &str, arn: &str) {
    for (operation, answer) in ask_each_operation(router, arn).await {
        assert_eq!(
            answer.status, 404,
            "case {case}/{operation}: expected HTTP 404 for {arn:?}, got {} ({})",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "EntityNotFound",
            "case {case}/{operation}: expected EntityNotFound for {arn:?}, got {:?} ({})",
            answer.error_code(),
            answer.raw
        );
        assert_eq!(
            message(&answer),
            NO_SUCH_RESOURCE_MESSAGE,
            "case {case}/{operation}: expected message {NO_SUCH_RESOURCE_MESSAGE:?}, got {:?}",
            message(&answer)
        );
    }
}

/// Assert that the resource `arn` names carries exactly `document`, as the surface renders a
/// document it stored ([`stored`]).
async fn assert_policy_is(router: &MultiServiceRouter, case: &str, arn: &str, document: &str) {
    let read = get_resource_policy(router, arn).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected HTTP 200 reading {arn:?}, got {} ({})",
        read.status, read.raw
    );
    let expected = stored(document);
    assert_eq!(
        reported_document(&read),
        expected.as_str(),
        "case {case}: expected the document {expected:?}, got {:?}",
        reported_document(&read)
    );
}

/// The resource ARNs that differ from `arn` in its resource ID ALONE, each labelled with the
/// change that produced it.
///
/// `arn` is one a create minted, so every row here is an ARN of the accepted resource type —
/// same partition, same account, same region segment, same resource type — whose id no create
/// ever issued. The changes differ along what the resolution could wrongly ignore: the case of
/// the id, its length in either direction, its namespace label and its body.
fn near_misses(arn: &str) -> Vec<(String, String)> {
    let (addressed, id) = arn
        .rsplit_once('/')
        .expect("a minted VPC origin ARN spells its resource as <type>/<id>");
    let mut rows = vec![
        (
            "the held id with a character appended".to_string(),
            format!("{addressed}/{id}A"),
        ),
        (
            "the held id with its last character dropped".to_string(),
            format!("{addressed}/{}", &id[..id.len() - 1]),
        ),
        (
            "the held id's body under another kind's label".to_string(),
            format!(
                "{addressed}/dt_{}",
                id.strip_prefix(VPC_ORIGIN_ID_PREFIX).unwrap_or(id)
            ),
        ),
        (
            "an id of the minted form that was never issued".to_string(),
            format!(
                "{addressed}/{VPC_ORIGIN_ID_PREFIX}{}",
                "0".repeat(ID_BODY_LEN)
            ),
        ),
    ];

    // A minted id's body is drawn from an UPPERCASE alphanumeric alphabet, so folding its case
    // names another id — unless that body happens to carry no letter at all, in which case the
    // fold is the id itself and names the resource that is held, so there is no row for it.
    let folded = id.to_lowercase();
    if folded != id {
        rows.push((
            "the held id folded to lower case".to_string(),
            format!("{addressed}/{folded}"),
        ));
    }
    rows
}

#[tokio::test]
async fn a_near_miss_of_a_held_resource_arn_is_reported_absent() {
    let router = emulator();

    // Several resources of the accepted type, so each absent answer below is given by a store
    // that DOES hold resources of that type — and two of them carry a policy, so a refusal
    // that leaked into the lookup would answer with a document rather than the 404.
    let mut held: Vec<(&str, String, Option<String>)> = Vec::new();
    for (case, name, attach) in [
        ("the first resource created", "vo-resolution-1", true),
        ("a one-character resource name", "7", false),
        ("a resource name carrying separators", "vo.res_2", true),
    ] {
        let arn = created_vpc_origin_arn(&router, name).await;
        let document = attach.then(|| policy(&arn, case));
        if let Some(document) = &document {
            let written = put_resource_policy(&router, &arn, document).await;
            assert_eq!(
                written.status, 200,
                "case {case}: expected HTTP 200 from the write, got {} ({})",
                written.status, written.raw
            );
        }
        held.push((case, arn, document));
    }

    for (case, arn, _) in &held {
        for (change, near) in near_misses(arn) {
            assert_ne!(
                &near, arn,
                "case {case}: {change} must name another resource than {arn:?}"
            );
            assert_absent(&router, &format!("{case}: {change}"), &near).await;
        }
    }

    // Nothing the refusals touched: each held resource still answers with the policy it
    // carried, and the one that carried none still reads as the policy that grants nothing.
    for (case, arn, document) in &held {
        match document {
            Some(document) => assert_policy_is(&router, case, arn, document).await,
            None => {
                let read = get_resource_policy(&router, arn).await;
                assert_eq!(
                    reported_document(&read),
                    EMPTY_RESOURCE_POLICY_DOCUMENT,
                    "case {case}: expected the empty document, got {:?}",
                    reported_document(&read)
                );
            }
        }
    }
}

/// The documents a write carries, each a document this surface ATTACHES to the resource
/// `resource_arn` names: `(case, document)`.
///
/// Every row names that resource, grants an action the surface shares and grants to an
/// identity that exists, so none of them is a document the surface refuses — which is what
/// makes a refused write below a statement about the RESOURCE rather than about the document.
/// The rows differ along the document's size, how many statements it carries, and the form of
/// the principal it grants to.
fn attachable_documents(resource_arn: &str) -> Vec<(&'static str, String)> {
    let one = policy(resource_arn, "resolution-document");
    let principal = granted_principal();
    vec![
        ("a document a caller would write", one),
        (
            "a document granting to every principal",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":\"*\",\"Action\":\"cloudfront:GetVpcOrigin\",\
                 \"Resource\":\"{resource_arn}\"}}]}}"
            ),
        ),
        (
            "a document carrying two statements",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[\
                 {{\"Sid\":\"first\",\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{principal}\"}},\
                 \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{resource_arn}\"}},\
                 {{\"Sid\":\"second\",\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{principal}\"}},\
                 \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{resource_arn}\"}}]}}"
            ),
        ),
        (
            "a document with a statement id of sixty-four characters",
            policy(resource_arn, &"s".repeat(64)),
        ),
    ]
}

#[tokio::test]
async fn the_absent_answer_stands_whatever_document_the_write_carries() {
    let router = emulator();

    // The resource the rows are measured against: a policy the surface attaches to a resource
    // that EXISTS is the control for every document below, so a refusal of the same document
    // under an ARN naming nothing is about the resource alone.
    let held = created_vpc_origin_arn(&router, "vo-resolution-documents").await;

    // Two ARNs of the accepted resource type that name no resource, differing in how they
    // resemble one: a near miss of the held ARN, and an id of the form a create mints that was
    // never issued.
    let absent = [
        (
            "a near miss of the held resource",
            format!("{held}X"),
        ),
        (
            "an id of the minted form never issued",
            format!(
                "{}/{VPC_ORIGIN_ID_PREFIX}{}",
                held.rsplit_once('/')
                    .expect("a minted VPC origin ARN spells its resource as <type>/<id>")
                    .0,
                "9".repeat(ID_BODY_LEN)
            ),
        ),
    ];

    for (case, document) in attachable_documents(&held) {
        // The same document under the held ARN is attached and reads back, so the surface does
        // accept it.
        let written = put_resource_policy(&router, &held, &document).await;
        assert_eq!(
            written.status, 200,
            "case {case}: expected HTTP 200 writing to the held resource, got {} ({})",
            written.status, written.raw
        );
        assert_policy_is(&router, case, &held, &document).await;

        // And under an ARN naming no resource the very same document is the absent answer.
        for (absent_case, arn) in &absent {
            let answer = put_resource_policy(&router, arn, &document.replace(&held, arn)).await;
            assert_eq!(
                answer.status, 404,
                "case {case}/{absent_case}: expected HTTP 404 for {arn:?}, got {} ({})",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "EntityNotFound",
                "case {case}/{absent_case}: expected EntityNotFound for {arn:?}, got {:?} ({})",
                answer.error_code(),
                answer.raw
            );
            assert_eq!(
                message(&answer),
                NO_SUCH_RESOURCE_MESSAGE,
                "case {case}/{absent_case}: expected message {NO_SUCH_RESOURCE_MESSAGE:?}, got {:?}",
                message(&answer)
            );
        }

        // Every refused write left the held resource's policy exactly as it was.
        assert_policy_is(&router, case, &held, &document).await;
    }
}
