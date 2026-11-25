// SPDX-License-Identifier: CC0-1.0

//! `DeleteResourcePolicy`, end to end over HTTP: removing the resource-based policy a
//! knowledge base carries, what it refuses, and what the read beside it reports
//! afterwards.
//!
//! RULE: `DeleteResourcePolicy` takes the policy document off the knowledge base its
//! `resourceArn` names and answers that knowledge base's own ARN with the `revisionId`
//! the removal left the policy at — a removal is a write of the policy, so it advances
//! that revision exactly as an attach does and no two writes of one knowledge base's
//! policy ever answer the same value. After it the knowledge base carries no policy,
//! while the knowledge base itself is untouched. It refuses, removing nothing: a
//! knowledge base of a kind that carries no resource policy (`ValidationException`,
//! 400); an ARN naming a knowledge base this store does not hold
//! (`ResourceNotFoundException`, 404); a knowledge base carrying no policy, which has
//! none to remove (`ResourceNotFoundException`, 404); and an `expectedRevisionId` that
//! is not the revision the stored policy is at (`ConflictException`, 409).
//!
//! The rows span what the rule generalises over: the form, size and characters of the
//! document the removal takes off, how many writes the policy has taken before it, which
//! precondition the request carries, every `KnowledgeBaseType` the model declares, and
//! whether the ARN names a knowledge base this store holds.
//!
//! The harness — the router over one shared engine, the knowledge-base fixtures and the
//! read — is `knowledge_base_resource_policy`'s, so the delete, the write and the read
//! are asserted against the very same emulator.

use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::MANAGED_KNOWLEDGE_BASE_TYPE;

use crate::knowledge_base_resource_policy::{
    grant, knowledge_base_absent, label, Answer, Emulator, ACCOUNT, KIND_CARRIES_NO_POLICY,
    KNOWLEDGE_BASE_TYPES, REGION,
};

/// The `ResourceNotFoundException` sentence this service answers when the knowledge base
/// the ARN names is there but carries no policy: the resource that could not be
/// retrieved is the policy, and the operation the caller invoked is the variable part.
const POLICY_ABSENT: &str = "Couldn't perform DeleteResourcePolicy operation. Failed to retrieve \
                             resource type ResourcePolicy because it doesn't exist. Check the \
                             resource ID and retry your request.";

/// The `ConflictException` sentence this service answers when the caller's
/// `expectedRevisionId` is not the revision the stored policy is at: another writer owns
/// the policy, and the operation the caller invoked is the one variable part.
const REVISION_CONFLICT: &str = "Couldn't perform DeleteResourcePolicy operation. The resource \
                                 is being updated by a concurrent request. Retry your request \
                                 after the other request has completed.";

/// `DeleteResourcePolicy` over the wire, with the ARN percent-encoded as a client that
/// sends a non-greedy `@httpLabel` encodes it and `expectedRevisionId` carried where the
/// model binds it (`@httpQuery`).
async fn delete_resource_policy(
    emulator: &Emulator,
    resource_arn: &str,
    expected_revision_id: Option<&str>,
) -> Answer {
    let mut uri = format!("/resourcepolicy/{}", label(resource_arn));
    if let Some(expected) = expected_revision_id {
        uri.push_str(&format!("?expectedRevisionId={}", label(expected)));
    }
    emulator.call("DELETE", &uri, None).await
}

/// `PutResourcePolicy` over the wire, reported as the `revisionId` the write left the
/// policy at.
async fn attach(emulator: &Emulator, case: &str, resource_arn: &str, policy: &str) -> String {
    let written = emulator
        .call(
            "PUT",
            &format!("/resourcepolicy/{}", label(resource_arn)),
            Some(json!({ "policy": policy })),
        )
        .await;
    assert_eq!(
        written.status, 200,
        "case {case}: expected PutResourcePolicy -> 200, got {written:?}"
    );
    written.string("revisionId")
}

