// SPDX-License-Identifier: CC0-1.0

//! WHAT a resource-policy mutation the status gate forbids SAYS, end to end over HTTP.
//!
//! "Update/Deletion is disallowed until Resource-based Policy is ACTIVE and Workspace is
//! ACTIVE" (`WorkspacePolicyStatusCode`) is one rule, and the refusal it produces is one
//! sentence: `ConflictException` (409) whose `message` is fixed text —
//! "Can't update the workspace resource policy in non-(ACTIVE) state. Current status is "
//! — closed by the ONE status that blocked the call, read from the service's state.
//!
//! This suite is a TABLE over that class: one row per status that can block a mutation
//! (`CREATING`, `UPDATING`, `DELETING`), reached both on the POLICY the call acts on and
//! on the WORKSPACE holding it, and each row drives both operations the gate stops —
//! `PutResourcePolicy` and `DeleteResourcePolicy`. The sentence is a LITERAL here, so the
//! wire text is what is under test: re-wording the constant in `src/state.rs` cannot keep
//! this suite green.
//!
//! The harness (`Amp`, `Reply`, `text`) is the one `tests/all/workspace_lifecycle.rs`
//! builds and the request helpers are the ones `tests/all/resource_policy_lifecycle.rs`
//! already exposes.

use crate::resource_policy_lifecycle::{delete_policy, describe_policy, policy_document, put_policy};
use crate::workspace_lifecycle::{text, Amp, Reply};
use service_com_amazonaws_amp_amazonprometheusservice::state::{
    POLICY_STATUS_ACTIVE, POLICY_STATUS_CREATING, POLICY_STATUS_DELETING, POLICY_STATUS_UPDATING,
    STATUS_CREATING, STATUS_DELETING, STATUS_UPDATING,
};

/// The fixed part of the sentence AWS answers for this refusal, as a LITERAL: the
/// recording shows "Can't update the workspace resource policy in non-(ACTIVE) state.
/// Current status is UPDATING", so everything up to the status token is constant —
/// including the parenthesised `(ACTIVE)` and the absence of a trailing period.
const FIXED_TEXT: &str =
    "Can't update the workspace resource policy in non-(ACTIVE) state. Current status is ";

/// The whole sentence for one blocking status.
fn expected_message(blocking_status: &str) -> String {
    format!("{FIXED_TEXT}{blocking_status}")
}

/// How long a row keeps looking for the settling the clock owes before it concludes the
/// policy is not settling at all, and how often it looks. The deadline is far past the
/// span the emulator gives a transition, so a loaded host still settles inside it;
/// nothing is asserted about how long it actually took.
const SETTLING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);
const SETTLING_LOOK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Wait for the CONDITION — a policy that reports `ACTIVE` — by polling the same reader a
/// client would, and answer with the revision it settled at.
async fn policy_settled(case: &str, amp: &Amp, region: &str, workspace_id: &str) -> String {
    let deadline = std::time::Instant::now() + SETTLING_DEADLINE;
    loop {
        let described = describe_policy(amp, region, workspace_id).await;
        let status = text(&described.body["policyStatus"]);
        if status == POLICY_STATUS_ACTIVE {
            return text(&described.body["revisionId"]);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the policy never reported {POLICY_STATUS_ACTIVE}, last status \
             {status:?}"
        );
        tokio::time::sleep(SETTLING_LOOK_INTERVAL).await;
    }
}

/// Which transient state a row drives the workspace or its policy into before it makes
/// the refused calls. Every value of `WorkspacePolicyStatusCode` other than `ACTIVE` is
/// covered, on the policy and — where a policy can be attached first — on the workspace.
#[derive(Debug, Clone, Copy)]
enum Blocked {
    /// The very first put, on a workspace that is still being created: nothing is
    /// attached, so the workspace's own status is what blocks.
    WorkspaceCreating,
    /// A settled policy on a workspace a rename left `UPDATING`.
    WorkspaceUpdating,
    /// A settled policy on a workspace a delete left `DELETING`.
    WorkspaceDeleting,
    /// A policy whose first put has not settled yet.
    PolicyCreating,
    /// A policy a second put is replacing.
    PolicyUpdating,
    /// A policy whose removal has been started but is not owed yet.
    PolicyDeleting,
}

