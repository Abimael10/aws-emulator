// SPDX-License-Identifier: CC0-1.0

//! `PutResourcePolicy`, end to end over HTTP: attaching a resource-based policy to a
//! knowledge base, what it refuses, and what a read reports afterwards.
//!
//! RULE: `PutResourcePolicy` stores the document the request carried on the knowledge
//! base its `resourceArn` names and answers that knowledge base's own ARN with the
//! revision the write left the policy at, which advances by one per accepted write. It
//! refuses, writing nothing: a document outside the IAM JSON policy language and one
//! whose grants reach a principal nobody named (`ValidationException`, 400); a knowledge
//! base of a kind that carries no resource policy (`ValidationException`, 400); an ARN
//! naming a knowledge base this store does not hold (`ResourceNotFoundException`, 404);
//! and an `expectedRevisionId` that is not the revision the stored policy is at
//! (`ConflictException`, 409).
//!
//! The rows span what the rule generalises over: the form, size and characters of the
//! document (minified, pretty-printed, one statement written as an object rather than a
//! list, multi-statement, Latin-1, a few kilobytes), which clause a refused document
//! breaks, which spelling a wildcard principal takes, whether a condition narrows it,
//! how many writes the policy has taken, every `KnowledgeBaseType` the model declares,
//! and whether the ARN names a knowledge base this store holds.
//!
//! The harness — the router over one shared engine, the knowledge-base fixtures and the
//! read — is `knowledge_base_resource_policy`'s, so the write and the read beside it are
//! asserted against the very same emulator.

use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::MANAGED_KNOWLEDGE_BASE_TYPE;

use crate::knowledge_base_resource_policy::{
    grant, knowledge_base_absent, label, Answer, Emulator, ACCOUNT, KIND_CARRIES_NO_POLICY,
    KNOWLEDGE_BASE_TYPES, REGION,
};

/// The `ValidationException` sentence AWS answers a document whose grants reach a
/// principal nobody named with: entirely fixed text, naming nothing of the request.
const PUBLIC_POLICY: &str = "Resource policies must not grant access to all principals. \
                             Replace any wildcard principal with specific principals, or add \
                             a condition that limits the callers to specific AWS accounts.";

/// The `ConflictException` sentence this service answers when the caller's
/// `expectedRevisionId` is not the revision the stored policy is at: another writer owns
/// the policy, and the operation the caller invoked is the one variable part.
const REVISION_CONFLICT: &str = "Couldn't perform PutResourcePolicy operation. The resource is \
                                 being updated by a concurrent request. Retry your request \
                                 after the other request has completed.";

/// The `ValidationException` sentence a document that is not a policy is refused with,
/// around the clause of the IAM JSON policy language it breaks.
///
/// Source of the clause phrases the rows below spell: the `PolicyDefect` readings of
/// `service_common::iam_policy`, which is the workspace's one grammar of a
/// resource-based policy document.
fn not_a_policy(clause: &str) -> String {
    format!("The resource policy is not a valid policy document: {clause}.")
}

/// `PutResourcePolicy` over the wire, with the ARN percent-encoded as a client that
/// sends a non-greedy `@httpLabel` encodes it.
async fn put_resource_policy(
    emulator: &Emulator,
    resource_arn: &str,
    policy: &str,
    expected_revision_id: Option<&str>,
) -> Answer {
    let mut body: Value = json!({ "policy": policy });
    if let Some(expected) = expected_revision_id {
        body["expectedRevisionId"] = json!(expected);
    }
    emulator
        .call(
            "PUT",
            &format!("/resourcepolicy/{}", label(resource_arn)),
            Some(body),
        )
        .await
}

/// One knowledge base of the kind that carries a policy, reported as its id and ARN.
async fn policy_bearing_knowledge_base(emulator: &Emulator, name: &str) -> (String, String) {
    emulator
        .knowledge_base(name, name, MANAGED_KNOWLEDGE_BASE_TYPE)
        .await
}

