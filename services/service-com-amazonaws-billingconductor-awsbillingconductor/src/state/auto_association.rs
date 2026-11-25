// SPDX-License-Identifier: CC0-1.0

//! Automatic account association: what the billing group electing
//! `AccountGrouping.AutoAssociate` does when an account joins the consolidated billing
//! family.
//!
//! One concern, so the rule the flag states — "Specifies if this billing group will
//! automatically associate newly added Amazon Web Services accounts that join your
//! consolidated billing family" — is decided in ONE place, beside the roster observation
//! it is decided from, and every reader of a membership (the account listing, a group's
//! `Size`, the one-account-one-group refusals, a disassociation) answers from the
//! membership index it writes rather than from a second rule of its own.
//!
//! Which accounts a family holds is an AWS Organizations fact (`organization_linked_accounts`),
//! so the join happens outside this service and the association it elects is owed from the
//! instant it happens, whether or not anybody calls: it is therefore SETTLED — observed and
//! materialised — ahead of every operation this service serves, through
//! [`settle_automatic_account_associations`], and the answer is the same whichever
//! operation looks first.

use super::*;

impl ComAmazonawsBillingconductorAwsbillingconductorState {
    /// The stored billing group that automatically associates the accounts newly joining
    /// the consolidated billing family, or `None` when no group elects that.
    ///
    /// At most one group may hold the flag at a time
    /// ([`MAX_AUTO_ASSOCIATE_BILLING_GROUPS`], enforced where it is turned on), and the
    /// one reading of the flag every rule that turns on it shares decides it here too
    /// ([`BillingGroupRecord::auto_associates`]). Keyed by ARN, so the answer is the
    /// same whatever order the groups were created in.
    pub fn auto_associate_billing_group(&self) -> Option<&str> {
        self.billing_groups
            .iter()
            .find(|(_, group)| group.auto_associates())
            .map(|(arn, _)| arn.as_str())
    }

    /// Take the accounts that have JOINED the consolidated billing family since this
    /// store last observed it into the billing group that elects automatic association,
    /// and answer the account ids that became members of it.
    ///
    /// The rule is `AccountGrouping.AutoAssociate`'s own: the electing group
    /// automatically associates the accounts newly added to the family, so an account the
    /// roster holds for the first time (`observed_family_accounts`) joins that group, and
    /// an account that was already in the family when the group came to elect the flag is
    /// not newly added and stays where it is. Nothing is taken from another group either:
    /// an account already in one belongs to at most one billing group, and an account a
    /// caller took OUT of a group has been observed, so a release is not undone by the
    /// next look.
    ///
    /// The membership is the one every other writer records
    /// ([`Self::associate_account`] beside the group's own account list), so a size, a
    /// listing, an exclusivity refusal and a `DisassociateAccounts` all answer for an
    /// automatic member exactly as they do for one a request named. Neither the group's
    /// generations nor its `LastModifiedTime` move: no generation carries a group's
    /// membership ([`BillingGroupVersion`]), and nothing a caller asked for changed.
    ///
    /// A store no roster is known for ([`Self::billing_family`]) knows of no account that
    /// joined, so it observes nothing and associates nothing — the same evidence
    /// [`Self::is_outside_billing_family`] needs.
    pub fn associate_accounts_that_joined_the_family(
        &mut self,
        organization: Option<&BTreeMap<String, LinkedAccountIdentity>>,
    ) -> Vec<String> {
        let Some(roster) = self.billing_family(organization) else {
            return Vec::new();
        };
        let joined: Vec<String> = roster
            .keys()
            .filter(|account_id| !self.observed_family_accounts.contains(*account_id))
            .cloned()
            .collect();
        // The observation IS the family as it stands now, so an account that left it is
        // forgotten with it and the set stays the size of the roster.
        self.observed_family_accounts = roster.into_keys().collect();
        let Some(group_arn) = self.auto_associate_billing_group().map(str::to_string) else {
            return Vec::new();
        };
        let mut associated: Vec<String> = Vec::new();
        for account_id in joined {
            if self.account_billing_group(&account_id).is_some() {
                continue;
            }
            let held = self
                .billing_groups
                .get(&group_arn)
                .map(|group| group.linked_account_ids.len())
                .unwrap_or_default();
            if held >= MAX_ACCOUNTS_PER_BILLING_GROUP {
                // A group holds at most this many accounts however the membership
                // accumulated, so the account stays in no billing group — which is what
                // the listing reports for it — rather than taking the group past a bound
                // an `AssociateAccounts` could not take it past either. Never the account
                // id, only how full the group is.
                ::tracing::warn!(
                    billing_group_arn = %group_arn,
                    cap = MAX_ACCOUNTS_PER_BILLING_GROUP,
                    "an account that joined the consolidated billing family was not \
                     associated automatically: the electing billing group is at its cap"
                );
                break;
            }
            if let Some(group) = self.billing_groups.get_mut(&group_arn) {
                if !group.linked_account_ids.contains(&account_id) {
                    group.linked_account_ids.push(account_id.clone());
                }
            }
            self.associate_account(&account_id, &group_arn);
            associated.push(account_id);
        }
        if !associated.is_empty() {
            // The audit record of a membership written without a caller asking for it:
            // the group that elected the flag and how many accounts joined it, never the
            // account ids themselves — the shape every other write of a membership logs
            // (`AssociateAccounts`).
            ::tracing::info!(
                billing_group_arn = %group_arn,
                associated_accounts = associated.len(),
                outcome = "associated",
                "accounts that joined the consolidated billing family were associated \
                 automatically"
            );
        }
        associated
    }
}

/// Settle the automatic associations the consolidated billing family's roster has made
/// due, ahead of the operation that is about to run.
///
/// An account joins a consolidated billing family in AWS Organizations, so the roster is
/// read from THAT store first, guard-free, and this store's guard is taken after it — one
/// lock at a time, in one direction, exactly as [`caller_is_payer_account`] reads the two.
/// The association the elected group owes such an account was owed from the instant it
/// joined, so settling it here makes every operation answer the same, whichever one looks
/// first; a store with no roster, no electing group or no new account settles nothing.
pub fn settle_automatic_account_associations<Eng: Send + Sync + 'static>(engine: &Eng) {
    let organization = organization_linked_accounts(engine);
    let Some(state) = service_state(engine) else {
        return;
    };
    state
        .lock()
        .associate_accounts_that_joined_the_family(organization.as_ref());
}
