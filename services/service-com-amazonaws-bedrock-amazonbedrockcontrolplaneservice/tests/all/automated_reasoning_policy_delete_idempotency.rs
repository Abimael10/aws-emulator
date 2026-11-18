// SPDX-License-Identifier: CC0-1.0

//! `DeleteAutomatedReasoningPolicy` resolves the ARN it is addressed by against the policies
//! this account HOLDS, and the artifact gate an unforced delete runs is about what the policy
//! STILL holds — both asserted end-to-end over the same `MultiServiceRouter` the emulator
//! boots.
//!
//! The rules under test, each asserted over a SET of inputs rather than over one recorded
//! request:
//!
//! 1. **Repeating the delete of a revision this account held is refused**: the ARN no longer
//!    resolves once the delete has taken the revision away, so a second, third and fourth
//!    delete of the same `AutomatedReasoningPolicyArn` answers `404`
//!    `ResourceNotFoundException` in the sentence the writes of the policy word that class
//!    with — over every form of revision a delete can take away (a policy holding nothing, a
//!    policy whose artifacts went with it, one numbered version of several, and a version the
//!    removal of its policy took), and over every spelling of `force`, which decides nothing
//!    once there is nothing left to resolve. RECORDED behaviour: the operation is modelled
//!    `@idempotent` with success code `202` and documented "This operation is idempotent. If
//!    you delete a policy more than once, each call succeeds"
//!    (`smithy_query(query="operation",operation="DeleteAutomatedReasoningPolicy")`), and the
//!    recording of a second delete of a deleted policy ARN shows that refusal instead — the
//!    trait is safe-retry semantics on the wire, not a no-op for a resource that is gone.
//! 2. **An ARN that never named a revision of this account is refused the same way**: `404
//!    ResourceNotFoundException` stands for a policy id nothing minted, for a live policy
//!    addressed in another region, and for a version number no policy of this account ever
//!    cut — whether the policy that would carry it is live or itself deleted. The two
//!    operations one ARN addresses keep their own wording of that class: the read reports an
//!    absent policy as `GetAutomatedReasoningPolicy` words it and the delete as the policy's
//!    writes word it, while a missing VERSION of a policy that IS there is the one sentence
//!    both share.
//! 3. **The artifact gate is about what the policy STILL holds**: with `force` false "Amazon
//!    Bedrock validates if all artifacts have been deleted (e.g. policy version, test case,
//!    test result) for a policy before deletion"
//!    (`smithy_query(query="doc",shape="DeleteAutomatedReasoningPolicyRequest",member="force")`),
//!    so an unforced delete is refused `400 ValidationException` while the policy holds one
//!    artifact of any kind, and the SAME unforced delete is accepted once that artifact has
//!    itself been deleted — asserted per artifact kind rather than for one of them.
//!
//! The requests go through `crate::support` — the ONE harness every suite in this crate
//! drives the emulator with.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::automated_reasoning_policy_test_cases::{create_test_case, CHECK_RESULTS};
use crate::support::{emulator, label, seed_build_workflow, Emulator, Reply, ACCOUNT, REGION};

/// The status an accepted `DeleteAutomatedReasoningPolicy` answers with (`@http(code: 202)`).
const DELETE_ACCEPTED: u16 = 202;

/// The `ResourceNotFoundException` sentence a WRITE of a policy this account holds no record
/// of is refused with.
const POLICY_NOT_FOUND_ON_WRITE: &str = "Policy was not found";

/// The same class as [`POLICY_NOT_FOUND_ON_WRITE`] for a version of a policy that IS there:
/// the policy resolved and the revision did not.
const POLICY_VERSION_NOT_FOUND: &str = "Policy version does not exist";

/// The same class as [`POLICY_NOT_FOUND_ON_WRITE`] as a READ of the policy words it: Amazon
/// Bedrock words this one class per operation, so the two sentences are not interchangeable.
const POLICY_NOT_FOUND_ON_READ: &str = "Policy does not exist";

/// The `buildWorkflowType` a seeded build workflow carries.
const INGEST_CONTENT: &str = "INGEST_CONTENT";

/// How far in the past a seeded build workflow was started, so the clock has already carried
/// it through its whole progression by the time a row reads it — reached without sleeping.
const LONG_BUILT: Duration =
    bedrock::state::BUILD_WORKFLOW_COMPLETE_AFTER.saturating_add(Duration::from_millis(150));

