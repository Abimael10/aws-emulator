// SPDX-License-Identifier: CC0-1.0

//! How far a `DeletePolicyEngine` reaches, and what it refuses before it reaches anything.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! `MultiServiceRouter::dispatch`, so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//! The harness, the reads and the refusal wording come from the `policy_engine_deletion`
//! suite, which already pins what an accepted deletion reports.
//!
//! The rules under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="DeletePolicyEngine")`):
//!
//! 1. A delete addresses ONE resource — the engine the `@httpLabel` names — so it retires
//!    that engine and nothing else: every other engine this account holds, and everything
//!    those engines hold, is exactly what it was both while the deletion is under way and
//!    after the retirement comes due.
//! 2. A path identifier INSIDE the published grammar of the member that names the resource
//!    (`ResourceId`, pattern `^[A-Za-z][A-Za-z0-9_]*-[a-z0-9_]{10}$`, len 12..59) addresses a
//!    resource of the family: within the published length window a delete carrying one no
//!    engine of this account answers to reaches the handler and is the modelled
//!    `ResourceNotFoundException` (HTTP 404) naming it, and outside that window it is the
//!    member's own modelled `ValidationException` (HTTP 400). An identifier outside the
//!    grammar can address no resource of the family at all, so the call names none of the
//!    account's resources and is decided on the caller's own grants, which for the credential
//!    this emulator publishes is a 403 on the action reporting nothing about the member. None
//!    of the three retires anything.
//!
//! The rows differ along what each rule generalises over: how many engines the account
//! holds and which of them the request names (rule 1), and the length and the shape of the
//! identifier the path carries — each published length bound and one character past it
//! (rule 2).

use serde_json::json;

use crate::policy_engine_deletion::{
    cedar_definition, create_policy, create_policy_engine, delete_policy_engine,
    engine_not_found_message, error_code, get_policy, get_policy_engine, poll_policy_engine_until,
    router, text,
};

/// The modelled bounds of `ResourceId`, the shape `policyEngineId` targets
/// (`smithy_query(query="shape",shape="ResourceId")`).
const RESOURCE_ID_MIN_LENGTH: usize = 12;
const RESOURCE_ID_MAX_LENGTH: usize = 59;

/// The ten-character suffix every identifier this service mints ends in, which is also what
/// `ResourceId`'s pattern requires after the separator.
const RESOURCE_ID_SUFFIX: &str = "0123456789";

/// A `ResourceId`-shaped identifier of exactly `length` characters: a name part padded out
/// to the length wanted, the separator, and the ten-character suffix. Used to drive the
/// length bound itself, so each row's only difference is how long the identifier is.
/// `length` is at least the minimum the pattern itself admits (a letter, the separator and
/// the ten-character suffix).
fn resource_id_of_length(length: usize) -> String {
    format!(
        "E{}-{RESOURCE_ID_SUFFIX}",
        "e".repeat(length - RESOURCE_ID_MIN_LENGTH)
    )
}

/// One row of the isolation table: how many engines the account holds, and which of them
/// (by creation order) the deletion names.
struct Scope {
    case: &'static str,
    engines: usize,
    deleted: usize,
}

fn scopes() -> Vec<Scope> {
    vec![
        Scope {
            case: "the first of two engines",
            engines: 2,
            deleted: 0,
        },
        Scope {
            case: "the middle of three engines",
            engines: 3,
            deleted: 1,
        },
        Scope {
            case: "the last of four engines",
            engines: 4,
            deleted: 3,
        },
    ]
}

