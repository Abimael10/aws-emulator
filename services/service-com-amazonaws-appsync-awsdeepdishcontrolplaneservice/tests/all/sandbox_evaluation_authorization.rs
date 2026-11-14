// SPDX-License-Identifier: CC0-1.0

//! IAM's decision in front of the two SANDBOX evaluations, end to end over HTTP:
//! `EvaluateCode` (`POST /v1/dataplane-evaluatecode`) and `EvaluateMappingTemplate`
//! (`POST /v1/dataplane-evaluatetemplate`), each modelling `AccessDeniedException` at HTTP
//! 403 (`smithy_query(query="operation",
//! operation="EvaluateCode,EvaluateMappingTemplate")`).
//!
//! The rule under test: neither evaluation addresses a resource of this service — the URI
//! carries no label and the input names none — so the action is judged against every
//! resource of this service in the account and region the request addresses, and a caller
//! whose own identity-based documents do not allow that evaluation's action there is refused
//! `AccessDeniedException` (403) with IAM's refusal sentence. The decision belongs to the
//! layer in FRONT of the operation, so it is taken on the way in whether or not the
//! operation behind it is implemented: a caller the documents allow reaches the operation
//! and gets the operation's OWN answer, and a caller they do not is refused before anything
//! runs. And it is taken from the documents in force at the moment of the request, so
//! removing the grant takes effect on the next call and re-attaching it restores access.
//!
//! The tables span what the rule generalises over: WHICH evaluation is asked for (two
//! actions, two URI paths, two input shapes), WHAT the caller's documents say (nothing at
//! all, the other evaluation's action, this action on one API's ARN rather than on the
//! account's resources, an explicit deny beside a wildcard allow, a wildcard allow of this
//! service) and the KIND of principal (an IAM user, an assumed-role session).
//!
//! The principals are made with IAM's and STS's own operations over the same engine, so the
//! documents this service reads are the ones those services wrote; the harness is the
//! identity-authorization suite's.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::authorization::{
    ACTION_PREFIX, ANY_RESOURCE,
};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use crate::identity_authorization::{
    allow, allow_all_but, arn, default_deny_message, Emulator, ACCESS_DENIED,
    ACCESS_DENIED_STATUS, ACCOUNT_KEY,
};

/// An API id of the modelled length (`state::API_ID_LEN`) that nothing created, used only to
/// write a policy about ONE resource of this service — which is not what an evaluation that
/// addresses no resource is judged against.
const ANOTHER_API_ID: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzz";

/// One sandbox evaluation under test: its URI path, the action IAM judges it as, and a
/// request body the operation accepts.
///
/// Source: `smithy_query(query="operation", operation="EvaluateCode,EvaluateMappingTemplate")`
/// — `EvaluateCode` requires `runtime`, `code` and `context`; `EvaluateMappingTemplate`
/// requires `template` and `context`.
struct Evaluation {
    operation: &'static str,
    path: &'static str,
    body: fn() -> Value,
}

/// A code evaluation's own input: the one supported runtime, a resolver's request function,
/// and the context it is called over.
fn code_body() -> Value {
    json!({
        "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
        "code": "export function request(ctx) { return {ok: true} }",
        "context": "{\"arguments\":{}}",
    })
}

/// A mapping-template evaluation's own input: a template and the context it is rendered
/// over.
fn template_body() -> Value {
    json!({
        "template": "$util.toJson($context)",
        "context": "{\"arguments\":{}}",
    })
}

const EVALUATIONS: &[Evaluation] = &[
    Evaluation {
        operation: "EvaluateCode",
        path: "/v1/dataplane-evaluatecode",
        body: code_body,
    },
    Evaluation {
        operation: "EvaluateMappingTemplate",
        path: "/v1/dataplane-evaluatetemplate",
        body: template_body,
    },
];