/// How `force` is spelled on the wire: left out, so the member's modelled `@default(false)`
/// applies, or spelled either way.
#[derive(Debug, Clone, Copy)]
enum Force {
    Omitted,
    False,
    True,
}

impl Force {
    fn query(self) -> &'static str {
        match self {
            Force::Omitted => "",
            Force::False => "?force=false",
            Force::True => "?force=true",
        }
    }
}

/// Every spelling of `force`, so a rule that does not depend on it is asserted over all three.
const EVERY_FORCE: [Force; 3] = [Force::Omitted, Force::False, Force::True];

/// One `DELETE /automated-reasoning-policies/{policyArn}`; the ARN is a path LABEL carrying
/// `:` and `/`, which are percent-encoded so it stays one path segment.
async fn delete_policy(emu: &Emulator, policy_arn: &str, force: Force) -> Reply {
    emu.call(
        "DELETE",
        &format!(
            "/automated-reasoning-policies/{}{}",
            label(policy_arn),
            force.query()
        ),
        None,
    )
    .await
}

/// `GetAutomatedReasoningPolicy` for one revision, by the ARN that names it.
async fn get_policy(emu: &Emulator, policy_arn: &str) -> Reply {
    emu.call(
        "GET",
        &format!("/automated-reasoning-policies/{}", label(policy_arn)),
        None,
    )
    .await
}

/// Create a policy named `name` and hand back its `(policyArn, definitionHash)` — the hash a
/// later cut of a version is held against.
async fn policy(emu: &Emulator, name: &str) -> (String, String) {
    let created = emu
        .call(
            "POST",
            "/automated-reasoning-policies",
            Some(json!({ "name": name })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: the create must answer 200, got {} {:?}",
        created.status, created.body
    );
    (created.text("policyArn"), created.text("definitionHash"))
}

/// `CreateAutomatedReasoningPolicyVersion`, held against the draft's current `definitionHash`.
async fn create_version(emu: &Emulator, case: &str, policy_arn: &str, hash: &str) -> String {
    let cut = emu
        .call(
            "POST",
            &format!(
                "/automated-reasoning-policies/{}/versions",
                label(policy_arn)
            ),
            Some(json!({ "lastUpdatedDefinitionHash": hash })),
        )
        .await;
    assert_eq!(
        cut.status, 200,
        "case {case:?}: cutting a version must answer 200, got {} {:?}",
        cut.status, cut.body
    );
    cut.text("version")
}

/// Assert `reply` is the answer the delete gives a request it accepted: the modelled status,
/// a body carrying no members (`DeleteAutomatedReasoningPolicyResponse` declares none) and no
/// error code.
fn assert_accepted(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, DELETE_ACCEPTED,
        "case {case:?}: the delete must answer {DELETE_ACCEPTED}, got {} {:?}",
        reply.status, reply.body
    );
    assert!(
        reply.body == Value::Null || reply.body == json!({}),
        "case {case:?}: an accepted delete carries no members, got {:?}",
        reply.body
    );
    assert!(
        reply.error.is_none(),
        "case {case:?}: an accepted delete carries no error code, got {:?}",
        reply.error
    );
}

/// Assert `reply` is a refusal carrying exactly `status`, `error` and `message`.
fn assert_refused(case: &str, reply: &Reply, status: u16, error: &str, message: &str) {
    assert_eq!(
        (reply.status, reply.error.as_deref()),
        (status, Some(error)),
        "case {case:?}: expected {status} {error}, got {} {:?}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.get("message"),
        json!(message),
        "case {case:?}: expected the message {message:?}, got {:?}",
        reply.body
    );
}

// ---------------------------------------------------------------------------------------
// 1. Repeating the delete of a revision this account held is refused.
// ---------------------------------------------------------------------------------------

/// What a row of rule 1 holds on its policy before the delete that takes the policy away, so
/// the rule is asserted over every form of revision a delete can remove rather than over one.
#[derive(Debug, Clone, Copy)]
enum Held {
    /// A policy holding no artifact at all.
    Nothing,
    /// A policy holding versions, which the forced delete takes with it.
    Versions,
    /// A policy holding a test, which the forced delete takes with it.
    TestCase,
    /// A policy holding a build workflow, which the forced delete takes with it.
    BuildWorkflow,
}

