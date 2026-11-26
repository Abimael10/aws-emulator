// SPDX-License-Identifier: CC0-1.0

//! When a record may carry an `endpointAccessMode`, and the sentence a request that sets
//! one without that is refused with.
//!
//! `endpointAccessMode` is "the endpoint access mode of the DomainName. Only available for
//! DomainNames that use security policies that start with `SecurityPolicy_`"
//! (`smithy_query(query="doc", shape="CreateDomainNameRequest",
//! member="endpointAccessMode")`), and `RestApi`'s member of the same name documents the
//! same restriction over the API's own security policies. So the member is a fact about the
//! POLICY the endpoint negotiates under, not about one operation: a record whose
//! `securityPolicy` is not one of those policies has no access mode to report, whichever
//! call tried to set one.
//!
//! One copy, reached by every writer of either member — the creates of both resources and
//! the patches that write the pair on a stored record — so the pairing cannot be stored
//! through one path and refused on another.

/// The prefix the policies that admit an `endpointAccessMode` are named by, from the
/// member's own documentation: "security policies that start with `SecurityPolicy_`".
///
/// A prefix rather than a list, because that is how the member states it: every value of the
/// modelled `SecurityPolicy` enum carrying it admits the mode, and the two legacy
/// `TLS_1_*` values do not (`crate::ops::types::SECURITY_POLICY_VALUES`).
pub const ENDPOINT_ACCESS_MODE_POLICY_PREFIX: &str = "SecurityPolicy_";

/// The `BadRequestException` message for an `endpointAccessMode` on a record whose
/// `securityPolicy` does not admit one: entirely fixed text about the pairing, naming
/// neither the mode nor the policy the request carried.
pub const ENDPOINT_ACCESS_MODE_UNSUPPORTED: &str =
    "Endpoint access mode is not supported for this security policy";

/// Whether `security_policy` is one of the policies an `endpointAccessMode` is available
/// for; see [`ENDPOINT_ACCESS_MODE_POLICY_PREFIX`].
pub fn admits_endpoint_access_mode(security_policy: &str) -> bool {
    security_policy.starts_with(ENDPOINT_ACCESS_MODE_POLICY_PREFIX)
}

/// Refuse a record that would carry `endpoint_access_mode` under `security_policy`.
///
/// Stated over the RECORD as it would be stored: `security_policy` is the policy the record
/// ends up with — the one the request set or the service's own default
/// ([`super::DEFAULT_SECURITY_POLICY`], which is not one of the policies that admit a mode)
/// — so a request that supplies a mode and no policy is refused exactly as one that supplies
/// a mode with a policy that does not admit it. A member set to no characters sets no mode,
/// the same unset member an absent one is.
pub fn require_endpoint_access_mode_policy(
    endpoint_access_mode: Option<&str>,
    security_policy: Option<&str>,
) -> Result<(), String> {
    let requested = endpoint_access_mode.filter(|mode| !mode.is_empty());
    if requested.is_none() {
        return Ok(());
    }
    if security_policy.is_some_and(admits_endpoint_access_mode) {
        return Ok(());
    }
    Err(ENDPOINT_ACCESS_MODE_UNSUPPORTED.to_string())
}
