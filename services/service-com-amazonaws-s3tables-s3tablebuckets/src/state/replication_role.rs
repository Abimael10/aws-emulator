// SPDX-License-Identifier: CC0-1.0

//! Whether the IAM role a replication configuration names can replicate at all.
//!
//! A table is not replicated by whoever set the configuration: S3 Tables carries
//! the replication out ACTING AS the configuration's own role — "the IAM role
//! that S3 Tables assumes to replicate the table on your behalf"
//! (`TableReplicationConfiguration$role`), passed "to
//! replication.s3tables.amazonaws.com" and required to "have permissions to read
//! from the source table and write to all destination tables"
//! (`PutTableReplication`). So a role that principal cannot assume, or that is
//! not allowed the reads and writes replication performs, is a replication with
//! nothing to copy under: it never completes, which is the modelled terminal
//! `failed` state and the reason
//! [`ReplicationDestinationStatus::failure_message`](super::ReplicationDestinationStatus::failure_message)
//! states for it.
//!
//! The pieces, each held in exactly one place:
//!
//! 1. **who** — the stored configuration's own `role`, read off the record that
//!    replicates the table, so a destination is judged by the role replicating
//!    to it;
//! 2. **what the role trusts and what it may do** — the
//!    `AssumeRolePolicyDocument` and the identity-based policies the IAM store
//!    holds for it, read out of IAM's OWN state in the SAME shared engine (as
//!    [`super::authorization`] reads the caller's), so this service sees exactly
//!    what `CreateRole`, `UpdateAssumeRolePolicy` and `PutRolePolicy` wrote
//!    rather than a second copy of them;
//! 3. **the decision** — the shared readings of those documents
//!    ([`::service_common::iam_trust_policy`] for the trust relationship,
//!    [`::service_common::iam_authorization`] for the permissions), so this
//!    service and every other one handed the same documents answer alike; and
//!    the SENTENCE each refusal carries, which is IAM's own from that same
//!    module, so nothing of its wording is spelled here.
//!
//! Nothing is failed on evidence this emulator does not have. Each of these
//! shows nothing about the role and leaves replication to proceed: an engine
//! carrying no IAM store, a `role` the store holds no role for (a replication
//! role may be provisioned out of band, and may belong to another account), a
//! role carrying no trust document at all, a trust document that cannot be read
//! as a policy, a role carrying no readable identity-based policy at all (the
//! routine state of a role provisioned out of band), and a role carrying an
//! attachment whose document this emulator does not hold — an AWS-managed
//! policy, whose grant cannot be read here. What a stored document DOES state is
//! decided in IAM's own order, because the document is the authority on the role.
//!
//! LOCKING: the IAM store's guard is taken with NO guard of this service's own
//! store held, exactly as [`super::kms_key_access`] takes KMS's. The work under
//! it is bounded by what IAM holds for ONE role and by the destinations the model
//! allows one configuration (`@length(min: 1, max: 5)`), and nothing is written
//! to either store.

use std::collections::BTreeMap;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The IAM store a configuration's `role` is resolved against: the IAM service's
/// OWN state struct, reached through the SAME shared engine this service's store
/// lives in, so a replication reads the very records IAM's handlers wrote.
pub type IamState = iam_store::ComAmazonawsIamAwsidentitymanagementv20100508State;

/// The operation whose permission the role needs on the SOURCE table — the read
/// replication copies: "s3tables:GetTable permission on the source table being
/// replicated" (`PutTableReplication`).
const SOURCE_READ_OPERATION: &str = "GetTable";

/// The operation whose permission the role needs at EACH DESTINATION — the write
/// that leaves the replica there: "s3tables:CreateTable permission for the
/// destination" (`PutTableReplication`). It is granted on the destination table
/// bucket, which is the resource the configuration's rule names and the one the
/// replica is created in.
const DESTINATION_WRITE_OPERATION: &str = "CreateTable";

