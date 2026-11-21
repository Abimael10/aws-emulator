// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of the FORMS a resource-based policy names a principal in, for every
//! service that decides whether a submitted policy document is one it can hold.
//!
//! The grammar beside it ([`super::iam_policy`]) settles which ELEMENTS a document carries;
//! this settles whether the principals those elements name are principals at all. A
//! `Principal` is not an opaque string: the policy language writes it either as the bare
//! wildcard or as a map from a principal TYPE ([`PRINCIPAL_TYPES`]) to the values of that
//! type, and each type has its own form — an account, an IAM or STS identity, a service's
//! principal name, an identity provider, a canonical user. A value outside its type's forms
//! denotes no identity any policy engine could evaluate, so a service that stores such a
//! document would hold a policy nothing can enforce.
//!
//! Which ARN forms name an identity is [`super::arn::is_iam_principal_arn`], the tree's one
//! reading of a principal ARN, so a value this admits is one the authorization evaluator
//! ([`super::iam_authorization`]) and the IAM resolution ([`super::iam_principal`]) read the
//! same way.
//!
//! What a service then DOES with a value in none of the forms stays the service's: this
//! REPORTS the offending text and words no refusal, because each service models its own.

/// The principal TYPES the policy language keys a `Principal`/`NotPrincipal` map by: an AWS
/// account or identity, an AWS service, a web-identity or SAML provider, and an Amazon S3
/// canonical user. A map keyed by any other name states the type of nothing.
pub const PRINCIPAL_TYPES: [&str; 4] = ["AWS", "Service", "Federated", "CanonicalUser"];

/// The wildcard principal: the one value the language writes WITHOUT a type (`"Principal":
/// "*"`), and the one that names every principal rather than one.
pub const EVERY_PRINCIPAL: &str = "*";

/// The statement elements that name principals: the one a statement grants to and its
/// complement.
///
/// These are the statement's own elements; the names a principal MAP is keyed by — the
/// principal types — are [`PRINCIPAL_TYPES`].
pub const PRINCIPAL_STATEMENT_ELEMENTS: [&str; 2] = ["Principal", "NotPrincipal"];

/// The characters a canonical user id is written with, and how many: "the canonical user ID
/// is a 64-character obfuscated identifier" of the Amazon S3 user guide, written as
/// alphanumerics.
const CANONICAL_USER_ID_LEN: usize = 64;

/// The resource types of the IAM identity-provider ARNs a `Federated` principal names a
/// provider by: a SAML provider and an OIDC provider, each followed by the provider's name.
const PROVIDER_RESOURCE_TYPES: [&str; 2] = ["saml-provider", "oidc-provider"];

/// The first value a statement's principal elements name that is in NONE of the forms the
/// policy language defines for its principal type, or `None` when every one of them names a
/// principal.
///
/// The value is reported as it was submitted — and a map keyed by a type the language does
/// not define is reported by that KEY — so a service whose refusal names what it read can
/// render it; one that states a single opaque phrase ignores it.
///
/// A statement naming no principal at all is answered `None`: whether a document HAS to name
/// one is the grammar's clause ([`super::iam_policy::PolicyDefect::MissingPrincipal`]), and
/// is decided for identity-based and resource-based documents differently.
#[must_use]
pub fn unreadable_principal(
    statement: &::serde_json::Map<String, ::serde_json::Value>,
) -> Option<String> {
    PRINCIPAL_STATEMENT_ELEMENTS
        .into_iter()
        .filter_map(|element| statement.get(element))
        .find_map(unreadable_principal_of)
}

/// [`unreadable_principal`] for one principal ELEMENT, however that element is written.
fn unreadable_principal_of(element: &::serde_json::Value) -> Option<String> {
    match element {
        // The typed map: every type it keys, and every value under each type.
        ::serde_json::Value::Object(types) => types.iter().find_map(|(principal_type, named)| {
            if !PRINCIPAL_TYPES.contains(&principal_type.as_str()) {
                return Some(principal_type.clone());
            }
            values(named)
                .into_iter()
                .find(|value| !names_a_principal(principal_type, value))
        }),
        // The untyped shorthand, which the language defines for the wildcard alone.
        ::serde_json::Value::String(value) => {
            (value != EVERY_PRINCIPAL).then(|| value.clone())
        }
        // Neither form: a list, a number or a boolean names no principal and keys no type.
        other => Some(other.to_string()),
    }
}