/// RULE: an accepted write stores the document as the request carried it, answers the
/// knowledge base's own ARN and the revision the write left the policy at, and that is
/// what a read reports back — over a table of documents differing in form, size, the
/// characters they carry and the grants they make.
#[tokio::test]
async fn the_document_written_is_the_document_the_read_reports() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: the form of the document
    // (minified, pretty-printed, the one statement written as an object rather than a
    // list, multi-statement, no `Version` at all — which the policy language lets a
    // document omit), its size, the characters it carries (the modelled pattern
    // ^[\u0009\u000A\u000D\u0020-\u00FF]+$ admits tabs, newlines and Latin-1 beyond
    // ASCII) and which principals its grants reach (one account, a service, several
    // accounts, and a wildcard a condition narrows to named callers).
    let long_statements: Vec<String> = (0..40)
        .map(|index| {
            format!(
                r#"{{"Sid":"grant{index}","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{:012}:root"}},"Action":["bedrock:Retrieve","bedrock:RetrieveAndGenerate"],"Resource":"*"}}"#,
                index + 1
            )
        })
        .collect();
    let cases: Vec<(&str, String)> = vec![
        (
            "a minified single-statement grant",
            grant("one", "111122223333"),
        ),
        (
            "the same grant with the whitespace a client's pretty-printer leaves",
            "{\n\t\"Version\": \"2012-10-17\",\r\n\t\"Statement\": [\n\t\t{\n\t\t\t\"Effect\": \
             \"Allow\",\n\t\t\t\"Principal\": {\"AWS\": \"arn:aws:iam::111122223333:root\"},\n\
             \t\t\t\"Action\": \"bedrock:Retrieve\",\n\t\t\t\"Resource\": \"*\"\n\t\t}\n\t]\n}"
                .to_string(),
        ),
        (
            "the one statement written as an object rather than a list",
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve","Resource":"*"}}"#
                .to_string(),
        ),
        (
            "a document declaring no Version, which the language lets it omit",
            r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
        ),
        (
            "a multi-statement document whose second statement denies",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{},{}]}}"#,
                r#"{"Sid":"first","Effect":"Allow","Principal":{"AWS":["arn:aws:iam::111122223333:root","arn:aws:iam::444455556666:root"]},"Action":"bedrock:Retrieve","Resource":"*"}"#,
                r#"{"Sid":"second","Effect":"Deny","Principal":"*","Action":"bedrock:RetrieveAndGenerate","Resource":"*"}"#,
            ),
        ),
        (
            "a grant to a service principal",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"bedrock.amazonaws.com"},"Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
        ),
        (
            "a wildcard principal narrowed to one organization",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"bedrock:Retrieve","Resource":"*","Condition":{"StringEquals":{"aws:PrincipalOrgID":"o-abcde12345"}}}]}"#
                .to_string(),
        ),
        (
            "a document carrying a Latin-1 character the pattern admits",
            r#"{"Version":"2012-10-17","Statement":[{"Sid":"café","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
        ),
        (
            "a document of a few kilobytes",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
                long_statements.join(",")
            ),
        ),
    ];

    for (index, (case, policy)) in cases.iter().enumerate() {
        // Each row gets its own knowledge base, so no row can pass on another's policy.
        let (_, knowledge_base_arn) =
            policy_bearing_knowledge_base(&emulator, &format!("policy-write-{index}")).await;

        let written = put_resource_policy(&emulator, &knowledge_base_arn, policy, None).await;
        assert_eq!(
            written.status, 200,
            "case {case}: expected PutResourcePolicy -> 200, got {written:?}"
        );
        assert_eq!(
            written.string("resourceArn"),
            knowledge_base_arn,
            "case {case}: expected the knowledge base's own ARN, got {:?}",
            written.string("resourceArn")
        );
        let revision_id = written.string("revisionId");
        assert!(
            !revision_id.is_empty(),
            "case {case}: expected a revisionId the next write can be compared against, \
             got {revision_id:?}"
        );

        let read = emulator.get_resource_policy(&knowledge_base_arn).await;
        assert_eq!(
            (
                read.status,
                read.string("policy"),
                read.string("resourceArn"),
                read.string("revisionId")
            ),
            (
                200,
                policy.clone(),
                knowledge_base_arn.clone(),
                revision_id.clone()
            ),
            "case {case}: expected the written document back at its own revision, got {read:?}"
        );
    }
}

