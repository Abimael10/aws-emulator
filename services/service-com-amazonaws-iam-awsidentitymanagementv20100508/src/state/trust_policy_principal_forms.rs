// SPDX-License-Identifier: CC0-1.0

//! Whether the values a role's trust policy names under each principal TYPE are principals of
//! that type AT ALL, and how IAM refuses one that is not.
//!
//! A `Principal` element is not a bag of strings: the policy language keys it by the TYPE of
//! the party being trusted — an Amazon Web Services account or one of its identities, an AWS
//! service, an identity provider, an Amazon S3 canonical user — and each type has its own
//! forms. A value outside the forms of the type it was written under denotes no party any
//! policy engine could evaluate, so a trust document carrying one states a trust nothing could
//! ever exercise: IAM refuses the write with the modelled `MalformedPolicyDocumentException`
//! (awsQuery code `MalformedPolicyDocument`, HTTP 400) and the shared invalid-principal
//! sentence ([`super::invalid_policy_principal`]), naming the TYPE the value sat under and the
//! value as the request wrote it, and nothing is stored.
//!
//! The commonest way a trust document breaks the rule is a principal written under the WRONG
//! type: a service's principal name is a DNS-style name, which is a `Service` principal and is
//! no account, identity ARN or wildcard, so the same text that names a party under `Service`
//! names nobody under `AWS`. Without this refusal such a role is minted, its trust document can
//! be satisfied by no principal at all, and every later assume of it fails somewhere else
//! entirely.
//!
//! WHICH forms each type admits is not decided here: it is the tree's one reading of a
//! resource-based policy's principals (`::service_common::iam_policy_principals`), shared with
//! every other service that holds such a document, so one value is read one way wherever it is
//! submitted. This module asks that reading about the principals of a TRUST document — the
//! shared walk every resolution of one takes ([`super::named_trust_policy_principals`]) — and
//! reports the type beside the value, because IAM's refusal names both.
//!
//! It is the FORM clause, and it sits above the two RESOLUTIONS beside it: a value in its
//! type's forms is then resolved against what that type names — a service that assumes its
//! roles under another service's principal ([`super::unassumable_service_principal`]) and an
//! identity of this account the account holds no record of
//! ([`super::unresolvable_identity_principal`]). All three are read by every operation that
//! WRITES the member — `CreateRole`, `UpdateAssumeRolePolicy`, and `AcquireRole`, which renders
//! a trust policy from a role template — so one document is judged the same way whichever of
//! them was handed it, and all three are read BEFORE anything is written, so a refused
//! `CreateRole` leaves no role and a refused `UpdateAssumeRolePolicy` leaves the document the
//! role already held.
//!
//! A value under a name the language does not define as a type at all is not this clause: the
//! type keys a map of nothing, which is a defect of the document's grammar rather than of a
//! principal, and it is left to the grammar beside this
//! ([`super::trust_policy_document_defect`]). A `document` that is no policy at all likewise
//! names nothing here.

#[allow(unused_imports)]
use super::*;

