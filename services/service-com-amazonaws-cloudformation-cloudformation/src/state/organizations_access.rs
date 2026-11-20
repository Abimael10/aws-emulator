// SPDX-License-Identifier: CC0-1.0

//! TRUSTED ACCESS with Organizations: the one setting `ActivateOrganizationsAccess` and
//! `DeactivateOrganizationsAccess` write and `DescribeOrganizationsAccess` reads back.
//!
//! Activating it is what gives the organization's management account "permissions to create
//! and manage StackSets for your organization" (`ActivateOrganizationsAccess`), and
//! deactivating it takes those permissions away again
//! (`DeactivateOrganizationsAccess`: "if trusted access is deactivated, the management
//! account does not have permissions to create and manage service-managed StackSets for your
//! organization"). It is a property of the ORGANIZATION rather than of one Region's stacks,
//! so the store holds exactly one of these — outside the per-Region namespacing everything
//! else in this service is read through — and a read of it answers the same setting whichever
//! Region the request was signed for.
//!
//! ## What the read reports
//!
//! `DescribeOrganizationsAccess` "retrieves information about the account's OrganizationAccess
//! status" as the modelled `OrganizationStatus`: `ENABLED | DISABLED |
//! DISABLED_PERMANENTLY`. An account nobody has activated trusted access for is
//! [`organization_status::DISABLED`], which is therefore the default this store starts in, and
//! an activation moves it to [`organization_status::ENABLED`] until something deactivates it
//! again. [`organization_status::DISABLED_PERMANENTLY`] is the third value the model carries
//! and NOTHING here produces it: it is the state trusted access is left in when it is
//! withdrawn outside CloudFormation, which no operation of this API performs and none reports,
//! so no call can put this store into it.
//!
//! ## Who may call as a delegated administrator
//!
//! The `CallAs` the read takes says which administrator account the call is made from: the
//! organization's management account (`SELF`, the default) or "a delegated administrator in a
//! member account" (`DELEGATED_ADMIN`), and for the latter "your Amazon Web Services account
//! must be registered as a delegated administrator in the management account". That
//! registration is made in Organizations rather than here — no CloudFormation operation
//! performs it and none reports it — and the one account this emulator stands for is the
//! MANAGEMENT account of its organization, which is the account that activates and deactivates
//! trusted access and which Organizations cannot also register as a delegated administrator of
//! that same organization. So no caller reaching this service is a delegated administrator,
//! and a call that says it is one is refused with the modelled `InvalidOperationException`
//! (400) and [`NOT_A_DELEGATED_ADMINISTRATOR`], reading and writing nothing.
//!
//! ## An account that is in no organization
//!
//! Trusted access is a setting OF AN ORGANIZATION, so all three calls require the account to
//! be in one: with no organization there is nothing to activate, deactivate or report on, and
//! each is refused with [`NO_ORGANIZATION`] ([`refuse_without_an_organization`]). Whether
//! there is an organization is AWS Organizations' own record rather than a copy kept here, so
//! it is read from that service's store through the shared engine.

/// The model's `OrganizationStatus` enum, "the status of the OrganizationAccess"
/// (`smithy_query(query="shape", shape="OrganizationStatus")`: `ENABLED | DISABLED |
/// DISABLED_PERMANENTLY`).
pub mod organization_status {
    /// Trusted access is activated for this organization.
    pub const ENABLED: &str = "ENABLED";
    /// Trusted access is not activated, which is where an organization nobody activated it
    /// for starts and where a deactivation puts it back.
    pub const DISABLED: &str = "DISABLED";
    /// Trusted access was withdrawn outside CloudFormation; see this module's docs for why
    /// nothing in this emulator reports it.
    pub const DISABLED_PERMANENTLY: &str = "DISABLED_PERMANENTLY";
}

/// The `CallAs` value that says the call is made "as a delegated administrator in a member
/// account" (`smithy_query(query="shape", shape="CallAs")`: `SELF | DELEGATED_ADMIN`).
///
/// Only that one value is named here, because it is the only one that CHANGES what an
/// operation does: `SELF` is the default and means the management account, which is the
/// account every request to this emulator is made by.
pub const DELEGATED_ADMIN: &str = "DELEGATED_ADMIN";

/// The sentence a call made as a delegated administrator is refused with, for an account that
/// is not registered as one.
///
/// Entirely FIXED text: nothing in it comes from the request or from the store, like this
/// service's other account-level refusals of a caller that may not make the call at all.
pub const NOT_A_DELEGATED_ADMINISTRATOR: &str = "Account used is not a delegated administrator";

/// Whether a request's `CallAs` says it was made from a delegated administrator account, which
/// is the claim no caller of this emulator can make good on (see this module's docs).
///
/// A request that named no `CallAs` is a management-account call — "by default, SELF is
/// specified" — so an absent value is NOT this claim.
pub fn claims_delegated_administrator(call_as: Option<&str>) -> bool {
    call_as == Some(DELEGATED_ADMIN)
}

/// The sentence every operation of this trusted-access switch is refused with for an account
/// that is in NO ORGANIZATION.
///
/// Entirely FIXED text: the account, the organization and the stack set it speaks of are the
/// caller's own and none of them reaches the sentence. It states the capability the switch is
/// for — service-managed stack sets — because that is what an account with no organization
/// cannot operate, whichever of the three calls asked.
pub const NO_ORGANIZATION: &str =
    "You must have an organization setup before operating a SERVICE_MANAGED stack set";

