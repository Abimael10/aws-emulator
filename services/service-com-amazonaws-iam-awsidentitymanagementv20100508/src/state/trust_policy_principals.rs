// SPDX-License-Identifier: CC0-1.0

//! WHICH service a role's trust policy may name as a principal that assumes the role, and how
//! IAM refuses one that cannot.
//!
//! A trust policy is a resource-based policy, so its statements name the parties the role
//! trusts — and IAM RESOLVES each SERVICE it is handed there rather than storing the name on
//! trust. A service that does not assume roles under that name has no principal to resolve, so
//! the document states a trust relationship that could never be exercised: the write is refused
//! with the modelled `MalformedPolicyDocumentException` (awsQuery code
//! `MalformedPolicyDocument`, HTTP 400) and the sentence
//! [`invalid_trust_policy_principal`], and nothing is stored. Without that refusal the caller
//! is told the role was created, and every later assume by the service it meant to trust fails
//! with an access error that names nothing — the mistake surfaces nowhere near the call that
//! made it.
//!
//! The services this is about are the ones whose roles are assumed under ANOTHER service's
//! principal ([`UNASSUMABLE_SERVICE_PRINCIPALS`]): a managed database engine delivered on the
//! Amazon RDS platform is addressed under its own endpoint domain, but a role it acts through
//! is assumed as [`RDS_SERVICE_PRINCIPAL`], which is the principal that engine's own
//! documentation tells a client to write. Nothing else about a `Service` value is judged here:
//! which FORMS the policy language admits under a principal type is the shared reading beside
//! this one (`::service_common::iam_policy_principals`), and a value in none of them is the
//! caller's own data, stored as it was written.
//!
//! A name this catalog does not carry is a principal IAM has nothing against here: the
//! catalog says which services cannot assume a role, never which services exist. The reading
//! is therefore deliberately UNDER-INCLUSIVE — a value naming no AWS service at all is stored
//! as written, which is the crate's own rule for a `Service` value
//! ([`super::stored_trust_policy`], whose reads report whatever the caller wrote) — and what
//! would make it complete is a registry of the service principals AWS publishes, feature and
//! regional sub-principals included (`monitoring.`, `ecs-tasks.`, `batchoperations.s3.`,
//! `logs.<region>.`), which nothing in this tree holds: resolving against a short list of
//! them would refuse trust policies AWS accepts. Until such a registry exists here, a service
//! whose roles are documented as assumed under another principal is filed below.
//!
//! The reading is shared by every operation that WRITES the member — `CreateRole`,
//! `UpdateAssumeRolePolicy`, and `AcquireRole`, which renders a trust policy from a role
//! template — so one document is judged the same way whichever of them was handed it. A trust
//! document IAM itself writes for a service-linked role
//! ([`super::service_linked_role_trust_policy`]) is not a caller's document and is not read
//! here.

#[allow(unused_imports)]
use super::*;

/// The principal TYPE a statement names a service under, one of the four the policy language
/// keys a `Principal`/`NotPrincipal` map by
/// (`::service_common::iam_policy_principals::PRINCIPAL_TYPES`). A value under any other type
/// names an account, a provider or a canonical user and is no service principal at all.
const SERVICE_PRINCIPAL_TYPE: &str = "Service";

/// How IAM's refusal SPELLS the principal type the rejected value sat under: the type's name
/// in upper case, which is the form its policy validator reports an offending principal with.
const REPORTED_PRINCIPAL_TYPE: &str = "SERVICE";

/// The service principal a role that Amazon RDS — and every managed database engine delivered
/// on the RDS platform — acts through is assumed under.
///
/// It is the principal those engines' own documentation writes into the trust policy of the
/// roles they assume, and the one AWS publishes a service-linked role for
/// ([`super::PUBLISHED_SERVICE_LINKED_ROLE_NAMES`]).
pub const RDS_SERVICE_PRINCIPAL: &str = "rds.amazonaws.com";

