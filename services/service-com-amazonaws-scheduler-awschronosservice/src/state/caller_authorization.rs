// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the principal a request speaks for perform the ACTION it asks for on the
//! resource it addresses.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at what the request
//! asks for: a principal whose identity-based policies do not allow the action — or that an
//! explicit `Deny` covers — is refused rather than served, and the call performs nothing.
//! Nothing is created, no schedule moves, no tag is written and nothing is read back. Every
//! operation of this service publishes an action under this service's own namespace
//! ([`ACTION_PREFIX`], the `arnNamespace` of its model's `awsService` trait), named after the
//! operation ("Actions defined by Amazon EventBridge Scheduler"), so the action one request
//! performs is `scheduler:<OperationName>`.
//!
//! This model declares no error shape for that decision — no operation of this service lists
//! an access error (`smithy_query(query="digest")`) — because the refusal is the AWS COMMON
//! `AccessDeniedException` answered by the layer in front of a service's own code: it travels
//! through [`crate::scaffolded::Scaffolded::access_denied`], which renders HTTP 403 with the
//! error code `AccessDeniedException`.
//!
//! Who the caller is, what it may do and how the refusal is worded are the one reading this
//! crate has ([`super::caller_identity`]); this module adds only WHICH action and WHICH
//! resource one request is judged on.
//!
//! The resource is the ARN the REQUEST addresses, built from the request alone, so the decision
//! does not depend on whether that resource exists — which is also what keeps a refused call
//! from telling an unauthorized caller what the account holds. A request that addresses no
//! single resource of its own — a listing, whose answer is about the account — is judged
//! against [`ANY_RESOURCE`], as IAM writes the resource of such a request.
//!
//! This is the identity-based decision ALONE, which is the whole decision for a control-plane
//! call of this service: no resource of EventBridge Scheduler carries a resource-based policy.
//! The other permission a request may need is the pass of an execution ROLE to this service,
//! which is a separate action on a separate resource ([`super::pass_role`]).

use ::service_common::iam_authorization as iam;

use super::caller_identity::{identity_denial, request_caller};

/// The namespace this service publishes its IAM actions under: the `arnNamespace` its model's
/// `awsService` trait carries (`smithy_query(query="service_info")`), which is also the prefix
/// of every action in "Actions defined by Amazon EventBridge Scheduler".
pub const ACTION_PREFIX: &str = "scheduler";

/// How IAM writes the resource of a request that addresses no single resource of its own —
/// a listing, which is about the account rather than about one schedule.
pub const ANY_RESOURCE: &str = "*";