/// Whether `value` is in one of the forms the policy language defines for a principal of
/// type `principal_type` (one of [`PRINCIPAL_TYPES`]).
///
/// * `AWS` — the wildcard, an account by its id ([`super::arn::is_account_id`]), or an
///   identity by its ARN ([`super::arn::is_iam_principal_arn`]: an account's root, an IAM
///   user or role, an STS assumed-role session or federated user).
/// * `Service` — a service's principal name, which AWS publishes as a DNS name
///   (`<service>.amazonaws.com`, and the regional `<service>.<region>.amazonaws.com`).
/// * `Federated` — an identity provider: a SAML or OIDC provider of an account by its IAM
///   ARN, or a web-identity provider by the host (and, for an OIDC issuer, the path) it is
///   published under.
/// * `CanonicalUser` — a canonical user id: [`CANONICAL_USER_ID_LEN`] alphanumerics.
///
/// A type the language does not define has no forms, so nothing names a principal under it.
#[must_use]
pub fn names_a_principal(principal_type: &str, value: &str) -> bool {
    match principal_type {
        "AWS" => {
            value == EVERY_PRINCIPAL
                || super::arn::is_account_id(value)
                || super::arn::is_iam_principal_arn(value)
        }
        "Service" => is_host(value),
        "Federated" => is_provider_arn(value) || is_provider_url(value),
        "CanonicalUser" => {
            value.len() == CANONICAL_USER_ID_LEN
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }
        _ => false,
    }
}

/// The partition a bare account id's root ARN is written in. An account id names the account
/// alone, so it carries no partition and the spelling has to supply one: the commercial
/// partition, which is the one these ARNs are written in.
pub const ACCOUNT_PARTITION: &str = "aws";

/// The ARN of the ROOT of the account `account_id`:
/// `arn:aws:iam::<account id>:root`.
///
/// The root ARN and the bare account id name the SAME principal — a whole account — and the
/// root ARN is the form a policy STORE writes it in, which is why a store that reports an
/// account principal canonically reports it as this
/// ([`super::iam_policy_rendering::ElementValues`] over the `AWS` type).
#[must_use]
pub fn account_root_arn(account_id: &str) -> String {
    format!("arn:{ACCOUNT_PARTITION}:iam::{account_id}:root")
}

/// How a policy store SPELLS an account principal: a whole account named by its bare account
/// id is written as that account's root ARN ([`account_root_arn`]), and every other value of
/// the `AWS` principal type — the wildcard, an identity's ARN, a root ARN already — is the
/// caller's own and is left exactly as it was submitted.
///
/// The `written_as` of an [`super::iam_policy_rendering::ElementValues`] over the `AWS`
/// type, for a service whose reads report an account principal in that one spelling. It is a
/// FIXED POINT — a root ARN is no account id, so a value it wrote is one it leaves alone —
/// which is what makes a client's re-put of the document it read store the same bytes again.
#[must_use]
pub fn account_root_principal(value: &str) -> Option<String> {
    super::arn::is_account_id(value).then(|| account_root_arn(value))
}

/// Whether `value` is a DNS name: at least two labels, each non-empty and written with
/// ASCII letters, digits and `-`. The form a service principal and a web-identity provider
/// are published under.
fn is_host(value: &str) -> bool {
    let mut labels = 0usize;
    for label in value.split('.') {
        if label.is_empty()
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return false;
        }
        labels += 1;
    }
    labels >= 2
}

/// Whether `value` is the IAM ARN of an identity provider of an account: a global `iam` ARN
/// of an account whose resource is one of [`PROVIDER_RESOURCE_TYPES`] followed by a
/// non-empty provider name.
fn is_provider_arn(value: &str) -> bool {
    let Some(arn) = super::arn::parse(value) else {
        return false;
    };
    arn.vendor == "iam"
        && arn.region.is_empty()
        && super::arn::is_account_id(arn.account)
        && PROVIDER_RESOURCE_TYPES.iter().any(|resource_type| {
            arn.resource
                .strip_prefix(resource_type)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|name| !name.is_empty())
        })
}

/// Whether `value` is a web-identity provider as a policy names one: the host it is
/// published under, and — for an OIDC issuer, whose principal is its issuer URL without the
/// scheme — the path that follows the host.
fn is_provider_url(value: &str) -> bool {
    let (host, path) = value.split_once('/').unwrap_or((value, ""));
    is_host(host) && !path.contains(char::is_whitespace)
}