/// The first principal a trust `document` names that is in NONE of the forms the policy
/// language defines for the TYPE it sits under, as the type and the value the document wrote,
/// or `None` when every principal it names is one of its type's.
///
/// The type is one of `::service_common::iam_policy_principals::PRINCIPAL_TYPES` and is what
/// the refusal spells in upper case ([`super::invalid_policy_principal`]); the value is taken
/// from the document exactly as it was written, so a caller that wrote a principal under the
/// wrong type is told which value was refused.
///
/// Every statement is read, both elements that name principals, and both forms a principal
/// type's values take (one string or a list of them) — the shared walk every resolution of a
/// trust document's principals takes ([`super::named_trust_policy_principals`]) — so where in
/// the document the value sits does not change the answer.
#[must_use]
pub fn unreadable_trust_policy_principal(document: &str) -> Option<(&'static str, String)> {
    ::service_common::iam_policy_principals::PRINCIPAL_TYPES
        .into_iter()
        .find_map(|principal_type| {
            super::named_trust_policy_principals(document, principal_type)
                .into_iter()
                .find(|value| {
                    !::service_common::iam_policy_principals::names_a_principal(
                        principal_type,
                        value,
                    )
                })
                .map(|value| (principal_type, value))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trust policy whose single statement names `principal` as written (a JSON fragment, so
    /// a row can write one type or several, one value or a list).
    fn trust(principal: &str) -> String {
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
             \"Principal\":{principal},\"Action\":\"sts:AssumeRole\"}}]}}"
        )
    }

    /// One row: what a failing assertion calls it, the document, and the TYPE and value the
    /// reading must report (`None` when every principal is in its type's forms).
    type Row = (&'static str, String, Option<(&'static str, &'static str)>);

    /// RULE: a value outside the forms its principal TYPE admits is reported with that type,
    /// whichever statement names it, under either element that names principals, and in either
    /// form the value takes; a value in its type's forms is not.
    #[test]
    fn a_value_outside_its_types_forms_is_reported_with_its_type() {
        // The rows differ along what the rule generalises over: the principal type, HOW the
        // value misses its type's forms, the form the value takes (one string, a list, the
        // second entry of a list), the element it sits under, the position of the statement,
        // the form `Statement` takes, and the types that are read side by side.
        let rows: [Row; 18] = [
            (
                "a service principal under its own type",
                trust("{\"Service\":\"sns.amazonaws.com\"}"),
                None,
            ),
            (
                "a regional service principal, which that type admits",
                trust("{\"Service\":\"logs.eu-west-1.amazonaws.com\"}"),
                None,
            ),
            (
                "an account by its id and the wildcard",
                trust("{\"AWS\":[\"210987654321\",\"*\"]}"),
                None,
            ),
            (
                "an account's root and an identity of it",
                trust(
                    "{\"AWS\":[\"arn:aws:iam::210987654321:root\",\
                     \"arn:aws:iam::210987654321:role/deploy\"]}",
                ),
                None,
            ),
            (
                "an identity provider by ARN and a web identity by host",
                trust(
                    "{\"Federated\":[\"arn:aws:iam::210987654321:saml-provider/corp\",\
                     \"accounts.google.com\"]}",
                ),
                None,
            ),
            (
                "several types at once, each in its own forms",
                trust(
                    "{\"AWS\":\"210987654321\",\"Service\":\"lambda.amazonaws.com\",\
                     \"Federated\":\"cognito-identity.amazonaws.com\"}",
                ),
                None,
            ),
            (
                "the untyped wildcard, which names every principal",
                trust("\"*\""),
                None,
            ),
            (
                "a statement naming no principal",
                "{\"Statement\":[{\"Effect\":\"Allow\",\"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                None,
            ),
            ("not a policy at all", "not a policy".to_string(), None),
            // The service principal written under the account type: a DNS-style name is no
            // account, no identity ARN and no wildcard, so it names nobody there.
            (
                "a service principal under the account type",
                trust("{\"AWS\":\"sns.amazonaws.com\"}"),
                Some(("AWS", "sns.amazonaws.com")),
            ),
            (
                "another service's principal under the account type",
                trust("{\"AWS\":\"lambda.amazonaws.com\"}"),
                Some(("AWS", "lambda.amazonaws.com")),
            ),
            (
                "the second value of a list, beside an account the type admits",
                trust("{\"AWS\":[\"210987654321\",\"ec2.amazonaws.com\"]}"),
                Some(("AWS", "ec2.amazonaws.com")),
            ),
            (
                "an account id of the wrong width",
                trust("{\"AWS\":\"21098765432\"}"),
                Some(("AWS", "21098765432")),
            ),
            (
                "an IAM entity that is no principal",
                trust("{\"AWS\":\"arn:aws:iam::210987654321:group/builders\"}"),
                Some(("AWS", "arn:aws:iam::210987654321:group/builders")),
            ),
            (
                "an identity ARN carrying a wildcard",
                trust("{\"AWS\":\"arn:aws:iam::210987654321:role/*\"}"),
                Some(("AWS", "arn:aws:iam::210987654321:role/*")),
            ),
            (
                "a service value that is no DNS-style name",
                trust("{\"Service\":\"Amazon SNS\"}"),
                Some(("Service", "Amazon SNS")),
            ),
            (
                "the complement element names it",
                "{\"Statement\":[{\"Effect\":\"Deny\",\
                 \"NotPrincipal\":{\"Federated\":\"arn:aws:iam::210987654321:role/corp\"},\
                 \"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                Some(("Federated", "arn:aws:iam::210987654321:role/corp")),
            ),
            (
                "the single statement object the grammar admits names it",
                "{\"Statement\":{\"Effect\":\"Allow\",\
                 \"Principal\":{\"CanonicalUser\":\"too-short\"},\
                 \"Action\":\"sts:AssumeRole\"}}"
                    .to_string(),
                Some(("CanonicalUser", "too-short")),
            ),
        ];

        for (case, document, expected) in rows {
            let found = unreadable_trust_policy_principal(&document);
            let read = found
                .as_ref()
                .map(|(principal_type, value)| (*principal_type, value.as_str()));
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?} for {document}"
            );
        }
    }

    /// RULE: the refusal is the shared invalid-principal sentence over the TYPE the rejected
    /// value sat under, spelled in upper case, and the value exactly as the request wrote it.
    #[test]
    fn the_refusal_names_the_type_and_the_value_as_written() {
        for (case, document, expected) in [
            (
                "a service principal under the account type",
                trust("{\"AWS\":\"sns.amazonaws.com\"}"),
                "Invalid principal in policy: \"AWS\":\"sns.amazonaws.com\"",
            ),
            (
                "a service value that is no DNS-style name",
                trust("{\"Service\":\"Amazon SNS\"}"),
                "Invalid principal in policy: \"SERVICE\":\"Amazon SNS\"",
            ),
            (
                "a federated value that names no provider",
                trust("{\"Federated\":\"arn:aws:iam::210987654321:role/corp\"}"),
                "Invalid principal in policy: \"FEDERATED\":\
                 \"arn:aws:iam::210987654321:role/corp\"",
            ),
        ] {
            let (principal_type, value) = unreadable_trust_policy_principal(&document)
                .unwrap_or_else(|| panic!("case {case:?}: expected a refusal for {document}"));
            let message = invalid_policy_principal(principal_type, &value);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}
