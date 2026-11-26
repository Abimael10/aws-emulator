// SPDX-License-Identifier: CC0-1.0

//! What a TOKEN DOMAIN has to be: a domain a client application can actually be
//! registered under.
//!
//! Every member of this API that takes one says the same thing about it — "public
//! suffixes aren't allowed. For example, you can't use `gov.au` or `co.uk` as token
//! domains" (`CreateAPIKey$TokenDomains`, and `TokenDomains` on a web ACL created or
//! updated, each shape's own doc) — because a token WAF issues for a domain is sent
//! back by every host under it: a cookie scoped to a public suffix would be shared by
//! every unrelated registration beneath it, which is exactly what a public suffix is
//! defined to be.
//!
//! So the vocabulary is not this emulator's to invent: WHICH names are public suffixes
//! is the Public Suffix List, read here through the maintained crate that carries it
//! ([`psl`]) rather than through a table written out beside the rule. A name whose
//! whole length is a suffix that list knows has no registrable label in front of it and
//! is refused; every other name — one label under a known suffix, and a name under a
//! suffix the list does not carry — is a domain a client can hold, and the member's own
//! `@length`/`@pattern` constraints judge its spelling.
//!
//! One thing those constraints do NOT judge is whether the value is a domain NAME at
//! all: `TokenDomain`'s pattern admits a `/`
//! (`smithy_query(query="shape",shape="TokenDomain")` → `@pattern ^[\w\.\-/]+$`), and
//! no domain name carries one — a value with a path segment names a location under a
//! host rather than the host, which is not something a token can be scoped to
//! ([`is_domain_name`]). A value that is no name, like a name nothing is registered
//! as, is the parameter-format refusal below.

use super::*;

/// `true` when `domain` is itself a public suffix — a name the Public Suffix List
/// carries in full, so nothing is registered AS it.
///
/// The comparison is by LENGTH rather than by text: [`psl::suffix`] answers the
/// suffix as a slice of the name it was given, so a suffix as long as the whole
/// name IS the whole name. A suffix the list does not know (`is_known` false, the
/// wildcard every unlisted top-level label falls under) is not one AWS's sentence is
/// about, so a name under one is left to the member's own constraints.
pub fn is_public_suffix(domain: &str) -> bool {
    // A domain name is case-insensitive and may be written with the root's own trailing
    // dot, so the list is asked about the name in one case and without that dot: a
    // caller that wrote a suffix in capitals, or fully qualified, wrote the same suffix.
    let domain = domain
        .strip_suffix('.')
        .unwrap_or(domain)
        .to_ascii_lowercase();
    ::psl::suffix(domain.as_bytes())
        .is_some_and(|suffix| suffix.is_known() && suffix.as_bytes().len() == domain.len())
}

/// Whether `domain` is a domain NAME: dot-separated labels, and nothing but labels.
///
/// A name may be written fully qualified, with the root's own trailing dot, which is
/// not an empty label; anything else that leaves a label empty (a leading dot, two
/// dots in a row) names nothing, and neither does a value carrying a path segment.
pub fn is_domain_name(domain: &str) -> bool {
    let name = domain.strip_suffix('.').unwrap_or(domain);
    !name.is_empty()
        && !name.contains('/')
        && name.split('.').all(|label| !label.is_empty())
}

/// The first token domain of `domains` WAF will not accept, or `None` when every one
/// of them is a domain a client application can be registered under.
///
/// One reading for every member that takes token domains ([`is_domain_name`] and
/// [`is_public_suffix`]), so a key and the web ACL whose tokens it is used with cannot
/// disagree about which domains are usable.
pub fn refused_token_domain(domains: &[String]) -> Option<&str> {
    domains
        .iter()
        .map(String::as_str)
        .find(|domain| !is_domain_name(domain) || is_public_suffix(domain))
}

/// [`invalid_parameter_exception`] for a token domain WAF will not accept: `Field`
/// [`FIELD_TOKEN_DOMAIN`] (the settings the domain was written in), `Parameter` the
/// rejected domain as the request wrote it and `Reason`
/// [`FORMAT_NOT_VALID_REASON`] — the sentence WAF answers for a parameter whose text
/// is not in the form that parameter has to take.
pub fn invalid_token_domain_exception(domain: &str) -> WAFInvalidParameterException {
    invalid_parameter_exception(FIELD_TOKEN_DOMAIN, domain, FORMAT_NOT_VALID_REASON)
}