/// A delete retires the engine the path names and leaves every other engine of the account,
/// and every policy those engines hold, exactly as it was.
#[tokio::test]
async fn deleting_a_policy_engine_leaves_every_other_engine_and_its_policies_untouched() {
    for scope in scopes() {
        let router = router();

        // The account's engines. Every one except the one to be deleted holds a policy, so
        // a bystander is a resource with children rather than an empty record.
        let mut engine_ids = Vec::new();
        let mut policy_ids = Vec::new();
        for index in 0..scope.engines {
            let created =
                create_policy_engine(&router, &json!({"name": format!("Engine_{index}")})).await;
            assert_eq!(
                created.status, 202,
                "case {:?}: engine {index} must be created, got {} ({})",
                scope.case, created.status, created.body
            );
            let policy_engine_id = text(&created.body, "policyEngineId");
            let policy_id = if index == scope.deleted {
                None
            } else {
                let policy = create_policy(
                    &router,
                    &policy_engine_id,
                    &json!({
                        "name": format!("Policy_{index}"),
                        "definition": cedar_definition("invokeTool"),
                    }),
                )
                .await;
                assert_eq!(
                    policy.status, 202,
                    "case {:?}: the policy of engine {index} must be created, got {} ({})",
                    scope.case, policy.status, policy.body
                );
                Some(text(&policy.body, "policyId"))
            };
            engine_ids.push(policy_engine_id);
            policy_ids.push(policy_id);
        }

        let target = engine_ids[scope.deleted].clone();
        let deleted = delete_policy_engine(&router, &target).await;
        assert_eq!(
            (deleted.status, text(&deleted.body, "status")),
            (202, "DELETING".to_string()),
            "case {:?}: the named engine's deletion is accepted, got {} ({})",
            scope.case,
            deleted.status,
            deleted.body
        );

        // While the deletion is under way, every other engine is readable, is on its way
        // out in no way, and still holds its own policy.
        assert_bystanders_intact(
            &router,
            scope.case,
            "under way",
            &engine_ids,
            &policy_ids,
            scope.deleted,
        )
        .await;

        // Once the clock-owed retirement comes due, the named engine resolves nothing —
        // and the others are still exactly what they were.
        let gone = poll_policy_engine_until(
            &router,
            &target,
            scope.case,
            "the deleted engine stops resolving",
            |answer| answer.status != 200,
        )
        .await;
        assert_eq!(
            (gone.status, text(&gone.body, "message")),
            (404, engine_not_found_message(&target)),
            "case {:?}: the retired engine is the modelled 404, got {} ({})",
            scope.case,
            gone.status,
            gone.body
        );
        assert_bystanders_intact(
            &router,
            scope.case,
            "completed",
            &engine_ids,
            &policy_ids,
            scope.deleted,
        )
        .await;
    }
}

/// Every engine of the account except `deleted` reads back, carries no deletion of its own,
/// and still holds the policy it was created with. `phase` names when the check runs.
async fn assert_bystanders_intact(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    phase: &str,
    engine_ids: &[String],
    policy_ids: &[Option<String>],
    deleted: usize,
) {
    for (index, policy_engine_id) in engine_ids.iter().enumerate() {
        if index == deleted {
            continue;
        }
        let bystander = get_policy_engine(router, policy_engine_id).await;
        assert_eq!(
            bystander.status, 200,
            "case {case:?} ({phase}): engine {index} is still readable, got {} ({})",
            bystander.status, bystander.body
        );
        assert_eq!(
            text(&bystander.body, "policyEngineId"),
            *policy_engine_id,
            "case {case:?} ({phase}): engine {index} reads back under its own id, got {}",
            bystander.body
        );
        assert_ne!(
            text(&bystander.body, "status"),
            "DELETING",
            "case {case:?} ({phase}): engine {index} was not started on its way out, got {}",
            bystander.body
        );
        let Some(policy_id) = policy_ids[index].as_deref() else {
            continue;
        };
        let policy = get_policy(router, policy_engine_id, policy_id).await;
        assert_eq!(
            (policy.status, text(&policy.body, "policyId")),
            (200, policy_id.to_string()),
            "case {case:?} ({phase}): the policy of engine {index} is still readable, got {} \
             ({})",
            policy.status,
            policy.body
        );
    }
}

/// The action a statement spells this operation under, which is what a refusal of a call no
/// readable grant covers states (`bedrock-agentcore` is this endpoint's signing name and ARN
/// namespace — `smithy_query(query="service_info")`).
const DELETE_ACTION: &str = "bedrock-agentcore:DeletePolicyEngine";

/// The member the route addresses its engine by, which a constraint clause names and an
/// authorization refusal does not.
const MEMBER: &str = "policyEngineId";

/// One row of the identifier table: the identifier the path carries, and the refusal that
/// identifier earns the request.
struct Identifier {
    case: &'static str,
    id: String,
    refusal: Refusal,
}

/// What a delete carrying an identifier of this kind is answered.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// Inside the member's published grammar and length window, so it addresses a resource of
    /// the family and reaches the handler, which holds no engine under it.
    NotFound,
    /// Inside the grammar but outside the published length window, so the member's own
    /// constraint clause is what the request is answered.
    Constraint,
    /// Outside the published grammar, so no resource of the family can be addressed by it and
    /// the call names none of the account's resources.
    Denied,
}

