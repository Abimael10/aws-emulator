// SPDX-License-Identifier: CC0-1.0

//! `DeleteAutomatedReasoningPolicy` decides every precondition BEFORE it removes anything, so
//! a refused delete is not a delete — asserted end-to-end over the same `MultiServiceRouter`
//! the emulator boots.
//!
//! The rule under test, over a SET of inputs rather than over one recorded teardown: a delete
//! this operation refuses leaves the revision it addressed, and everything hanging off it,
//! exactly as it found them, so the NEXT delete of that same `AutomatedReasoningPolicyArn` —
//! the one a client makes once the blocker is cleared, or with `force` set where `force`
//! governs the blocker — answers the modelled `202` with the empty body; only an ARN that
//! resolves to nothing answers `404`. A teardown is therefore refuse-then-delete, and the
//! delete that follows a refusal is the one that performs the removal.
//!
//! Rows differ along what blocks the delete (each kind of artifact `force` governs — a policy
//! version, a test, the build workflow a result is produced by — and a guardrail's use of the
//! revision, which `force` does not govern), along the form of ARN the delete addresses (the
//! policy's unversioned ARN and the `:<version>` form the shape's `@pattern` equally admits),
//! and along how the caller cleared the blocker (deleting it, or forcing the delete where
//! forcing is a way out).
//!
//! The expectations are the model's
//! (`smithy_query(query="operation",operation="DeleteAutomatedReasoningPolicy")`:
//! `@http(code: 202, method: "DELETE", uri: "/automated-reasoning-policies/{policyArn}")` with
//! an output shape declaring no members, `force` bound to the `force` query parameter with
//! `@default(false)`, `policyArn` `@pattern
//! ^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:automated-reasoning-policy/[a-z0-9]{12}(:([1-9][0-9]{0,11}))?$`,
//! `ValidationException` 400, `ResourceInUseException` 400, `ResourceNotFoundException` 404)
//! and its documentation of `force` ("When false, Amazon Bedrock validates if all artifacts
//! have been deleted (e.g. policy version, test case, test result) for a policy before
//! deletion").
//!
//! The requests go through `crate::support` — the ONE harness every suite in this crate drives
//! the emulator with.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::automated_reasoning_policy_test_cases::{create_test_case, get_test_case, CHECK_RESULTS};
use crate::support::{emulator, label, seed_build_workflow, Emulator, Reply};

/// The status `DeleteAutomatedReasoningPolicy` answers a delete it PERFORMED with
/// (`@http(code: 202)`).
const DELETE_ACCEPTED: u16 = 202;

/// The status every operation of this service answers an accepted delete of one of a policy's
/// artifacts with (`@http(code: 202)` on the test, build-workflow and guardrail deletes).
const ARTIFACT_DELETE_ACCEPTED: u16 = 202;

/// The `ResourceNotFoundException` sentence a WRITE of a policy this account holds no record
/// of is refused with — which is what the delete of an ARN that resolves to nothing answers,
/// and what the delete that PERFORMED the removal must not have answered.
const POLICY_NOT_FOUND_ON_WRITE: &str = "Policy was not found";

/// The same class for a version of a policy that IS there: the policy resolved and the
/// revision did not.
const POLICY_VERSION_NOT_FOUND: &str = "Policy version does not exist";

/// The `buildWorkflowType` a seeded build workflow carries
/// (`AutomatedReasoningPolicyBuildWorkflowType$INGEST_CONTENT`).
const INGEST_CONTENT: &str = "INGEST_CONTENT";

/// How far in the past a seeded build workflow was started, so the clock has already carried
/// it through its whole progression — and so into the terminal status its own delete requires
/// — by the time a row addresses it, reached without sleeping for it.
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

