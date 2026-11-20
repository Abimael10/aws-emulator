// SPDX-License-Identifier: CC0-1.0

//! Whether a request PRESENTS a SigV4 credential at all, and how AWS's front door words one
//! that presents none.
//!
//! SigV4 has two presentations and a request uses one of them: the `Authorization` header a
//! signing client writes, or the `X-Amz-*` query parameters a presigned request carries. The
//! ABSENCE of both is its own failure class — the caller sent no credentials — and AWS keeps
//! it apart from a credential that IS presented and is not one the service accepts: the
//! missing-credential class is what tells a client with an empty credential chain (no
//! profile, no environment, no instance role) that credential DISCOVERY is what failed, where
//! the unknown-credential class says the credentials it found were rejected. An SDK branches
//! on that difference, so collapsing the two sends an operator to debug key validity instead
//! of configuration.
//!
//! The rule is one rule for every service the emulator's one front door signs for, so the
//! reading lives here once:
//!
//! * a header presents a SigV4 signature when it names the AWS4 signature FAMILY
//!   ([`SIGV4_FAMILY_PREFIX`]) — a header naming any other scheme presents no AWS signature
//!   at all, and one naming the family with components missing has presented an incomplete
//!   signature rather than none;
//! * a query string presents one when it names the algorithm ([`QUERY_PRESENTATION`]), which
//!   is the query form's counterpart of the header's algorithm token.
//!
//! A TEMPORARY credential is served with a SECURITY TOKEN beside the key and the secret, and
//! whether one travelled is read the same way ([`PresentedCredential::security_token`]): it
//! decides which of two failures a credential the service will not accept had — a token that
//! was presented and is not the issued one, or a key id presented ALONE that resolves to no
//! usable credential.
//!
//! Only the PRESENCE is read. Nothing here reads a credential, a secret, a signature value or
//! any stored state, and nothing of the request is echoed back: [`MISSING_CREDENTIAL_MESSAGE`]
//! is fixed text, so a refusal cannot reflect a caller's own header to whoever sent it.
//!
//! What a service supplies is the CLASS it names this failure, which differs by protocol
//! family and by front door — the `awsQuery` identity services answer
//! `MissingAuthenticationToken`, an `awsJson` endpoint a namespaced exception shape — and the
//! status its own front door answers it with. The classes are therefore the service's to name
//! (see [`crate::authentication_refusal::StatedAuthenticationRefusal`]) and only the rule and
//! the sentence live here.

/// How AWS's SigV4 front door words a request that presented no credential at all: entirely
/// fixed text, with no part taken from the request.
pub const MISSING_CREDENTIAL_MESSAGE: &str = "Request is missing Authentication Token";

/// The prefix every member of the AWS4 signature family shares. A header carrying it presents
/// a SigV4 signature; a header carrying anything else presents no AWS signature at all.
const SIGV4_FAMILY_PREFIX: &str = "AWS4-";

/// The query parameter whose presence means the query string is presenting a signature,
/// however much of the rest of it is missing: the algorithm it names.
const QUERY_PRESENTATION: &str = "X-Amz-Algorithm";

/// The header a temporary credential's SECURITY TOKEN travels in, and the query field a
/// presigned call carries the same token in — the two places SigV4 puts it, as the emulator's
/// own verifier reads them.
pub const SECURITY_TOKEN_HEADER: &str = "x-amz-security-token";
const SECURITY_TOKEN_FIELD: &str = "X-Amz-Security-Token";

/// WHAT a request presented to authenticate with, as this module's readings answer it. Only
/// presence is recorded: no value of either presentation is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedCredential {
    /// Whether either SigV4 presentation carried a credential at all.
    pub credential: bool,
    /// Whether a security token travelled with it, as a temporary credential is served.
    pub security_token: bool,
}

impl PresentedCredential {
    /// What the request carrying these presented: its `Authorization` header, its security
    /// token header, and its raw query string.
    #[must_use]
    pub fn of(
        authorization: Option<&str>,
        security_token: Option<&str>,
        query: Option<&str>,
    ) -> Self {
        Self {
            credential: presents_a_credential(authorization, query),
            security_token: presents_a_security_token(security_token, query),
        }
    }
}