/// One service that does NOT assume roles under its own name, and the principal its roles are
/// assumed under instead.
#[derive(Debug, Clone, Copy)]
pub struct UnassumableServicePrincipal {
    /// The DNS-style name a client reaches the service under, and so mistakes for the
    /// principal it would assume a role as. IAM resolves no role-assuming principal for it.
    pub principal: &'static str,
    /// The service principal that service's roles ARE assumed under — what the trust policy
    /// has to name for the role to be usable.
    pub assumed_under: &'static str,
}

impl UnassumableServicePrincipal {
    /// One catalog row: the name that resolves to no role-assuming principal, and the one that
    /// does for the same service.
    const fn new(principal: &'static str, assumed_under: &'static str) -> Self {
        Self {
            principal,
            assumed_under,
        }
    }
}

/// The service principals IAM resolves no role assumption for, ascending by principal.
///
/// SOURCE: the "identity and access management" / service-role page of each service's own user
/// guide, which writes the trust policy of the roles that service assumes — and writes
/// [`RDS_SERVICE_PRINCIPAL`] in it, not the domain the service itself is reached under. Every
/// row is such a service: a managed database engine delivered on the Amazon RDS platform,
/// whose control plane is RDS's and whose role assumption therefore happens as RDS. The
/// substitute each row names is asserted to be a principal this catalog does NOT carry, so a
/// client that follows the refusal arrives at a document IAM accepts.
///
/// A name this catalog does not carry is a principal IAM has nothing against: the catalog says
/// which services cannot assume a role, never which services exist, so a document naming a
/// service principal nothing here mentions is stored as written.
pub const UNASSUMABLE_SERVICE_PRINCIPALS: &[UnassumableServicePrincipal] = &[
    // Amazon Aurora is an engine of Amazon RDS, reached under the RDS API: "Authorizing Amazon
    // Aurora MySQL to access other AWS services on your behalf" writes the cluster's role with
    // a trust policy naming the RDS principal.
    UnassumableServicePrincipal::new("aurora.amazonaws.com", RDS_SERVICE_PRINCIPAL),
    // Amazon DocumentDB (with MongoDB compatibility) is likewise delivered on the RDS
    // platform: the roles it assumes are created with the RDS principal in their trust policy.
    UnassumableServicePrincipal::new("docdb.amazonaws.com", RDS_SERVICE_PRINCIPAL),
    // Amazon Neptune: "Prerequisites: IAM role and Amazon S3 access" writes the bulk loader's
    // role with a trust policy naming the RDS principal, because a Neptune DB cluster assumes
    // its roles as RDS.
    UnassumableServicePrincipal::new("neptune.amazonaws.com", RDS_SERVICE_PRINCIPAL),
];

/// The principal the service named by `principal` assumes its roles under instead, or `None`
/// when nothing says that name resolves to no role assumption.
///
/// Matched without regard to ASCII case, the way every service principal in this crate is
/// ([`super::published_service_linked_role_name`]): a service principal is a DNS-style name,
/// so a differently-cased spelling addresses the same service.
#[must_use]
pub fn assumed_under_another_principal(principal: &str) -> Option<&'static str> {
    UNASSUMABLE_SERVICE_PRINCIPALS
        .iter()
        .find(|entry| entry.principal.eq_ignore_ascii_case(principal))
        .map(|entry| entry.assumed_under)
}

/// The first service principal `document` names that IAM resolves no role assumption for, as
/// the document wrote it, or `None` when every service it names can assume a role.
///
/// Every statement is read, both elements that name principals, and both forms a principal
/// type's values take (one string or a list of them) — so where in the document the name sits
/// does not change the answer. A `document` that is no policy at all is `None`: that is the
/// grammar's own clause ([`super::trust_policy_document_defect`]), which the operations read
/// first.
#[must_use]
pub fn unassumable_service_principal(document: &str) -> Option<String> {
    let statements = match super::policy_statements(document) {
        Ok(::serde_json::Value::Array(entries)) => entries,
        Ok(single) => vec![single],
        Err(_) => return None,
    };
    statements
        .iter()
        .filter_map(::serde_json::Value::as_object)
        .flat_map(named_service_principals)
        .find(|named| assumed_under_another_principal(named).is_some())
}

