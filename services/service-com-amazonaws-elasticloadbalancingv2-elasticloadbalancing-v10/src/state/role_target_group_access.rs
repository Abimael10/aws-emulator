// SPDX-License-Identifier: CC0-1.0

//! Whether an IAM ROLE of this account may act on a TARGET GROUP of this service — the
//! question another service puts to this one before it hands that role work to do here.
//!
//! # The rule this module implements
//!
//! A service that registers a workload it runs as a target of a group does it UNDER A ROLE the
//! caller handed it, and the actions it then performs here are this service's own
//! ([`TARGET_REGISTRATION_ACTIONS`]): it reads the group back, which is what tells it the group
//! can carry the workload at all, and it puts the target into the group. Both are actions IAM
//! publishes for this service on the target-group resource type, so whether the role may
//! perform them is decided exactly as it is for a CALLER of this API
//! ([`crate::service::target_group_authorization`]) — by the identity-based documents in force
//! on the role and the permissions boundary that caps them, evaluated by the tree's ONE
//! evaluation of policy documents ([`::service_common::iam_authorization`]) against the ARN the
//! request named.
//!
//! The decision belongs HERE and not to the service that asks, for the same reason the caller's
//! does: the actions, the resource type they are published for and the store that holds the
//! groups are this service's, so two services handed the same role and the same group must
//! answer alike.
//!
//! # Only what IAM ANSWERED is withheld
//!
//! The answer is `None` — nothing withheld — for everything that is not an answer about the
//! role's permissions:
//!
//! * an engine carrying no IAM store, where no role can exist;
//! * a role this account's IAM holds no record for (it may have been provisioned out of band,
//!   which is an ordinary way these roles exist);
//! * a role carrying an attachment whose document this emulator does not hold — an AWS-managed
//!   policy belongs to AWS rather than to this account, so refusing on a grant that cannot be
//!   read would withhold access AWS permits.
//!
//! The same fail-open reading [`crate::service::target_group_authorization`] takes for a
//! caller.
//!
//! LOCK ORDER: the IAM store's lock is taken and released inside this call, and no lock of this
//! service's store is taken at all — so a caller holding no guard (which is how a cross-service
//! read is made in this tree) can never close a cycle through it.

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The operations of this service a registration performs on the target group it registers
/// into, in the order they happen: reading the group back — which is what VALIDATES that the
/// group exists and can carry the workload — and registering the target.
///
/// Source: the actions IAM publishes for this service on its `targetgroup` resource type
/// ("Actions defined by Elastic Load Balancing V2", IAM Service Authorization Reference), named
/// here by the operation they are the action of, so the action text is composed with this
/// service's one action prefix
/// ([`crate::service::target_group_authorization::ACTION_PREFIX`]) rather than spelled twice.
pub const TARGET_REGISTRATION_ACTIONS: &[&str] = &["DescribeTargetGroups", "RegisterTargets"];

/// The FIRST action of [`TARGET_REGISTRATION_ACTIONS`] the role `role_arn` may not perform on
/// the target group `target_group_arn`, named in full, or `None` when the role may perform
/// every one of them — and for everything that is not an answer about the role at all (see the
/// module docs).
///
/// `target_group_arn` is the ARN the REQUEST carried, never one read back out of this service's
/// store: it is the resource IAM judges the actions against, and taking it from the request is
/// what lets the decision be taken before anything is resolved.
pub fn withheld_target_registration_action<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    target_group_arn: &str,
) -> Option<String> {
    let state = iam_store::service_state(engine)?;
    let store = state.lock();
    // The role as THIS account holds it: the ARN the account holds it under carries its path,
    // and that is the ARN its policies are held against.
    let held_arn = store
        .roles
        .values()
        .find(|role| role.arn == role_arn)
        .map(|role| role.arn.clone())?;
    let policies = store.principal_policies(&held_arn)?;
    // An attachment this emulator resolved no document for is a grant it cannot read, so
    // nothing is withheld on the strength of what it can.
    if !policies.uncarried.is_empty() {
        return None;
    }
    let boundary = store.permissions_boundary_document(&held_arn);
    drop(store);
    TARGET_REGISTRATION_ACTIONS
        .iter()
        .map(|operation| {
            format!(
                "{}:{operation}",
                crate::service::target_group_authorization::ACTION_PREFIX
            )
        })
        .find(|action| {
            withholds_action(
                &policies.documents,
                boundary.as_deref(),
                action,
                target_group_arn,
            )
        })
}

