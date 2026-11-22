// SPDX-License-Identifier: CC0-1.0

//! The `resourceArn` the tagging surface addresses a resource by: the ARN FORMS this
//! service issues for a resource that carries tags, and the refusal a value of no such
//! form takes.
//!
//! `TagResource`, `UntagResource` and `ListTagsForResource` share one route
//! (`/v1/tags/{resourceArn}`) whose label targets `ResourceArn`, a string `@length` 70..75
//! with `@pattern "^arn:aws:appsync:[A-Za-z0-9_/.-]{0,63}:\d{12}:apis/[0-9A-Za-z_-]{26}$"`
//! (`smithy_query(query="shape", shape="ResourceArn")`). That form describes an API's ARN
//! alone, yet a tag map is carried by every resource whose create takes `tags` — a
//! `GraphqlApi`, an `Api` and a `ChannelNamespace` (`tags` on each of those shapes) — and
//! a channel namespace's ARN is the one THIS SERVICE mints for it, the API's ARN followed
//! by the path a namespace is addressed under ([`super::channel_namespace_resource`]). An
//! ARN the service itself issued names a resource whose tags the surface holds, so it is
//! served rather than refused; what the member's own form cannot do is state the whole
//! surface, because a namespace's ARN is longer than its `@length` window admits.
//!
//! So the ACCEPTED set is the union of the forms this service publishes for a taggable
//! resource, each checked as its own expression:
//!
//! * [`API_ARN_PATTERN`] — the member's own modelled expression AND its `@length` window,
//!   which together admit exactly the API ARNs this service issues: every field of that
//!   form is fixed-width but the Region label, so the 70..75 window is the Region label
//!   being [`ARN_REGION_MIN_LEN`]..[`ARN_REGION_MAX_LEN`] characters long and is written
//!   into the expression as that group's own bound;
//! * [`CHANNEL_NAMESPACE_ARN_PATTERN`] — that same API ARN, then the namespace path and a
//!   name inside `Namespace`'s own modelled expression. The member's `@length` window
//!   bounds the API form alone and cannot bound this one, which is longer by the path and
//!   the name; what carries over is the API ARN it is built on, Region label included.
//!
//! A value of NEITHER form names no resource this service could address, which is a defect
//! of the request: it is refused with the surface's own sentence
//! ([`super::invalid_resource_arn_message`], which names the vendor it expected when the
//! value is an ARN of another one) under the one client error the tagging operations model,
//! `BadRequestException` (HTTP 400). A value of either form that this account holds no
//! resource for is a different answer — the absent resource
//! ([`super::RESOURCE_NOT_FOUND_MESSAGE`]) — decided by the store rather than here.
//!
//! The check runs HERE rather than at the request boundary (`skip_validation` on
//! `resourceArn` in `src/ops/types/`, the same split [`super::api_name_refusal`] and
//! [`super::resource_name_refusal`] are there for): the boundary's walk enforces the
//! member's modelled expression as written, which would refuse an ARN AWS serves, and the
//! seam that words a boundary refusal is handed the broken clauses alone. The member of
//! ANOTHER shape that merely shares the name (`RdsDataApiConfig$resourceArn`, an RDS
//! cluster ARN) keeps the boundary's check: it is a different member with a pattern of its
//! own.

/// `ResourceArn`'s modelled `@length` window.
pub const RESOURCE_ARN_MIN_LEN: usize = 70;
pub const RESOURCE_ARN_MAX_LEN: usize = 75;

/// How many characters an API's ARN carries BESIDE its Region label: everything the
/// modelled expression fixes the width of (`arn:aws:appsync:` and the two separators, a
/// twelve-digit account, `apis/` and a 26-character api id).
const API_ARN_FIXED_LEN: usize = 61;

/// The Region label widths the two modelled constraints TOGETHER admit: an API ARN of
/// [`RESOURCE_ARN_MIN_LEN`]..[`RESOURCE_ARN_MAX_LEN`] characters is one whose Region label
/// is this long, because every other field of the form is fixed-width.
pub const ARN_REGION_MIN_LEN: usize = RESOURCE_ARN_MIN_LEN - API_ARN_FIXED_LEN;
pub const ARN_REGION_MAX_LEN: usize = RESOURCE_ARN_MAX_LEN - API_ARN_FIXED_LEN;

