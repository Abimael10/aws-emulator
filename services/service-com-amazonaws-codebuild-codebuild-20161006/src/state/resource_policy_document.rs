// SPDX-License-Identifier: CC0-1.0

//! The FORM a resource policy is held in, and so the form every read of one reports.
//!
//! `PutResourcePolicy` takes "A JSON-formatted resource policy" as a string member, and what
//! a later `GetResourcePolicy` reports is not the bytes the request spelled: the service
//! reads the policy and renders it again, so two callers that submit the same grant written
//! differently read the same text back, and a client that puts back what it read sees the
//! same text once more.
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
//! count, the `Statement` array itself (a one-statement policy keeps its one-element array),
//! the type and spelling of every other value, and every byte inside a string literal —
//! whitespace, escapes and non-ASCII characters included.

/// What is STORED against a resource's ARN for the submitted `document`, and so what every
/// later read of that resource's policy reports: this service's own rendering of the policy
/// the caller wrote (see the module docs for the two rewrites it makes).
///
/// Applied at the ONE place a policy is attached, so a project's and a report group's grants
/// cannot be held in two different forms, and the write, the read and the shared-resource
/// listings all read the same bytes.
///
/// A fixed point: rendering a rendered document returns it unchanged, because the whitespace
/// and one-value-list rules are idempotent and a root ARN is no account id. A document that is
/// not a JSON policy at all never reaches here — [`super::is_policy_document`] refuses it
/// before anything is stored.
#[must_use]
pub fn stored_resource_policy(document: &str) -> String {
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