/// The OTHER evaluation's action: what a document allowing it must still not serve this one.
fn other_evaluation(operation: &str) -> &'static str {
    EVALUATIONS
        .iter()
        .map(|evaluation| evaluation.operation)
        .find(|name| *name != operation)
        .expect("the table holds both evaluations")
}

/// What a refused caller's documents say, and the clause of IAM's sentence that answers it.
struct Refused {
    case: &'static str,
    document: fn(&str) -> Option<String>,
    assumed_role: bool,
    explicit_deny: bool,
}

/// No document at all: IAM's default deny.
fn nothing(_operation: &str) -> Option<String> {
    None
}

/// An allow of the other evaluation's action on everything.
fn the_other_evaluations_action(operation: &str) -> Option<String> {
    Some(allow(
        &format!("{ACTION_PREFIX}:{}", other_evaluation(operation)),
        ANY_RESOURCE,
    ))
}

/// An allow of THIS action, but about one API's own ARN rather than about the account's
/// resources, which is what an evaluation addressing no resource is judged against.
fn this_action_on_one_api(operation: &str) -> Option<String> {
    Some(allow(
        &format!("{ACTION_PREFIX}:{operation}"),
        &arn(&state::api_resource(ANOTHER_API_ID)),
    ))
}

/// A wildcard allow over this service with an explicit deny of this action beside it.
fn an_explicit_deny(operation: &str) -> Option<String> {
    Some(allow_all_but(&format!("{ACTION_PREFIX}:{operation}")))
}

const REFUSALS: &[Refused] = &[
    Refused {
        case: "an IAM user whose documents allow nothing",
        document: nothing,
        assumed_role: false,
        explicit_deny: false,
    },
    Refused {
        case: "an assumed-role session whose role allows nothing",
        document: nothing,
        assumed_role: true,
        explicit_deny: false,
    },
    Refused {
        case: "an allow of the other evaluation's action",
        document: the_other_evaluations_action,
        assumed_role: false,
        explicit_deny: false,
    },
    Refused {
        case: "an allow of this action on one API rather than on the account's resources",
        document: this_action_on_one_api,
        assumed_role: false,
        explicit_deny: false,
    },
    Refused {
        case: "an explicit deny beside a wildcard allow",
        document: an_explicit_deny,
        assumed_role: true,
        explicit_deny: true,
    },
];

/// IAM's refusal sentence for an evaluation: the principal, the action, the resources such a
/// call is judged against, and which of IAM's rules withheld the grant.
fn expected_message(principal: &str, operation: &str, explicit_deny: bool) -> String {
    if explicit_deny {
        format!(
            "User: {principal} is not authorized to perform: {ACTION_PREFIX}:{operation} on \
             resource: {} with an explicit deny in an identity-based policy",
            arn(ANY_RESOURCE)
        )
    } else {
        default_deny_message(principal, operation, &arn(ANY_RESOURCE))
    }
}

