// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a listing's `Marker` when the value is no resume point that
//! listing hands out.
//!
//! A `Marker` is where the previous page ended, so WHAT the value is decides which refusal an
//! unusable one gets — three answers, each read off the listing's own model:
//!
//! * the marker is the identifier of a resource of the kind the listing pages, and the model
//!   binds a not-found error NAMED for that kind: a marker that resolves to no such resource
//!   is reported as that resource not existing, under that code, in the sentence that names
//!   the MARKER of that resource ([`invalid_resource_marker_message`]). The FORM of the value
//!   is no separate condition, because this service publishes no form for the identifiers it
//!   mints;
//! * the model publishes a CONSTRAINT for the shape such a value has to be: a value outside it
//!   is a value this API cannot read, stated in the frame every constraint violation of this
//!   API is stated in ([`unreadable_pagination_token_message`]) and answered under the code an
//!   unreadable request carries (`crate::service::envelope`);
//! * the model offers neither: the refusal states the CONSTRAINT on the parameter — which kind
//!   of identifier a marker of that listing has to be ([`invalid_marker_message`]) — which is
//!   all such a listing can say, under the `InvalidArgument` its model does bind.
//!
//! Each sentence is a property of the LISTING rather than of the value submitted, so nothing
//! read off the request (the marker itself) and nothing read off the service's own state (an
//! identifier of a resource the listing is scoped to) belongs in it — the one exception being
//! the constraint frame, which states the value it is about as every other statement of that
//! frame does.
//!
//! One renderer builds each family of sentence, so the listings that reach it cannot disagree
//! about how this service states the same condition. The listings whose refusal already stated
//! the parameter's constraint keep their own constant beside the rest of their modelled error
//! text ([`crate::state::INVALID_PUBLIC_KEY_MARKER_MESSAGE`] and its neighbours): the SENTENCE
//! is the same text in the same template, written out once per listing because each is the
//! wire text recorded for that listing and is pinned verbatim by that listing's own suite.

/// The template every invalid-`Marker` sentence of this service is one filling of: the
/// parameter, the constraint, and the kind of identifier it has to be.
///
/// Read the pieces off [`invalid_marker_message`] rather than splicing this yourself.
const MARKER_CONSTRAINT: &str = "Marker must be a valid";

/// The sentence a listing of DISTRIBUTIONS refuses an unusable `Marker` with — the id the
/// previous page of distributions ended at.
///
/// Stated for every listing whose marker is a distribution id, which is `ListDistributions`
/// and each listing that selects distributions by something they reference: the parameter and
/// its constraint are the same in all of them.
pub const DISTRIBUTION_MARKER_KIND: &str = "Distribution Id";

/// The sentence a listing of INVALIDATIONS refuses an unusable `Marker` with — the id of the
/// invalidation the previous page ended at, on either invalidation surface.
pub const INVALIDATION_MARKER_KIND: &str = "Invalidation Id";

/// The kind a DISTRIBUTION TENANT listing's marker names.
pub const DISTRIBUTION_TENANT_MARKER_KIND: &str = "CF distribution tenant Id";

/// The kind a CONNECTION GROUP listing's marker names.
pub const CONNECTION_GROUP_MARKER_KIND: &str = "CF connection group Id";

/// The kind a CONNECTION FUNCTION listing's marker names.
pub const CONNECTION_FUNCTION_MARKER_KIND: &str = "CF connection function Id";

/// The kind an ORIGIN ACCESS CONTROL listing's marker names.
pub const ORIGIN_ACCESS_CONTROL_MARKER_KIND: &str = "CF origin access control Id";

/// The kind a TRUST STORE listing's marker names, worded as the sibling listings of the
/// namespace-prefixed resource kinds word theirs ([`CONNECTION_GROUP_MARKER_KIND`]).
pub const TRUST_STORE_MARKER_KIND: &str = "CF trust store Id";

/// The kind a VPC ORIGIN listing's marker names, worded as the sibling listings of the
/// namespace-prefixed resource kinds word theirs ([`TRUST_STORE_MARKER_KIND`]), over the name
/// this service's own VPC origin sentences spell the kind with
/// ([`crate::state::VPC_ORIGIN_NOT_FOUND_MESSAGE`]).
pub const VPC_ORIGIN_MARKER_KIND: &str = "CF vpc origin Id";

/// The sentence this service refuses a `Marker` that is no resume point with, for a listing
/// whose marker names `kind`.
///
/// `kind` is the kind of identifier the parameter has to be — a property of the listing, not of
/// the request — so every value this takes is one of the constants above and never a value
/// read off a request or out of the store.
pub fn invalid_marker_message(kind: &str) -> String {
    format!("{MARKER_CONSTRAINT} {kind}")
}