#[tokio::test]
async fn repeating_the_delete_of_a_policy_this_account_held_is_refused() {
    // Rows differ along what the policy still held when it was taken away, and along how
    // `force` is spelled on the REPEAT — which decides nothing once nothing is left to hold.
    let rows: [(&str, Held, Force); 6] = [
        ("a policy holding nothing", Held::Nothing, Force::Omitted),
        ("a policy holding nothing, force=false", Held::Nothing, Force::False),
        ("a policy holding nothing, force=true", Held::Nothing, Force::True),
        ("a policy holding versions", Held::Versions, Force::Omitted),
        ("a policy holding a test", Held::TestCase, Force::False),
        (
            "a policy holding a build workflow",
            Held::BuildWorkflow,
            Force::True,
        ),
    ];

    let emu = emulator();
    for (index, (case, held, repeat)) in rows.iter().enumerate() {
        // `AutomatedReasoningPolicyName` is `^[0-9a-zA-Z-_ ]+$`, so the row's index names
        // its policy rather than the case text.
        let (policy_arn, hash) = policy(&emu, &format!("repeated delete {index}")).await;
        let mut version_arns: Vec<String> = Vec::new();
        match held {
            Held::Nothing => {}
            Held::Versions => {
                for _ in 0..2 {
                    let number = create_version(&emu, case, &policy_arn, &hash).await;
                    version_arns.push(format!("{policy_arn}:{number}"));
                }
            }
            Held::TestCase => {
                let created = create_test_case(
                    &emu,
                    &policy_arn,
                    json!({
                        "guardContent": "the claim under test",
                        "expectedAggregatedFindingsResult": CHECK_RESULTS[0],
                    }),
                )
                .await;
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: the test create must answer 200, got {} {:?}",
                    created.status, created.body
                );
            }
            Held::BuildWorkflow => {
                seed_build_workflow(&emu, &policy_arn, INGEST_CONTENT, LONG_BUILT, |_| {});
            }
        }

        // The delete that takes the policy away: forced, so whatever the row held goes with
        // it and the repeats below have a policy this account HELD to repeat.
        assert_accepted(case, &delete_policy(&emu, &policy_arn, Force::True).await);

        // Repeating it is refused, however often it is repeated and however `force` is
        // spelled on the repeat: the ARN resolves to nothing, so there is no revision to
        // report a delete of.
        for round in 1..=3 {
            assert_refused(
                &format!("{case}, repeat {round}"),
                &delete_policy(&emu, &policy_arn, *repeat).await,
                404,
                "ResourceNotFoundException",
                POLICY_NOT_FOUND_ON_WRITE,
            );
        }
        // The refusals changed nothing: the policy is still gone from the read.
        let read = get_policy(&emu, &policy_arn).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a repeated delete must leave the policy gone, got {} {:?}",
            read.status, read.body
        );

        // Every version the policy held went with it, so the versioned ARNs resolve to
        // nothing either — and the policy that would carry them is gone, so the refusal
        // reports the absent POLICY rather than an absent version.
        for version_arn in &version_arns {
            for force in EVERY_FORCE {
                assert_refused(
                    &format!("{case}, the version {version_arn} it held, force {force:?}"),
                    &delete_policy(&emu, version_arn, force).await,
                    404,
                    "ResourceNotFoundException",
                    POLICY_NOT_FOUND_ON_WRITE,
                );
            }
        }
    }
}