/// Why replication under one configuration cannot proceed — nothing when the
/// role shows no such obstacle.
///
/// Two kinds, because the role is asked two different questions: one about the
/// role and the source, which stops replication to EVERY destination, and one
/// per destination, about the write at that destination alone. So a role allowed
/// to write to one destination and not another fails exactly the destination it
/// may not write to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplicationRoleRefusals {
    /// The refusal that covers the whole configuration: the role cannot be
    /// assumed by the replication principal, or it may not read the source
    /// table, so no destination can be replicated to.
    whole: Option<String>,
    /// Destination table bucket ARN -> the refusal of the write at that
    /// destination, for the destinations the role may not write to.
    per_destination: BTreeMap<String, String>,
}

impl ReplicationRoleRefusals {
    /// The refusal replication to `destination` carries, or `None` when the role
    /// shows no obstacle to it.
    ///
    /// The refusal about the role itself comes first: a role the replication
    /// principal cannot assume replicates to no destination at all, so that is
    /// what every destination of such a configuration states.
    #[must_use]
    pub fn refusal_for(&self, destination: &str) -> Option<&str> {
        self.whole
            .as_deref()
            .or_else(|| self.per_destination.get(destination).map(String::as_str))
    }

    /// Whether the role shows no obstacle to any destination of the
    /// configuration.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.whole.is_none() && self.per_destination.is_empty()
    }
}

/// [`refusals`] against the IAM store of the shared engine this service runs in,
/// for a caller that holds the engine rather than a locked IAM state.
///
/// An engine carrying no IAM store answers nothing about any role, so nothing is
/// failed on evidence that is not there.
#[must_use]
pub fn engine_refusals<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    source_table_arn: &str,
    destinations: &[String],
) -> ReplicationRoleRefusals {
    let Some(iam) = iam_store::service_state(engine) else {
        return ReplicationRoleRefusals::default();
    };
    let held = iam.lock();
    refusals(&held, role_arn, source_table_arn, destinations)
}

/// Why replication of the table `source_table_arn` under the role `role_arn`
/// cannot proceed to each of `destinations`, as the records `iam` holds for that
/// role state it.
///
/// The questions, in the order the work would be done: can the replication
/// principal assume the role at all, may the role read the source table, and may
/// it write the replica at each destination. The first two are about the whole
/// configuration; the third is per destination.
///
/// Every answer that is not one of those three refusals is no refusal at all —
/// see the module docs for the cases that show nothing about a role.
#[must_use]
pub fn refusals(
    iam: &IamState,
    role_arn: &str,
    source_table_arn: &str,
    destinations: &[String],
) -> ReplicationRoleRefusals {
    let Some(role) = iam.roles.values().find(|held| held.arn == role_arn) else {
        return ReplicationRoleRefusals::default();
    };
    // The IDENTITY first: a role the replication principal cannot assume is a
    // role no read or write can be made under, so no permission of it is asked
    // about.
    if trust_withholds_assume(role.assume_role_policy_document.as_deref()) {
        return ReplicationRoleRefusals {
            whole: Some(assume_refusal(role_arn)),
            per_destination: BTreeMap::new(),
        };
    }
    // The documents in force on the role, from IAM's own collection of them. A
    // role carrying an attachment whose document this emulator does not hold,
    // and a role carrying no readable document at all, state nothing about what
    // the role may do (see the module docs).
    let Some(policies) = iam.principal_policies(&role.arn) else {
        return ReplicationRoleRefusals::default();
    };
    if policies.documents.is_empty() || !policies.uncarried.is_empty() {
        return ReplicationRoleRefusals::default();
    }
    let source = super::authorization::action_for(SOURCE_READ_OPERATION);
    if let Some(refusal) = withheld(&policies.documents, role_arn, &source, source_table_arn) {
        return ReplicationRoleRefusals {
            whole: Some(refusal),
            per_destination: BTreeMap::new(),
        };
    }
    let write = super::authorization::action_for(DESTINATION_WRITE_OPERATION);
    ReplicationRoleRefusals {
        whole: None,
        per_destination: destinations
            .iter()
            .filter_map(|destination| {
                withheld(&policies.documents, role_arn, &write, destination)
                    .map(|refusal| (destination.clone(), refusal))
            })
            .collect(),
    }
}