/// The pattern a listing's pagination TOKEN is written in, as the SERVICE publishes it in the
/// refusal of one it cannot read — recorded behaviour, not a trait of this crate's model, which
/// declares the parameter as a plain `string`
/// (`smithy_query(query="shape",shape="ListKeyValueStoresRequest")`).
///
/// A value outside a constraint the service publishes is a value this API cannot read, whichever
/// layer read that constraint: the request boundary refuses one against a constraint the SCHEMA
/// carries, and this service answers every one of those under the code it answers an unreadable
/// request with ([`crate::service::envelope`]) — so a listing that reads such a constraint
/// itself has to reach the same answer rather than a second one.
///
/// `ListKeyValueStores`' `Marker` is "the marker associated with the key value stores list":
/// the API publishes no identifier for it, so what bounds the value is this pattern, which the
/// member it travels in does not declare — nothing enforces it before the handler runs, and the
/// handler does ([`is_pagination_token`]).
pub const PAGINATION_TOKEN_PATTERN: &str = r"[\w :+=./\\-]*";

/// The member a refusal of such a token names it by: the name the SERVICE states for the token
/// in that refusal (recorded behaviour, as [`PAGINATION_TOKEN_PATTERN`] is), which is not the
/// name of the parameter it travelled in.
pub const PAGINATION_TOKEN_MEMBER: &str = "nextToken";

/// Whether `value` is written in the characters a pagination token admits
/// ([`PAGINATION_TOKEN_PATTERN`]): a word character, or one of the punctuation marks a token
/// may carry. `\w` is the ASCII word alphabet, so a letter outside it is outside the pattern.
pub fn is_pagination_token(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || matches!(character, '_' | ' ' | ':' | '+' | '=' | '.' | '/' | '\\' | '-')
    })
}

/// The sentence a pagination token outside that pattern is refused with.
///
/// The frame is the one every constraint violation of this API is stated in — the member, the
/// value it carried and the constraint it broke — because that is what the defect IS: a value
/// outside a constraint the API publishes, which it answers under the same code whichever layer
/// read the constraint ([`crate::service::envelope`]). Recorded behaviour: the service states
/// this clause, naming the token's own member and the pattern it admits, for a `Marker` it
/// cannot read.
///
/// The value is echoed as every clause of this service quotes one — through the workspace's
/// one character-boundary-safe elision, held to this service's own ceiling
/// ([`MAX_ECHOED_TOKEN`]) — so a caller cannot amplify one request into an arbitrarily long
/// response and no two clauses of this service elide differently.
pub fn unreadable_pagination_token_message(value: &str) -> String {
    format!(
        "1 validation error detected: Value '{}' at '{PAGINATION_TOKEN_MEMBER}' failed to \
         satisfy constraint: Member must satisfy regular expression pattern: \
         {PAGINATION_TOKEN_PATTERN}",
        service_common::validation::quoted_echo_up_to(value, MAX_ECHOED_TOKEN)
    )
}

/// The most bytes of a token a refusal echoes, which is the bound the layer that states this
/// frame for a constraint the schema carries elides a value at.
const MAX_ECHOED_TOKEN: usize = 256;

/// The sentence a listing refuses a `Marker` that resolves to no resource of the kind it pages
/// with, where the refusal names the MARKER rather than the resource: the listings whose model
/// binds a not-found error named for that kind, which is the CODE such a refusal carries.
///
/// The wording is the service's own for this condition, recorded on the origin request policy
/// listing and stated for each resource kind the same way. FIXED text around the kind of
/// resource the listing pages — a property of the listing rather than of the value submitted —
/// so nothing from the request reaches it and two runs of the same mistake are refused in the
/// same words.
pub fn invalid_resource_marker_message(resource: &str) -> String {
    format!("The request contains an invalid {resource} marker.")
}

/// The resource a CACHE POLICY listing pages, as its marker refusal names it.
pub const CACHE_POLICY_MARKER_RESOURCE: &str = "cache policy";

/// The resource an ORIGIN REQUEST POLICY listing pages, as its marker refusal names it.
pub const ORIGIN_REQUEST_POLICY_MARKER_RESOURCE: &str = "origin request policy";

/// The resource a RESPONSE HEADERS POLICY listing pages, as its marker refusal names it.
pub const RESPONSE_HEADERS_POLICY_MARKER_RESOURCE: &str = "response headers policy";

