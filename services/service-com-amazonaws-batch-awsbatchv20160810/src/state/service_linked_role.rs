// SPDX-License-Identifier: CC0-1.0

//! The role AWS Batch ASSUMES for a compute environment it manages, and where it comes from
//! when the request names none.
//!
//! # The rule
//!
//! `ComputeEnvironmentDetail$serviceRole` is "the service role that's associated with the
//! compute environment that allows Batch to make calls to Amazon Web Services API operations
//! on your behalf", and `CreateComputeEnvironmentRequest$serviceRole` states where it comes
//! from when the create carries none: "if your account already created the Batch
//! service-linked role, that role is used by default for your compute environment unless you
//! specify a different role here. If the Batch service-linked role doesn't exist in your
//! account, and no role is specified here, the service attempts to create the Batch
//! service-linked role in your account. This automatic service-linked role creation only
//! applies to MANAGED compute environments. For UNMANAGED compute environments, you must
//! explicitly specify a serviceRole."
//!
//! So a MANAGED environment ALWAYS has a service role — the caller's where the request names
//! one, this account's Batch service-linked role otherwise — and the substitution is made
//! once, where the environment is created, so every later read of it reports the same role
//! and no read has to invent one.
//!
//! # The role is IAM's, so its name and path are IAM's
//!
//! A service-linked role is an IAM resource: it is filed under the reserved path
//! `/aws-service-role/<service principal>/` and carries the name that service publishes. Both
//! come from the emulated IAM service's OWN catalog of published service-linked roles here,
//! so the ARN this service reports for the role and the one IAM answers a read of that role
//! with are one string — a client that takes `serviceRole` off a described environment and
//! asks IAM about it finds the role, rather than an ARN only this crate believes in.
//!
//! The role's own name ([`SERVICE_LINKED_ROLE_NAME`]) is the fallback for a catalog that does
//! not carry this service's principal; the name itself is the one this model names, in
//! `UpdateComputeEnvironment`'s own documentation of an infrastructure update ("the
//! AWSServiceRoleForBatch service-linked role must exist in your account").

use super::*;

// The IAM service's state, for the catalog that resolves a service principal to the role
// name and path IAM publishes for it. Reached exactly as this crate's authorization gate
// reaches that store's policy documents: one source of truth per fact, held by the service
// that owns it.
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;

/// The service principal AWS Batch's service-linked role is linked to: this service's own
/// endpoint prefix under `amazonaws.com` ([`ARN_SERVICE`], `OPERATIONS.md`), which is also
/// the principal IAM's catalog lists the role under.
pub const SERVICE_PRINCIPAL: &str = "batch.amazonaws.com";

/// The name AWS Batch publishes for its service-linked role, as this model's own
/// `UpdateComputeEnvironment` documentation names it ("the AWSServiceRoleForBatch
/// service-linked role").
pub const SERVICE_LINKED_ROLE_NAME: &str = "AWSServiceRoleForBatch";

/// The ARN of this account's Batch service-linked role:
/// `arn:aws:iam::<account>:role/aws-service-role/batch.amazonaws.com/AWSServiceRoleForBatch`.
///
/// The role is an IAM resource, so the ARN names IAM rather than this service, and IAM is
/// global: it carries NO Region, whichever Region the caller signed for. One role is kept per
/// account and reused, so every environment that falls back to it reports the same ARN.
pub fn service_linked_role_arn() -> String {
    let role_name = iam_store::published_service_linked_role_name(SERVICE_PRINCIPAL)
        .unwrap_or(SERVICE_LINKED_ROLE_NAME);
    iam_store::role_arn(
        &iam_store::service_linked_role_path(SERVICE_PRINCIPAL),
        role_name,
    )
}

/// The `serviceRole` a compute environment of this `ce_type` is CREATED with, given the
/// `requested` one.
///
/// The role the request names wins, whatever the type. With none named, a
/// [`CE_TYPE_MANAGED`] environment falls back to this account's Batch service-linked role
/// ([`service_linked_role_arn`]) — the automatic fallback the create documents — and an
/// environment of any other type keeps none, because that fallback "only applies to MANAGED
/// compute environments".
pub fn created_service_role(ce_type: &str, requested: Option<String>) -> Option<String> {
    match requested {
        Some(role) => Some(role),
        None if ce_type == CE_TYPE_MANAGED => Some(service_linked_role_arn()),
        None => None,
    }
}
