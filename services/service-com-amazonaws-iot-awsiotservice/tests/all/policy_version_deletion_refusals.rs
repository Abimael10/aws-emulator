// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of what `DeletePolicyVersion` REFUSES, and of the sentence
//! each refusal states.
//!
//! The wire harness is the sibling suite's (`crate::policy_version_deletion`), so every
//! case here drives the same real path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — and nothing of it is written twice.
//!
//! The rules under test:
//!
//! * each class this delete refuses with states its OWN sentence: a `policyVersionId` the
//!   policy does not hold is `ResourceNotFoundException` (404) whichever shape the id
//!   takes, a `policyName` the account does not hold is that same class stated after the
//!   POLICY, and the version in effect is `DeleteConflictException` (409);
//! * a refused delete WRITES NOTHING: the versions the policy holds and the generation its
//!   content is under are what they were;
//! * "You cannot delete the default version of a policy using this action" is a rule about
//!   the policy's POINTER, so which version is held back moves when
//!   `SetDefaultPolicyVersion` moves it, and a delete that IS served moves both the
//!   versions held and the generation.

use crate::policy_version_deletion::{Emulator, Reply};

/// `PATCH /policies/{policyName}/version/{policyVersionId}` — `SetDefaultPolicyVersion`,
/// which moves the pointer a delete is held back by.
async fn set_default(emulator: &Emulator, policy_name: &str, version_id: &str) -> Reply {
    emulator
        .dispatch(
            "PATCH",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
}

/// `GET /policies/{policyName}` — `GetPolicy`, read here for the generation token the
/// policy's content is under.
async fn get_policy(emulator: &Emulator, policy_name: &str) -> Reply {
    emulator
        .dispatch("GET", &format!("/policies/{policy_name}"), None)
        .await
}

/// One string member of an answer.
fn string_member(reply: &Reply, member: &str) -> String {
    reply.json()[member]
        .as_str()
        .unwrap_or_else(|| panic!("no {member} in {}", reply.text()))
        .to_string()
}

/// The diagnostic a refusal states.
fn error_message(reply: &Reply) -> String {
    string_member(reply, "message")
}

/// What the policy holds right now: the version ids it reports, and the generation token
/// its content is under — the pair every assertion below reads a write, or the absence of
/// one, off.
async fn snapshot(emulator: &Emulator, policy_name: &str) -> (Vec<String>, String) {
    let listed = emulator.list_versions(policy_name).await;
    assert_eq!(
        listed.status,
        200,
        "the listing of {policy_name} must be readable, got {} body {}",
        listed.status,
        listed.text()
    );
    let policy = get_policy(emulator, policy_name).await;
    assert_eq!(
        policy.status,
        200,
        "the policy {policy_name} must be readable, got {} body {}",
        policy.status,
        policy.text()
    );
    (listed.version_ids(), string_member(&policy, "generationId"))
}

/// RULE: each class this delete refuses with states its own sentence, and the refusal
/// writes nothing a later reader can observe.
///
/// The ids the rows span are the shapes a `policyVersionId` the policy does not hold can
/// take under the member's modelled `^[0-9]+$` pattern
/// (`smithy_query(query="operation",operation="DeletePolicyVersion")`): one past the
/// highest held, zero, and one past the range a 32-bit counter holds — none of which the
/// policy holds, so one sentence covers them all. The sentences are written out here
/// rather than taken from the crate, so a change of wording fails this case.
#[tokio::test]
async fn every_refusal_states_its_own_sentence_and_writes_nothing() {
    let emulator = Emulator::new();
    let case = "a policy holding three versions, the first in effect";
    emulator.create_policy(case, "held").await;
    let second = emulator.create_version(case, "held", 2, false).await;
    let third = emulator.create_version(case, "held", 3, false).await;
    assert_eq!(
        (second.as_str(), third.as_str()),
        ("2", "3"),
        "case {case}: expected the added versions to be 2 and 3, got {second} and {third}"
    );

    // (case, the policy named, the version named, the status, the class, the sentence).
    let rows = [
        (
            "a policy the account does not hold",
            "never-created",
            "1",
            404,
            "ResourceNotFoundException",
            "Policy never-created does not exist",
        ),
        (
            "a version one past the highest the policy holds",
            "held",
            "4",
            404,
            "ResourceNotFoundException",
            "Policy version does not exist (name=held, versionId=4)",
        ),
        (
            "the zero version id",
            "held",
            "0",
            404,
            "ResourceNotFoundException",
            "Policy version does not exist (name=held, versionId=0)",
        ),
        (
            "a version id past the 32-bit range",
            "held",
            "4294967296",
            404,
            "ResourceNotFoundException",
            "Policy version does not exist (name=held, versionId=4294967296)",
        ),
        (
            "the version in effect",
            "held",
            "1",
            409,
            "DeleteConflictException",
            "Cannot delete a default policy version (name=held, versionId=1)",
        ),
    ];

    let before = snapshot(&emulator, "held").await;
    for (label, policy_name, version_id, status, code, message) in rows {
        let refused = emulator.delete_version(policy_name, version_id).await;
        assert_eq!(
            refused.status, status,
            "case {label}: expected HTTP {status}, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            code,
            "case {label}: expected {code}, got {}",
            refused.text()
        );
        assert_eq!(
            error_message(&refused),
            message,
            "case {label}: expected the sentence {message:?}, got {:?}",
            error_message(&refused)
        );

        // Nothing was written: the versions held and the content generation stand.
        let after = snapshot(&emulator, "held").await;
        assert_eq!(
            after, before,
            "case {label}: a refused delete must leave the policy as it was, expected \
             {before:?}, got {after:?}"
        );
    }
}

/// RULE: the delete that is held back is the one the policy's POINTER names, so moving the
/// pointer moves which version is deletable — and a served delete moves both the versions
/// the policy holds and the generation its content is under.
#[tokio::test]
async fn the_held_back_version_follows_the_pointer() {
    let emulator = Emulator::new();
    let case = "a pointer walked over every version held";
    emulator.create_policy(case, "pointed").await;
    let versions = [
        "1".to_string(),
        emulator.create_version(case, "pointed", 2, false).await,
        emulator.create_version(case, "pointed", 3, false).await,
    ];

    for operative in &versions {
        assert_eq!(
            set_default(&emulator, "pointed", operative).await.status,
            200,
            "case {case}: version {operative} must become the operative one"
        );
        let (held, generation) = snapshot(&emulator, "pointed").await;
        assert!(
            held.contains(operative),
            "case {case}: expected version {operative} to still be held, got {held:?}"
        );

        // The version in effect is the one version this delete refuses.
        let refused = emulator.delete_version("pointed", operative).await;
        assert_eq!(
            refused.status,
            409,
            "case {case}: expected 409 for the operative version {operative}, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            snapshot(&emulator, "pointed").await,
            (held.clone(), generation.clone()),
            "case {case}: the refused delete of version {operative} must write nothing"
        );
    }

    // With the pointer left on the last version it named, the other two are served, and
    // each served delete moves what the policy holds AND its generation. The listing
    // reports the versions held newest-first, which is what the expected set starts as.
    let operative = versions[2].clone();
    let mut expected: Vec<String> = versions.iter().rev().cloned().collect();
    for version_id in versions.iter().filter(|held| **held != operative) {
        let (_, generation_before) = snapshot(&emulator, "pointed").await;
        let deleted = emulator.delete_version("pointed", version_id).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case}: expected version {version_id} to be deletable, got {} body {}",
            deleted.status,
            deleted.text()
        );
        expected.retain(|held| held != version_id);
        let (held, generation) = snapshot(&emulator, "pointed").await;
        assert_eq!(
            held, expected,
            "case {case}: after deleting version {version_id} expected {expected:?}, got {held:?}"
        );
        assert_ne!(
            generation, generation_before,
            "case {case}: deleting version {version_id} changes what the policy holds, so \
             its generation must move, got {generation}"
        );
    }
}