/// What a row holds after its setup: the workspace, and the revision the policy is at —
/// so the delete can take the optimistic lock and be refused by the STATUS rather than by
/// the lock. `None` when the row attached no policy at all, where a delete has nothing to
/// name and answers for a missing resource instead.
struct Blocking {
    workspace_id: String,
    revision_id: Option<String>,
}

async fn drive_into(case: &str, amp: &Amp, region: &str, blocked: Blocked) -> Blocking {
    // A workspace that is still CREATING cannot be given a policy, so that row holds
    // none and only a put reaches the gate (a delete has no policy to name).
    if matches!(blocked, Blocked::WorkspaceCreating) {
        let created = amp
            .call(
                "POST",
                "/workspaces",
                region,
                &serde_json::json!({ "alias": "still-creating" }).to_string(),
            )
            .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: creating the workspace: {created:?}"
        );
        return Blocking {
            workspace_id: text(&created.body["workspaceId"]),
            revision_id: None,
        };
    }

    let workspace_id = amp.active_workspace(region, "policy-conflict").await;
    let first = put_policy(
        amp,
        region,
        &workspace_id,
        &serde_json::json!({ "policyDocument": policy_document("Settling", 1) }),
    )
    .await;
    assert_eq!(
        first.status, 202,
        "case {case:?}: seeding the policy: {first:?}"
    );
    let mut revision_id = text(&first.body["revisionId"]);

    // CREATING is the status the first put leaves behind, so that row makes its refused
    // calls straight away; every other row waits for the policy to settle first.
    if !matches!(blocked, Blocked::PolicyCreating) {
        revision_id = policy_settled(case, amp, region, &workspace_id).await;
    }

    match blocked {
        Blocked::WorkspaceCreating | Blocked::PolicyCreating => {}
        Blocked::PolicyUpdating => {
            let update = put_policy(
                amp,
                region,
                &workspace_id,
                &serde_json::json!({ "policyDocument": policy_document("Replacing", 2) }),
            )
            .await;
            assert_eq!(
                update.status, 202,
                "case {case:?}: the put that starts the update: {update:?}"
            );
            revision_id = text(&update.body["revisionId"]);
        }
        Blocked::PolicyDeleting => {
            // No `clientToken`, so the refused delete below is a new request against a
            // policy that is going away and not a replay of this one.
            let started = delete_policy(amp, region, &workspace_id, &[]).await;
            assert_eq!(
                started.status, 202,
                "case {case:?}: the delete that starts the removal: {started:?}"
            );
        }
        Blocked::WorkspaceUpdating => {
            let renamed = amp
                .rename(
                    region,
                    &workspace_id,
                    &serde_json::json!({ "alias": "renamed-while-policy-active" }),
                )
                .await;
            assert_eq!(
                renamed.status, 204,
                "case {case:?}: the rename that leaves the workspace updating: {renamed:?}"
            );
        }
        Blocked::WorkspaceDeleting => {
            let deleted = amp.delete(region, &workspace_id, None).await;
            assert_eq!(
                deleted.status, 202,
                "case {case:?}: the delete that leaves the workspace deleting: {deleted:?}"
            );
        }
    }

    Blocking {
        workspace_id,
        revision_id: Some(revision_id),
    }
}

/// The refusal, whichever mutation met the gate: 409, the modelled `ConflictException`,
/// and the one sentence closed by the status that blocked it — with nothing of the
/// request, and no second status, in the text.
fn assert_refusal(case: &str, operation: &str, reply: &Reply, blocking_status: &str) {
    assert_eq!(
        reply.status, 409,
        "case {case:?}: {operation} must be refused with 409, got {reply:?}"
    );
    assert_eq!(
        reply.error_code(),
        "ConflictException",
        "case {case:?}: {operation} returned an unexpected error code, got {reply:?}"
    );
    let message = text(&reply.body["message"]);
    assert_eq!(
        message,
        expected_message(blocking_status),
        "case {case:?}: {operation} must answer the service's policy-conflict sentence, \
         got {message:?}"
    );
    assert!(
        message.starts_with(FIXED_TEXT),
        "case {case:?}: {operation} must keep the sentence's fixed text, got {message:?}"
    );
}