/// One knowledge base of the kind that carries a policy, reported as its id and ARN.
async fn policy_bearing_knowledge_base(emulator: &Emulator, name: &str) -> (String, String) {
    emulator
        .knowledge_base(name, name, MANAGED_KNOWLEDGE_BASE_TYPE)
        .await
}

/// `GetKnowledgeBase`, so a test can assert the removal left the knowledge base itself
/// exactly where it was.
async fn get_knowledge_base(emulator: &Emulator, knowledge_base_id: &str) -> Answer {
    emulator
        .call("GET", &format!("/knowledgebases/{knowledge_base_id}"), None)
        .await
}

/// RULE: an accepted removal takes the document off, answers the knowledge base's own
/// ARN with a revision of its own, and leaves nothing for the read to report — over a
/// table of documents differing in form, size, the characters they carry and how many
/// writes the policy had taken before the removal.
#[tokio::test]
async fn the_removal_takes_the_document_off_the_knowledge_base() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: the form of the document the
    // removal takes off (minified, pretty-printed, multi-statement, one carrying a
    // Latin-1 character the modelled pattern admits, a long one), and how many accepted
    // writes the policy had taken before the removal.
    let long_statements: Vec<String> = (0..40)
        .map(|index| {
            format!(
                r#"{{"Sid":"grant{index}","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{:012}:root"}},"Action":["bedrock:Retrieve"],"Resource":"*"}}"#,
                index + 1
            )
        })
        .collect();
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "one minified grant",
            vec![grant("single", "111122223333")],
        ),
        (
            "a document carrying the whitespace a client's pretty-printer leaves",
            vec!["{\n\t\"Version\": \"2012-10-17\",\r\n\t\"Statement\": [\n\t\t{\n\t\t\t\
                  \"Effect\": \"Allow\",\n\t\t\t\"Principal\": {\"AWS\": \
                  \"arn:aws:iam::111122223333:root\"},\n\t\t\t\"Action\": \
                  \"bedrock:Retrieve\",\n\t\t\t\"Resource\": \"*\"\n\t\t}\n\t]\n}"
                .to_string()],
        ),
        (
            "a document carrying a Latin-1 character the pattern admits",
            vec![
                r#"{"Version":"2012-10-17","Statement":[{"Sid":"café","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve","Resource":"*"}]}"#
                    .to_string(),
            ],
        ),
        (
            "a document of a few kilobytes",
            vec![format!(
                r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
                long_statements.join(",")
            )],
        ),
        (
            "the third document a sequence of writes left attached",
            vec![
                grant("first", "111122223333"),
                grant("second", "444455556666"),
                grant("third", "777788889999"),
            ],
        ),
    ];

    for (index, (case, documents)) in cases.iter().enumerate() {
        // Each row gets its own knowledge base, so no row can pass on another's policy.
        let name = format!("policy-delete-{index}");
        let (knowledge_base_id, knowledge_base_arn) =
            policy_bearing_knowledge_base(&emulator, &name).await;

        let mut revisions: Vec<String> = Vec::new();
        for document in documents {
            revisions.push(attach(&emulator, case, &knowledge_base_arn, document).await);
        }

        let removed = delete_resource_policy(&emulator, &knowledge_base_arn, None).await;
        assert_eq!(
            removed.status, 200,
            "case {case}: expected DeleteResourcePolicy -> 200, got {removed:?}"
        );
        assert_eq!(
            removed.string("resourceArn"),
            knowledge_base_arn,
            "case {case}: expected the knowledge base's own ARN, got {:?}",
            removed.string("resourceArn")
        );
        let removal_revision = removed.string("revisionId");
        assert!(
            !removal_revision.is_empty() && !revisions.contains(&removal_revision),
            "case {case}: expected the removal's revision to be its own, unlike every \
             earlier one {revisions:?}, got {removal_revision:?}"
        );

        // Nothing is left for the read: the policy is the missing resource it has become.
        let read = emulator.get_resource_policy(&knowledge_base_arn).await;
        assert_eq!(
            (read.status, read.error_type.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case}: expected GetResourcePolicy after the removal -> 404 \
             ResourceNotFoundException, got {read:?}"
        );

        // The knowledge base itself survives the removal of its policy: what the delete
        // takes off is the policy, not the resource it hung on.
        let after = get_knowledge_base(&emulator, &knowledge_base_id).await;
        let knowledge_base = after.member("knowledgeBase").clone();
        assert_eq!(
            (
                after.status,
                knowledge_base["knowledgeBaseId"].as_str(),
                knowledge_base["knowledgeBaseArn"].as_str(),
                knowledge_base["name"].as_str()
            ),
            (
                200,
                Some(knowledge_base_id.as_str()),
                Some(knowledge_base_arn.as_str()),
                Some(name.as_str())
            ),
            "case {case}: expected the knowledge base itself still stored, got {after:?}"
        );
    }
}