/// The first value the statements of the policy document `text` name as a principal that is
/// in NONE of the forms the policy language defines for its principal type, or `None` when
/// every one of them names a principal.
///
/// [`unreadable_principal`] is this reading for a statement the caller has already parsed;
/// a service that holds the document as TEXT reads it here, so it parses once and walks the
/// same statements the grammar beside it walks ([`super::iam_policy`]) rather than carrying a
/// second reading of the `Statement` element. A text that is no policy names no principal and
/// is answered `None`: that it is no policy is the grammar's clause, not this one's.
#[must_use]
pub fn unreadable_policy_principal(text: &str) -> Option<String> {
    let document = ::serde_json::from_str::<::serde_json::Value>(text).ok()?;
    super::iam_policy::statements(&document)
        .into_iter()
        .filter_map(::serde_json::Value::as_object)
        .find_map(unreadable_principal)
}

/// Every identity the statements of the policy document `text` grant to: the values their
/// principal elements ([`PRINCIPAL_STATEMENT_ELEMENTS`]) name, in the order the document
/// names them.
///
/// A service that RESOLVES the identities a document grants to — against its account's IAM
/// ([`super::iam_principal`]) or against its own records — reads them here, so every such
/// service resolves the same values this module reads the forms of. Empty for a text that is
/// no policy, and for a document whose statements name no principal.
#[must_use]
pub fn policy_principals(text: &str) -> Vec<String> {
    let Ok(document) = ::serde_json::from_str::<::serde_json::Value>(text) else {
        return Vec::new();
    };
    super::iam_policy::statements(&document)
        .into_iter()
        .flat_map(|statement| {
            PRINCIPAL_STATEMENT_ELEMENTS
                .into_iter()
                .filter_map(|element| statement.get(element))
                .flat_map(named_principals)
                .collect::<Vec<String>>()
        })
        .collect()
}

/// The principals one principal element names, in whichever of the language's two forms it
/// is written: the untyped shorthand, or a map from a principal type to one value or a list
/// of them. A value under a name the language does not define as a type names no principal,
/// because nothing is granted to a type that does not exist.
fn named_principals(element: &::serde_json::Value) -> Vec<String> {
    match element {
        ::serde_json::Value::String(value) => vec![value.clone()],
        ::serde_json::Value::Object(types) => types
            .iter()
            .filter(|(principal_type, _)| PRINCIPAL_TYPES.contains(&principal_type.as_str()))
            .flat_map(|(_, named)| values(named))
            .collect(),
        _ => Vec::new(),
    }
}

/// `element`'s values: a principal type names one string or a list of them, and anything
/// else names no value — a nested object under a type is not a principal, so it is reported
/// through its own rendering rather than ignored, which is the one way this differs from the
/// element flattener the grammar beside it reads an action or a resource with
/// ([`super::iam_policy`]), where a value it cannot read is simply not a value.
fn values(element: &::serde_json::Value) -> Vec<String> {
    match element {
        ::serde_json::Value::String(value) => vec![value.clone()],
        ::serde_json::Value::Array(entries) => entries.iter().flat_map(values).collect(),
        other => vec![other.to_string()],
    }
}

/// The policy document `text`'s own MEMBER that names `principal`, written as a policy
/// member is ([`super::iam_policy::canonical_member`]), or `None` when no statement of the
/// document names it.
///
/// A principal is not a value standing on its own: the policy language writes it as a member
/// keyed by the principal TYPE, and the same ARN is a principal under one type and nothing
/// under another — so what a service's refusal quotes back is the member, key included,
/// rather than the value lifted out of it. `principal` is the offending text one of the
/// readings above reported ([`unreadable_policy_principal`], [`policy_principals`]):
///
/// * a VALUE under a type is quoted under the type that named it (`"AWS" : "<value>"`),
///   whether the document wrote one value or a list of them — the fault is the one value, so
///   the member names it rather than the whole list;
/// * a type KEY the language does not define is quoted with the value it keyed, because the
///   key is itself what names no principal type;
/// * a value written in the untyped shorthand is quoted under the statement element that
///   carried it (`"Principal" : "<value>"`), which is the member there is.
#[must_use]
pub fn principal_member(text: &str, principal: &str) -> Option<String> {
    let document = ::serde_json::from_str::<::serde_json::Value>(text).ok()?;
    super::iam_policy::statements(&document)
        .into_iter()
        .find_map(|statement| {
            let statement = statement.as_object()?;
            PRINCIPAL_STATEMENT_ELEMENTS
                .into_iter()
                .find_map(|element| {
                    let named = statement.get(element)?;
                    naming_member(element, named, principal)
                })
        })
}

