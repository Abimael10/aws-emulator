// SPDX-License-Identifier: CC0-1.0

//! The form a `ResourceARN` must have before the tagging operations look anything up.
//!
//! `ListTagsForResource`, `TagResource` and `UntagResource` address their resource by
//! `ResourceARN` (`AmazonResourceName`, `@length` 1..1011, the only constraint the model
//! carries — `smithy_query(query="operation",operation="ListTagsForResource,TagResource,UntagResource",model_path="services/service-com-amazonaws-servicediscovery-route53autonaming-v20170314/smithy-model/com-amazonaws-servicediscovery-route53autonaming-v20170314.json")`),
//! and each declares `InvalidInput` beside `ResourceNotFoundException`. The two classes
//! answer different questions, so which one a call gets is decided by the ARN's SYNTAX
//! and not by the store: an ARN whose resource portion is not one this service could ever
//! have minted names no resource at any point in time, so it is bad input
//! ([`INVALID_RESOURCE_FORMAT_MESSAGE`]), while a well-formed one that resolves to
//! nothing is a resource that is absent.
//!
//! What an ARN IS — its six fields, the `arn` literal, the partition and vendor that may
//! not be empty, and the resource field that keeps its own colons — is the tree's one
//! parse, `service_common::arn`, read here through `parse_of` against this service's
//! `arnNamespace` ([`super::ARN_NAMESPACE`]): an ARN of another vendor names nothing in
//! this service's namespace, which is the same bad request. What stays here is only this
//! service's own resource FORM.
//!
//! Only two resource kinds carry tags here, and this emulator mints exactly one
//! identifier form for each: `namespace/ns-…` and `service/srv-…`, the kind prefix (with
//! the `/` separator the refusal's own sentence names, the one Cloud Map writes) followed
//! by the lowercase-alphanumeric identifier body every id this store mints carries —
//! [`super::RESOURCE_ID_DIGITS`] characters, the width the creates and
//! [`super::derived_service_id`] mint. So that pair of prefixes and that identifier body
//! are what a taggable ARN carries, which is what the refusal's own sentence states and
//! illustrates.
//!
//! The rule is checked BEFORE the lookup and in one place, so the three tagging
//! operations cannot drift apart in which inputs they call malformed.

/// The sentence this service refuses a malformed `ResourceARN` with. Fixed text: it
/// names the two resource prefixes and one example identifier of each, and echoes
/// nothing of the request.
pub const INVALID_RESOURCE_FORMAT_MESSAGE: &str = "Invalid resource format in ARN. Resource \
     must start with 'service/' or 'namespace/' followed by a valid identifier. Example: \
     'namespace/ns-a2b3c4d5e6f7g2h3' or 'service/srv-b2c3d4e5f6g7h2i3'";

/// Each taggable resource kind, as the ARN's resource field names it, and as that kind's
/// identifier is prefixed.
const TAGGABLE_RESOURCES: [(&str, &str); 2] = [("namespace/", "ns-"), ("service/", "srv-")];

/// [`INVALID_RESOURCE_FORMAT_MESSAGE`] when `resource_arn` names no resource form this
/// service can tag, and `None` when its resource field is well formed — whether or not
/// any resource is stored under it, which is the store's question and not this one's.
pub fn invalid_resource_format(resource_arn: &str) -> Option<&'static str> {
    (!names_a_taggable_resource(resource_arn)).then_some(INVALID_RESOURCE_FORMAT_MESSAGE)
}

/// Whether `resource_arn` is a well-formed ARN of this service's vendor whose resource
/// field is `namespace/` or `service/` followed by an identifier of that kind's minted
/// form.
fn names_a_taggable_resource(resource_arn: &str) -> bool {
    // What an ARN is — the fields, the literal, the non-empty partition, vendor and
    // resource — is the tree's one parse; an ARN of another vendor addresses nothing in
    // this service's namespace, so it is the same bad request as a malformed resource.
    let Some(arn) = ::service_common::arn::parse_of(resource_arn, super::ARN_NAMESPACE) else {
        return false;
    };
    TAGGABLE_RESOURCES.iter().any(|(kind, id_prefix)| {
        arn.resource
            .strip_prefix(kind)
            .and_then(|id| id.strip_prefix(id_prefix))
            .is_some_and(minted_identifier_body)
    })
}