#[tokio::test]
async fn repeating_the_delete_of_one_version_this_account_held_is_refused() {
    let emu = emulator();
    let (policy_arn, hash) = policy(&emu, "versioned repeated delete").await;
    let mut version_arns: Vec<String> = Vec::new();
    for _ in 0..3 {
        let number = create_version(&emu, "setup", &policy_arn, &hash).await;
        version_arns.push(format!("{policy_arn}:{number}"));
    }

    // Rows differ along WHICH of the policy's versions the delete addresses — the first, a
    // middle one and the last — so the rule is not keyed to one of them.
    for (case, version_arn) in [
        ("the first of three", &version_arns[0]),
        ("a middle version of three", &version_arns[1]),
        ("the last of three", &version_arns[2]),
    ] {
        assert_accepted(case, &delete_policy(&emu, version_arn, Force::Omitted).await);
        // The policy is still there and the version is not, so the refusal reports the
        // missing VERSION rather than the policy — whichever way `force` is spelled.
        for force in EVERY_FORCE {
            assert_refused(
                &format!("{case}, repeated with force {force:?}"),
                &delete_policy(&emu, version_arn, force).await,
                404,
                "ResourceNotFoundException",
                POLICY_VERSION_NOT_FOUND,
            );
        }
        let read = get_policy(&emu, version_arn).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: the deleted version must stay gone from the read, got {} {:?}",
            read.status, read.body
        );
    }

    // The policy itself was never deleted, so the refusals above took nothing else with
    // them: its draft still reads back, and the unforced delete of the emptied policy goes
    // through.
    let read = get_policy(&emu, &policy_arn).await;
    assert_eq!(
        read.status, 200,
        "case \"the draft\": deleting every version must leave the policy readable, got {} {:?}",
        read.status, read.body
    );
    assert_accepted(
        "the emptied policy",
        &delete_policy(&emu, &policy_arn, Force::Omitted).await,
    );
}

// ---------------------------------------------------------------------------------------
// 2. An ARN that never named a revision of this account is still refused.
// ---------------------------------------------------------------------------------------

/// Assert the ARN `policy_arn` resolves to nothing for BOTH operations addressed by it, each
/// in its own sentence, and that repeating either call changes neither answer.
async fn assert_gone(
    emu: &Emulator,
    case: &str,
    policy_arn: &str,
    read_message: &str,
    delete_message: &str,
) {
    for round in 1..=2 {
        let spelled = format!("{case}, round {round}");
        assert_refused(
            &format!("{spelled}, the read"),
            &get_policy(emu, policy_arn).await,
            404,
            "ResourceNotFoundException",
            read_message,
        );
        for force in EVERY_FORCE {
            assert_refused(
                &format!("{spelled}, the delete with force {force:?}"),
                &delete_policy(emu, policy_arn, force).await,
                404,
                "ResourceNotFoundException",
                delete_message,
            );
        }
    }
}

#[tokio::test]
async fn a_gone_revision_is_refused_by_the_read_and_the_delete_in_their_own_sentences() {
    let emu = emulator();
    let (policy_arn, hash) = policy(&emu, "distinct sentences").await;
    let taken = create_version(&emu, "setup", &policy_arn, &hash).await;
    let kept = create_version(&emu, "setup", &policy_arn, &hash).await;
    let taken_arn = format!("{policy_arn}:{taken}");
    let kept_arn = format!("{policy_arn}:{kept}");
    // A version number this policy never cut, so the rows cover a revision that was taken
    // away and one that never existed under the same policy.
    let uncut_arn = format!("{policy_arn}:4096");

    // While the POLICY is still there, every ARN of it that resolves to no version is
    // reported as the missing version by both operations — the one sentence they share.
    assert_accepted(
        "the version taken from a live policy",
        &delete_policy(&emu, &taken_arn, Force::Omitted).await,
    );
    for (case, arn) in [
        ("the version taken from a live policy", &taken_arn),
        ("a version the live policy never cut", &uncut_arn),
    ] {
        assert_gone(
            &emu,
            case,
            arn,
            POLICY_VERSION_NOT_FOUND,
            POLICY_VERSION_NOT_FOUND,
        )
        .await;
    }

    // Once the POLICY is gone, every ARN of it resolves to nothing and the two operations
    // part: the read reports the absent policy in its sentence and the delete in its own.
    assert_accepted(
        "the policy itself",
        &delete_policy(&emu, &policy_arn, Force::True).await,
    );
    for (case, arn) in [
        ("the policy itself", &policy_arn),
        ("the version it still held", &kept_arn),
        ("the version taken before it", &taken_arn),
        ("a version it never cut", &uncut_arn),
    ] {
        assert_gone(
            &emu,
            case,
            arn,
            POLICY_NOT_FOUND_ON_READ,
            POLICY_NOT_FOUND_ON_WRITE,
        )
        .await;
    }
}