/// [`principal_member`] for ONE principal element, however that element is written.
fn naming_member(
    element: &str,
    named: &::serde_json::Value,
    principal: &str,
) -> Option<String> {
    let quoted = |key: &str| {
        super::iam_policy::canonical_member(
            key,
            &::serde_json::Value::String(principal.to_string()),
        )
    };
    match named {
        ::serde_json::Value::Object(types) => types.iter().find_map(|(principal_type, under)| {
            if principal_type == principal {
                return Some(super::iam_policy::canonical_member(principal_type, under));
            }
            values(under)
                .iter()
                .any(|value| value == principal)
                .then(|| quoted(principal_type))
        }),
        other => values(other)
            .iter()
            .any(|value| value == principal)
            .then(|| quoted(element)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A statement whose `Principal` is `principal`, as a service hands one over.
    fn statement(principal: ::serde_json::Value) -> ::serde_json::Map<String, ::serde_json::Value> {
        let value = ::serde_json::json!({
            "Effect": "Allow",
            "Action": "svc:DoThing",
            "Principal": principal,
        });
        value
            .as_object()
            .expect("the statement is an object")
            .clone()
    }

    /// RULE: every value a statement's principal elements name is in one of the forms the
    /// policy language defines for its principal type; a value outside them is reported.
    ///
    /// The rows differ along what the rule generalises over: the principal type, the form the
    /// element is written in (the bare shorthand, a typed map, a list under one type, several
    /// types at once), the identity an ARN names, and HOW a value misses its type's forms.
    #[test]
    fn a_value_outside_its_types_forms_is_the_one_reported() {
        let rows: [(&str, ::serde_json::Value, Option<&str>); 24] = [
            ("the bare wildcard", ::serde_json::json!("*"), None),
            (
                "the wildcard under the AWS type",
                ::serde_json::json!({"AWS": "*"}),
                None,
            ),
            (
                "an account by its id",
                ::serde_json::json!({"AWS": "123456789012"}),
                None,
            ),
            (
                "an account's root",
                ::serde_json::json!({"AWS": "arn:aws:iam::123456789012:root"}),
                None,
            ),
            (
                "an IAM user and role, in another partition",
                ::serde_json::json!({"AWS": [
                    "arn:aws-cn:iam::123456789012:user/builder",
                    "arn:aws-cn:iam::123456789012:role/service-role/pull",
                ]}),
                None,
            ),
            (
                "an STS session and a federated user",
                ::serde_json::json!({"AWS": [
                    "arn:aws:sts::123456789012:assumed-role/pull/session",
                    "arn:aws:sts::123456789012:federated-user/visitor",
                ]}),
                None,
            ),
            (
                "a service principal, plain and regional",
                ::serde_json::json!({"Service": [
                    "ecs-tasks.amazonaws.com",
                    "logs.eu-west-1.amazonaws.com",
                ]}),
                None,
            ),
            (
                "a SAML and an OIDC provider by ARN",
                ::serde_json::json!({"Federated": [
                    "arn:aws:iam::123456789012:saml-provider/corp",
                    "arn:aws:iam::123456789012:oidc-provider/accounts.google.com",
                ]}),
                None,
            ),
            (
                "a web-identity provider by host and by issuer URL",
                ::serde_json::json!({"Federated": [
                    "accounts.google.com",
                    "oidc.eks.eu-west-1.amazonaws.com/id/EXAMPLE",
                ]}),
                None,
            ),
            (
                "a canonical user id",
                ::serde_json::json!({"CanonicalUser": "a".repeat(CANONICAL_USER_ID_LEN)}),
                None,
            ),
            (
                "several types at once, each in its own forms",
                ::serde_json::json!({
                    "AWS": "123456789012",
                    "Service": "ecr.amazonaws.com",
                    "Federated": "cognito-identity.amazonaws.com",
                }),
                None,
            ),
            (
                "a statement naming no principal at all",
                ::serde_json::Value::Null,
                None,
            ),
            // A value in none of its type's forms, one row per way of missing them.
            (
                "an account id of the wrong width",
                ::serde_json::json!({"AWS": "12345678901"}),
                Some("12345678901"),
            ),
            (
                "an account id that is not all digits",
                ::serde_json::json!({"AWS": "12345678901x"}),
                Some("12345678901x"),
            ),
            (
                "an entity name rather than an ARN",
                ::serde_json::json!({"AWS": "builder"}),
                Some("builder"),
            ),
            (
                "an ARN of an IAM entity that is no principal",
                ::serde_json::json!({"AWS": "arn:aws:iam::123456789012:group/builders"}),
                Some("arn:aws:iam::123456789012:group/builders"),
            ),
            (
                "a principal ARN carrying a wildcard",
                ::serde_json::json!({"AWS": "arn:aws:iam::123456789012:user/*"}),
                Some("arn:aws:iam::123456789012:user/*"),
            ),
            (
                "the second value of a list",
                ::serde_json::json!({"AWS": ["arn:aws:iam::123456789012:root", "nobody"]}),
                Some("nobody"),
            ),
            (
                "a service principal that is not a DNS name",
                ::serde_json::json!({"Service": "Amazon ECR"}),
                Some("Amazon ECR"),
            ),
            (
                "a provider ARN of the wrong resource type",
                ::serde_json::json!({"Federated": "arn:aws:iam::123456789012:role/corp"}),
                Some("arn:aws:iam::123456789012:role/corp"),
            ),
            (
                "a canonical user id of the wrong length",
                ::serde_json::json!({"CanonicalUser": "a".repeat(CANONICAL_USER_ID_LEN - 1)}),
                Some(&"a".repeat(CANONICAL_USER_ID_LEN - 1)),
            ),
            (
                "a principal type the language does not define",
                ::serde_json::json!({"IAM": "arn:aws:iam::123456789012:root"}),
                Some("IAM"),
            ),
            (
                "the shorthand carrying anything but the wildcard",
                ::serde_json::json!("123456789012"),
                Some("123456789012"),
            ),
            (
                "a principal element that is a list",
                ::serde_json::json!(["*"]),
                Some("[\"*\"]"),
            ),
        ];

        for (case, principal, expected) in rows {
            let mut entry = statement(principal.clone());
            if principal.is_null() {
                entry.remove("Principal");
            }
            let read = unreadable_principal(&entry);
            assert_eq!(
                read.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {read:?} for {principal}"
            );
        }
    }

    /// RULE: the complement element names principals in the same forms, and every element of
    /// the statement is read.
    #[test]
    fn the_complement_element_is_read_the_same_way() {
        let statement = ::serde_json::json!({
            "Effect": "Deny",
            "Action": "svc:DoThing",
            "Principal": {"AWS": "arn:aws:iam::123456789012:root"},
            "NotPrincipal": {"AWS": "nobody"},
        });
        let entry = statement.as_object().expect("the statement is an object");
        let read = unreadable_principal(entry);
        assert_eq!(
            read.as_deref(),
            Some("nobody"),
            "a NotPrincipal value outside the AWS forms must be reported, got {read:?}"
        );
    }

    /// RULE: the same two readings over a document held as TEXT — the statements are the
    /// ones the grammar beside this module walks, in both forms it writes them.
    ///
    /// The rows differ along what the readings generalise over: the form `Statement` takes
    /// (a list, the one statement as the object itself), which element names the principals,
    /// how many statements name them, and a text that is no policy at all.
    #[test]
    fn a_document_held_as_text_is_read_over_the_same_statements() {
        let rows: [(&str, &str, Vec<&str>, Option<&str>); 6] = [
            (
                "one statement naming two identities",
                "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Action\":\"svc:A\",\"Principal\":{\"AWS\":[\"123456789012\",\
                 \"arn:aws:iam::123456789012:root\"]}}]}",
                vec!["123456789012", "arn:aws:iam::123456789012:root"],
                None,
            ),
            (
                "the statement written as the object itself",
                "{\"Version\":\"2012-10-17\",\"Statement\":{\"Effect\":\"Allow\",\
                 \"Action\":\"svc:A\",\"Principal\":\"*\"}}",
                vec!["*"],
                None,
            ),
            (
                "two statements, one naming the complement",
                "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Action\":\"svc:A\",\"Principal\":{\"Service\":\"svc.amazonaws.com\"}},\
                 {\"Effect\":\"Deny\",\"Action\":\"svc:B\",\
                 \"NotPrincipal\":{\"AWS\":\"arn:aws:iam::123456789012:user/one\"}}]}",
                vec!["svc.amazonaws.com", "arn:aws:iam::123456789012:user/one"],
                None,
            ),
            (
                "a value outside its type's forms",
                "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Action\":\"svc:A\",\"Principal\":{\"AWS\":\"nobody\"}}]}",
                vec!["nobody"],
                Some("nobody"),
            ),
            (
                "a document naming no principal at all",
                "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Deny\"}]}",
                vec![],
                None,
            ),
            ("a text that is no policy", "not a policy", vec![], None),
        ];

        for (case, document, named, unreadable) in rows {
            let principals = policy_principals(document);
            assert_eq!(
                principals, named,
                "case {case:?}: expected the identities {named:?}, got {principals:?}"
            );
            let read = unreadable_policy_principal(document);
            assert_eq!(
                read.as_deref(),
                unreadable,
                "case {case:?}: expected {unreadable:?}, got {read:?}"
            );
        }
    }

    /// RULE: the member a document names a principal in is quoted as a policy member is —
    /// the type key and the value — whatever layout the document was written in.
    ///
    /// The rows differ along what the rule generalises over: the layout (compact, indented,
    /// a space on either side of the separator), the form the element takes (the typed map,
    /// a one-value list, a list whose later value is the one asked about, the untyped
    /// shorthand), which statement element names it, which statement of several carries it,
    /// a type the language does not define, and a value no statement names at all.
    #[test]
    fn the_member_naming_a_principal_is_quoted_as_a_policy_member_is() {
        let compact = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                       \"Action\":\"svc:A\",\"Principal\":{\"AWS\":\"nobody\"}}]}";
        let indented = "{\n  \"Statement\": [\n    {\n      \"Principal\": {\n        \
                        \"AWS\": \"nobody\"\n      }\n    }\n  ]\n}";
        let spaced = "{ \"Statement\" : [ { \"Principal\" : { \"AWS\" : \"nobody\" } } ] }";
        let listed = "{\"Statement\":[{\"Principal\":{\"AWS\":[\"nobody\"]}}]}";
        let later = "{\"Statement\":[{\"Principal\":{\"AWS\":[\"123456789012\",\"nobody\"]}}]}";
        let shorthand = "{\"Statement\":[{\"Principal\":\"nobody\"}]}";
        let complement = "{\"Statement\":[{\"NotPrincipal\":{\"Service\":\"nobody\"}}]}";
        let second = "{\"Statement\":[{\"Principal\":{\"AWS\":\"123456789012\"}},\
                      {\"Principal\":{\"Federated\":\"nobody\"}}]}";
        let undefined = "{\"Statement\":[{\"Principal\":{\"Account\":\"123456789012\"}}]}";
        let rows: [(&str, &str, &str, Option<&str>); 10] = [
            ("a compact document", compact, "nobody", Some("\"AWS\" : \"nobody\"")),
            ("an indented document", indented, "nobody", Some("\"AWS\" : \"nobody\"")),
            (
                "a document spaced on both sides of the separator",
                spaced,
                "nobody",
                Some("\"AWS\" : \"nobody\""),
            ),
            ("a one-value list", listed, "nobody", Some("\"AWS\" : \"nobody\"")),
            (
                "the later value of a list",
                later,
                "nobody",
                Some("\"AWS\" : \"nobody\""),
            ),
            (
                "the untyped shorthand",
                shorthand,
                "nobody",
                Some("\"Principal\" : \"nobody\""),
            ),
            (
                "the complement element",
                complement,
                "nobody",
                Some("\"Service\" : \"nobody\""),
            ),
            (
                "the second statement of two",
                second,
                "nobody",
                Some("\"Federated\" : \"nobody\""),
            ),
            (
                "a type the language does not define, reported by its key",
                undefined,
                "Account",
                Some("\"Account\" : \"123456789012\""),
            ),
            ("a value no statement names", compact, "somebody", None),
        ];

        for (case, document, principal, expected) in rows {
            let quoted = principal_member(document, principal);
            assert_eq!(
                quoted.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {quoted:?}"
            );
        }
        assert_eq!(
            principal_member("not a policy", "nobody"),
            None,
            "case \"a text that is no policy\": expected no member, got a quote"
        );
    }
}