/// RULE: a caller whose identity-based documents do not allow a sandbox evaluation's own
/// action on the resources such a call is judged against is refused 403
/// `AccessDeniedException` with IAM's sentence, and the refusal carries nothing of the
/// evaluation — no result, no logs, no stash.
#[tokio::test]
async fn an_unauthorized_caller_is_refused_both_sandbox_evaluations() {
    let emulator = Emulator::new();

    for evaluation in EVALUATIONS {
        for (index, row) in REFUSALS.iter().enumerate() {
            let label = format!("{}_{index}", evaluation.operation.to_lowercase());
            let document = (row.document)(evaluation.operation);
            let principal = if row.assumed_role {
                emulator.assumed_role(&label, document.as_deref()).await
            } else {
                emulator.user(&label, document.as_deref()).await
            };
            let case = format!("{} for {}", evaluation.operation, row.case);
            let reply = emulator
                .call(
                    &principal.access_key_id,
                    "POST",
                    evaluation.path,
                    Some(&(evaluation.body)()),
                )
                .await;
            assert_eq!(
                (reply.status, reply.error_code.as_str()),
                (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                "case {case}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} \
                 with {:?}",
                reply.status,
                reply.error_code,
                reply.body
            );
            assert_eq!(
                reply.message(),
                expected_message(&principal.arn, evaluation.operation, row.explicit_deny),
                "case {case}: expected IAM's refusal sentence about this action and the \
                 resources the evaluation is judged against, got {:?}",
                reply.message()
            );
            for member in ["evaluationResult", "logs", "stash", "outErrors"] {
                assert!(
                    reply.body.get(member).is_none(),
                    "case {case}: expected the refusal to carry no {member}, got {:?}",
                    reply.body
                );
            }
        }
    }
}

/// RULE: the decision is taken in FRONT of the operation, so a caller the documents allow
/// reaches it and is answered by the operation itself rather than by the gate — whatever
/// that answer is — and the decision follows the documents as they change: removing the
/// grant refuses the identical call on the next request, and re-attaching it lets it through
/// again.
#[tokio::test]
async fn the_grant_in_force_at_the_request_decides_each_evaluation() {
    let emulator = Emulator::new();
    let user_name = "sandbox_grant_cycle";
    let grant = allow(&format!("{ACTION_PREFIX}:*"), ANY_RESOURCE);
    let principal = emulator.user(user_name, Some(&grant)).await;

    // The account's own credential holds no IAM policies, so it stands for a caller this
    // gate does not bound: whatever the operation answers it, the granted caller gets the
    // same answer, which is what makes the refusals below the gate's work and not the
    // operation's.
    for evaluation in EVALUATIONS {
        let body = (evaluation.body)();
        let ambient = emulator
            .call(ACCOUNT_KEY, "POST", evaluation.path, Some(&body))
            .await;
        let granted = emulator
            .call(
                &principal.access_key_id,
                "POST",
                evaluation.path,
                Some(&body),
            )
            .await;
        assert_ne!(
            (ambient.status, ambient.error_code.as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {:?}: expected the caller this gate does not bound to reach the operation, \
             got {:?}",
            evaluation.operation,
            ambient.body
        );
        assert_eq!(
            (granted.status, granted.error_code.as_str()),
            (ambient.status, ambient.error_code.as_str()),
            "case {:?} under a wildcard allow: expected the caller to reach the operation \
             and get its own answer ({} {:?}), got {} {:?} with {:?}",
            evaluation.operation,
            ambient.status,
            ambient.error_code,
            granted.status,
            granted.error_code,
            granted.body
        );

        // The grant is gone: the identical call is refused on the authorization boundary.
        emulator.delete_user_policy(user_name).await;
        let refused = emulator
            .call(
                &principal.access_key_id,
                "POST",
                evaluation.path,
                Some(&body),
            )
            .await;
        assert_eq!(
            (refused.status, refused.error_code.as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {:?} once the grant is removed: expected {ACCESS_DENIED_STATUS} \
             {ACCESS_DENIED}, got {} {:?} with {:?}",
            evaluation.operation,
            refused.status,
            refused.error_code,
            refused.body
        );
        assert_eq!(
            refused.message(),
            expected_message(&principal.arn, evaluation.operation, false),
            "case {:?} once the grant is removed: expected IAM's default-deny sentence, got \
             {:?}",
            evaluation.operation,
            refused.message()
        );

        // And the identical document again: the same call is let through to the operation.
        emulator.put_user_policy(user_name, &grant).await;
        let restored = emulator
            .call(
                &principal.access_key_id,
                "POST",
                evaluation.path,
                Some(&body),
            )
            .await;
        assert_eq!(
            (restored.status, restored.error_code.as_str()),
            (ambient.status, ambient.error_code.as_str()),
            "case {:?} once the grant is re-attached: expected the operation's own answer \
             again, got {} {:?} with {:?}",
            evaluation.operation,
            restored.status,
            restored.error_code,
            restored.body
        );
    }
}