/// Whether the documents in force withhold `action` on `resource`: IAM's own evaluation order,
/// where an explicit `Deny` wins, an `Allow` about the request grants it, neither falls to the
/// default deny, and a permissions boundary that does not also allow it caps the grant away.
fn withholds_action(
    documents: &[String],
    boundary: Option<&str>,
    action: &str,
    resource: &str,
) -> bool {
    let request = iam::Request::new(action, resource, &[]);
    let said = iam::identity_decision(documents, &request);
    if said == Decision::Denied {
        return true;
    }
    if !iam::is_allowed(said, Decision::NotStated) {
        return true;
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the role has.
    match boundary {
        None => false,
        Some(boundary) => {
            iam::identity_decision(::std::slice::from_ref(&boundary.to_string()), &request)
                != Decision::Allowed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target group ARN of this service, as `CreateTargetGroup` mints one.
    fn target_group_arn(name: &str) -> String {
        format!(
            "arn:aws:elasticloadbalancing:us-east-1:000000000000:targetgroup/{name}/0123456789abcdef"
        )
    }

    /// The action text one operation of [`TARGET_REGISTRATION_ACTIONS`] is judged as.
    fn action(operation: &str) -> String {
        format!(
            "{}:{operation}",
            crate::service::target_group_authorization::ACTION_PREFIX
        )
    }

    /// RULE: the registration's actions are withheld unless the documents in force allow EVERY
    /// one of them on the group the request named.
    #[test]
    fn every_action_the_registration_performs_has_to_be_allowed() {
        let group = target_group_arn("a-group");
        let elsewhere = target_group_arn("elsewhere");
        let allow = |actions: &str, resource: &str| {
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":{actions},"Resource":"{resource}"}}]}}"#
            )
        };
        // (case, the documents in force, whether an action is withheld)
        let rows: Vec<(&str, Vec<String>, bool)> = vec![
            ("no document at all", Vec::new(), true),
            (
                "the service role policy's own grant: every action of this service",
                vec![allow(r#""elasticloadbalancing:*""#, "*")],
                false,
            ),
            (
                "the two actions, named on the group the request carried",
                vec![allow(
                    r#"["elasticloadbalancing:DescribeTargetGroups","elasticloadbalancing:RegisterTargets"]"#,
                    &group,
                )],
                false,
            ),
            (
                "the registration alone: the read that validates the group is missing",
                vec![allow(r#""elasticloadbalancing:RegisterTargets""#, "*")],
                true,
            ),
            (
                "every action, but on ANOTHER group",
                vec![allow(r#""elasticloadbalancing:*""#, &elsewhere)],
                true,
            ),
            (
                "another service's actions only",
                vec![allow(r#""ec2:Describe*""#, "*")],
                true,
            ),
            (
                "an allow beside an explicit deny of the registration",
                vec![
                    allow(r#""elasticloadbalancing:*""#, "*"),
                    r#"{"Statement":[{"Effect":"Deny","Action":"elasticloadbalancing:RegisterTargets","Resource":"*"}]}"#
                        .to_string(),
                ],
                true,
            ),
        ];
        for (case, documents, withheld) in rows {
            let answered = TARGET_REGISTRATION_ACTIONS
                .iter()
                .any(|operation| withholds_action(&documents, None, &action(operation), &group));
            assert_eq!(
                answered, withheld,
                "case {case:?}: expected withheld={withheld}, got {answered}"
            );
        }
    }

    /// RULE: a permissions boundary that does not allow an action caps the grant an
    /// identity-based document gave.
    #[test]
    fn a_boundary_that_does_not_allow_the_action_caps_the_grant() {
        let group = target_group_arn("a-group");
        let granted = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"elasticloadbalancing:*","Resource":"*"}]}"#
            .to_string();
        let wide =
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#;
        let narrow = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"ec2:Describe*","Resource":"*"}]}"#;
        // (case, the boundary in force, whether the registration is withheld)
        let rows: &[(&str, Option<&str>, bool)] = &[
            ("no boundary at all", None, false),
            ("a boundary that admits every action", Some(wide), false),
            (
                "a boundary that admits nothing of this service",
                Some(narrow),
                true,
            ),
        ];
        for (case, boundary, withheld) in rows {
            let answered = TARGET_REGISTRATION_ACTIONS.iter().any(|operation| {
                withholds_action(
                    ::std::slice::from_ref(&granted),
                    *boundary,
                    &action(operation),
                    &group,
                )
            });
            assert_eq!(
                answered, *withheld,
                "case {case:?}: expected withheld={withheld}, got {answered}"
            );
        }
    }
}
