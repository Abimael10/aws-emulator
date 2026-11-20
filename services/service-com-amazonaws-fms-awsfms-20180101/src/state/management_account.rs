// SPDX-License-Identifier: CC0-1.0

//! The refusal of a Firewall Manager call that only the organization's
//! management account can perform.
//!
//! Two operations of this service CHANGE or ENUMERATE the organization's
//! Firewall Manager ADMINISTRATION — the set of administrator accounts and the
//! `AdminScope` each of them manages — and the model documents that
//! administration as the management account's alone:
//! `ListAdminAccountsForOrganization` ("This operation can be called only from
//! the organization's management account") and `PutAdminAccount` ("Only the
//! organization's management account can create an Firewall Manager
//! administrator account").
//!
//! Until an organization is onboarded to Firewall Manager by
//! `AssociateAdminAccount` there is no such administration to enumerate or
//! write: the organization has no Firewall Manager administrator, so the call is
//! not possible for the account making it, and the live service refuses it as
//! one only the organization's management account can perform. That is the class
//! of the modelled `InvalidOperationException` ("The operation failed because
//! there was nothing to do or the operation wasn't possible"), which both
//! operations declare; `AccessDeniedException` — the refusal the broader
//! administrator gate states ([`super::no_administrator_refusal`]) — is in NONE
//! of their modelled error unions, so it cannot be the refusal here.
//!
//! `GetAdminScope`, the read side of what `PutAdminAccount` writes, is NOT in
//! this class: it reports what the ONE account its request names resolves to, so
//! it answers from its own lookup — an account that is no administrator is its
//! modelled `ResourceNotFoundException` — rather than taking an admission test
//! ahead of that lookup.
//!
//! The refusal's message is built from state, never from a literal: the account
//! the request is made by, and the organization's management account as AWS
//! Organizations records it ([`organization_management_account`], the same value
//! `DescribeOrganization` reports as `Organization$MasterAccountId`). Both are
//! read where they live, so a change to the organization's management account
//! changes what this names.

use std::time::SystemTime;

use super::{AdminAccountRecord, ComAmazonawsFmsAwsfms20180101State};
use crate::ops::types::InvalidOperationException;

/// The organization's management account, read from the AWS Organizations
/// service's OWN store through the shared engine — the same account its
/// `DescribeOrganization` reports as `Organization$MasterAccountId`.
///
/// Read across services the way this crate already reads the stores of the
/// services that own the organization's resources
/// ([`super::discovered_resources`]): the engine is shared, so the organization
/// a Firewall Manager call is scoped to is the one Organizations holds, not a
/// second copy kept here that could drift from it.
///
/// LOCK ORDER: this takes the Organizations store's lock, holds no other while
/// it does, and hands back an owned value rather than a guard — so it is called
/// with NO Firewall Manager store guard held, which is what keeps the
/// cross-service read one-directional and a lock cycle unreachable.
///
/// Falls back to the account this emulator presents
/// ([`::engine::emulator_account_id`]) when no Organizations control plane is
/// mounted on this engine, or when it holds no organization: the account then
/// belongs to no organization any other service can see, and the only account
/// there is to name is that one — which is also the account Organizations
/// records as the management account of the organization it belongs to, because
/// an organization's management account is the account it was created from.
pub fn organization_management_account<Eng: Send + Sync + 'static>(engine: &Eng) -> String {
    let stored = ::service_com_amazonaws_organizations_awsorganizationsv20161128::state::service_state(engine)
        .and_then(|state| {
            let store = state.lock();
            store
                .organization
                .as_ref()
                .map(|organization| organization.master_account_id.clone())
        });
    stored.unwrap_or_else(|| ::engine::emulator_account_id().to_string())
}

/// The modelled `InvalidOperationException` the live service answers a caller
/// that performs an action only the organization's management account can
/// perform.
///
/// The text is the service's own, with the two accounts it names taken from
/// state: `caller_account` is the account the request is made by, and
/// `management_account` the organization's management account
/// ([`organization_management_account`]). Neither is a literal — an account id
/// is per-environment — and the fixed text around them is the whole requirement.
pub fn not_management_account(
    caller_account: &str,
    management_account: &str,
) -> InvalidOperationException {
    InvalidOperationException {
        message: Some(format!(
            "{caller_account} is not the management account of the Organization but is \
             attempting to perform an action that only the Organization management account can \
             perform. The Organization management account is {management_account}"
        )),
    }
}

/// [`not_management_account`] for the account THIS request is made by: the
/// account the emulator presents, against the organization's management account
/// as Organizations records it.
///
/// The ONE refusal constructor the management-account-only operations share, so
/// no two of them can word this refusal differently. Called with no Firewall
/// Manager store guard held (see [`organization_management_account`]).
pub fn management_account_only_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> InvalidOperationException {
    let caller = ::engine::emulator_account_id();
    let management_account = organization_management_account(engine);
    not_management_account(caller, &management_account)
}

/// The organization's Firewall Manager administration AS OF `now`: the
/// default-administrator association that makes the organization onboarded to
/// Firewall Manager, or `None` when it holds none at that instant.
///
/// The gate of the operations only the organization's management account can
/// perform, and the counterpart of [`super::require_administrator_at`] for them:
/// the ADMISSION test is the same one — the association that is still an
/// administrator at the instant the request decides from
/// ([`ComAmazonawsFmsAwsfms20180101State::active_admin_account_at`]), so a
/// tear-down the clock has completed closes this gate too — and only the REFUSAL
/// differs, because what these operations model for it differs
/// ([`management_account_only_refusal`]).
///
/// It answers with an OWNED record so the caller can drop the store guard before
/// building that refusal, which the cross-service read it does needs.
pub fn management_account_administration_at(
    store: &ComAmazonawsFmsAwsfms20180101State,
    now: SystemTime,
) -> Option<AdminAccountRecord> {
    store.active_admin_account_at(now).cloned()
}
