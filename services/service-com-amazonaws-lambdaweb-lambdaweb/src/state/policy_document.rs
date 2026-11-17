// SPDX-License-Identifier: CC0-1.0

//! The document rule the resource-policy put holds a `policy` to.
//!
//! `PutResourcePolicyRequest$policy` is "The JSON-formatted resource-based policy
//! to attach to the web function." (source: `smithy_query(query="operation",
//! operation="PutResourcePolicy")`, member `policy`), and the model bounds the
//! member's length (1..20480) only — it carries no pattern — so the request
//! boundary can judge the document's SIZE and nothing else. A document is a
//! POLICY, not any JSON value, and the IAM JSON policy language's grammar is what
//! separates the two. That grammar is read ONCE for the whole tree
//! ([`::service_common::iam_policy::resource_policy_defect`]), so this service
//! and every other service that takes a resource-based policy document agree
//! about what one is; all this module does is turn that reading into the one
//! yes/no the handler asks, because the refusal this service words
//! ([`super::POLICY_SYNTAX_ERROR_MESSAGE`]) is one fixed sentence for every clause
//! a document can break.

/// Whether `policy` is a document this service can attach: a resource-based
/// policy document in the IAM JSON policy language.
///
/// This is where the document's well-formedness is judged, before anything is
/// written and before the function the request addresses is resolved: a document
/// that merely parses as JSON — or that is a JSON object carrying no statement
/// the language can read — is not yet a policy.
pub fn is_policy_document(policy: &str) -> bool {
    ::service_common::iam_policy::resource_policy_defect(policy).is_none()
}