/// Whether `body` is an identifier body this store mints: exactly
/// [`super::RESOURCE_ID_DIGITS`] lowercase alphanumerics (the alphabet the store folds a
/// digest into when it mints one).
fn minted_identifier_body(body: &str) -> bool {
    body.len() == super::RESOURCE_ID_DIGITS
        && body
            .chars()
            .all(|character| character.is_ascii_digit() || character.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed ARN for `resource`, as this service mints them.
    fn arn(resource: &str) -> String {
        format!("arn:aws:servicediscovery:us-east-1:123456789012:{resource}")
    }

    /// RULE: the resource field decides, and it is well formed only as one of the two
    /// taggable kinds followed by a minted identifier. The rows differ along what the
    /// rule generalises over: the resource kind, the identifier's length, its character
    /// repertoire, the kind prefix and the resource separator.
    #[test]
    fn only_a_minted_resource_identifier_is_well_formed() {
        // (case, the resource field of the ARN, well formed)
        let rows: &[(&str, &str, bool)] = &[
            ("a namespace id of the minted width", "namespace/ns-a2b3c4d5e6f7g2h3", true),
            ("a service id of the minted width", "service/srv-b2c3d4e5f6g7h2i3", true),
            ("digits only", "namespace/ns-0123456789012345", true),
            ("letters only", "service/srv-abcdefghijklmnop", true),
            ("one character short", "namespace/ns-a2b3c4d5e6f7g2h", false),
            ("one character long", "namespace/ns-a2b3c4d5e6f7g2h3x", false),
            ("a word where the identifier belongs", "namespace/ns-doesnotexist000", false),
            ("no identifier at all", "namespace/", false),
            ("no kind prefix", "ns-a2b3c4d5e6f7g2h3", false),
            ("the other kind's id prefix", "namespace/srv-b2c3d4e5f6g7h2i3", false),
            ("a kind this service does not tag", "instance/i-a2b3c4d5e6f7g2h3", false),
            ("uppercase in the identifier", "namespace/ns-A2B3C4D5E6F7G2H3", false),
            ("punctuation in the identifier", "namespace/ns-a2b3c4d5e6f7g2h.", false),
            ("a trailing path segment", "namespace/ns-a2b3c4d5e6f7g2h3/x", false),
            ("a colon where the separator belongs", "namespace:ns-a2b3c4d5e6f7g2h3", false),
        ];

        for (case, resource, well_formed) in rows {
            let answered = invalid_resource_format(&arn(resource));
            assert_eq!(
                answered.is_none(),
                *well_formed,
                "case {case:?}: expected well_formed={well_formed}, got {answered:?}"
            );
            if !well_formed {
                assert_eq!(
                    answered,
                    Some(INVALID_RESOURCE_FORMAT_MESSAGE),
                    "case {case:?}: the refusal states the one fixed sentence"
                );
            }
        }
    }

    /// RULE: a value that is not a well-formed ARN of this service's vendor names no
    /// taggable resource either, whatever its resource field reads like — a bare resource
    /// field is not waved through for want of an ARN around it, and another vendor's ARN
    /// addresses nothing in this namespace.
    #[test]
    fn only_a_well_formed_arn_of_this_vendor_names_a_taggable_resource() {
        for case in [
            "",
            "ns-a2b3c4d5e6f7g2h3",
            "namespace/ns-a2b3c4d5e6f7g2h3",
            "arn:aws:servicediscovery",
            "arn:aws:servicediscovery:us-east-1:123456789012",
            "arn:aws:servicediscovery:us-east-1:123456789012:",
            "arn::servicediscovery:us-east-1:123456789012:namespace/ns-a2b3c4d5e6f7g2h3",
            "arn:aws::us-east-1:123456789012:namespace/ns-a2b3c4d5e6f7g2h3",
            "arn:aws:s3:us-east-1:123456789012:namespace/ns-a2b3c4d5e6f7g2h3",
            "arn:aws:ServiceDiscovery:us-east-1:123456789012:namespace/ns-a2b3c4d5e6f7g2h3",
        ] {
            assert_eq!(
                invalid_resource_format(case),
                Some(INVALID_RESOURCE_FORMAT_MESSAGE),
                "case {case:?}: expected the resource-format refusal"
            );
        }
    }

    /// RULE: the Region and the account an ARN names are not this rule's business — a
    /// taggable resource field is well formed under any of them, the empty ones AWS writes
    /// for a partition-global or account-less resource included, and under any partition.
    #[test]
    fn the_region_and_account_do_not_decide() {
        for case in [
            "arn:aws:servicediscovery:us-east-1:123456789012:namespace/ns-a2b3c4d5e6f7g2h3",
            "arn:aws-cn:servicediscovery:cn-north-1:210987654321:namespace/ns-a2b3c4d5e6f7g2h3",
            "arn:aws:servicediscovery:::namespace/ns-a2b3c4d5e6f7g2h3",
            "arn:aws:servicediscovery:eu-west-2:123456789012:service/srv-b2c3d4e5f6g7h2i3",
        ] {
            assert_eq!(
                invalid_resource_format(case),
                None,
                "case {case:?}: expected a well-formed resource field"
            );
        }
    }
}