#[tokio::test]
async fn an_arn_this_account_never_held_is_still_refused() {
    let emu = emulator();
    let (live, live_hash) = policy(&emu, "the live policy").await;
    create_version(&emu, "setup", &live, &live_hash).await;
    // A policy this account HELD and deleted, so the rows below cover both ways an ARN can
    // resolve to nothing — never held, and held and taken away — which this operation
    // refuses in the same sentence.
    let (gone, gone_hash) = policy(&emu, "the gone policy").await;
    create_version(&emu, "setup", &gone, &gone_hash).await;
    assert_accepted(
        "the gone policy",
        &delete_policy(&emu, &gone, Force::True).await,
    );

    // Rows differ along WHICH part of a well-formed `AutomatedReasoningPolicyArn` names
    // nothing this account ever held: the policy id, the region, or the version number —
    // and the sentence follows from that, since a version of a policy that IS there is
    // reported as the missing revision rather than as a missing policy.
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a policy id nothing minted",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:automated-reasoning-policy/zzzzzzzzzzzz"),
            POLICY_NOT_FOUND_ON_WRITE,
        ),
        (
            "the live policy addressed in another region",
            live.replace(REGION, "eu-west-1"),
            POLICY_NOT_FOUND_ON_WRITE,
        ),
        (
            "a versioned ARN of a policy nothing minted",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:automated-reasoning-policy/zzzzzzzzzzzz:1"),
            POLICY_NOT_FOUND_ON_WRITE,
        ),
        (
            "a version the live policy has not cut",
            format!("{live}:2"),
            POLICY_VERSION_NOT_FOUND,
        ),
        (
            "a version number far past the ones cut",
            format!("{live}:4294967295"),
            POLICY_VERSION_NOT_FOUND,
        ),
        (
            "a version the deleted policy never cut",
            format!("{gone}:9"),
            POLICY_NOT_FOUND_ON_WRITE,
        ),
        (
            "the version the deleted policy did cut",
            format!("{gone}:1"),
            POLICY_NOT_FOUND_ON_WRITE,
        ),
    ];

    for (case, policy_arn, message) in &rows {
        // Forcing a delete does not conjure a revision, so every spelling of `force` is
        // refused the same way.
        for force in EVERY_FORCE {
            assert_refused(
                &format!("{case}, force {force:?}"),
                &delete_policy(&emu, policy_arn, force).await,
                404,
                "ResourceNotFoundException",
                message,
            );
        }
    }

    // The revision each row is contrasted with is unaffected: the live policy and its
    // version still read back, and the policy this account held and deleted stays refused
    // however often the delete is repeated.
    for (case, arn) in [("the draft", live.clone()), ("version 1", format!("{live}:1"))] {
        let read = get_policy(&emu, &arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the revision must still read back, got {} {:?}",
            read.status, read.body
        );
    }
    assert_refused(
        "the gone policy, repeated",
        &delete_policy(&emu, &gone, Force::Omitted).await,
        404,
        "ResourceNotFoundException",
        POLICY_NOT_FOUND_ON_WRITE,
    );
}

// ---------------------------------------------------------------------------------------
// 3. The artifact gate is about what the policy STILL holds.
// ---------------------------------------------------------------------------------------

/// The one sentence Amazon Bedrock refuses an unforced delete of a policy that still holds an
/// artifact with, for the artifact `kind` that is in the way.
fn artifact_in_the_way(kind: &str) -> String {
    format!("There are {kind} that have not been deleted.")
}

