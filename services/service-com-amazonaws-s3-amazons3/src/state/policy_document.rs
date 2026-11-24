// SPDX-License-Identifier: CC0-1.0

//! The FORM a bucket policy is held in, and so the form every read of one reports.
//!
//! `PutBucketPolicy` takes "the bucket policy as a JSON document" as its payload, and what a
//! later `GetBucketPolicy` reports is not the bytes the request spelled: Amazon S3 reads the
//! policy the document expresses and renders it again, so two callers that submit the same
//! grant written differently read the same text back, and a client that puts back what it
//! read sees the same text once more.
//!
//! The rendering is the tree's one rendering of an IAM (Aspen) policy document
//! ([`::service_common::iam_policy_rendering`]): the whitespace BETWEEN the document's JSON
//! tokens is gone, and an element the policy language types as "String or Array of strings"
//! — a statement's `Action`/`NotAction` and `Resource`/`NotResource`, and the principal lists
//! inside `Principal`/`NotPrincipal` — is written as that string where the caller submitted a
//! one-element array of one string. Walked with it is the one spelling these reads add: an
//! ACCOUNT principal named by the account's id alone is written as that account's ROOT ARN,
//! the other spelling of the very same principal.
//!
//! Everything else is the caller's own data and survives untouched: member order, statement
//! count, the `Statement` array itself (a one-statement policy keeps its one-element array,
//! and a statement submitted as a bare object stays one), the type and spelling of every
//! other value, and every byte inside a string literal — whitespace, escapes and non-ASCII
//! characters included.

/// What is STORED against a bucket for the submitted `document`, and so what every later read
/// of that bucket's policy reports: S3's own rendering of the policy the caller wrote (see the
/// module docs for the rewrites it makes).
///
/// Applied at the ONE place a bucket policy is attached, so the document the policy status
/// classifies and the one the authorization decision is taken against are the same bytes the
/// read reports.
///
/// A fixed point: rendering a rendered document returns it unchanged, because the whitespace
/// and one-value-list rules are idempotent and a root ARN is no account id — which is what
/// makes a caller's re-put of the document it read store the same bytes again. A document that
/// is not a JSON policy at all never reaches here: [`super::is_valid_policy_document`] refuses
/// it before anything is stored.
#[must_use]
pub fn stored_bucket_policy(document: &str) -> String {
    ::service_common::iam_policy_rendering::rendered_rewriting(
        document,
        &::service_common::iam_policy_rendering::ElementValues {
            element: ACCOUNT_PRINCIPAL,
            written_as: &::service_common::iam_policy_principals::account_root_principal,
        },
    )
}

/// The key a policy names an ACCOUNT principal under — the "AWS" type of a
/// `Principal`/`NotPrincipal` map, as opposed to the `Service`, `Federated` and
/// `CanonicalUser` types, whose values name no account and are reported as they were
/// submitted.
const ACCOUNT_PRINCIPAL: &str = "AWS";