/// The `Authorization` header value when it PRESENTS a SigV4 signature, trimmed of
/// surrounding space; `None` for a request carrying no such header, an empty one, or one
/// naming another scheme.
///
/// The value is answered, not just the verdict, because a reader that goes on to ask WHICH
/// components the presentation states reads them off this same string.
#[must_use]
pub fn presenting_authorization(authorization: Option<&str>) -> Option<&str> {
    let value = authorization?.trim();
    value.starts_with(SIGV4_FAMILY_PREFIX).then_some(value)
}

/// Whether `query` — a URI's raw query string — presents a SigV4 signature.
///
/// Read through this crate's one reader of a raw query string ([`crate::query_parameter`]),
/// which compares the name as written: a SigV4 signer spells these names literally, because
/// they are part of the string it signed. A parameter present with no value has presented
/// nothing.
#[must_use]
pub fn query_presents_a_credential(query: Option<&str>) -> bool {
    query
        .and_then(|query| crate::query_parameter(query, QUERY_PRESENTATION))
        .is_some_and(|algorithm| !algorithm.is_empty())
}

/// Whether the request presents a SigV4 credential in EITHER of SigV4's two presentations.
///
/// `false` is the missing-credential class above: nothing in the request offers a credential
/// to resolve.
#[must_use]
pub fn presents_a_credential(authorization: Option<&str>, query: Option<&str>) -> bool {
    presenting_authorization(authorization).is_some() || query_presents_a_credential(query)
}