/// Whether the trust document a role carries withholds the assume the
/// replication principal would make.
///
/// Only a document that PARSED and withholds the grant does: a role with no
/// trust document, and one whose document cannot be read as a policy, show
/// nothing (see the module docs). The rule over the document itself is the
/// shared one, so this service and every other one read one trust policy alike.
fn trust_withholds_assume(trust: Option<&str>) -> bool {
    trust
        .and_then(|document| {
            ::service_common::iam_trust_policy::admits_service_principal(
                document,
                super::REPLICATION_SERVICE_PRINCIPAL,
            )
        })
        .is_some_and(|admitted| !admitted)
}

/// The refusal the role's own permissions state for `action` on `resource`, or
/// `None` when they allow it.
///
/// IAM's order, as every gate of this service takes it
/// ([`super::authorization`]): an explicit `Deny` refuses, a document set that
/// says nothing about the call grants nothing, and the sentence each case
/// carries is IAM's own.
fn withheld(
    documents: &[String],
    role_arn: &str,
    action: &str,
    resource: &str,
) -> Option<String> {
    // An identity-based document states no `Principal` element, so the decision
    // rests on the action and the resource the work would address.
    let request = iam::Request::new(action, resource, &[]);
    let reason = match iam::identity_decision(documents, &request) {
        Decision::Allowed => return None,
        Decision::Denied => iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
        Decision::NotStated => iam::no_identity_policy_allows(action),
    };
    Some(iam::not_authorized_by_identity_policy(
        role_arn, action, resource, &reason,
    ))
}

/// The refusal a role the replication principal cannot assume carries: IAM's own
/// sentence for a resource whose policy grants the principal nothing — the trust
/// policy is that resource-based policy — naming the principal that was refused,
/// the action it was refused and the role it was refused on, which is the one
/// variable part and comes from the stored configuration.
fn assume_refusal(role_arn: &str) -> String {
    iam::not_authorized_by_resource_policy(
        super::REPLICATION_SERVICE_PRINCIPAL,
        ::service_common::iam_trust_policy::ASSUME_ROLE_ACTION,
        role_arn,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A role ARN of this account, as the modelled `IAMRole` pattern accepts one
    /// (`^arn:.+:iam::[0-9]{12}:role/.+$`).
    fn role_arn(name: &str) -> String {
        format!(
            "arn:aws:iam::{account}:role/{name}",
            account = ::engine::emulator_account_id()
        )
    }

    /// RULE: a role the IAM store holds nothing for shows nothing about the
    /// replication, whatever the destinations are — another account's role, and
    /// one provisioned out of band, are in that position.
    #[test]
    fn a_role_iam_holds_nothing_for_refuses_nothing() {
        let iam = IamState::default();
        for name in ["unheld", "also-unheld"] {
            let arn = role_arn(name);
            let answered = refusals(
                &iam,
                &arn,
                "arn:aws:s3tables:us-east-1:000000000000:bucket/source/table/t",
                &["arn:aws:s3tables:us-east-1:000000000000:bucket/destination".to_string()],
            );
            assert!(
                answered.is_empty(),
                "case {name:?}: expected no refusal for a role IAM holds nothing for, got \
                 {answered:?}"
            );
        }
    }

    /// RULE: the sentence an unassumable role's refusal carries is IAM's own for
    /// a resource-based policy that grants the principal nothing — spelled out
    /// here, independently of the helper that builds it — and its only variable
    /// part is the role.
    #[test]
    fn the_assume_refusal_is_iams_own_sentence_about_the_role() {
        for name in ["one-role", "another-role"] {
            let arn = role_arn(name);
            let stated = assume_refusal(&arn);
            let expected = format!(
                "User: {principal} is not authorized to perform: sts:AssumeRole on resource: \
                 {arn} because no resource-based policy allows the sts:AssumeRole action",
                principal = super::super::REPLICATION_SERVICE_PRINCIPAL,
            );
            assert_eq!(
                stated, expected,
                "case {name:?}: expected IAM's own sentence, got {stated:?}"
            );
        }
    }
}
