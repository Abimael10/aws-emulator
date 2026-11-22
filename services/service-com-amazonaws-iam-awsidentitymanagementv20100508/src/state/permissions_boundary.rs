// SPDX-License-Identifier: CC0-1.0

//! How a `PermissionsBoundary` a request carries is RESOLVED, and what the operation
//! carrying it is refused with when that value bounds nothing.
//!
//! "The ARN of the managed policy that is used to set the permissions boundary"
//! (`CreateUser$PermissionsBoundary`, `CreateRole$PermissionsBoundary`) is a REFERENCE to a
//! policy of the account: a boundary "defines the maximum permissions that identity-based
//! policies can grant to an entity", which is a question asked of that policy's document
//! every time the entity is authorized — so an identity may carry only a boundary the
//! account can resolve to a managed policy. The rule is stated once, here, because every
//! operation that sets a boundary applies it: the two creates that carry one (`CreateUser`,
//! `CreateRole`) and the two puts that set one on a held identity
//! (`PutUserPermissionsBoundary`, `PutRolePermissionsBoundary`).

use super::ComAmazonawsIamAwsidentitymanagementv20100508State as IamState;
use super::{arn_not_valid, boundary_scope_not_attachable, is_arn, parse_policy_arn};

/// Why a `PermissionsBoundary` the request supplied cannot bound an identity — the three
/// layers a member that must be a managed policy's ARN is taken in, in the order they are
/// decided, each answered under a different error code by the operation that reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionsBoundaryDefect {
    /// The value is not an ARN at all ([`is_arn`]), so it names no resource of any
    /// service: the FORM of the member is wrong, which IAM answers `ValidationError`.
    NotAnArn,
    /// The value IS an ARN but not a managed policy's ([`parse_policy_arn`]) — another
    /// service's, or another IAM resource type's — so it is an "invalid … value …
    /// supplied for an input parameter", the operation's modelled `InvalidInputException`.
    NotAPolicyArn,
    /// The value is a managed policy's ARN the account holds no policy under
    /// ([`IamState::managed_policy_exists`]), so it names no policy any evaluation of the
    /// boundary could read.
    NoSuchPolicy,
}

impl PermissionsBoundaryDefect {
    /// The sentence IAM refuses `permissions_boundary` with for this defect.
    ///
    /// The two FORM layers answer the sentence of the policy-ARN family
    /// ([`arn_not_valid`]), which both layers share because the error CODE is what
    /// separates them; a boundary that resolves to no policy answers the sentence of the
    /// boundary family ([`boundary_scope_not_attachable`]), what a boundary call names
    /// being a SCOPE on an identity's permissions rather than an attachment — so a create
    /// that carries a dangling boundary and a put that sets one are refused in the same
    /// words.
    pub fn message(self, permissions_boundary: &str) -> String {
        match self {
            Self::NotAnArn | Self::NotAPolicyArn => arn_not_valid(permissions_boundary),
            Self::NoSuchPolicy => boundary_scope_not_attachable(permissions_boundary),
        }
    }
}

/// Whether `permissions_boundary` is of the FORM a boundary is named by at all — a managed
/// policy's ARN — without asking the account anything: the layer every operation taking the
/// member decides before it locks the store.
pub fn permissions_boundary_form_defect(
    permissions_boundary: &str,
) -> Option<PermissionsBoundaryDefect> {
    if parse_policy_arn(permissions_boundary).is_some() {
        return None;
    }
    Some(if is_arn(permissions_boundary) {
        PermissionsBoundaryDefect::NotAPolicyArn
    } else {
        PermissionsBoundaryDefect::NotAnArn
    })
}

/// Why `permissions_boundary` cannot bound an identity of this account, or `None` when it
/// resolves to a managed policy the account holds: the form layers above, and then the
/// account's own resolution of the policy ([`IamState::managed_policy_exists`], the ONE
/// resolution every `PolicyArn` of this service is resolved by).
///
/// A READ: the store is only consulted.
pub fn permissions_boundary_defect(
    store: &IamState,
    permissions_boundary: &str,
) -> Option<PermissionsBoundaryDefect> {
    if let Some(defect) = permissions_boundary_form_defect(permissions_boundary) {
        return Some(defect);
    }
    (!store.managed_policy_exists(permissions_boundary))
        .then_some(PermissionsBoundaryDefect::NoSuchPolicy)
}