/// RULE: each accepted write replaces the stored document and leaves the policy at a
/// revision of its own, and it touches no other knowledge base's policy.
#[tokio::test]
async fn each_write_replaces_the_document_at_a_revision_of_its_own() {
    let emulator = Emulator::new();

    let (_, first_arn) = policy_bearing_knowledge_base(&emulator, "policy-write-revisions").await;
    let (_, second_arn) = policy_bearing_knowledge_base(&emulator, "policy-write-untouched").await;

    let untouched = grant("untouched", "444455556666");
    let written = put_resource_policy(&emulator, &second_arn, &untouched, None).await;
    assert_eq!(
        written.status, 200,
        "case the other knowledge base: expected PutResourcePolicy -> 200, got {written:?}"
    );

    // The rows differ in how many writes the policy has taken.
    let mut revisions: Vec<String> = Vec::new();
    for write in 0..4 {
        let document = grant(&format!("write{write}"), "111122223333");
        let answer = put_resource_policy(&emulator, &first_arn, &document, None).await;
        assert_eq!(
            answer.status, 200,
            "case write {write}: expected PutResourcePolicy -> 200, got {answer:?}"
        );
        let revision_id = answer.string("revisionId");
        assert!(
            !revisions.contains(&revision_id),
            "case write {write}: expected a revision unlike every earlier one {revisions:?}, \
             got {revision_id:?}"
        );
        revisions.push(revision_id.clone());

        let read = emulator.get_resource_policy(&first_arn).await;
        assert_eq!(
            (read.status, read.string("policy"), read.string("revisionId")),
            (200, document.clone(), revision_id.clone()),
            "case write {write}: expected the replacing document at its own revision, \
             got {read:?}"
        );
    }

    let other = emulator.get_resource_policy(&second_arn).await;
    assert_eq!(
        (other.status, other.string("policy")),
        (200, untouched),
        "case the other knowledge base after the writes: expected its own policy, got {other:?}"
    );
}