/// The modelled `@pattern` of `ResourceArn` with its `@length` window folded into the one
/// group whose width varies: an API of either resource family, which is the one form the
/// member itself describes.
pub const API_ARN_PATTERN: &str =
    r"^arn:aws:appsync:[A-Za-z0-9_/.-]{9,14}:\d{12}:apis/[0-9A-Za-z_-]{26}$";

/// The form a CHANNEL NAMESPACE's ARN takes: the API's own ARN
/// ([`API_ARN_PATTERN`], with its closing anchor opened up) followed by the path this
/// service names a namespace of that API under, and a name inside the expression
/// `Namespace` declares (`^([A-Za-z0-9](?:[A-Za-z0-9\-]{0,48}[A-Za-z0-9])?)$`,
/// `smithy_query(query="shape", shape="Namespace")`).
///
/// It is the ARN this service MINTS for the resource
/// ([`super::channel_namespace_resource`]), which is what `ChannelNamespace$channelNamespaceArn`
/// answers and therefore what a client holds when it asks for that namespace's tags.
pub const CHANNEL_NAMESPACE_ARN_PATTERN: &str = r"^arn:aws:appsync:[A-Za-z0-9_/.-]{9,14}:\d{12}:apis/[0-9A-Za-z_-]{26}/channelNamespace/([A-Za-z0-9](?:[A-Za-z0-9\-]{0,48}[A-Za-z0-9])?)$";

/// [`API_ARN_PATTERN`] compiled, which is how it is EVALUATED: see
/// [`super::declared_pattern`] for the one reading of a `@pattern` a handler checks itself.
static API_ARN_REGEX: super::declared_pattern::CompiledPattern =
    ::std::sync::LazyLock::new(|| ::regex::Regex::new(API_ARN_PATTERN).ok());

/// [`CHANNEL_NAMESPACE_ARN_PATTERN`] compiled; see [`API_ARN_REGEX`].
static CHANNEL_NAMESPACE_ARN_REGEX: super::declared_pattern::CompiledPattern =
    ::std::sync::LazyLock::new(|| ::regex::Regex::new(CHANNEL_NAMESPACE_ARN_PATTERN).ok());

/// Whether `candidate` is an ARN of one of the forms this service issues for a resource
/// that carries tags.
pub fn is_tagged_resource_arn(candidate: &str) -> bool {
    super::declared_pattern::matches_declared_pattern(&API_ARN_REGEX, candidate)
        || super::declared_pattern::matches_declared_pattern(
            &CHANNEL_NAMESPACE_ARN_REGEX,
            candidate,
        )
}

