// SPDX-License-Identifier: CC0-1.0

//! `DeletePolicyEngine` end to end over HTTP, with the reads that observe what it did.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="DeletePolicyEngine,GetPolicyEngine")`): a
//! delete of a resource this account holds RETIRES it, so the resource stops resolving —
//! `DeletePolicyEngine` is accepted (HTTP 202) and reports the stored engine in the
//! `DELETING` status an accepted-but-unfinished deletion carries ("This is an asynchronous
//! operation. Use the GetPolicyEngine operation to poll the status field to track
//! completion"), the engine stays readable in `DELETING` until the span the clock owes
//! passes, and from then on every read of it is the modelled `ResourceNotFoundException`
//! (HTTP 404) whose message names the id the request carried. An engine that still holds a
//! policy is refused the modelled `ConflictException` (HTTP 409) — "The policy engine must
//! not have any associated policies before deletion" — and is left exactly as it was.
//!
//! The rows differ along what the rule generalises over: the shape and the length of the
//! engine's name, whether a description and an encryption key are supplied, and whether the
//! engine is still `CREATING` or already `ACTIVE` when the deletion arrives.
//!
//! The request harness and the refusal wording below are `pub` because the
//! `policy_engine_deletion_scope` suite drives the same operations with them: one copy, so
//! two suites cannot drift apart over how a request is signed or how a missing engine reads.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// How long a row waits for a clock-owed transition before it gives up, and how long it
/// waits between two looks. The deadline is generous: a row waits for the CONDITION it
/// needs, never for a span, so a slow host costs it a few more looks rather than the test.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the HTTP status and the parsed body.
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// The error code `restJson1` puts in the body's `__type`.
pub fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

pub fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub async fn create_policy_engine(router: &MultiServiceRouter, body: &Value) -> Answer {
    send(router, "POST", "/policy-engines", Some(body)).await
}

pub async fn get_policy_engine(router: &MultiServiceRouter, policy_engine: &str) -> Answer {
    send(router, "GET", &format!("/policy-engines/{policy_engine}"), None).await
}

pub async fn delete_policy_engine(router: &MultiServiceRouter, policy_engine: &str) -> Answer {
    send(
        router,
        "DELETE",
        &format!("/policy-engines/{policy_engine}"),
        None,
    )
    .await
}

pub async fn create_policy(
    router: &MultiServiceRouter,
    policy_engine: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        "POST",
        &format!("/policy-engines/{policy_engine}/policies"),
        Some(body),
    )
    .await
}

pub async fn get_policy(router: &MultiServiceRouter, policy_engine: &str, policy: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/policy-engines/{policy_engine}/policies/{policy}"),
        None,
    )
    .await
}

async fn delete_policy(router: &MultiServiceRouter, policy_engine: &str, policy: &str) -> Answer {
    send(
        router,
        "DELETE",
        &format!("/policy-engines/{policy_engine}/policies/{policy}"),
        None,
    )
    .await
}