/// RULE: a value that is not a resource-based policy document, and one whose grants
/// reach a principal nobody named, are both bad requests — and a refused write leaves
/// the knowledge base the policy it had.
#[tokio::test]
async fn a_document_that_cannot_be_attached_is_a_bad_request() {
    let emulator = Emulator::new();

    let (_, knowledge_base_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-write-refusals").await;
    // The policy the knowledge base already carries, which every refusal below has to
    // leave exactly as it is.
    let stored = grant("stored", "111122223333");
    let accepted = put_resource_policy(&emulator, &knowledge_base_arn, &stored, None).await;
    assert_eq!(
        accepted.status, 200,
        "case the document to be left alone: expected PutResourcePolicy -> 200, got {accepted:?}"
    );
    let stored_revision = accepted.string("revisionId");

    // The rows differ along what the rule generalises over: which clause of the policy
    // grammar the document breaks, and — for a document that IS a policy — which
    // spelling its wildcard principal takes.
    let cases: Vec<(&str, String, String)> = vec![
        (
            "text that is not JSON at all",
            "this is not a policy".to_string(),
            not_a_policy("the document is not JSON"),
        ),
        (
            "JSON that is not an object",
            r#"["Allow"]"#.to_string(),
            not_a_policy("the document is not a JSON object"),
        ),
        (
            "a policy version the language does not define",
            r#"{"Version":"2029-01-01","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve"}]}"#
                .to_string(),
            not_a_policy("the document declares a policy version that does not exist"),
        ),
        (
            "a document declaring no statement",
            r#"{"Version":"2012-10-17","Statement":[]}"#.to_string(),
            not_a_policy("the document declares no statement"),
        ),
        (
            "a statement declaring no Effect the language defines",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"AllowAndDeny","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve"}]}"#
                .to_string(),
            not_a_policy("a statement declares no Effect of Allow or Deny"),
        ),
        (
            "a statement naming no action",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Resource":"*"}]}"#
                .to_string(),
            not_a_policy("a statement names no Action or NotAction"),
        ),
        (
            "a statement naming no principal, which a resource policy has to",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
            not_a_policy("a statement names no Principal or NotPrincipal"),
        ),
        (
            "a bare wildcard principal",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
            PUBLIC_POLICY.to_string(),
        ),
        (
            "the wildcard under a principal type",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"*"},"Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
            PUBLIC_POLICY.to_string(),
        ),
        (
            "the wildcard among named accounts",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["arn:aws:iam::111122223333:root","*"]},"Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
            PUBLIC_POLICY.to_string(),
        ),
        (
            "a grant to everyone outside a named account",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","NotPrincipal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
            PUBLIC_POLICY.to_string(),
        ),
        (
            "a public statement beside a narrow one",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{},{}]}}"#,
                r#"{"Sid":"narrow","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve","Resource":"*"}"#,
                r#"{"Sid":"wide","Effect":"Allow","Principal":"*","Action":"bedrock:RetrieveAndGenerate","Resource":"*"}"#,
            ),
            PUBLIC_POLICY.to_string(),
        ),
        (
            "a wildcard principal under a condition that narrows nothing",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"bedrock:Retrieve","Resource":"*","Condition":{"StringEquals":{"aws:ResourceAccount":"000000000000"}}}]}"#
                .to_string(),
            PUBLIC_POLICY.to_string(),
        ),
    ];

    for (case, policy, expected) in cases {
        let refused = put_resource_policy(&emulator, &knowledge_base_arn, &policy, None).await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (400, Some("ValidationException")),
            "case {case}: expected 400 ValidationException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            expected,
            "case {case}: expected {expected:?}, got {:?}",
            refused.message()
        );

        let read = emulator.get_resource_policy(&knowledge_base_arn).await;
        assert_eq!(
            (read.status, read.string("policy"), read.string("revisionId")),
            (200, stored.clone(), stored_revision.clone()),
            "case {case}: expected the refused write to leave the stored policy alone, \
             got {read:?}"
        );
    }
}