/// What stands in the way of a row's delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Blocker {
    /// A version the policy has cut: one of the artifact kinds `force` names.
    PolicyVersion,
    /// A test stored under the policy: another kind `force` names.
    TestCase,
    /// A build workflow of the policy, which the results of a test run hang off.
    BuildWorkflow,
    /// A guardrail whose Automated Reasoning policy configuration names the revision — the
    /// case `ResourceInUseException` documents, which `force` does not govern.
    Guardrail,
}

impl Blocker {
    /// The noun the unforced-delete refusal names this kind of artifact with, or `None` for a
    /// blocker that is not an artifact of the policy.
    fn artifact_noun(self) -> Option<&'static str> {
        match self {
            Blocker::PolicyVersion => Some("policy version"),
            Blocker::TestCase => Some("test case"),
            Blocker::BuildWorkflow => Some("build workflow"),
            Blocker::Guardrail => None,
        }
    }
}

/// How the caller got past the blocker before issuing the delete that performs the removal.
#[derive(Debug, Clone, Copy)]
enum Cleared {
    /// The blocker was deleted through the operation that owns it, and the SAME unforced
    /// delete re-issued.
    BlockerRemoved,
    /// The delete was re-issued with `force` set, which is a way out only of the policy's own
    /// artifacts.
    Forced,
}

/// One `DELETE /automated-reasoning-policies/{policyArn}`; the ARN is a path LABEL and carries
/// `:` and `/`, which are percent-encoded so it stays one path segment, exactly as an SDK
/// writes it.
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

/// Create a policy named `name` and hand back its `(policyArn, definitionHash)` — the hash a
/// later cut of a version is held against.
async fn policy(emu: &Emulator, case: &str, name: &str) -> (String, String) {
    let created = emu
        .call(
            "POST",
            "/automated-reasoning-policies",
            Some(json!({ "name": name })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateAutomatedReasoningPolicy must answer 200, got {} {:?}",
        created.status, created.body
    );
    (created.text("policyArn"), created.text("definitionHash"))
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

/// `CreateAutomatedReasoningPolicyVersion`, held against the draft's current `definitionHash`
/// — the concurrency token the operation requires.
async fn create_version(emu: &Emulator, case: &str, policy_arn: &str, definition_hash: &str) {
    let cut = emu
        .call(
            "POST",
            &format!(
                "/automated-reasoning-policies/{}/versions",
                label(policy_arn)
            ),
            Some(json!({ "lastUpdatedDefinitionHash": definition_hash })),
        )
        .await;
    assert_eq!(
        cut.status, 200,
        "case {case:?}: cutting a version must answer 200, got {} {:?}",
        cut.status, cut.body
    );
}

/// Every `policyArn` `ListAutomatedReasoningPolicies` reports.
async fn listed_arns(case: &str, emu: &Emulator) -> Vec<String> {
    let listed = emu
        .call("GET", "/automated-reasoning-policies?maxResults=1000", None)
        .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: the listing must answer 200, got {} {:?}",
        listed.status, listed.body
    );
    listed
        .get("automatedReasoningPolicySummaries")
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the summaries are a required list, got {:?}",
                listed.body
            )
        })
        .iter()
        .map(|summary| {
            summary
                .get("policyArn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Create a test under `policy_arn` and hand back its `testCaseId`.
async fn seed_test_case(emu: &Emulator, case: &str, policy_arn: &str) -> String {
    let created = create_test_case(
        emu,
        policy_arn,
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

/// `GetAutomatedReasoningPolicyBuildWorkflow`, by the policy ARN and the workflow id.
async fn get_workflow(emu: &Emulator, policy_arn: &str, build_workflow_id: &str) -> Reply {
    emu.call(
        "GET",
        &format!(
            "/automated-reasoning-policies/{}/build-workflows/{build_workflow_id}",
            label(policy_arn)
        ),
        None,
    )
    .await
}

/// `CreateGuardrail` carrying an `automatedReasoningPolicyConfig` that names `policies`, plus
/// the members the model requires of every guardrail; hands back its `guardrailId`.
async fn guardrail_checking(emu: &Emulator, case: &str, name: &str, policies: &[String]) -> String {
    let created = emu
        .create(json!({
            "name": name,
            "blockedInputMessaging": "blocked in",
            "blockedOutputsMessaging": "blocked out",
            "automatedReasoningPolicyConfig": { "policies": policies },
        }))
        .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateGuardrail must answer 202, got {} {:?}",
        created.status, created.body
    );
    created.text("guardrailId")
}

/// The `updatedAt` a read reports for the test `test_case_id`, as the `date-time` text a
/// caller echoes back as the delete's `lastUpdatedAt`.
async fn test_case_updated_at(emu: &Emulator, case: &str, policy_arn: &str, id: &str) -> String {
    let read = get_test_case(emu, policy_arn, id).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the test must read back, got {} {:?}",
        read.status, read.body
    );
    let test_case = read.get("testCase");
    test_case
        .get("updatedAt")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the read must report the test's updatedAt, got {test_case:?}")
        })
        .to_string()
}

/// Assert `reply` is the answer the delete gives a request it PERFORMED: the modelled status, a
/// body carrying no members (`DeleteAutomatedReasoningPolicyResponse` declares none) and no
/// error.
fn assert_accepted(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, DELETE_ACCEPTED,
        "case {case:?}: the delete must answer {DELETE_ACCEPTED}, got {} {:?}",
        reply.status, reply.body
    );
    assert!(
        reply.body == Value::Null || reply.body == json!({}),
        "case {case:?}: the performed delete answers the empty body, got {:?}",
        reply.body
    );
    assert!(
        reply.error.is_none(),
        "case {case:?}: the performed delete carries no error, got {:?}",
        reply.error
    );
}