/// The text `BadRequestException` (HTTP 400) carries for a tagging `resourceArn` of no
/// form this service issues, or `None` for a value of one of them.
///
/// The sentence is the surface's own, which names the vendor it expected when the value is
/// an ARN of another vendor and otherwise states only that the value is not a resource ARN
/// ([`super::invalid_resource_arn_message`]); nothing of the submitted value is echoed.
pub fn tagged_resource_arn_refusal(candidate: &str) -> Option<String> {
    (!is_tagged_resource_arn(candidate))
        .then(|| super::invalid_resource_arn_message(Some(candidate)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Region label of the length the member's `@length` window admits.
    const REGION: &str = "us-west-2";
    const ACCOUNT: &str = "000000000000";
    const API_ID: &str = "abcdefghijklmnopqrstuvwxyz";

    /// RULE: every ARN form this service issues for a taggable resource is accepted.
    #[test]
    fn every_issued_form_is_accepted() {
        for (case, candidate) in [
            ("an API's ARN", format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{API_ID}")),
            (
                "a channel namespace's ARN",
                format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{API_ID}/channelNamespace/default"),
            ),
            (
                "a namespace name of one character, the shortest the name admits",
                format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{API_ID}/channelNamespace/n"),
            ),
            (
                "a namespace name at the longest the name admits",
                format!(
                    "arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{API_ID}/channelNamespace/{}",
                    "n".repeat(50)
                ),
            ),
            (
                "a namespace name carrying the hyphen its expression admits",
                format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{API_ID}/channelNamespace/a-1"),
            ),
            (
                "an API in another Region and account",
                format!("arn:aws:appsync:eu-central-1:123456789012:apis/{API_ID}"),
            ),
            // The two published bounds of the member's `@length` window, as the widths of
            // the one field whose width varies.
            (
                "an API ARN on the modelled minimum length",
                format!(
                    "arn:aws:appsync:{}:{ACCOUNT}:apis/{API_ID}",
                    "r".repeat(ARN_REGION_MIN_LEN)
                ),
            ),
            (
                "an API ARN on the modelled maximum length",
                format!(
                    "arn:aws:appsync:{}:{ACCOUNT}:apis/{API_ID}",
                    "r".repeat(ARN_REGION_MAX_LEN)
                ),
            ),
            (
                "a namespace under an API ARN on the modelled maximum length",
                format!(
                    "arn:aws:appsync:{}:{ACCOUNT}:apis/{API_ID}/channelNamespace/default",
                    "r".repeat(ARN_REGION_MAX_LEN)
                ),
            ),
        ] {
            let refusal = tagged_resource_arn_refusal(&candidate);
            assert!(
                refusal.is_none(),
                "case {case:?} ({candidate}): expected no refusal, got {refusal:?}"
            );
        }

        // The two bound rows above really are ON the member's published bounds, so what
        // they accept is those lengths rather than some other width.
        for (case, length, region) in [
            ("the modelled minimum", RESOURCE_ARN_MIN_LEN, ARN_REGION_MIN_LEN),
            ("the modelled maximum", RESOURCE_ARN_MAX_LEN, ARN_REGION_MAX_LEN),
        ] {
            let arn = format!(
                "arn:aws:appsync:{}:{ACCOUNT}:apis/{API_ID}",
                "r".repeat(region)
            );
            assert_eq!(
                arn.chars().count(),
                length,
                "case {case:?}: expected a Region label of {region} characters to make an ARN \
                 of {length}, got {arn}"
            );
        }
    }

    /// RULE: a value of no issued form is refused, and the sentence names the vendor it
    /// expected only where the value's vendor field is another one.
    #[test]
    fn a_value_of_no_issued_form_is_refused() {
        for (case, candidate, expected) in [
            (
                "an ARN of another vendor",
                "arn:aws:s3:::bucket".to_string(),
                "Invalid input resource arn: expected vendor appsync",
            ),
            (
                "an API id one character short",
                format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{}", &API_ID[..25]),
                "Invalid input resource arn",
            ),
            (
                "another resource type",
                format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apiz/{API_ID}"),
                "Invalid input resource arn",
            ),
            (
                "a namespace name one character past the longest admitted",
                format!(
                    "arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{API_ID}/channelNamespace/{}",
                    "n".repeat(51)
                ),
                "Invalid input resource arn",
            ),
            (
                "a namespace path with no name",
                format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{API_ID}/channelNamespace/"),
                "Invalid input resource arn",
            ),
            (
                "a child path this service publishes no tag map for",
                format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{API_ID}/datasources/orders"),
                "Invalid input resource arn",
            ),
            // One past each published bound of the member's `@length` window.
            (
                "an API ARN one character under the modelled minimum length",
                format!(
                    "arn:aws:appsync:{}:{ACCOUNT}:apis/{API_ID}",
                    "r".repeat(ARN_REGION_MIN_LEN - 1)
                ),
                "Invalid input resource arn",
            ),
            (
                "an API ARN one character over the modelled maximum length",
                format!(
                    "arn:aws:appsync:{}:{ACCOUNT}:apis/{API_ID}",
                    "r".repeat(ARN_REGION_MAX_LEN + 1)
                ),
                "Invalid input resource arn",
            ),
            (
                "a namespace under an API ARN past the modelled maximum length",
                format!(
                    "arn:aws:appsync:{}:{ACCOUNT}:apis/{API_ID}/channelNamespace/default",
                    "r".repeat(ARN_REGION_MAX_LEN + 1)
                ),
                "Invalid input resource arn",
            ),
            ("no ARN at all", "not-an-arn".to_string(), "Invalid input resource arn"),
        ] {
            let refusal = tagged_resource_arn_refusal(&candidate);
            assert_eq!(
                refusal.as_deref(),
                Some(expected),
                "case {case:?} ({candidate}): expected {expected:?}, got {refusal:?}"
            );
        }
    }
}
