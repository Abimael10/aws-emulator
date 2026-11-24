// SPDX-License-Identifier: CC0-1.0

//! What the bulk loader reads out of a load request before it loads anything.
//!
//! A load runs UNDER an IAM role: the loader assumes the role the request names and reads
//! the source with that role's credentials, so a value it cannot resolve a credential for
//! is a load that could never start. `iamRoleArn` is an unconstrained string in the model,
//! so the request boundary admits any text at all and reading the value is this service's
//! own job ([`resolves_credentials`]) — and a value that names no IAM role is refused
//! naming both halves of what failed: the source the load was for, and the role it would
//! have run under ([`unusable_iam_role`]).
//!
//! The ARN itself is read with the crate-wide ARN requirement for an IAM role
//! (`::service_common::arn::IAM_ROLE`), so this service and every other agree on what the
//! ARN of a role is.

use crate::ops::types::InvalidParameterException;

/// Whether the loader can resolve a credential for `iam_role_arn`.
///
/// A credential is resolved for an IAM ROLE, so the value has to name one: an ARN of this
/// partition's `iam` namespace addressing a `role` resource. A value that is not an ARN at
/// all, an ARN of another service, and an ARN naming another kind of IAM entity each name
/// no role to assume, so no credential can be found for any of them.
#[must_use]
pub fn resolves_credentials(iam_role_arn: &str) -> bool {
    ::service_common::arn::IAM_ROLE.admits(iam_role_arn)
}

/// The `InvalidParameterException` payload for a load the loader could not start because
/// it has no credential for the role the request named.
///
/// Both variable parts are the request's own: the source the load was to read, and the
/// role it was to run under, so the caller can see which load and which role from the
/// message alone.
#[must_use]
pub fn unusable_iam_role(source: &str, iam_role_arn: &str) -> InvalidParameterException {
    InvalidParameterException {
        detailed_message: format!(
            "Failed to start new load from the source {}. Couldn't find the aws credential \
             for iam_role_arn: {}",
            super::echoed(source),
            super::echoed(iam_role_arn)
        ),
        request_id: super::new_request_id(),
    }
}