/// Why this caller may not make this request, or `None` when it may.
///
/// `operation` is the operation's own name ([`::smithy_server_runtime::Operation::NAME`]), which
/// names the action `scheduler:<OperationName>` this service publishes for it; `resource` is
/// the ARN the request ADDRESSES, or [`ANY_RESOURCE`] for a request that addresses no single
/// resource.
///
/// Call it BEFORE this service's store is locked and before anything is resolved, so a refused
/// call changes nothing and tells an unauthorized caller nothing about what the account holds.
/// Nothing is cached, so a policy just written decides the next request.
pub fn caller_denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    let action = format!("{ACTION_PREFIX}:{operation}");
    let caller = request_caller(engine, ctx);
    identity_denial(
        &caller,
        &action,
        resource,
        // The operation publishes no condition key of its own, so the decision rests on the
        // action and the resource; a clause over any other key is left undecided by the shared
        // evaluator rather than guessed at.
        &iam::Request::new(&action, resource, &caller.principals),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::caller_identity::Caller;

    /// A principal holding `documents` and nothing else — no boundary, not the account's own
    /// credential, no unreadable attachment.
    fn principal(documents: &[&str]) -> Caller {
        let arn = "arn:aws:sts::000000000000:assumed-role/author/session".to_string();
        Caller {
            arn: arn.clone(),
            principals: vec![arn],
            documents: documents.iter().map(|held| (*held).to_string()).collect(),
            boundary: None,
            account_credential: false,
            unreadable_attachment: false,
        }
    }

    /// The decision over a principal already read, as [`caller_denial`] takes it.
    fn denial(caller: &Caller, action: &str, resource: &str) -> Option<String> {
        identity_denial(
            caller,
            action,
            resource,
            &iam::Request::new(action, resource, &caller.principals),
        )
    }

    /// RULE: a request is served only where a readable identity-based document ALLOWS this
    /// service's action for the resource the request addresses, and an explicit `Deny` about it
    /// refuses whatever a grant beside it says.
    ///
    /// The rows differ along what the rule generalises over: the width a grant is written at
    /// (one action, the service's whole namespace, every action), the resource it is scoped to
    /// (one schedule, a group's schedules, every resource), and which effect decides.
    #[test]
    fn a_request_is_served_only_where_a_document_allows_its_action_on_its_resource() {
        let action = format!("{ACTION_PREFIX}:CreateSchedule");
        let resource = "arn:aws:scheduler:us-west-2:000000000000:schedule/orders/nightly";
        let allow = |named: &str, scope: &str| {
            format!(r#"{{"Statement":[{{"Effect":"Allow","Action":"{named}","Resource":"{scope}"}}]}}"#)
        };
        // (case, the documents in force, whether the request is permitted)
        let rows: &[(&str, Vec<String>, bool)] = &[
            (
                "a grant of this action on this schedule",
                vec![allow(&action, resource)],
                true,
            ),
            (
                "a grant of every action of this service on every resource",
                vec![allow(&format!("{ACTION_PREFIX}:*"), "*")],
                true,
            ),
            (
                "a grant scoped to the schedules of the group the request names",
                vec![allow(
                    &action,
                    "arn:aws:scheduler:us-west-2:000000000000:schedule/orders/*",
                )],
                true,
            ),
            (
                "a grant scoped to another group's schedules",
                vec![allow(
                    &action,
                    "arn:aws:scheduler:us-west-2:000000000000:schedule/billing/*",
                )],
                false,
            ),
            (
                "a grant of another action of this service",
                vec![allow(&format!("{ACTION_PREFIX}:GetSchedule"), "*")],
                false,
            ),
            (
                "a grant of another service's actions",
                vec![allow("events:PutRule", "*")],
                false,
            ),
            ("no document at all", Vec::new(), false),
            (
                "a wildcard grant with this action denied beside it",
                vec![format!(
                    r#"{{"Statement":[{{"Effect":"Allow","Action":"*","Resource":"*"}},
                        {{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
                )],
                false,
            ),
            (
                "the deny in a SECOND document beside an allowing one",
                vec![
                    allow("*", "*"),
                    format!(
                        r#"{{"Statement":[{{"Effect":"Deny","Action":"{action}","Resource":"{resource}"}}]}}"#
                    ),
                ],
                false,
            ),
        ];

        for (case, documents, permitted) in rows {
            let held: Vec<&str> = documents.iter().map(String::as_str).collect();
            let answered = denial(&principal(&held), &action, resource);
            assert_eq!(
                answered.is_none(),
                *permitted,
                "case {case:?}: expected permitted={permitted}, got {answered:?}"
            );
            if let Some(message) = answered {
                assert!(
                    message.contains(&action) && message.contains(resource),
                    "case {case:?}: expected the refusal to name the action and the resource, \
                     got {message:?}"
                );
            }
        }
    }

    /// RULE: the caller a decision does not bound is served — the account's own published
    /// credential, which holds no IAM policies, and a principal carrying a grant this emulator
    /// cannot read — while an explicit `Deny` in a document that CAN be read still refuses it.
    #[test]
    fn a_caller_the_decision_does_not_bound_is_refused_only_by_an_explicit_deny() {
        let action = format!("{ACTION_PREFIX}:ListSchedules");
        let deny = format!(
            r#"{{"Statement":[{{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
        );
        // (case, the caller, whether the request is permitted)
        let rows: Vec<(&str, Caller, bool)> = vec![
            (
                "the account's own published credential",
                Caller {
                    account_credential: true,
                    ..principal(&[])
                },
                true,
            ),
            (
                "a principal holding an attachment this emulator cannot read",
                Caller {
                    unreadable_attachment: true,
                    ..principal(&[])
                },
                true,
            ),
            (
                "the same principal, with the action explicitly denied",
                Caller {
                    unreadable_attachment: true,
                    ..principal(&[deny.as_str()])
                },
                false,
            ),
            (
                "a bounded principal holding nothing",
                principal(&[]),
                false,
            ),
        ];
        for (case, caller, permitted) in rows {
            let answered = denial(&caller, &action, ANY_RESOURCE);
            assert_eq!(
                answered.is_none(),
                permitted,
                "case {case:?}: expected permitted={permitted}, got {answered:?}"
            );
        }
    }

    /// RULE: a grant the principal's PERMISSIONS BOUNDARY does not also allow is not a
    /// permission it has, and the refusal names the boundary as the rule that withheld it.
    #[test]
    fn a_boundary_caps_the_grant_an_identity_policy_gave() {
        let action = format!("{ACTION_PREFIX}:DeleteSchedule");
        let resource = "arn:aws:scheduler:us-west-2:000000000000:schedule/orders/nightly";
        let granting = format!(
            r#"{{"Statement":[{{"Effect":"Allow","Action":"{ACTION_PREFIX}:*","Resource":"*"}}]}}"#
        );
        // (case, the boundary document, the reason clause the refusal must carry)
        let rows: Vec<(&str, Option<String>, Option<String>)> = vec![
            ("no boundary at all", None, None),
            (
                "a boundary allowing the action",
                Some(granting.clone()),
                None,
            ),
            (
                "a boundary that states nothing about it",
                Some(
                    r#"{"Statement":[{"Effect":"Allow","Action":"sqs:*","Resource":"*"}]}"#
                        .to_string(),
                ),
                Some(iam::no_permissions_boundary_allows(&action)),
            ),
            (
                "a boundary that denies it",
                Some(format!(
                    r#"{{"Statement":[{{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
                )),
                Some(iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()),
            ),
        ];
        for (case, boundary, expected) in rows {
            let caller = Caller {
                boundary,
                ..principal(&[granting.as_str()])
            };
            let answered = denial(&caller, &action, resource);
            match (&expected, &answered) {
                (None, None) => {}
                (Some(reason), Some(message)) => assert!(
                    message.ends_with(reason.as_str()),
                    "case {case:?}: expected the refusal to end with {reason:?}, got {message:?}"
                ),
                _ => panic!("case {case:?}: expected {expected:?}, got {answered:?}"),
            }
        }
    }
}