/// The resource a CONTINUOUS DEPLOYMENT POLICY listing pages, as its marker refusal names it.
pub const CONTINUOUS_DEPLOYMENT_POLICY_MARKER_RESOURCE: &str = "continuous deployment policy";

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence is the constraint on the parameter and the kind of identifier it names,
    /// and it carries nothing else — one row per kind of marker this service pages by.
    #[test]
    fn an_invalid_marker_sentence_states_the_constraint_and_nothing_from_a_request() {
        // (case, the kind the listing's marker names, the sentence it is refused with)
        let rows: [(&str, &str, &str); 6] = [
            (
                "a distribution listing",
                DISTRIBUTION_MARKER_KIND,
                "Marker must be a valid Distribution Id",
            ),
            (
                "an invalidation listing",
                INVALIDATION_MARKER_KIND,
                "Marker must be a valid Invalidation Id",
            ),
            (
                "a distribution tenant listing",
                DISTRIBUTION_TENANT_MARKER_KIND,
                "Marker must be a valid CF distribution tenant Id",
            ),
            (
                "a connection group listing",
                CONNECTION_GROUP_MARKER_KIND,
                "Marker must be a valid CF connection group Id",
            ),
            (
                "a connection function listing",
                CONNECTION_FUNCTION_MARKER_KIND,
                "Marker must be a valid CF connection function Id",
            ),
            (
                "an origin access control listing",
                ORIGIN_ACCESS_CONTROL_MARKER_KIND,
                "Marker must be a valid CF origin access control Id",
            ),
        ];
        for (case, kind, expected) in rows {
            let actual = invalid_marker_message(kind);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the sentence {expected:?}, got {actual:?}"
            );
        }
    }

    /// The sentence a listing whose model names the resource its marker identifies refuses an
    /// unresolvable one with: fixed text around that resource, and nothing else — one row per
    /// listing that states it.
    #[test]
    fn an_invalid_resource_marker_sentence_names_the_resource_and_nothing_else() {
        let rows: [(&str, &str, &str); 4] = [
            (
                "a cache policy listing",
                CACHE_POLICY_MARKER_RESOURCE,
                "The request contains an invalid cache policy marker.",
            ),
            (
                "an origin request policy listing",
                ORIGIN_REQUEST_POLICY_MARKER_RESOURCE,
                "The request contains an invalid origin request policy marker.",
            ),
            (
                "a response headers policy listing",
                RESPONSE_HEADERS_POLICY_MARKER_RESOURCE,
                "The request contains an invalid response headers policy marker.",
            ),
            (
                "a continuous deployment policy listing",
                CONTINUOUS_DEPLOYMENT_POLICY_MARKER_RESOURCE,
                "The request contains an invalid continuous deployment policy marker.",
            ),
        ];
        for (case, resource, expected) in rows {
            let actual = invalid_resource_marker_message(resource);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the sentence {expected:?}, got {actual:?}"
            );
        }
    }

    /// A pagination token is admitted exactly where every character of it is one the published
    /// pattern allows, and a token outside it is refused in the frame every constraint
    /// violation of this API is stated in — with the value echoed, and elided when it is long
    /// enough to amplify one request into a long answer.
    #[test]
    fn a_pagination_token_is_read_against_the_published_pattern() {
        // (case, the value, whether the pattern admits it) — every character class the
        // pattern names, each bound of it, and the classes it excludes.
        let long = "t".repeat(MAX_ECHOED_TOKEN * 2);
        let rows: [(&str, &str, bool); 10] = [
            ("the empty token", "", true),
            ("a word", "orders", true),
            ("the word alphabet with an underscore", "a_Z9", true),
            (
                "every punctuation mark the pattern admits",
                r"a b:c+d=e.f/g\h-i",
                true,
            ),
            ("a token far longer than any name", long.as_str(), true),
            ("an exclamation mark", "a!b", false),
            ("a hash", "a#b", false),
            ("a comma", "a,b", false),
            ("a letter outside the word alphabet", "héllo", false),
            ("the recorded shape of a bad marker", "not a valid marker !! ###", false),
        ];
        for (case, value, admitted) in rows {
            assert_eq!(
                is_pagination_token(value),
                admitted,
                "case {case:?}: expected the pattern to admit {value:?}: {admitted}"
            );
        }

        let message = unreadable_pagination_token_message("a!b");
        assert_eq!(
            message,
            format!(
                "1 validation error detected: Value 'a!b' at 'nextToken' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: \
                 {PAGINATION_TOKEN_PATTERN}"
            ),
            "case \"a token outside the pattern\": expected the constraint frame, got {message:?}"
        );

        let elided = unreadable_pagination_token_message(&long);
        assert!(
            elided.len() < long.len(),
            "case \"a long token\": the echoed value must be elided, got {} bytes",
            elided.len()
        );
        assert!(
            elided.contains(&format!("[{} bytes]", long.len())),
            "case \"a long token\": the elision must state the length that was sent, got \
             {elided:?}"
        );
    }
}
