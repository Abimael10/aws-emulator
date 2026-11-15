// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLING PRINCIPAL may read the Amazon S3 object a request addresses BY
//! REFERENCE — the question answered before the object behind a `TemplateURL`, a
//! `StackPolicyURL` or any other reference is fetched.
//!
//! ## The rule this module implements
//!
//! A reference says WHERE a document is, and CloudFormation goes and reads it: "CloudFormation
//! uses the credentials you provide" for that read, so the object is fetched AS THE CALLER and
//! not with permissions of the service's own. A principal that may not read the object
//! therefore cannot submit it either — the fetch is refused, and the operation is refused with
//! it, before a stack, a change set or a policy is written. That is the whole of what this
//! module decides; [`super::template_url`] words the refusal, because what CloudFormation
//! reports for a read Amazon S3 would not serve is the same sentence whichever reason that
//! read was not served for.
//!
//! WHO the caller is, what it may do and how the decision is taken are the CALLING
//! PRINCIPAL's, held in one place for every call this service decides
//! ([`super::calling_principal`]); what this module owns is the half particular to a
//! reference: which ACTION the read asks for and which RESOURCE it is about.
//!
//! WHICH action is asked about is the one the read itself needs: `s3:GetObject` for the
//! current version of an object and `s3:GetObjectVersion` for a reference that names a
//! version, which is a permission of its own. The resource is the ARN of the object the
//! reference addressed, built from the bucket and key the URL named — never resolved out of a
//! store, so the decision is the same whether or not the object is there and a refusal tells
//! an unauthorized caller nothing about what the account holds.

/// The IAM action reading the CURRENT version of one object asks for: "you must have the
/// `s3:GetObject` permission" (`GetObject`'s own documentation of the permissions a read
/// needs).
const GET_OBJECT: &str = "s3:GetObject";

/// The action a read that names an object VERSION asks for instead: "if you include a
/// `versionId` in your request header, you must have the `s3:GetObjectVersion` permission …
/// The `s3:GetObject` permission is not required in this scenario" (`GetObject`) — so a
/// reference carrying `?versionId=` is decided on THIS action and a reference without one on
/// [`GET_OBJECT`].
const GET_OBJECT_VERSION: &str = "s3:GetObjectVersion";

/// Whether the account's IAM records show the calling principal may NOT read `object`.
///
/// `true` is the one answer that refuses a reference: the documents in force deny the read, or
/// none of them grants it and the caller is not one this decision cannot bound (see the module
/// docs). `false` is every other case, the ones where nothing establishes that the read would
/// be refused.
///
/// Asked with NO store of this service locked, and before the object is read.
pub fn read_is_unauthorized<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    object: &super::template_url::Object,
) -> bool {
    let action = if object.version.is_some() {
        GET_OBJECT_VERSION
    } else {
        GET_OBJECT
    };
    let resource = object_arn(&object.bucket, &object.key);
    super::calling_principal::Caller::of(engine, ctx).unauthorized(action, &resource)
}

/// The ARN of the object `key` in `bucket`: `arn:aws:s3:::<bucket>/<key>`.
///
/// An Amazon S3 ARN carries no Region and no account segment — "because Amazon S3 bucket names
/// are global, you don't specify the AWS Region or account ID" — so the bucket name opens the
/// resource field and the key follows it, which is how a policy statement names one object.
fn object_arn(bucket: &str, key: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{key}")
}

/// The decisions this module's own rule is pinned by: which action a read asks for, and the
/// object ARN it is about.
#[cfg(test)]
mod tests {
    use ::service_common::iam_authorization::{self as iam, Decision};

    use super::*;

    /// What one stored document says about reading one object, through the shared evaluator
    /// this decision consults.
    fn decision(document: &str, action: &str, bucket: &str, key: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, &object_arn(bucket, key), &[]),
        )
    }

    /// RULE: the read is decided on the ARN of the object the reference addressed, so a grant
    /// scoped to one prefix of one bucket covers the objects under it and nothing else.
    #[test]
    fn a_grant_is_read_for_the_object_the_reference_addressed() {
        let document = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
            "Action":"s3:GetObject","Resource":"arn:aws:s3:::staging/templates/*"}]}"#;
        // (case, bucket, key, allowed)
        let rows: &[(&str, &str, &str, bool)] = &[
            ("the granted prefix", "staging", "templates/main.json", true),
            (
                "a nested key under it",
                "staging",
                "templates/nested/child.json",
                true,
            ),
            ("another prefix of the bucket", "staging", "other.json", false),
            ("another bucket", "elsewhere", "templates/main.json", false),
        ];
        for (case, bucket, key, expected) in rows {
            let said = decision(document, GET_OBJECT, bucket, key);
            let actual = iam::is_allowed(said, Decision::NotStated);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected allowed == {expected}, got {actual} ({said:?})"
            );
        }
    }

    /// RULE: naming a VERSION asks for `s3:GetObjectVersion`, which a grant of `s3:GetObject`
    /// alone does not cover.
    #[test]
    fn a_versioned_read_asks_for_its_own_action() {
        let document = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
            "Action":"s3:GetObject","Resource":"*"}]}"#;
        let current = decision(document, GET_OBJECT, "staging", "main.json");
        assert_eq!(
            current,
            Decision::Allowed,
            "case \"the current version\": expected Allowed, got {current:?}"
        );
        let versioned = decision(document, GET_OBJECT_VERSION, "staging", "main.json");
        assert_eq!(
            versioned,
            Decision::NotStated,
            "case \"a named version\": expected the grant to state nothing, got {versioned:?}"
        );
    }

    /// RULE: an explicit `Deny` is final, whatever else the principal carries.
    #[test]
    fn an_explicit_deny_wins_over_a_wildcard_grant() {
        let document = r#"{"Version":"2012-10-17","Statement":[
            {"Effect":"Allow","Action":"s3:*","Resource":"*"},
            {"Effect":"Deny","Action":"s3:GetObject","Resource":"arn:aws:s3:::staging/*"}]}"#;
        let denied = decision(document, GET_OBJECT, "staging", "main.json");
        assert_eq!(
            denied,
            Decision::Denied,
            "case \"the denied bucket\": expected Denied, got {denied:?}"
        );
        let allowed = decision(document, GET_OBJECT, "elsewhere", "main.json");
        assert_eq!(
            allowed,
            Decision::Allowed,
            "case \"a bucket the deny does not name\": expected Allowed, got {allowed:?}"
        );
    }
}