/// The AWS Organizations store this service asks whether the account is in an organization.
///
/// It is that service's OWN state struct, reached through the SAME shared engine this
/// service's store lives in, so CloudFormation reads the very record `CreateOrganization`
/// wrote rather than a second copy of it — the same seam a stack's `RoleARN` is resolved
/// against IAM through ([`super::service_role_trust`]).
pub type OrganizationsState =
    ::service_com_amazonaws_organizations_awsorganizationsv20161128::ComAmazonawsOrganizationsAwsorganizationsv20161128State;

/// That store in the shared engine, through Organizations' own accessor — called rather than
/// copied. `None` has the same meaning as for [`super::service_state`]: the engine carries no
/// such store.
pub fn organizations_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<OrganizationsState>> {
    ::service_com_amazonaws_organizations_awsorganizationsv20161128::state::service_state(engine)
}

/// Refuse the call when this account is in no organization, so no trusted-access status is
/// reported or written for an organization that does not exist.
///
/// Trusted access is a setting OF AN ORGANIZATION: it is what gives the management account
/// "permissions to create and manage StackSets for your organization", so with no organization
/// there is nothing for the three calls of this switch to report on, activate or deactivate,
/// and each is refused with [`NO_ORGANIZATION`] — HTTP 400 with the wire code
/// `ValidationError`, the service-common refusal, since none of them models an error for it.
///
/// The question is put to AWS Organizations, which owns the record, at the instant the
/// decision is made: an account is in an organization only once `CreateOrganization` has put
/// it in one, and that call is the only thing that can change this answer. An engine carrying
/// no Organizations store establishes nothing either way — the same reading the service-role
/// check gives an engine with no IAM store ([`super::refuse_unassumable_role`]) — so the call
/// goes ahead there.
///
/// Generic over the operation's error enum, so all three calls refuse alike. Called with the
/// handler's `&*self.engine` and holding no store lock.
pub fn refuse_without_an_organization<Eng: Send + Sync + 'static, E>(
    engine: &Eng,
) -> Result<(), crate::scaffolded::Scaffolded<E>> {
    let Some(organizations) = organizations_state(engine) else {
        return Ok(());
    };
    if organizations.lock().organization.is_some() {
        return Ok(());
    }
    Err(crate::scaffolded::Scaffolded::validation_error(
        NO_ORGANIZATION.to_string(),
    ))
}

/// Whether trusted access between StackSets and Organizations is activated for this
/// organization.
///
/// A type of its own rather than a bare `bool` so that the one thing a caller can ask of it is
/// the modelled status it reports, and so the state struct keeps its derived `Default`: an
/// organization nobody activated trusted access for is the default, which reports
/// [`organization_status::DISABLED`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TrustedAccess {
    activated: bool,
}

impl TrustedAccess {
    /// Activate trusted access, as `ActivateOrganizationsAccess` does.
    ///
    /// Stating what is already in force changes nothing: the operation activates trusted
    /// access rather than claiming it was not active, and the model gives it no output and no
    /// already-activated error to say otherwise with.
    pub fn activate(&mut self) {
        self.activated = true;
    }

    /// Deactivate trusted access, as `DeactivateOrganizationsAccess` does — the inverse of
    /// [`Self::activate`], and equally indifferent to what was in force before.
    pub fn deactivate(&mut self) {
        self.activated = false;
    }

    /// The modelled `OrganizationStatus` this setting reports
    /// ([`organization_status`]).
    pub const fn status(self) -> &'static str {
        if self.activated {
            organization_status::ENABLED
        } else {
            organization_status::DISABLED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The statuses this module reports, and the `CallAs` value it refuses, name values the
    /// MODEL carries — whose value sets codegen puts in `types` — so neither can drift from
    /// what a caller can actually send or read back.
    #[test]
    fn the_named_values_are_modelled_values() {
        for status in [
            organization_status::ENABLED,
            organization_status::DISABLED,
            organization_status::DISABLED_PERMANENTLY,
        ] {
            let modelled = crate::ops::types::ORGANIZATION_STATUS_VALUES;
            assert!(
                modelled.contains(&status),
                "case {status:?}: expected the modelled OrganizationStatus value set to carry \
                 it, got {modelled:?}"
            );
        }
        let modelled = crate::ops::types::CALL_AS_VALUES;
        assert!(
            modelled.contains(&DELEGATED_ADMIN),
            "case {DELEGATED_ADMIN:?}: expected the modelled CallAs value set to carry it, got \
             {modelled:?}"
        );
    }

    /// Activating and deactivating are what move the reported status, and neither is sensitive
    /// to what was in force before it.
    #[test]
    fn the_reported_status_follows_the_writes() {
        let mut access = TrustedAccess::default();
        assert_eq!(
            access.status(),
            organization_status::DISABLED,
            "case \"never activated\": expected DISABLED, got {:?}",
            access.status()
        );
        for (case, writes, expected) in [
            ("activated once", 1, organization_status::ENABLED),
            ("activated twice", 2, organization_status::ENABLED),
        ] {
            let mut repeated = TrustedAccess::default();
            for _ in 0..writes {
                repeated.activate();
            }
            assert_eq!(
                repeated.status(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                repeated.status()
            );
        }
        access.activate();
        access.deactivate();
        assert_eq!(
            access.status(),
            organization_status::DISABLED,
            "case \"deactivated after an activation\": expected DISABLED, got {:?}",
            access.status()
        );
    }

    /// Only the `DELEGATED_ADMIN` value claims the call is a delegated administrator's; the
    /// default — a request that names no `CallAs` — and `SELF` are management-account calls.
    #[test]
    fn only_the_delegated_admin_value_is_that_claim() {
        for (case, call_as, expected) in [
            ("no CallAs", None, false),
            ("CallAs=SELF", Some("SELF"), false),
            ("CallAs=DELEGATED_ADMIN", Some(DELEGATED_ADMIN), true),
        ] {
            let actual = claims_delegated_administrator(call_as);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