/// RULE: every write of one knowledge base's policy — each attach and each removal —
/// leaves it at a revision no other write of it answered, so a precondition compared by
/// equality cannot be satisfied by a stale value.
#[tokio::test]
async fn no_two_writes_of_one_policy_answer_the_same_revision() {
    let emulator = Emulator::new();

    let (_, knowledge_base_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-delete-revisions").await;

    // The rows differ along what the rule generalises over: whether the write is an
    // attach or a removal, and how many writes came before it.
    let mut revisions: Vec<String> = Vec::new();
    for round in 0..4 {
        let case = format!("round {round}");
        let attached = attach(
            &emulator,
            &case,
            &knowledge_base_arn,
            &grant(&format!("round{round}"), "111122223333"),
        )
        .await;
        assert!(
            !revisions.contains(&attached),
            "case {case} attach: expected a revision unlike every earlier one \
             {revisions:?}, got {attached:?}"
        );
        revisions.push(attached);

        let removed = delete_resource_policy(&emulator, &knowledge_base_arn, None).await;
        assert_eq!(
            removed.status, 200,
            "case {case} removal: expected DeleteResourcePolicy -> 200, got {removed:?}"
        );
        let removal_revision = removed.string("revisionId");
        assert!(
            !revisions.contains(&removal_revision),
            "case {case} removal: expected a revision unlike every earlier one \
             {revisions:?}, got {removal_revision:?}"
        );
        revisions.push(removal_revision);
    }
}

/// RULE: a knowledge base carrying no policy has none to remove, however it came to
/// carry none, and the removal touches no other knowledge base's policy.
#[tokio::test]
async fn a_knowledge_base_carrying_no_policy_has_none_to_remove() {
    let emulator = Emulator::new();

    let (_, never_written_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-delete-never-written").await;
    let (_, removed_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-delete-already-removed").await;
    attach(
        &emulator,
        "a knowledge base whose policy a removal already took off",
        &removed_arn,
        &grant("removed", "111122223333"),
    )
    .await;
    let first = delete_resource_policy(&emulator, &removed_arn, None).await;
    assert_eq!(
        first.status, 200,
        "case the first removal: expected DeleteResourcePolicy -> 200, got {first:?}"
    );

    // One knowledge base keeps its policy throughout, so no refusal above may reach it.
    let (_, untouched_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-delete-untouched").await;
    let untouched = grant("untouched", "444455556666");
    let untouched_revision = attach(
        &emulator,
        "the knowledge base that keeps its policy",
        &untouched_arn,
        &untouched,
    )
    .await;

    // The rows differ along what the rule generalises over: whether no write ever
    // attached a policy, or a removal took the document off again.
    for (case, resource_arn) in [
        ("a knowledge base no write ever attached a policy to", never_written_arn),
        ("a knowledge base whose policy a removal already took off", removed_arn),
    ] {
        let refused = delete_resource_policy(&emulator, &resource_arn, None).await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case}: expected 404 ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            POLICY_ABSENT,
            "case {case}: expected {POLICY_ABSENT:?}, got {:?}",
            refused.message()
        );
    }

    let read = emulator.get_resource_policy(&untouched_arn).await;
    assert_eq!(
        (read.status, read.string("policy"), read.string("revisionId")),
        (200, untouched, untouched_revision),
        "case the knowledge base that keeps its policy: expected its own policy, got {read:?}"
    );
}

/// RULE: an `expectedRevisionId` the stored policy is not at refuses the removal and
/// leaves the document attached; the revision the policy IS at removes it, and a request
/// carrying no precondition removes unconditionally.
#[tokio::test]
async fn the_expected_revision_precondition_decides_the_removal() {
    let emulator = Emulator::new();

    let (_, knowledge_base_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-delete-precondition").await;

    // The rows differ along what the rule generalises over: whether the precondition
    // names the revision the policy is at, a revision an earlier write left behind, a
    // value no write ever produced, or nothing at all.
    for (case, precondition, removes) in [
        ("a revision an earlier write left behind", Some("stale"), false),
        ("a value no write ever produced", Some("not-a-revision"), false),
        ("the revision the policy is at", Some("current"), true),
        ("no precondition at all", None, true),
    ] {
        let document = grant(&case.replace(' ', "-"), "111122223333");
        let stale = attach(&emulator, case, &knowledge_base_arn, &document).await;
        let current = attach(&emulator, case, &knowledge_base_arn, &document).await;
        let expected_revision_id = match precondition {
            Some("stale") => Some(stale.clone()),
            Some("current") => Some(current.clone()),
            Some(literal) => Some(literal.to_string()),
            None => None,
        };

        let answer =
            delete_resource_policy(&emulator, &knowledge_base_arn, expected_revision_id.as_deref())
                .await;
        if removes {
            assert_eq!(
                answer.status, 200,
                "case {case}: expected DeleteResourcePolicy -> 200, got {answer:?}"
            );
            let read = emulator.get_resource_policy(&knowledge_base_arn).await;
            assert_eq!(
                (read.status, read.error_type.as_deref()),
                (404, Some("ResourceNotFoundException")),
                "case {case}: expected the policy gone after the removal, got {read:?}"
            );
        } else {
            assert_eq!(
                (answer.status, answer.error_type.as_deref(), answer.message()),
                (409, Some("ConflictException"), REVISION_CONFLICT.to_string()),
                "case {case}: expected 409 ConflictException, got {answer:?}"
            );
            let read = emulator.get_resource_policy(&knowledge_base_arn).await;
            assert_eq!(
                (read.status, read.string("policy"), read.string("revisionId")),
                (200, document.clone(), current.clone()),
                "case {case}: expected the refused removal to leave the stored policy \
                 alone, got {read:?}"
            );
            // The policy survives the refusal, so the row that follows starts from a
            // knowledge base carrying one: take it off unconditionally.
            let cleared = delete_resource_policy(&emulator, &knowledge_base_arn, None).await;
            assert_eq!(
                cleared.status, 200,
                "case {case}: expected the unconditional removal -> 200, got {cleared:?}"
            );
        }
    }
}

/// RULE: a knowledge base whose kind carries no resource policy has none to remove
/// either, over every `KnowledgeBaseType` the model declares but the service-managed one.
#[tokio::test]
async fn a_knowledge_base_of_a_kind_that_carries_no_policy_cannot_be_removed() {
    let emulator = Emulator::new();

    for knowledge_base_type in KNOWLEDGE_BASE_TYPES {
        if knowledge_base_type == MANAGED_KNOWLEDGE_BASE_TYPE {
            continue;
        }
        let case = knowledge_base_type;
        let (_, knowledge_base_arn) = emulator
            .knowledge_base(
                case,
                &format!("policy-delete-kind-{}", knowledge_base_type.to_lowercase()),
                knowledge_base_type,
            )
            .await;

        // The rows differ in whether the request carries a precondition as well: the
        // kind is read before any precondition is.
        for (row, precondition) in [("no precondition", None), ("a precondition", Some("1"))] {
            let refused =
                delete_resource_policy(&emulator, &knowledge_base_arn, precondition).await;
            assert_eq!(
                (refused.status, refused.error_type.as_deref()),
                (400, Some("ValidationException")),
                "case {case} with {row}: expected 400 ValidationException, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                KIND_CARRIES_NO_POLICY,
                "case {case} with {row}: expected {KIND_CARRIES_NO_POLICY:?}, got {:?}",
                refused.message()
            );
        }
    }
}

/// RULE: an ARN naming a knowledge base this store does not hold is a missing resource,
/// whatever partition, region or account the ARN carries and however the knowledge base
/// came to be gone.
#[tokio::test]
async fn an_absent_knowledge_base_has_no_policy_to_remove() {
    let emulator = Emulator::new();

    let (deleted_id, deleted_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-delete-deleted").await;
    attach(
        &emulator,
        "a knowledge base a delete was accepted for",
        &deleted_arn,
        &grant("deleted", "111122223333"),
    )
    .await;
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
            "a knowledge base a delete was accepted for, policy and all",
            deleted_arn,
            knowledge_base_absent(&deleted_id),
        ),
    ];

    for (case, resource_arn, expected) in cases {
        let refused = delete_resource_policy(&emulator, &resource_arn, None).await;
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

/// RULE: the removal is about the one knowledge base its `resourceArn` names, so every
/// other knowledge base keeps the policy it carries.
#[tokio::test]
async fn the_removal_reaches_only_the_knowledge_base_the_arn_names() {
    let emulator = Emulator::new();

    // The rows differ in how many knowledge bases carry a policy beside the one the
    // removal names, and which of them it names.
    let mut carried: Vec<(String, String, String)> = Vec::new();
    for index in 0..4 {
        let (_, knowledge_base_arn) =
            policy_bearing_knowledge_base(&emulator, &format!("policy-delete-one-of-{index}")).await;
        let document = grant(&format!("one-of-{index}"), "111122223333");
        let revision = attach(
            &emulator,
            &format!("knowledge base {index}"),
            &knowledge_base_arn,
            &document,
        )
        .await;
        carried.push((knowledge_base_arn, document, revision));
    }

    for (index, (knowledge_base_arn, _, _)) in carried.clone().iter().enumerate() {
        let case = format!("removing knowledge base {index}'s policy");
        let removed = delete_resource_policy(&emulator, knowledge_base_arn, None).await;
        assert_eq!(
            removed.status, 200,
            "case {case}: expected DeleteResourcePolicy -> 200, got {removed:?}"
        );

        for (other, (other_arn, document, revision)) in carried.iter().enumerate() {
            let read = emulator.get_resource_policy(other_arn).await;
            if other <= index {
                assert_eq!(
                    (read.status, read.error_type.as_deref()),
                    (404, Some("ResourceNotFoundException")),
                    "case {case}: expected knowledge base {other} to carry no policy, \
                     got {read:?}"
                );
            } else {
                assert_eq!(
                    (read.status, read.string("policy"), read.string("revisionId")),
                    (200, document.clone(), revision.clone()),
                    "case {case}: expected knowledge base {other} to keep its own policy, \
                     got {read:?}"
                );
            }
        }
    }
}

/// The modelled `DeleteResourcePolicyResponse` members, so the answer carries what the
/// model declares and nothing else.
///
/// Source: `smithy_query(query="operation",operation="DeleteResourcePolicy")` — output
/// `resourceArn` (required) and `revisionId`.
#[tokio::test]
async fn the_answer_carries_the_modelled_members() {
    let emulator = Emulator::new();

    let (_, knowledge_base_arn) =
        policy_bearing_knowledge_base(&emulator, "policy-delete-members").await;
    attach(
        &emulator,
        "the modelled members",
        &knowledge_base_arn,
        &grant("members", "111122223333"),
    )
    .await;

    let removed = delete_resource_policy(&emulator, &knowledge_base_arn, None).await;
    assert_eq!(
        removed.status, 200,
        "case the modelled members: expected DeleteResourcePolicy -> 200, got {removed:?}"
    );
    for member in ["resourceArn", "revisionId"] {
        assert!(
            matches!(removed.member(member), Value::String(_)),
            "case {member}: expected a string member in the answer, got {:?}",
            removed.member(member)
        );
    }
}