/// Assert `reply` is a refusal carrying `status` and `error`, and hand back its `message`.
fn assert_refused(case: &str, reply: &Reply, status: u16, error: &str) -> String {
    assert_eq!(
        (reply.status, reply.error.as_deref()),
        (status, Some(error)),
        "case {case:?}: expected {status} {error}, got {} {:?}",
        reply.status,
        reply.body
    );
    reply
        .get("message")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no message: {:?}", reply.body))
        .to_string()
}

/// Assert the revision `policy_arn` reads back, which is what a refused delete must have left
/// behind.
async fn assert_readable(case: &str, emu: &Emulator, policy_arn: &str) {
    let read = get_policy(emu, policy_arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: a refused delete must leave {policy_arn} readable, got {} {:?}",
        read.status, read.body
    );
}

#[tokio::test]
async fn the_delete_after_a_refused_delete_performs_the_removal() {
    struct Row {
        case: &'static str,
        blocker: Blocker,
        /// `None` addresses the policy's unversioned ARN, `Some(n)` the ARN of version `n`.
        addressed: Option<u32>,
        cleared: Cleared,
    }

    let rows = [
        Row {
            case: "a version in the way, the version deleted",
            blocker: Blocker::PolicyVersion,
            addressed: None,
            cleared: Cleared::BlockerRemoved,
        },
        Row {
            case: "a version in the way, the delete forced",
            blocker: Blocker::PolicyVersion,
            addressed: None,
            cleared: Cleared::Forced,
        },
        Row {
            case: "a test in the way, the test deleted",
            blocker: Blocker::TestCase,
            addressed: None,
            cleared: Cleared::BlockerRemoved,
        },
        Row {
            case: "a test in the way, the delete forced",
            blocker: Blocker::TestCase,
            addressed: None,
            cleared: Cleared::Forced,
        },
        Row {
            case: "a build workflow in the way, the workflow deleted",
            blocker: Blocker::BuildWorkflow,
            addressed: None,
            cleared: Cleared::BlockerRemoved,
        },
        Row {
            case: "a build workflow in the way, the delete forced",
            blocker: Blocker::BuildWorkflow,
            addressed: None,
            cleared: Cleared::Forced,
        },
        Row {
            case: "a guardrail checking the policy, the guardrail deleted",
            blocker: Blocker::Guardrail,
            addressed: None,
            cleared: Cleared::BlockerRemoved,
        },
        Row {
            case: "a guardrail checking a version, the guardrail deleted",
            blocker: Blocker::Guardrail,
            addressed: Some(1),
            cleared: Cleared::BlockerRemoved,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // One emulator per row, so what a row's delete leaves behind is only ever its own.
        let emu = emulator();
        let (policy_arn, hash) = policy(&emu, case, &format!("teardown policy {index}")).await;

        // The blocker of this row, seeded through the operation that owns it. A row addressing
        // a versioned ARN needs that version whatever its blocker is.
        let versioned = |number: u32| format!("{policy_arn}:{number}");
        if row.blocker == Blocker::PolicyVersion || row.addressed.is_some() {
            create_version(&emu, case, &policy_arn, &hash).await;
        }
        let mut test_case_id = String::new();
        let mut build_workflow_id = String::new();
        let mut guardrail = String::new();
        match row.blocker {
            Blocker::PolicyVersion => {}
            Blocker::TestCase => test_case_id = seed_test_case(&emu, case, &policy_arn).await,
            Blocker::BuildWorkflow => {
                build_workflow_id =
                    seed_build_workflow(&emu, &policy_arn, INGEST_CONTENT, LONG_BUILT, |_| {});
            }
            Blocker::Guardrail => {
                // `GuardrailName` is `^[0-9a-zA-Z-_]+$`, so the row's index names its guardrail.
                let referenced = match row.addressed {
                    None => policy_arn.clone(),
                    Some(number) => versioned(number),
                };
                guardrail = guardrail_checking(
                    &emu,
                    case,
                    &format!("checking-guardrail-{index}"),
                    std::slice::from_ref(&referenced),
                )
                .await;
            }
        }

        let addressed = match row.addressed {
            None => policy_arn.clone(),
            Some(number) => versioned(number),
        };

        // Every spelling of the delete the blocker stands in the way of is refused — and
        // `force` is one of them where the blocker is another resource's use of the revision,
        // which `force` does not govern.
        let refused_spellings: &[Force] = match row.blocker {
            Blocker::Guardrail => &[Force::Omitted, Force::False, Force::True],
            _ => &[Force::Omitted, Force::False],
        };
        for force in refused_spellings {
            let spelled = format!("{case}, force {force:?}");
            let refused = delete_policy(&emu, &addressed, *force).await;
            let message = match row.blocker.artifact_noun() {
                Some(noun) => {
                    let message = assert_refused(&spelled, &refused, 400, "ValidationException");
                    assert_eq!(
                        message,
                        format!("There are {noun} that have not been deleted."),
                        "case {spelled:?}: the refusal names the artifact kind in the way, got \
                         {message:?}"
                    );
                    message
                }
                None => {
                    let message = assert_refused(&spelled, &refused, 400, "ResourceInUseException");
                    assert!(
                        message.contains(&guardrail),
                        "case {spelled:?}: the refusal names the guardrail using it, got \
                         {message:?}"
                    );
                    message
                }
            };
            assert!(
                !message.is_empty(),
                "case {spelled:?}: the refusal carries a message"
            );

            // NOTHING the refused delete addressed moved: the revision reads back, the policy
            // is still listed, and the blocker is still there to be cleared.
            assert_readable(&spelled, &emu, &addressed).await;
            assert_readable(&spelled, &emu, &policy_arn).await;
            let listed = listed_arns(&spelled, &emu).await;
            assert!(
                listed.contains(&policy_arn),
                "case {spelled:?}: the listing must still report {policy_arn}, got {listed:?}"
            );
            match row.blocker {
                Blocker::PolicyVersion => assert_readable(&spelled, &emu, &versioned(1)).await,
                Blocker::TestCase => {
                    let read = get_test_case(&emu, &policy_arn, &test_case_id).await;
                    assert_eq!(
                        read.status, 200,
                        "case {spelled:?}: the test in the way must still read back, got {} {:?}",
                        read.status, read.body
                    );
                }
                Blocker::BuildWorkflow => {
                    let read = get_workflow(&emu, &policy_arn, &build_workflow_id).await;
                    assert_eq!(
                        read.status, 200,
                        "case {spelled:?}: the workflow in the way must still read back, got {} \
                         {:?}",
                        read.status, read.body
                    );
                }
                Blocker::Guardrail => {
                    let read = emu.get(&guardrail, None).await;
                    assert_eq!(
                        read.status, 200,
                        "case {spelled:?}: the guardrail using it must still read back, got {} \
                         {:?}",
                        read.status, read.body
                    );
                }
            }
        }

        // The caller clears the blocker the way the refusal asks for — or forces the delete,
        // where forcing is a way out — and issues the delete again.
        let performing = match row.cleared {
            Cleared::Forced => Force::True,
            Cleared::BlockerRemoved => {
                let dropped = match row.blocker {
                    Blocker::PolicyVersion => {
                        delete_policy(&emu, &versioned(1), Force::Omitted).await
                    }
                    Blocker::TestCase => {
                        let token =
                            test_case_updated_at(&emu, case, &policy_arn, &test_case_id).await;
                        emu.call(
                            "DELETE",
                            &format!(
                                "/automated-reasoning-policies/{}/test-cases/{}?updatedAt={}",
                                label(&policy_arn),
                                label(&test_case_id),
                                label(&token)
                            ),
                            None,
                        )
                        .await
                    }
                    Blocker::BuildWorkflow => {
                        let read = get_workflow(&emu, &policy_arn, &build_workflow_id).await;
                        let token = read.text("updatedAt");
                        emu.call(
                            "DELETE",
                            &format!(
                                "/automated-reasoning-policies/{}/build-workflows/{}?updatedAt={}",
                                label(&policy_arn),
                                label(&build_workflow_id),
                                label(&token)
                            ),
                            None,
                        )
                        .await
                    }
                    Blocker::Guardrail => emu.delete(&guardrail, None).await,
                };
                assert_eq!(
                    dropped.status, ARTIFACT_DELETE_ACCEPTED,
                    "case {case:?}: clearing the blocker must answer \
                     {ARTIFACT_DELETE_ACCEPTED}, got {} {:?}",
                    dropped.status, dropped.body
                );
                Force::Omitted
            }
        };

        let performed = delete_policy(&emu, &addressed, performing).await;
        assert_accepted(&format!("{case}, the delete that performs it"), &performed);

        // Only now does the ARN resolve to nothing: the SAME delete answers the modelled 404,
        // and which half of the ARN stopped resolving decides the sentence.
        let again = delete_policy(&emu, &addressed, performing).await;
        let expected_message = if row.addressed.is_some() {
            POLICY_VERSION_NOT_FOUND
        } else {
            POLICY_NOT_FOUND_ON_WRITE
        };
        let message = assert_refused(
            &format!("{case}, deleted again"),
            &again,
            404,
            "ResourceNotFoundException",
        );
        assert_eq!(
            message, expected_message,
            "case {case:?}: the delete of an ARN resolving to nothing is worded as a write of an \
             absent revision is, got {message:?}"
        );

        // A row that took one VERSION away left the policy itself, which is deletable now that
        // it holds nothing — the whole teardown, ending in the modelled success.
        if row.addressed.is_some() {
            assert_readable(&format!("{case}, the policy behind the version"), &emu, &policy_arn)
                .await;
            assert_accepted(
                &format!("{case}, the policy after its version"),
                &delete_policy(&emu, &policy_arn, Force::Omitted).await,
            );
        }

        let gone = get_policy(&emu, &policy_arn).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the policy the teardown removed must read 404, got {} {:?}",
            gone.status, gone.body
        );
    }
}
