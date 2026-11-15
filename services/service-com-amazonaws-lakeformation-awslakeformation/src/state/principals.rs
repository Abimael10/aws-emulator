// SPDX-License-Identifier: CC0-1.0

//! How a `DataLakePrincipalIdentifier` is READ: what identifies a Lake Formation
//! principal, and the refusal a value that identifies none draws.
//!
//! `DataLakePrincipal$DataLakePrincipalIdentifier` is "An identifier for the Lake Formation
//! principal" and carries no pattern in the model, so the boundary validator admits any
//! string and the handler is what refuses a value that identifies nobody. A principal is
//! addressed by Amazon Resource Name ([`super::ARN_PREFIX`]) with the few exceptions below,
//! so a value that is neither is refused with the ARN reader's own sentence
//! ([`super::invalid_arn`]) — the same voice this service already refuses a designated
//! administrator that is not an ARN in.
//!
//! Every site that reads a grantee or a grantee FILTER applies this, so the grant, the
//! batch grant, the revoke and the listing agree on what names a principal.

use super::*;

/// The AWS Organizations identifier prefixes Lake Formation accepts as a principal: an
/// organization and an organizational unit, the two scopes a cross-account grant can name
/// beside a single account.
const ORGANIZATION_PRINCIPAL_PREFIXES: [&str; 2] = ["ou-", "o-"];

/// Whether `identifier` is an AWS Organizations organization or organizational-unit id:
/// one of [`ORGANIZATION_PRINCIPAL_PREFIXES`] followed by a non-empty id body of
/// lower-case letters, digits and hyphens.
fn is_organization_principal(identifier: &str) -> bool {
    ORGANIZATION_PRINCIPAL_PREFIXES
        .iter()
        .filter_map(|prefix| identifier.strip_prefix(prefix))
        .any(|body| {
            !body.is_empty()
                && body
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// Whether `identifier` names a Lake Formation principal WITHOUT being an ARN.
///
/// Three forms, and nothing else: the [`IAM_ALLOWED_PRINCIPALS`] group this service already
/// models, an AWS account id — the tree's one reading of that form,
/// [`::service_common::arn::is_account_id`], since Lake Formation names the recipient of a
/// cross-account grant by account id — and an AWS Organizations organization or
/// organizational-unit id, the other scopes such a grant names its recipient by.
pub fn is_non_arn_principal(identifier: &str) -> bool {
    identifier == IAM_ALLOWED_PRINCIPALS
        || ::service_common::arn::is_account_id(identifier)
        || is_organization_principal(identifier)
}

/// Why `identifier` names no Lake Formation principal, as the message of the modelled
/// `InvalidInputException` — `None` when it names one.
///
/// A principal is addressed by ARN unless it is one of the forms
/// [`is_non_arn_principal`] admits, so anything else (a bare role name, a role path
/// without the ARN fields around it) draws the ARN reader's refusal carrying the value the
/// request wrote.
pub fn malformed_principal(identifier: &str) -> Option<String> {
    let names_a_principal =
        identifier.starts_with(ARN_PREFIX) || is_non_arn_principal(identifier);
    (!names_a_principal).then(|| invalid_arn(identifier))
}
