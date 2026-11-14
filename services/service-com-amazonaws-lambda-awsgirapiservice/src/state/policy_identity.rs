// SPDX-License-Identifier: CC0-1.0

//! WHICH identity a function's resource-based policy belongs to, and which identity can
//! hold none at all.
//!
//! `AddPermission` says what the identities are: "You can apply the policy at the function
//! level, or specify a qualifier to restrict access to a single version or alias. [...]
//! Note: Lambda does not support adding policies to version $LATEST", and `GetPolicy`
//! reports "the resource-based IAM policy for a function, version, or alias". So a policy
//! belongs to the FUNCTION, to a published version or to an alias — and the unpublished
//! version is none of the three. A request that spells `$LATEST` out therefore addresses an
//! identity that holds no policy document whatever grants the function itself carries: a
//! read of it reports no document and a removal finds none to change, exactly as for a
//! version or an alias nothing was ever granted on.
//!
//! The function-level policy is stored under the version label an UNQUALIFIED request
//! resolves to (`crate::state::VERSION_LATEST`), which is what keys
//! [`crate::state::FunctionRecord::policies`]. The two identities are therefore told apart
//! from the REQUEST — whether it named the unpublished version at all, which is what
//! [`names_unpublished_version`] reads — rather than from the resolved label, which cannot
//! say how it was arrived at ([`policy_bearing_version`]).

use super::{named_qualifier, VERSION_LATEST};

/// Whether a `FunctionName`/`Qualifier` pair EXPLICITLY names the unpublished version,
/// either as the qualifier appended to the name or as the `Qualifier` member.
///
/// The qualifier a pair NAMES is the service's one reading of those two members
/// ([`named_qualifier`]), and a pair naming two different ones names no single resource at
/// all and is refused where the pair is resolved (`crate::state::resolve_function`); this
/// asks only whether what it names is the unpublished version.
#[must_use]
pub fn names_unpublished_version(function_name: &str, qualifier: Option<&str>) -> bool {
    named_qualifier(function_name, qualifier).as_deref() == Some(VERSION_LATEST)
}

/// The key the resource-based policy of the identity a `FunctionName`/`Qualifier` pair
/// addressed is stored under, or `None` when that identity can hold no policy at all.
///
/// `version` is the label the pair resolved to (`crate::state::resolve_function`), which is
/// what a policy is keyed by. The answer is `None` exactly when the request spelled the
/// unpublished version out, because no call can attach a policy to it, so there is no
/// document for that identity to report, replace or remove — see the module header.
#[must_use]
pub fn policy_bearing_version<'v>(
    function_name: &str,
    qualifier: Option<&str>,
    version: &'v str,
) -> Option<&'v str> {
    (!names_unpublished_version(function_name, qualifier)).then_some(version)
}

/// The refusal a call that would ATTACH a permission to the unpublished version gets:
/// "Lambda does not support adding policies to version $LATEST", worded as the service
/// words it, with the version named from [`VERSION_LATEST`] rather than from the request.
#[must_use]
pub fn unpublished_version_grant_message() -> String {
    format!("We currently do not support adding policies for {VERSION_LATEST}.")
}