fn identifiers() -> Vec<Identifier> {
    vec![
        Identifier {
            // One character under the minimum length. The grammar admits nothing shorter
            // than the minimum either — a letter, the separator and a ten-character suffix
            // are already the minimum — so this row is short by exactly one character of
            // that suffix.
            case: "one character under the minimum length",
            id: format!("E-{}", &RESOURCE_ID_SUFFIX[..RESOURCE_ID_SUFFIX.len() - 1]),
            refusal: Refusal::Denied,
        },
        Identifier {
            case: "exactly the minimum length",
            id: resource_id_of_length(RESOURCE_ID_MIN_LENGTH),
            refusal: Refusal::NotFound,
        },
        Identifier {
            case: "exactly the maximum length",
            id: resource_id_of_length(RESOURCE_ID_MAX_LENGTH),
            refusal: Refusal::NotFound,
        },
        Identifier {
            case: "one character over the maximum length",
            id: resource_id_of_length(RESOURCE_ID_MAX_LENGTH + 1),
            refusal: Refusal::Constraint,
        },
        Identifier {
            case: "an identifier that does not start with a letter",
            id: "0engine-abcdefghij".to_string(),
            refusal: Refusal::Denied,
        },
        Identifier {
            case: "an identifier whose suffix is not lower case",
            id: "Engine-ABCDEFGHIJ".to_string(),
            refusal: Refusal::Denied,
        },
        Identifier {
            case: "an identifier carrying no separator",
            id: "Engineabcdefghijk".to_string(),
            refusal: Refusal::Denied,
        },
        Identifier {
            case: "an identifier whose suffix is one character short",
            id: "Engine_name-abcdefghi".to_string(),
            refusal: Refusal::Denied,
        },
    ]
}

/// A delete whose identifier the modelled constraint admits, but which names no engine of
/// this account, is the modelled 404 naming that identifier; one carrying an identifier no
/// resource of the family can be addressed by names no resource of the account at all and is
/// refused 403 on the action, with nothing reported about the member. Neither retires
/// anything.
#[tokio::test]
async fn a_policy_engine_delete_naming_no_engine_of_the_account_retires_nothing() {
    let router = router();
    let created = create_policy_engine(&router, &json!({"name": "Bystander_of_refusals"})).await;
    assert_eq!(
        created.status, 202,
        "setup: the bystander engine must be created, got {} ({})",
        created.status, created.body
    );
    let bystander_id = text(&created.body, "policyEngineId");
    let policy = create_policy(
        &router,
        &bystander_id,
        &json!({"name": "StaysEnforced", "definition": cedar_definition("listTools")}),
    )
    .await;
    assert_eq!(
        policy.status, 202,
        "setup: the bystander's policy must be created, got {} ({})",
        policy.status, policy.body
    );
    let policy_id = text(&policy.body, "policyId");

    for row in identifiers() {
        let refused = delete_policy_engine(&router, &row.id).await;
        let message = text(&refused.body, "message");
        match row.refusal {
            Refusal::NotFound => assert_eq!(
                (refused.status, error_code(&refused.body), message),
                (
                    404,
                    "ResourceNotFoundException".to_string(),
                    engine_not_found_message(&row.id)
                ),
                "case {:?} ({}): an identifier a resource of the family can carry that names no \
                 engine is the modelled 404, got {} ({})",
                row.case,
                row.id,
                refused.status,
                refused.body
            ),
            Refusal::Constraint => {
                assert_eq!(
                    (refused.status, error_code(&refused.body)),
                    (400, "ValidationException".to_string()),
                    "case {:?} ({}): an identifier outside the member's published length window \
                     is the modelled 400, got {} ({})",
                    row.case,
                    row.id,
                    refused.status,
                    refused.body
                );
                assert!(
                    message.contains(MEMBER)
                        && message.contains(&RESOURCE_ID_MAX_LENGTH.to_string()),
                    "case {:?} ({}): the clause names the member and the bound it failed, got {}",
                    row.case,
                    row.id,
                    refused.body
                );
            }
            Refusal::Denied => {
                assert_eq!(
                    (refused.status, error_code(&refused.body)),
                    (403, "AccessDeniedException".to_string()),
                    "case {:?} ({}): an identifier no resource of the family can carry addresses \
                     none of the account's resources, so the call is refused on the action, got \
                     {} ({})",
                    row.case,
                    row.id,
                    refused.status,
                    refused.body
                );
                assert!(
                    message.contains(DELETE_ACTION),
                    "case {:?} ({}): the refusal states the action it asked for, got {}",
                    row.case,
                    row.id,
                    refused.body
                );
                assert!(
                    !message.contains(MEMBER),
                    "case {:?} ({}): the refusal reports nothing about the member, got {}",
                    row.case,
                    row.id,
                    refused.body
                );
            }
        }
    }

    // Nothing of the account changed under the refusals: the one engine it holds is
    // readable, is on its way out in no way, and still holds its policy.
    let bystander = get_policy_engine(&router, &bystander_id).await;
    assert_eq!(
        bystander.status, 200,
        "a refused delete leaves the account's engine readable, got {} ({})",
        bystander.status, bystander.body
    );
    assert_ne!(
        text(&bystander.body, "status"),
        "DELETING",
        "a refused delete starts no engine on its way out, got {}",
        bystander.body
    );
    let kept = get_policy(&router, &bystander_id, &policy_id).await;
    assert_eq!(
        (kept.status, text(&kept.body, "policyId")),
        (200, policy_id.clone()),
        "a refused delete leaves the policy the engine holds readable, got {} ({})",
        kept.status,
        kept.body
    );
}