/// Whether the request presents a SECURITY TOKEN — in the header a signing client writes it
/// in, or the field a presigned call carries it in.
///
/// A header or field present but empty has presented none: a temporary credential is served
/// with the token minted beside it, so an empty one is no more a token than an absent one.
#[must_use]
pub fn presents_a_security_token(security_token: Option<&str>, query: Option<&str>) -> bool {
    if security_token.is_some_and(|token| !token.trim().is_empty()) {
        return true;
    }
    query
        .and_then(|query| crate::query_parameter(query, SECURITY_TOKEN_FIELD))
        .is_some_and(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        presenting_authorization, presents_a_credential, presents_a_security_token,
        query_presents_a_credential, PresentedCredential,
    };

    /// A request presents a SigV4 credential when a header names the AWS4 family or the query
    /// string names the algorithm, and presents none otherwise.
    ///
    /// The rows differ along what the rule generalises over: which of the two presentations
    /// carries it, whether the presentation is complete, and the forms that look like one
    /// without being one (another scheme, an empty header, a bare parameter name, a query
    /// string of the service's own fields).
    #[test]
    fn a_credential_is_presented_in_either_sigv4_scheme_and_in_nothing_else() {
        /// One row: the `Authorization` header, the query string, and whether a credential is
        /// presented.
        struct Row {
            case: &'static str,
            authorization: Option<&'static str>,
            query: Option<&'static str>,
            presented: bool,
        }
        let rows = vec![
            Row {
                case: "a header-signed request",
                authorization: Some(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/sts/\
                     aws4_request, SignedHeaders=host, Signature=00",
                ),
                query: None,
                presented: true,
            },
            Row {
                // Incomplete, but still a presentation: what it left out is another layer's
                // question.
                case: "a header naming the family and nothing else",
                authorization: Some("AWS4-"),
                query: None,
                presented: true,
            },
            Row {
                case: "a header padded with space",
                authorization: Some("  AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE"),
                query: None,
                presented: true,
            },
            Row {
                case: "a presigned request",
                authorization: None,
                query: Some(
                    "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F20260101\
                     %2Fus-east-1%2Fsts%2Faws4_request&X-Amz-Signature=00",
                ),
                presented: true,
            },
            Row {
                case: "a presigned request signed under another algorithm",
                authorization: None,
                query: Some("X-Amz-Algorithm=AWS4-ECDSA-P256-SHA256&X-Amz-Signature=00"),
                presented: true,
            },
            Row {
                case: "both presentations at once",
                authorization: Some("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE"),
                query: Some("X-Amz-Algorithm=AWS4-HMAC-SHA256"),
                presented: true,
            },
            Row {
                case: "a header naming another scheme",
                authorization: Some("Basic YWxpY2U6c2VjcmV0"),
                query: None,
                presented: false,
            },
            Row {
                case: "an empty header",
                authorization: Some("   "),
                query: None,
                presented: false,
            },
            Row {
                case: "the algorithm named with no value",
                authorization: None,
                query: Some("X-Amz-Algorithm="),
                presented: false,
            },
            Row {
                // Signature fields without the one that MAKES a presentation.
                case: "a query string carrying every field but the algorithm",
                authorization: None,
                query: Some("X-Amz-Credential=AKIDEXAMPLE&X-Amz-Signature=00"),
                presented: false,
            },
            Row {
                case: "the service's own query fields",
                authorization: None,
                query: Some("Action=GetCallerIdentity&Version=2011-06-15"),
                presented: false,
            },
            Row {
                case: "nothing at all",
                authorization: None,
                query: None,
                presented: false,
            },
        ];

        for Row {
            case,
            authorization,
            query,
            presented: expected,
        } in rows
        {
            let actual = presents_a_credential(authorization, query);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected a credential presented == {expected}, got {actual}"
            );
            // The two halves answer for their own presentation only, so a reader that tells
            // the forms apart reads the same verdict this one does.
            let halves = presenting_authorization(authorization).is_some()
                || query_presents_a_credential(query);
            assert_eq!(
                halves, expected,
                "case {case:?}: the two halves must agree with the whole, got {halves}"
            );
        }
    }

    /// The header reader answers the value a later reader of its components needs, with the
    /// surrounding space gone.
    #[test]
    fn the_presenting_header_is_answered_as_written_without_surrounding_space() {
        let padded = "  AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE  ";
        let actual = presenting_authorization(Some(padded));
        assert_eq!(
            actual,
            Some("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE"),
            "case 'a padded header': expected the trimmed presentation, got {actual:?}"
        );
    }

    /// A security token is presented in the header a signing client writes it in or the field
    /// a presigned call carries it in, and an empty one is none.
    ///
    /// The rows differ along which place carries it, and the forms that carry none (absent,
    /// empty, space, a bare parameter name, another field of the same prefix).
    #[test]
    fn a_security_token_is_presented_in_either_place_sigv4_puts_one() {
        // (case, the security-token header, the query string, whether a token is presented)
        let rows: [(&str, Option<&str>, Option<&str>, bool); 8] = [
            ("a header-carried token", Some("IQoJb3JpZ2luX2VjEX"), None, true),
            (
                "a presigned token",
                None,
                Some("X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Security-Token=IQoJb3JpZ2luX2Vj"),
                true,
            ),
            ("a token in both places", Some("IQoJ"), Some("X-Amz-Security-Token=IQoJ"), true),
            ("no token anywhere", None, None, false),
            ("an empty header", Some(""), None, false),
            ("a header of space", Some("  "), None, false),
            ("the field named with no value", None, Some("X-Amz-Security-Token="), false),
            (
                // Another signing field of the same prefix, which is not the token.
                "a presigned credential scope alone",
                None,
                Some("X-Amz-Credential=AKIDEXAMPLE&X-Amz-SignedHeaders=host"),
                false,
            ),
        ];
        for (case, header, query, expected) in rows {
            let actual = presents_a_security_token(header, query);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected a security token presented == {expected}, got {actual}"
            );
        }
    }

    /// What a request presented is read in ONE pass over its two presentations, so the two
    /// halves of the note can never be read from different requests.
    #[test]
    fn what_a_request_presented_is_read_in_one_pass() {
        let presented = PresentedCredential::of(
            Some("AWS4-HMAC-SHA256 Credential=ASIAEXAMPLE"),
            Some("IQoJb3JpZ2luX2Vj"),
            Some("Action=GetCallerIdentity"),
        );
        assert_eq!(
            presented,
            PresentedCredential {
                credential: true,
                security_token: true,
            },
            "case 'a temporary credential signed in the header': got {presented:?}"
        );
        let unsigned = PresentedCredential::of(None, None, None);
        assert_eq!(
            unsigned,
            PresentedCredential {
                credential: false,
                security_token: false,
            },
            "case 'an unsigned request': got {unsigned:?}"
        );
    }
}
