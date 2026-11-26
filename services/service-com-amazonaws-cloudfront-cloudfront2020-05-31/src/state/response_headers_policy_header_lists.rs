// SPDX-License-Identifier: CC0-1.0

//! The response headers a policy may NOT take off a response.
//!
//! `ResponseHeadersPolicyConfig$RemoveHeadersConfig` is "a list of HTTP header names that
//! CloudFront removes from HTTP responses to requests that match this response headers policy",
//! and CloudFront documents the headers it keeps whatever a policy says
//! ([`HEADERS_A_RESPONSE_HEADERS_POLICY_CANNOT_REMOVE`]). The list is a property of the
//! CONFIGURATION, so the rule is judged here, once, for every payload that states one — the call
//! that creates a policy and the call that replaces one's configuration alike — and before
//! anything is minted or written, so a refused request leaves no policy and no name of its own
//! behind.

/// The response headers a policy may NOT take off a response, as CloudFront documents them for
/// `RemoveHeadersConfig` ("Remove HTTP headers from CloudFront responses": the headers you
/// cannot remove).
///
/// These are the headers CloudFront keeps whatever a policy says: the ones that FRAME the
/// response — how its body is delimited and how it is encoded — the ones that belong to the one
/// CONNECTION it was served over rather than to the response, and the two that record which hops
/// served it. A response with any of them taken off could not be read by the viewer that asked
/// for it, or would describe a connection that is not the one it arrived on. Everything else — a
/// `Server`, an `X-Powered-By`, any header the application set — stays removable, which is what
/// the member is for.
///
/// The member carries no `@pattern` and no enumeration in the model, so the set is not refused
/// before a handler runs and the policy path refuses it itself
/// ([`remove_header_not_allowed_message`]). Recorded behaviour covers one entry of it
/// (`Content-Length` is refused); the rest is the documented list, which is why it is written out
/// here rather than inferred from one recording.
pub const HEADERS_A_RESPONSE_HEADERS_POLICY_CANNOT_REMOVE: &[&str] = &[
    "Connection",
    "Content-Encoding",
    "Content-Length",
    "Expect",
    "Host",
    "Keep-Alive",
    "Proxy-Authenticate",
    "Proxy-Authorization",
    "Proxy-Connection",
    "Trailer",
    "Transfer-Encoding",
    "Upgrade",
    "Via",
    "Warning",
];

/// The header `config` asks CloudFront to remove that it will not take off a response, AS THE
/// REQUEST SPELLED IT, or `None` when every header the configuration names may be removed.
///
/// Matched case-insensitively, because a header NAME is case-insensitive (RFC 9110 section 5.1):
/// the header a configuration names is the same header however it is spelled, so whether it is
/// refused does not depend on the spelling — but the sentence reports the spelling the request
/// carried, since what it names is the value that was submitted. The FIRST such header in the
/// list the configuration states is the one reported, so one request is refused the same way
/// however many of them it names.
pub fn response_headers_policy_unremovable_header(
    config: &crate::ops::types::ResponseHeadersPolicyConfig,
) -> Option<String> {
    config
        .remove_headers_config
        .as_ref()
        .and_then(|section| section.items.as_ref())
        .and_then(|items| {
            items
                .iter()
                .find(|entry| {
                    HEADERS_A_RESPONSE_HEADERS_POLICY_CANNOT_REMOVE
                        .iter()
                        .any(|kept| entry.header.eq_ignore_ascii_case(kept))
                })
                .map(|entry| entry.header.clone())
        })
}

/// The modelled `InvalidArgument` (HTTP 400) for a configuration asking CloudFront to remove a
/// header it keeps ([`HEADERS_A_RESPONSE_HEADERS_POLICY_CANNOT_REMOVE`]).
pub fn remove_header_not_allowed(header: &str) -> crate::ops::types::InvalidArgument {
    crate::ops::types::InvalidArgument {
        message: Some(remove_header_not_allowed_message(header)),
    }
}

/// The sentence CloudFront refuses such a configuration with: fixed text naming the PARAMETER
/// the list arrived in ([`REMOVE_HEADERS_PARAMETER`]) around the header the request named, and
/// nothing else of the request — neither the rest of the list nor which headers it could have
/// named instead.
pub fn remove_header_not_allowed_message(header: &str) -> String {
    format!("The parameter {REMOVE_HEADERS_PARAMETER} contains {header} that is not allowed.")
}

/// The name the WIRE reports `ResponseHeadersPolicyConfig$RemoveHeadersConfig`'s list under,
/// which is the name CloudFront's refusal of that list carries.
pub const REMOVE_HEADERS_PARAMETER: &str = "RemoveHeaders";