/// One row per status that can block a mutation of a workspace's resource-based policy,
/// reached on the policy and on the workspace, each driving both operations the gate
/// stops: the refusal is 409 `ConflictException` carrying the service's one sentence with
/// that status as its only variable part.
#[tokio::test]
async fn a_blocked_policy_mutation_states_one_sentence_with_the_blocking_status() {
    struct Case {
        name: &'static str,
        region: &'static str,
        blocked: Blocked,
        /// The status the refusal must report — the one of the pair the gate checks that
        /// is not `ACTIVE`.
        blocking_status: &'static str,
    }

    let cases = [
        Case {
            name: "the first put while the workspace is still being created",
            region: "us-west-2",
            blocked: Blocked::WorkspaceCreating,
            blocking_status: STATUS_CREATING,
        },
        Case {
            name: "a mutation while the policy is still being created",
            region: "us-east-1",
            blocked: Blocked::PolicyCreating,
            blocking_status: POLICY_STATUS_CREATING,
        },
        Case {
            name: "a mutation while the policy is being replaced",
            region: "eu-central-1",
            blocked: Blocked::PolicyUpdating,
            blocking_status: POLICY_STATUS_UPDATING,
        },
        Case {
            name: "a mutation while the policy is being removed",
            region: "ap-south-1",
            blocked: Blocked::PolicyDeleting,
            blocking_status: POLICY_STATUS_DELETING,
        },
        Case {
            name: "a mutation while the workspace is being renamed",
            region: "eu-north-1",
            blocked: Blocked::WorkspaceUpdating,
            blocking_status: STATUS_UPDATING,
        },
        Case {
            name: "a mutation while the workspace is being deleted",
            region: "us-east-2",
            blocked: Blocked::WorkspaceDeleting,
            blocking_status: STATUS_DELETING,
        },
    ];

    for case in &cases {
        let amp = Amp::new();
        let blocking = drive_into(case.name, &amp, case.region, case.blocked).await;

        let put = put_policy(
            &amp,
            case.region,
            &blocking.workspace_id,
            &serde_json::json!({ "policyDocument": policy_document("Racing", 1) }),
        )
        .await;
        assert_refusal(case.name, "PutResourcePolicy", &put, case.blocking_status);

        // The delete takes the optimistic lock on the revision the policy is actually at,
        // so what refuses it is the STATUS and not a stale `revisionId`. The row that
        // holds no policy has no revision to name, and a delete there would be answering
        // for a policy that does not exist at all.
        if let Some(revision_id) = blocking.revision_id.as_deref() {
            let delete = delete_policy(
                &amp,
                case.region,
                &blocking.workspace_id,
                &[("revisionId", revision_id)],
            )
            .await;
            assert_refusal(
                case.name,
                "DeleteResourcePolicy",
                &delete,
                case.blocking_status,
            );
        }
    }
}

/// The sentence carries ONE status and nothing else: no workspace id, no second status,
/// and no part of the request. Asserted on the row the recording covers — a put that
/// loses the race against an in-flight policy update — where the two statuses of the pair
/// the gate checks differ, so reporting the wrong one is visible.
#[tokio::test]
async fn the_sentence_reports_only_the_status_that_blocked_the_call() {
    let region = "us-west-2";
    let case = "a put racing an in-flight policy update";
    let amp = Amp::new();
    let blocking = drive_into(case, &amp, region, Blocked::PolicyUpdating).await;

    let put = put_policy(
        &amp,
        region,
        &blocking.workspace_id,
        &serde_json::json!({ "policyDocument": policy_document("Loser", 3) }),
    )
    .await;
    assert_refusal(case, "PutResourcePolicy", &put, POLICY_STATUS_UPDATING);

    let message = text(&put.body["message"]);
    assert!(
        !message.contains(&blocking.workspace_id),
        "case {case:?}: the workspace must not appear in the sentence, got {message:?}"
    );
    let active_mentions = message.matches(POLICY_STATUS_ACTIVE).count();
    assert_eq!(
        active_mentions, 1,
        "case {case:?}: the workspace's own ACTIVE status must not be reported beside the \
         blocking one, got {message:?}"
    );
    assert_eq!(
        message.matches(POLICY_STATUS_UPDATING).count(),
        1,
        "case {case:?}: the blocking status belongs in the sentence exactly once, \
         got {message:?}"
    );
    // The refusal also reports the resource it was about, in the members the model makes
    // required on `ConflictException`.
    assert_eq!(
        text(&put.body["resourceId"]),
        blocking.workspace_id,
        "case {case:?}: resourceId must name the workspace the policy hangs off, got {put:?}"
    );
    assert_eq!(
        text(&put.body["resourceType"]),
        "AWS::APS::ResourcePolicy",
        "case {case:?}: resourceType must name the resource-policy type, got {put:?}"
    );
}
