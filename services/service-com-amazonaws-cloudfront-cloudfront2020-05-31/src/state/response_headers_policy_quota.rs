// SPDX-License-Identifier: CC0-1.0

//! The published per-policy QUOTA on the response headers a policy adds of its own.
//!
//! `ResponseHeadersPolicyConfig$CustomHeadersConfig` is "a list of HTTP response header names
//! and their values" that "CloudFront includes … in HTTP responses that it sends for requests
//! that match a cache behavior that's associated with this response headers policy", and
//! CloudFront publishes a maximum for how many such headers ONE policy may carry. The list is a
//! property of the CONFIGURATION, so the bound is judged here, once, for every payload that
//! states one — the call that creates a policy and the call that replaces one's configuration
//! alike — and a configuration past it is refused with the error the model binds for exactly
//! that condition, before anything is minted or written, so a refused request leaves no policy
//! and no name of its own behind.

/// The most response headers of its own one response headers policy may add, over which a
/// configuration is refused with the modelled `TooManyCustomHeadersInResponseHeadersPolicy`.
///
/// `ResponseHeadersPolicyCustomHeadersConfig` carries no `@length` in the model, so the bound is
/// not refused before a handler runs. CloudFront's published quota for "Custom headers per
/// response headers policy" is 10, and the error itself points at that quota page ("For more
/// information, see Quotas … in the Amazon CloudFront Developer Guide"), so the bound is that
/// number rather than one chosen here.
pub const MAX_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY: usize = 10;

/// Whether `config` adds more response headers of its own than
/// [`MAX_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY`].
///
/// Counted over the headers the configuration actually LISTS, not over the `Quantity` it states:
/// the two are held to agree by the wrapper rule
/// ([`super::response_headers_policy_quantity_mismatch`]), and what the policy would add is its
/// items. A configuration that states no custom headers section, or one whose section lists
/// none, adds nothing and so is inside the bound.
pub fn response_headers_policy_custom_headers_over_quota(
    config: &crate::ops::types::ResponseHeadersPolicyConfig,
) -> bool {
    config
        .custom_headers_config
        .as_ref()
        .and_then(|section| section.items.as_ref())
        .is_some_and(|items| items.len() > MAX_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY)
}

/// The modelled `TooManyCustomHeadersInResponseHeadersPolicy` (HTTP 400) for a configuration
/// adding more response headers of its own than the published quota
/// ([`MAX_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY`]).
///
/// The sentence states the CONDITION — the request carries more headers than are allowed — so
/// nothing from the request belongs in it, neither the headers nor how many there were, and
/// every refusal of this condition reads the same.
pub fn too_many_custom_headers_in_response_headers_policy(
) -> crate::ops::types::TooManyCustomHeadersInResponseHeadersPolicy {
    crate::ops::types::TooManyCustomHeadersInResponseHeadersPolicy {
        message: Some(TOO_MANY_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY_MESSAGE.to_string()),
    }
}

/// `TooManyCustomHeadersInResponseHeadersPolicy`'s message, on the wire.
///
/// The WIRE text — what the recorded service answers this code with, and what this emulator
/// therefore serves. It differs from the sentence the MODEL's documentation of the shape uses
/// for the same condition ("The number of custom headers in the response headers policy exceeds
/// the maximum. For more information, see Quotas …"): that one documents the condition and
/// points at the quota page, this one is served. Do not "correct" this constant to the
/// documentation sentence.
pub const TOO_MANY_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY_MESSAGE: &str =
    "Your request contains more headers in the Response Headers Policy than are allowed.";
