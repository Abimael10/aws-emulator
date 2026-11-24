// SPDX-License-Identifier: CC0-1.0

//! Whether the principals a bucket-policy statement names are written in a form that names a
//! principal at all.
//!
//! The forms are the policy LANGUAGE's, not this service's — an account or an IAM identity
//! under `AWS`, a service principal's DNS name under `Service`, an identity provider under
//! `Federated`, a canonical user id under `CanonicalUser`, and the wildcard as the untyped
//! shorthand — so the reading is the shared one every service judges a principal element by
//! (`::service_common::iam_policy_principals::unreadable_policy_principal`, which walks the
//! same statements the policy grammar does and reports the first value in none of its type's
//! forms, naming a map keyed by a type the language does not define by that key).
//!
//! What this module adds is the two values that reading reports which S3 nevertheless grants
//! to:
//!
//! * a legacy CloudFront origin access identity is an IAM user of CLOUDFRONT's own account
//!   (`arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity <id>`), so the account
//!   field of its ARN is no account id and the reading above reports it — while S3 grants to
//!   such an identity every day. Whether that identity EXISTS is the question
//!   [`crate::service::policy_principals`] asks of the store that holds it.
//! * an account or identity written as the UNTYPED shorthand (`"Principal": "<role ARN>"`):
//!   S3 reads a shorthand value as the `AWS` type, which is the form a running handler's read
//!   is granted by in `service-com-amazonaws-lambda-awsgirapiservice`'s
//!   `handler_s3_bucket_policy_principal` suite, so a value that names a principal under that
//!   type is no fault of the FORM.
//!
//! A value in none of those forms names nobody, so the statement grants to nobody and S3
//! refuses the document rather than storing it — the same `MalformedPolicy` (400) it answers
//! for a principal that does not exist. This reading is of the document alone, so it needs no
//! store.

#[allow(unused_imports)]
use super::*;

use ::service_com_amazonaws_cloudfront_cloudfront2020_05_31::state as cloudfront_store;

/// The principal type an account or an IAM identity stands under, which is also the type S3
/// reads the untyped shorthand as.
const ACCOUNT_PRINCIPAL_TYPE: &str = "AWS";

/// The first `Principal` or `NotPrincipal` value of `policy` that is in none of the forms a
/// principal is written in, or `None` when every value is in one.
///
/// The statements are walked in the order the document writes them, so the value reported is
/// the first a reader of the document would reach, and it is reported as submitted. A text
/// that is no policy carries no fault here: what makes a document unusable as a policy at all
/// is [`super::is_valid_policy_document`]'s reading, taken before this one.
#[must_use]
pub fn unreadable_principal(policy: &str) -> Option<String> {
    let value = ::service_common::iam_policy_principals::unreadable_policy_principal(policy)?;
    let published = cloudfront_store::origin_access_identity_principal_id(&value).is_some()
        || ::service_common::iam_policy_principals::names_a_principal(
            ACCOUNT_PRINCIPAL_TYPE,
            &value,
        );
    (!published).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared form reading over the statements of a document held as text, and the two
    /// values it reports that S3 grants to all the same.
    #[test]
    fn the_first_value_in_no_published_form_is_reported() {
        let rows: &[(&str, Option<&str>)] = &[
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:*"}]}"#,
                None,
            ),
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":{"Service":"s3.amazonaws.com"},"Action":"s3:*"}]}"#,
                None,
            ),
            // An identity ARN and an account id written as the untyped shorthand, which S3
            // reads as the account type.
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":"arn:aws:iam::111122223333:role/reader","Action":"s3:*"}]}"#,
                None,
            ),
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":"111122223333","Action":"s3:*"}]}"#,
                None,
            ),
            // The legacy origin access identity: a published form the language's reading of
            // an identity ARN does not place, so it is no fault of the FORM.
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity E123"},"Action":"s3:*"}]}"#,
                None,
            ),
            // A service principal written under the key that names accounts and identities.
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":"cloudfront.amazonaws.com"},"Action":"s3:*"}]}"#,
                Some("cloudfront.amazonaws.com"),
            ),
            // The second value of a list, and the statement written as the object itself.
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":["111122223333","nobody"]},"Action":"s3:*"}]}"#,
                Some("nobody"),
            ),
            (
                r#"{"Statement":{"Effect":"Allow","Principal":{"AWS":"nobody"},"Action":"s3:*"}}"#,
                Some("nobody"),
            ),
            // The second statement of a document, naming the complement element.
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:*"},{"Effect":"Deny","NotPrincipal":{"AWS":"nobody"},"Action":"s3:*"}]}"#,
                Some("nobody"),
            ),
            // A key the language defines no principal type under is reported BY that key.
            (
                r#"{"Statement":[{"Effect":"Allow","Principal":{"Services":"s3.amazonaws.com"},"Action":"s3:*"}]}"#,
                Some("Services"),
            ),
            // A statement naming no principal has no value to be wrong about, and a text
            // that is no policy names no statement.
            (
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:*"}]}"#,
                None,
            ),
            ("not a policy at all", None),
        ];
        for (document, expected) in rows {
            let reported = unreadable_principal(document);
            assert_eq!(
                reported.as_deref(),
                *expected,
                "case {document:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }
}
