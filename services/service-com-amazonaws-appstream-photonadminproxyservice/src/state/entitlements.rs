// SPDX-License-Identifier: CC0-1.0

//! The entitlements a stack's applications are entitled to SAML 2.0 federated users
//! through: the record `CreateEntitlement` writes and `DescribeEntitlements` renders back.
//!
//! An entitlement has no ARN and no name of its own in the account: "the name of the stack
//! with which the entitlement is associated" is part of its identity, so the store is keyed
//! by `(StackName, Name)` ([`EntitlementKey`]) and two stacks can each hold an entitlement
//! of the same name. Everything about the record lives here — the identity, the stored
//! members, how one is rendered on the wire, and how one stack's entitlements are read —
//! so a create and a read can never disagree about any of it.

use ::std::collections::BTreeSet;

use crate::ops::types::{Entitlement, EntitlementAttribute};

/// The word AWS's refusal sentences use for an entitlement.
pub const ENTITLEMENT_KIND: &str = "entitlement";

/// AWS's `EntitlementAlreadyExistsException` sentence for an identity the stack already
/// holds an entitlement under.
///
/// Unlike the duplicate-name sentence for a resource named in the account
/// ([`super::resource_already_exists_message`]), this one states the condition about the
/// entitlement the request asked for and echoes NOTHING of the request: not the
/// entitlement's name, not the stack it would belong to, not the account. A recorded
/// AppStream refusal of a second create of one entitlement establishes it.
pub fn entitlement_already_exists_message() -> String {
    format!("The requested {ENTITLEMENT_KIND} already exists.")
}

/// AWS's `EntitlementNotFoundException` sentence for an entitlement the stack holds none
/// of, the mirror image of [`entitlement_already_exists_message`].
///
/// The same template with the condition this refusal states: an entitlement refusal says
/// what is true of the requested entitlement and names none of the request's values, so
/// this sentence has no variable part either.
pub fn entitlement_not_found_message() -> String {
    format!("The requested {ENTITLEMENT_KIND} was not found.")
}

/// How many entitlements one `DescribeEntitlements` page carries.
///
/// The model declares `MaxResults` and `NextToken` on that read but publishes no page
/// limit, so this ceiling is the emulator's own; what matters on the wire is that a caller
/// who follows `NextToken` sees every entitlement of the stack exactly once, in a stable
/// order.
pub const ENTITLEMENT_PAGE_SIZE: usize = 25;

/// The identity an entitlement is stored under: the stack it is associated with, then its
/// own name.
///
/// In that order, so one stack's entitlements form a contiguous range of the store and a
/// read of that stack walks them without scanning the others.
pub type EntitlementKey = (String, String);

/// The identity the `(StackName, Name)` pair of a request names.
pub fn entitlement_key(stack_name: &str, name: &str) -> EntitlementKey {
    (stack_name.to_string(), name.to_string())
}

/// A persisted AppStream entitlement: which applications of one stack a federated user
/// identity is entitled to, and the user attributes that decide whether it applies.
#[derive(Debug, Clone)]
pub struct EntitlementRecord {
    pub name: String,
    pub stack_name: String,
    pub description: Option<String>,
    /// One of `APP_VISIBILITY_VALUES` (`ops/types/shared_types.rs`): whether all or
    /// selected apps are entitled.
    pub app_visibility: String,
    /// The user attributes the entitlement matches on, as the request supplied them —
    /// `EntitlementAttributeList` is `@length` 1.., so an entitlement always has at least
    /// one.
    pub attributes: Vec<EntitlementAttribute>,
    pub created_time: ::aws_smithy_types::DateTime,
    /// When the entitlement was last modified, which for one nothing has modified yet is
    /// the instant it was created at.
    pub last_modified_time: ::aws_smithy_types::DateTime,
}

impl EntitlementRecord {
    /// Render the stored record as the modelled `Entitlement` every reader returns.
    ///
    /// `Description` is reported exactly as it is stored: its target is `@length` 0..256,
    /// so the empty string is a description an entitlement can carry rather than the
    /// absence of one.
    pub fn to_entitlement(&self) -> Entitlement {
        Entitlement {
            name: self.name.clone(),
            stack_name: self.stack_name.clone(),
            description: self.description.clone(),
            app_visibility: self.app_visibility.clone(),
            attributes: self.attributes.clone(),
            created_time: Some(self.created_time),
            last_modified_time: Some(self.last_modified_time),
        }
    }
}

impl super::ComAmazonawsAppstreamPhotonadminproxyserviceState {
    /// The entitlement the `(StackName, Name)` pair names; `None` when the stack holds
    /// none under that name.
    pub fn entitlement(&self, stack_name: &str, name: &str) -> Option<&EntitlementRecord> {
        self.entitlements.get(&entitlement_key(stack_name, name))
    }

    /// The names of the entitlements the named stack holds, in the stable order a page of
    /// `DescribeEntitlements` walks.
    ///
    /// Read straight from the stored keys, so the names are exactly the records the store
    /// holds for that stack — there is no second index that could miss one.
    pub fn entitlement_names_for_stack(&self, stack_name: &str) -> BTreeSet<String> {
        self.entitlements
            .range(entitlement_key(stack_name, "")..)
            .take_while(|((stored_stack, _), _)| stored_stack == stack_name)
            .map(|((_, name), _)| name.clone())
            .collect()
    }

    /// Drop every entitlement the named stack holds.
    ///
    /// An entitlement belongs to the stack it is associated with — it is identified by it
    /// and controls access to that stack's applications — so it cannot outlive it: a stack
    /// later created under the same name starts with no entitlements.
    pub fn remove_entitlements_for_stack(&mut self, stack_name: &str) {
        for name in self.entitlement_names_for_stack(stack_name) {
            self.entitlements.remove(&entitlement_key(stack_name, &name));
        }
    }
}