/// Every service principal ONE statement names, in the order it wrote them: the values under
/// the service type of each element a statement names principals in
/// (`::service_common::iam_policy_principals::PRINCIPAL_STATEMENT_ELEMENTS`, the tree's one
/// list of those elements).
fn named_service_principals(
    statement: &::serde_json::Map<String, ::serde_json::Value>,
) -> Vec<String> {
    ::service_common::iam_policy_principals::PRINCIPAL_STATEMENT_ELEMENTS
        .into_iter()
        .filter_map(|element| statement.get(element))
        .filter_map(::serde_json::Value::as_object)
        .filter_map(|types| types.get(SERVICE_PRINCIPAL_TYPE))
        .flat_map(service_values)
        .collect()
}

/// The values one principal type names: the policy language writes them as one string or as a
/// list of them, and anything else names no service.
fn service_values(named: &::serde_json::Value) -> Vec<String> {
    match named {
        ::serde_json::Value::String(value) => vec![value.clone()],
        ::serde_json::Value::Array(entries) => entries.iter().flat_map(service_values).collect(),
        _ => Vec::new(),
    }
}

/// How IAM refuses a trust policy naming a service principal it resolves no role assumption
/// for: the fixed text `Invalid principal in policy: ` followed by the principal TYPE the value
/// sat under, in upper case, and the value itself, each in double quotes and separated by a
/// colon.
///
/// The one variable part is the rejected value, taken from the request exactly as it was
/// written, so a caller that mis-typed a principal is told which name was refused.
#[must_use]
pub fn invalid_trust_policy_principal(principal: &str) -> String {
    format!("Invalid principal in policy: \"{REPORTED_PRINCIPAL_TYPE}\":\"{principal}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trust policy whose single statement names `principal` as written (a JSON fragment, so
    /// a row can write one string or a list).
    fn trust(principal: &str) -> String {
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
             \"Principal\":{{\"Service\":{principal}}},\"Action\":\"sts:AssumeRole\"}}]}}"
        )
    }

    /// RULE: the principal type this reading looks under is one of the four the policy
    /// language defines, and the refusal spells that type in upper case.
    #[test]
    fn the_service_type_is_one_the_policy_language_defines() {
        assert!(
            ::service_common::iam_policy_principals::PRINCIPAL_TYPES
                .contains(&SERVICE_PRINCIPAL_TYPE),
            "expected {SERVICE_PRINCIPAL_TYPE:?} to be one of the principal types the language \
             defines, got {:?}",
            ::service_common::iam_policy_principals::PRINCIPAL_TYPES
        );
        assert_eq!(
            REPORTED_PRINCIPAL_TYPE,
            SERVICE_PRINCIPAL_TYPE.to_ascii_uppercase(),
            "expected the refusal to spell the type in upper case, got \
             {REPORTED_PRINCIPAL_TYPE:?}"
        );
    }

    /// RULE: a trust policy naming a service that assumes its roles under another service's
    /// principal is reported, whichever statement names it, under either element that names
    /// principals, in either form the value takes and in any casing; every other document is
    /// `None`.
    #[test]
    fn a_service_that_cannot_assume_a_role_is_the_one_reported() {
        // The rows differ along what the rule generalises over: which catalog entry is named
        // (each of the three, none of them cited by one request), the form of the value (a
        // string, a list, the second entry of a list), the element it sits under, the
        // position of the statement, the form `Statement` takes, and the casing.
        let rows: [(&str, String, Option<&str>); 14] = [
            (
                "the RDS principal, which every row's service assumes its roles under",
                trust("\"rds.amazonaws.com\""),
                None,
            ),
            (
                "another service's principal",
                trust("\"lambda.amazonaws.com\""),
                None,
            ),
            (
                "a service principal of the same family that does assume roles",
                trust("\"neptune-graph.amazonaws.com\""),
                None,
            ),
            (
                "a value that is no DNS name at all, which this reading does not judge",
                trust("\"210987654321\""),
                None,
            ),
            (
                "a statement naming no principal",
                "{\"Statement\":[{\"Effect\":\"Allow\",\"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                None,
            ),
            ("no statement at all", "{\"Statement\":[]}".to_string(), None),
            ("not a policy at all", "not a policy".to_string(), None),
            (
                "the cited engine's own domain",
                trust("\"neptune.amazonaws.com\""),
                Some("neptune.amazonaws.com"),
            ),
            (
                "a second engine of the same platform",
                trust("\"docdb.amazonaws.com\""),
                Some("docdb.amazonaws.com"),
            ),
            (
                "a third engine of the same platform",
                trust("\"aurora.amazonaws.com\""),
                Some("aurora.amazonaws.com"),
            ),
            (
                "the second value of a list, beside a principal that can assume roles",
                trust("[\"rds.amazonaws.com\",\"docdb.amazonaws.com\"]"),
                Some("docdb.amazonaws.com"),
            ),
            (
                "the complement element names it",
                "{\"Statement\":[{\"Effect\":\"Deny\",\
                 \"NotPrincipal\":{\"Service\":\"aurora.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                Some("aurora.amazonaws.com"),
            ),
            (
                "the second statement names it",
                "{\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"rds.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"},{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"neptune.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                Some("neptune.amazonaws.com"),
            ),
            (
                "the single statement object the grammar admits, named in another casing",
                "{\"Statement\":{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"Neptune.AmazonAWS.com\"},\
                 \"Action\":\"sts:AssumeRole\"}}"
                    .to_string(),
                Some("Neptune.AmazonAWS.com"),
            ),
        ];

        for (case, document, expected) in rows {
            let found = unassumable_service_principal(&document);
            assert_eq!(
                found.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {found:?} for {document}"
            );
        }
    }

    /// RULE: the refusal is the fixed text around the principal TYPE in upper case and the
    /// rejected value exactly as the request wrote it.
    #[test]
    fn the_refusal_names_the_rejected_principal_as_it_was_written() {
        for (case, principal, expected) in [
            (
                "the cited engine",
                "neptune.amazonaws.com",
                "Invalid principal in policy: \"SERVICE\":\"neptune.amazonaws.com\"",
            ),
            (
                "another engine of the same platform",
                "docdb.amazonaws.com",
                "Invalid principal in policy: \"SERVICE\":\"docdb.amazonaws.com\"",
            ),
            (
                "a value written in another casing",
                "Aurora.AmazonAWS.com",
                "Invalid principal in policy: \"SERVICE\":\"Aurora.AmazonAWS.com\"",
            ),
        ] {
            let message = invalid_trust_policy_principal(principal);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    /// RULE: every row's substitute is a principal a role CAN be assumed under — a client that
    /// follows the refusal and writes it reaches a document this reading accepts — and the
    /// catalog is strictly ascending by principal, so a service is filed once.
    #[test]
    fn each_row_names_a_principal_a_role_can_be_assumed_under() {
        for entry in UNASSUMABLE_SERVICE_PRINCIPALS {
            assert!(
                assumed_under_another_principal(entry.assumed_under).is_none(),
                "case {:?}: the substitute {:?} must itself be assumable",
                entry.principal,
                entry.assumed_under
            );
            let found = unassumable_service_principal(&trust(&format!(
                "\"{}\"",
                entry.assumed_under
            )));
            assert_eq!(
                found, None,
                "case {:?}: a document naming {:?} must be accepted, got {found:?}",
                entry.principal, entry.assumed_under
            );
        }

        for pair in UNASSUMABLE_SERVICE_PRINCIPALS.windows(2) {
            assert!(
                pair[0].principal < pair[1].principal,
                "the catalog must ascend by principal, got {:?} before {:?}",
                pair[0].principal,
                pair[1].principal
            );
        }
    }
}