#[tokio::test]
async fn an_unforced_delete_is_refused_until_the_artifact_in_the_way_is_itself_deleted() {
    // One row per kind of artifact a policy can hold: the kind the refusal names while the
    // artifact is there, and the delete that clears it. Each row then asserts the SAME
    // unforced delete is accepted once the artifact is gone, so the gate is about what the
    // policy still holds rather than about what it ever held.
    #[derive(Debug, Clone, Copy)]
    enum Artifact {
        Version,
        TestCase,
        BuildWorkflow,
    }

    let rows: [(&str, Artifact, &str); 3] = [
        ("a policy version", Artifact::Version, "policy version"),
        ("a test case", Artifact::TestCase, "test case"),
        (
            "a build workflow",
            Artifact::BuildWorkflow,
            "build workflow",
        ),
    ];

    let emu = emulator();
    for (index, (case, artifact, kind)) in rows.iter().enumerate() {
        let (policy_arn, hash) = policy(&emu, &format!("artifact policy {index}")).await;
        // The artifact of this row, and the request that clears it again.
        let clear: String = match artifact {
            Artifact::Version => {
                let number = create_version(&emu, case, &policy_arn, &hash).await;
                format!("{policy_arn}:{number}")
            }
            Artifact::TestCase => {
                let created = create_test_case(
                    &emu,
                    &policy_arn,
                    json!({
                        "guardContent": "the claim under test",
                        "expectedAggregatedFindingsResult": CHECK_RESULTS[0],
                    }),
                )
                .await;
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: the test create must answer 200, got {} {:?}",
                    created.status, created.body
                );
                created.text("testCaseId")
            }
            Artifact::BuildWorkflow => {
                seed_build_workflow(&emu, &policy_arn, INGEST_CONTENT, LONG_BUILT, |_| {})
            }
        };

        // While the artifact is there, neither spelling of an unforced delete may take the
        // policy, and the refusal says which kind is in the way.
        for unforced in [Force::Omitted, Force::False] {
            let spelled = format!("{case}, force {unforced:?}");
            assert_refused(
                &spelled,
                &delete_policy(&emu, &policy_arn, unforced).await,
                400,
                "ValidationException",
                &artifact_in_the_way(kind),
            );
            let read = get_policy(&emu, &policy_arn).await;
            assert_eq!(
                read.status, 200,
                "case {spelled:?}: a refused delete must leave the policy readable, got {} {:?}",
                read.status, read.body
            );
        }

        // Deleting the artifact is what the refusal asks for; the same unforced delete then
        // goes through, and repeating it succeeds.
        match artifact {
            Artifact::Version => {
                assert_accepted(
                    &format!("{case}, the artifact itself"),
                    &delete_policy(&emu, &clear, Force::Omitted).await,
                );
            }
            Artifact::TestCase => {
                let read = emu
                    .call(
                        "GET",
                        &format!(
                            "/automated-reasoning-policies/{}/test-cases/{clear}",
                            label(&policy_arn)
                        ),
                        None,
                    )
                    .await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: the test must read back before it is deleted, got {} {:?}",
                    read.status, read.body
                );
                // `lastUpdatedAt` is the concurrency token the delete requires, bound to the
                // `updatedAt` query parameter, and the test carries it inside `testCase`.
                let updated_at = read
                    .get("testCase")
                    .get("updatedAt")
                    .and_then(|updated_at| updated_at.as_str())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the test must report updatedAt, got {:?}", read.body)
                    })
                    .to_string();
                let removed = emu
                    .call(
                        "DELETE",
                        &format!(
                            "/automated-reasoning-policies/{}/test-cases/{clear}?updatedAt={}",
                            label(&policy_arn),
                            label(&updated_at)
                        ),
                        None,
                    )
                    .await;
                assert_eq!(
                    removed.status, DELETE_ACCEPTED,
                    "case {case:?}: deleting the test must answer {DELETE_ACCEPTED}, got {} {:?}",
                    removed.status, removed.body
                );
            }
            Artifact::BuildWorkflow => {
                let read = emu
                    .call(
                        "GET",
                        &format!(
                            "/automated-reasoning-policies/{}/build-workflows/{clear}",
                            label(&policy_arn)
                        ),
                        None,
                    )
                    .await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: the workflow must read back before it is deleted, got {} {:?}",
                    read.status, read.body
                );
                let removed = emu
                    .call(
                        "DELETE",
                        &format!(
                            "/automated-reasoning-policies/{}/build-workflows/{clear}\
                             ?updatedAt={}",
                            label(&policy_arn),
                            label(&read.text("updatedAt"))
                        ),
                        None,
                    )
                    .await;
                assert_eq!(
                    removed.status, DELETE_ACCEPTED,
                    "case {case:?}: deleting the workflow must answer {DELETE_ACCEPTED}, got {} \
                     {:?}",
                    removed.status, removed.body
                );
            }
        }

        assert_accepted(
            &format!("{case}, cleared"),
            &delete_policy(&emu, &policy_arn, Force::Omitted).await,
        );
        // The policy is gone, so the same unforced delete no longer resolves an ARN and is
        // refused rather than reporting the removal again.
        assert_refused(
            &format!("{case}, cleared and repeated"),
            &delete_policy(&emu, &policy_arn, Force::Omitted).await,
            404,
            "ResourceNotFoundException",
            POLICY_NOT_FOUND_ON_WRITE,
        );
    }
}