/// Look at `GET /policy-engines/{id}` again until `accept` holds for the answer, and report
/// that answer. The wait is on the CONDITION the clock owes, bounded by a generous
/// deadline, so nothing here depends on how fast the host runs.
pub async fn poll_policy_engine_until(
    router: &MultiServiceRouter,
    policy_engine: &str,
    case: &str,
    expectation: &str,
    accept: impl Fn(&Answer) -> bool,
) -> Answer {
    let started = Instant::now();
    loop {
        let answer = get_policy_engine(router, policy_engine).await;
        if accept(&answer) {
            return answer;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: {expectation} never happened; the last read answered {} ({})",
            answer.status,
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A Cedar definition: `CedarPolicy$statement` is 35..10000 characters
/// (`smithy_query(query="shape",shape="CedarPolicy")`).
pub fn cedar_definition(action: &str) -> Value {
    json!({"cedar": {
        "statement": format!("permit(principal, action == Action::\"{action}\", resource);"),
    }})
}

/// The sentence this service refuses with when the path names no policy engine of this
/// account. The fixed text is the service's; the only variable part is the id the path
/// carried.
pub fn engine_not_found_message(policy_engine_id: &str) -> String {
    format!("Policy engine with ID {policy_engine_id} not found")
}

/// The sentence a policy READ is refused with when no policy of this account carries the
/// id the path named: a policy is addressed by its own id, so the refusal names that id.
fn policy_not_found_message(policy_id: &str) -> String {
    format!("Policy with ID {policy_id} not found")
}

/// The sentence this service refuses a delete of an engine that still holds policies with:
/// what the engine still contains, counted off the service's state.
fn engine_has_policies_message(policies: usize) -> String {
    format!("Policy engine still contains {policies} policy and cannot be deleted")
}

/// The reason a policy resource reports while a deletion the caller asked for is under way.
const DELETION_REASON: &str = "Deletion initiated by user";

/// The members `DeletePolicyEngineResponse` and `GetPolicyEngineResponse` both declare about
/// the engine itself, apart from the status the deletion moves and the instant it stamps
/// (`smithy_query(query="operation",operation="DeletePolicyEngine,GetPolicyEngine")`).
const ENGINE_MEMBERS: &[&str] = &[
    "policyEngineId",
    "name",
    "policyEngineArn",
    "description",
    "encryptionKeyArn",
    "createdAt",
];

/// One row of the deletion table.
struct Row {
    case: &'static str,
    /// The engine's name (`PolicyEngineName` is `^[A-Za-z][A-Za-z0-9_]*$`, 1..48).
    name: &'static str,
    description: Option<&'static str>,
    /// A `KmsKeyArn`: `^arn:aws(|-cn|-us-gov):kms:[a-zA-Z0-9-]*:[0-9]{12}:key/[a-zA-Z0-9-]{36}$`
    /// (`smithy_query(query="operation",operation="CreatePolicyEngine")`).
    encryption_key_arn: Option<&'static str>,
    /// Whether the clock has already settled the engine to `ACTIVE` before the deletion
    /// arrives; `false` deletes it while it is still `CREATING`.
    settled_before_delete: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "required members only, deleted while still CREATING",
            name: "Orders_engine",
            description: None,
            encryption_key_arn: None,
            settled_before_delete: false,
        },
        Row {
            case: "the shortest name the pattern admits, with a description, deleted once \
                   ACTIVE",
            name: "A",
            description: Some("one policy engine"),
            encryption_key_arn: None,
            settled_before_delete: true,
        },
        Row {
            case: "the longest name this suite uses, with a customer managed key",
            name: "Engine_with_a_long_but_valid_name_for_account",
            description: Some("x"),
            encryption_key_arn: Some(
                "arn:aws:kms:us-east-1:000000000000:key/\
                 12345678-1234-1234-1234-123456789012",
            ),
            settled_before_delete: false,
        },
    ]
}

impl Row {
    /// The `CreatePolicyEngine` body this row sends.
    fn body(&self) -> Value {
        let mut body = json!({"name": self.name});
        if let Some(description) = self.description {
            body["description"] = json!(description);
        }
        if let Some(encryption_key_arn) = self.encryption_key_arn {
            body["encryptionKeyArn"] = json!(encryption_key_arn);
        }
        body
    }
}

/// An accepted deletion reports the stored engine in `DELETING`, the engine stays readable
/// in that status until the clock-owed removal comes due, and every read of it afterwards is
/// the modelled 404 that names the id the request carried.
#[tokio::test]
async fn an_accepted_policy_engine_deletion_reports_deleting_and_then_the_engine_is_gone() {
    for row in rows() {
        let router = router();
        let created = create_policy_engine(&router, &row.body()).await;
        assert_eq!(
            created.status, 202,
            "case {:?}: expected HTTP 202 from CreatePolicyEngine, got {} ({})",
            row.case, created.status, created.body
        );
        let policy_engine_id = text(&created.body, "policyEngineId");

        if row.settled_before_delete {
            let active = poll_policy_engine_until(
                &router,
                &policy_engine_id,
                row.case,
                "the engine settles to ACTIVE",
                |answer| answer.status == 200 && text(&answer.body, "status") == "ACTIVE",
            )
            .await;
            assert_eq!(
                text(&active.body, "status"),
                "ACTIVE",
                "case {:?}: the engine settles to ACTIVE before the deletion, got {}",
                row.case,
                active.body
            );
        }

        let deleted = delete_policy_engine(&router, &policy_engine_id).await;
        assert_eq!(
            deleted.status, 202,
            "case {:?}: expected HTTP 202 from DeletePolicyEngine, got {} ({})",
            row.case, deleted.status, deleted.body
        );
        assert_eq!(
            (
                text(&deleted.body, "status"),
                deleted.body.get("statusReasons").cloned()
            ),
            (
                "DELETING".to_string(),
                Some(json!([DELETION_REASON]))
            ),
            "case {:?}: an accepted deletion reports the engine DELETING with the reason the \
             caller's own deletion is, got {}",
            row.case,
            deleted.body
        );
        for member in ENGINE_MEMBERS {
            assert_eq!(
                deleted.body.get(member),
                created.body.get(member),
                "case {:?}: the deletion reports the {member} the create did, got {}",
                row.case,
                deleted.body
            );
        }
        assert!(
            !text(&deleted.body, "updatedAt").is_empty(),
            "case {:?}: the deletion reports when the engine was last modified, got {}",
            row.case,
            deleted.body
        );

        // The deletion is asynchronous, so the engine is still readable — in `DELETING`,
        // which is what a caller polling the status field tracks completion by.
        let polled = get_policy_engine(&router, &policy_engine_id).await;
        assert_eq!(
            (polled.status, text(&polled.body, "status")),
            (200, "DELETING".to_string()),
            "case {:?}: the engine polls DELETING while the deletion is under way, got {} \
             ({})",
            row.case,
            polled.status,
            polled.body
        );
        assert_eq!(
            polled.body.get("statusReasons").cloned(),
            Some(json!([DELETION_REASON])),
            "case {:?}: the read reports the same reason the deletion did, got {}",
            row.case,
            polled.body
        );
        for member in ENGINE_MEMBERS {
            assert_eq!(
                polled.body.get(member),
                created.body.get(member),
                "case {:?}: an engine on its way out still reports its {member}, got {}",
                row.case,
                polled.body
            );
        }

        // Once the span the clock owes passes, the engine resolves nothing at all.
        let gone = poll_policy_engine_until(
            &router,
            &policy_engine_id,
            row.case,
            "the deleted engine stops resolving",
            |answer| answer.status != 200,
        )
        .await;
        assert_eq!(
            (
                gone.status,
                error_code(&gone.body),
                text(&gone.body, "message")
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                engine_not_found_message(&policy_engine_id)
            ),
            "case {:?}: once the deletion completes the engine is the modelled 404, got {} \
             ({})",
            row.case,
            gone.status,
            gone.body
        );

        // And it stays gone: a read a moment later answers the same way, rather than the
        // resource coming back.
        let still_gone = get_policy_engine(&router, &policy_engine_id).await;
        assert_eq!(
            (still_gone.status, text(&still_gone.body, "message")),
            (404, engine_not_found_message(&policy_engine_id)),
            "case {:?}: a retired engine does not come back, got {} ({})",
            row.case,
            still_gone.status,
            still_gone.body
        );

        // Every other operation that addresses the engine resolves nothing either: a
        // delete of it is the same 404, and so is a create of a policy in it.
        let repeated = delete_policy_engine(&router, &policy_engine_id).await;
        assert_eq!(
            (repeated.status, text(&repeated.body, "message")),
            (404, engine_not_found_message(&policy_engine_id)),
            "case {:?}: a delete of a retired engine is the modelled 404, got {} ({})",
            row.case,
            repeated.status,
            repeated.body
        );
        let orphan = create_policy(
            &router,
            &policy_engine_id,
            &json!({"name": "AfterTheEngineWent", "definition": cedar_definition("invokeTool")}),
        )
        .await;
        assert_eq!(
            (orphan.status, text(&orphan.body, "message")),
            (404, engine_not_found_message(&policy_engine_id)),
            "case {:?}: nothing is created in a retired engine, got {} ({})",
            row.case,
            orphan.status,
            orphan.body
        );
    }
}

/// `DeletePolicyEngine` is `@idempotent`: a repeat while the deletion is still under way
/// reports the same engine and does not re-arm the clock that owes its removal.
#[tokio::test]
async fn repeating_a_policy_engine_deletion_under_way_reports_the_same_engine() {
    let router = router();
    let created = create_policy_engine(&router, &json!({"name": "Repeat_engine"})).await;
    assert_eq!(
        created.status, 202,
        "setup: the engine must be created, got {} ({})",
        created.status, created.body
    );
    let policy_engine_id = text(&created.body, "policyEngineId");

    let first = delete_policy_engine(&router, &policy_engine_id).await;
    assert_eq!(
        first.status, 202,
        "the first delete is accepted, got {} ({})",
        first.status, first.body
    );

    // Repeats while the deletion is under way: each is accepted, reports the same engine,
    // and leaves the instant the first delete stamped alone, which is what makes the
    // operation idempotent rather than a second acceptance.
    for attempt in 1..=3u32 {
        let repeated = delete_policy_engine(&router, &policy_engine_id).await;
        if repeated.status == 404 {
            // The clock the first delete armed came due between two repeats, which is the
            // other correct answer here: the engine is gone.
            assert_eq!(
                text(&repeated.body, "message"),
                engine_not_found_message(&policy_engine_id),
                "case repeat {attempt}: a delete of a retired engine is the modelled 404, \
                 got {}",
                repeated.body
            );
            break;
        }
        assert_eq!(
            (repeated.status, text(&repeated.body, "status")),
            (202, "DELETING".to_string()),
            "case repeat {attempt}: a repeat while the deletion is under way is accepted and \
             still DELETING, got {} ({})",
            repeated.status,
            repeated.body
        );
        for member in ENGINE_MEMBERS {
            assert_eq!(
                repeated.body.get(member),
                first.body.get(member),
                "case repeat {attempt}: the repeat reports the {member} the first delete \
                 did, got {}",
                repeated.body
            );
        }
        assert_eq!(
            repeated.body.get("updatedAt"),
            first.body.get("updatedAt"),
            "case repeat {attempt}: a repeat changes nothing, so the engine was not modified \
             again, got {}",
            repeated.body
        );
    }

    // The clock the FIRST delete armed is the one that removes the record.
    let gone = poll_policy_engine_until(
        &router,
        &policy_engine_id,
        "repeated deletes",
        "the deleted engine stops resolving",
        |answer| answer.status != 200,
    )
    .await;
    assert_eq!(
        (gone.status, text(&gone.body, "message")),
        (404, engine_not_found_message(&policy_engine_id)),
        "the engine is gone once the span the first delete armed passes, got {} ({})",
        gone.status,
        gone.body
    );
}

/// "The policy engine must not have any associated policies before deletion": an engine
/// still holding one is refused the modelled 409 and left exactly as it was, and the policy
/// it holds is untouched. A policy whose own deletion has been accepted is on its way out
/// and holds the engine back no longer; once the engine goes, so does what it held.
#[tokio::test]
async fn an_engine_that_still_holds_a_policy_is_refused_and_keeps_its_policy() {
    let router = router();
    let created = create_policy_engine(&router, &json!({"name": "Holds_policies"})).await;
    let policy_engine_id = text(&created.body, "policyEngineId");
    let policy = create_policy(
        &router,
        &policy_engine_id,
        &json!({"name": "StillEnforced", "definition": cedar_definition("invokeTool")}),
    )
    .await;
    assert_eq!(
        policy.status, 202,
        "setup: the policy must be created, got {} ({})",
        policy.status, policy.body
    );
    let policy_id = text(&policy.body, "policyId");

    // A second policy in the same engine, so the count the refusal carries is read off the
    // state rather than fixed at one.
    let second = create_policy(
        &router,
        &policy_engine_id,
        &json!({"name": "AlsoEnforced", "definition": cedar_definition("listTools")}),
    )
    .await;
    assert_eq!(
        second.status, 202,
        "setup: the second policy must be created, got {} ({})",
        second.status, second.body
    );
    let second_id = text(&second.body, "policyId");
    let refused_two = delete_policy_engine(&router, &policy_engine_id).await;
    assert_eq!(
        (
            refused_two.status,
            error_code(&refused_two.body),
            text(&refused_two.body, "message")
        ),
        (
            409,
            "ConflictException".to_string(),
            engine_has_policies_message(2)
        ),
        "the refusal counts every policy the engine still holds, got {} ({})",
        refused_two.status,
        refused_two.body
    );

    // One of the two deleted: the count the refusal carries follows the state down.
    let second_deleted = delete_policy(&router, &policy_engine_id, &second_id).await;
    assert_eq!(
        second_deleted.status, 202,
        "setup: the second policy's deletion must be accepted, got {} ({})",
        second_deleted.status, second_deleted.body
    );

    let refused = delete_policy_engine(&router, &policy_engine_id).await;
    assert_eq!(
        (
            refused.status,
            error_code(&refused.body),
            text(&refused.body, "message")
        ),
        (
            409,
            "ConflictException".to_string(),
            engine_has_policies_message(1)
        ),
        "an engine that still holds a policy is refused the modelled 409, got {} ({})",
        refused.status,
        refused.body
    );

    // The refusal changed nothing: the engine is readable and not on its way out, and the
    // policy it holds is still there.
    let intact = get_policy_engine(&router, &policy_engine_id).await;
    assert_eq!(
        intact.status, 200,
        "a refused delete leaves the engine readable, got {} ({})",
        intact.status, intact.body
    );
    assert_ne!(
        text(&intact.body, "status"),
        "DELETING",
        "a refused delete does not start the engine on its way out, got {}",
        intact.body
    );
    let policy_intact = get_policy(&router, &policy_engine_id, &policy_id).await;
    assert_eq!(
        policy_intact.status, 200,
        "a refused engine delete leaves the policy it holds readable, got {} ({})",
        policy_intact.status, policy_intact.body
    );

    // A policy whose own deletion has been accepted is associated with nothing the caller
    // still has to delete, so the engine's deletion is accepted from then on.
    let policy_deleted = delete_policy(&router, &policy_engine_id, &policy_id).await;
    assert_eq!(
        policy_deleted.status, 202,
        "the policy's own deletion is accepted, got {} ({})",
        policy_deleted.status, policy_deleted.body
    );
    let accepted = delete_policy_engine(&router, &policy_engine_id).await;
    assert_eq!(
        (accepted.status, text(&accepted.body, "status")),
        (202, "DELETING".to_string()),
        "an engine whose last policy is already on its way out is deletable, got {} ({})",
        accepted.status,
        accepted.body
    );

    // What the engine held goes with it: the policy is addressed only through the engine
    // that evaluates it, so once the engine is retired the policy resolves nothing either.
    let gone = poll_policy_engine_until(
        &router,
        &policy_engine_id,
        "an engine whose policy was deleted first",
        "the deleted engine stops resolving",
        |answer| answer.status != 200,
    )
    .await;
    assert_eq!(
        (gone.status, text(&gone.body, "message")),
        (404, engine_not_found_message(&policy_engine_id)),
        "the engine is the modelled 404 once its deletion completes, got {} ({})",
        gone.status,
        gone.body
    );
    let policy_gone = get_policy(&router, &policy_engine_id, &policy_id).await;
    assert_eq!(
        (policy_gone.status, text(&policy_gone.body, "message")),
        (404, policy_not_found_message(&policy_id)),
        "a policy of a retired engine resolves nothing, and the read names the policy it \
         asked for, got {} ({})",
        policy_gone.status,
        policy_gone.body
    );
}

/// A delete the path cannot resolve is refused the modelled 404, names the id it carried,
/// and deletes nothing — including an engine of this account that the request did not name.
#[tokio::test]
async fn a_policy_engine_delete_the_path_does_not_resolve_is_refused_and_deletes_nothing() {
    let router = router();
    // Every id below satisfies `ResourceId`'s pattern `^[A-Za-z][A-Za-z0-9_]*-[a-z0-9_]{10}$`,
    // so the refusal is the handler's lookup and not the boundary constraint walk: the
    // shortest id the pattern admits, one carrying underscores, one at the top of the
    // length range, and one whose suffix is all underscores.
    let unknown = [
        "A-0123456789",
        "Missing_engine-abcdefghij",
        "Engine_with_a_long_but_valid_name_for_account-zz00zz00zz",
        "Underscores-__________",
    ];

    let created = create_policy_engine(&router, &json!({"name": "Bystander_engine"})).await;
    let policy_engine_id = text(&created.body, "policyEngineId");

    for id in unknown {
        let refused = delete_policy_engine(&router, id).await;
        assert_eq!(
            (
                refused.status,
                error_code(&refused.body),
                text(&refused.body, "message")
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                engine_not_found_message(id)
            ),
            "case {id:?}: the refusal names the engine the path carried, got {} ({})",
            refused.status,
            refused.body
        );
    }

    let bystander = get_policy_engine(&router, &policy_engine_id).await;
    assert_eq!(
        bystander.status, 200,
        "a refused delete leaves the account's other engine readable, got {} ({})",
        bystander.status, bystander.body
    );
    assert_ne!(
        text(&bystander.body, "status"),
        "DELETING",
        "a refused delete starts no other engine on its way out, got {}",
        bystander.body
    );
}