/// RULE: an `expectedRevisionId` the stored policy is not at refuses the write; the
/// revision the policy IS at writes, and a request carrying no precondition writes
/// unconditionally.
#[tokio::test]
async fn the_expected_revision_precondition_decides_the_write() {
    let emulator = Emulator::new();

    let (_, knowledge_base_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-write-precondition").await;

    // A precondition on a policy that is not there cannot hold: the knowledge base
    // carries none yet, so no value names its revision.
    let on_nothing = put_resource_policy(
        &emulator,
        &knowledge_base_arn,
        &grant("first", "111122223333"),
        Some("1"),
    )
    .await;
    assert_eq!(
        (on_nothing.status, on_nothing.error_type.as_deref()),
        (409, Some("ConflictException")),
        "case a precondition on a knowledge base carrying no policy: expected 409 \
         ConflictException, got {on_nothing:?}"
    );
    assert_eq!(
        on_nothing.message(),
        REVISION_CONFLICT,
        "case a precondition on a knowledge base carrying no policy: expected \
         {REVISION_CONFLICT:?}, got {:?}",
        on_nothing.message()
    );

    // With no precondition the first write is taken.
    let first = grant("unconditional", "111122223333");
    let accepted = put_resource_policy(&emulator, &knowledge_base_arn, &first, None).await;
    assert_eq!(
        accepted.status, 200,
        "case no precondition at all: expected PutResourcePolicy -> 200, got {accepted:?}"
    );
    let mut revision = accepted.string("revisionId");
    let mut stored = first;

    // The rows differ along what the rule generalises over: whether the precondition
    // names the revision the policy is at, a revision an earlier write left behind, a
    // value no write ever produced, or nothing at all.
    let stale = revision.clone();
    for (case, expected_revision_id, accepted) in [
        ("the revision the policy is at", Some(revision.clone()), true),
        ("a revision an earlier write left behind", Some(stale.clone()), false),
        ("a value no write ever produced", Some("not-a-revision".to_string()), false),
        ("no precondition at all", None, true),
    ] {
        let document = grant(&format!("after-{}", case.replace(' ', "-")), "444455556666");
        let answer = put_resource_policy(
            &emulator,
            &knowledge_base_arn,
            &document,
            expected_revision_id.as_deref(),
        )
        .await;
        if accepted {
            assert_eq!(
                answer.status, 200,
                "case {case}: expected PutResourcePolicy -> 200, got {answer:?}"
            );
            revision = answer.string("revisionId");
            stored = document;
        } else {
            assert_eq!(
                (answer.status, answer.error_type.as_deref(), answer.message()),
                (409, Some("ConflictException"), REVISION_CONFLICT.to_string()),
                "case {case}: expected 409 ConflictException, got {answer:?}"
            );
        }
        let read = emulator.get_resource_policy(&knowledge_base_arn).await;
        assert_eq!(
            (read.status, read.string("policy"), read.string("revisionId")),
            (200, stored.clone(), revision.clone()),
            "case {case}: expected the store to hold the document the accepted writes left, \
             got {read:?}"
        );
    }
}

/// RULE: a knowledge base whose kind carries no resource policy cannot have one
/// attached either, over every `KnowledgeBaseType` the model declares but the
/// service-managed one.
#[tokio::test]
async fn a_knowledge_base_of_a_kind_that_carries_no_policy_cannot_be_written() {
    let emulator = Emulator::new();

    for knowledge_base_type in KNOWLEDGE_BASE_TYPES {
        if knowledge_base_type == MANAGED_KNOWLEDGE_BASE_TYPE {
            continue;
        }
        let case = knowledge_base_type;
        let (_, knowledge_base_arn) = emulator
            .knowledge_base(
                case,
                &format!("policy-write-kind-{}", knowledge_base_type.to_lowercase()),
                knowledge_base_type,
            )
            .await;

        let refused = put_resource_policy(
            &emulator,
            &knowledge_base_arn,
            &grant("kind", "111122223333"),
            None,
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (400, Some("ValidationException")),
            "case {case}: expected 400 ValidationException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            KIND_CARRIES_NO_POLICY,
            "case {case}: expected {KIND_CARRIES_NO_POLICY:?}, got {:?}",
            refused.message()
        );
    }
}

/// RULE: an ARN naming a knowledge base this store does not hold is a missing resource,
/// whatever partition, region or account the ARN carries and however the knowledge base
/// came to be gone.
#[tokio::test]
async fn an_absent_knowledge_base_has_no_policy_to_attach() {
    let emulator = Emulator::new();

    let (deleted_id, deleted_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-write-deleted").await;
    emulator
        .delete_knowledge_base("a knowledge base a delete was accepted for", &deleted_id)
        .await;

    // The rows differ along what the rule generalises over: the partition, the region
    // and the account the ARN carries — none of which is what selects the record — and
    // whether the knowledge base was never stored or is on its way out.
    let cases: Vec<(&str, String, String)> = vec![
        (
            "a knowledge base id nothing holds",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:knowledge-base/ZZZZZZZZZZ"),
            knowledge_base_absent("ZZZZZZZZZZ"),
        ),
        (
            "an id nothing holds, in another partition, region and account",
            "arn:aws-us-gov:bedrock:us-gov-west-1:123456789012:knowledge-base/0a1b2c3d4e"
                .to_string(),
            knowledge_base_absent("0a1b2c3d4e"),
        ),
        (
            "a knowledge base a delete was accepted for",
            deleted_arn,
            knowledge_base_absent(&deleted_id),
        ),
    ];

    for (case, resource_arn, expected) in cases {
        let refused = put_resource_policy(
            &emulator,
            &resource_arn,
            &grant("absent", "111122223333"),
            None,
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case}: expected 404 ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            expected,
            "case {case}: expected {expected:?}, got {:?}",
            refused.message()
        );
    }
}
