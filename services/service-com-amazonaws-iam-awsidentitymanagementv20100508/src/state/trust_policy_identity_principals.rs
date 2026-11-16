// SPDX-License-Identifier: CC0-1.0

//! WHICH identity of this account a role's trust policy may name as a party that assumes the
//! role, and how IAM refuses one the account does not hold.
//!
//! A trust policy is a resource-based policy, so its statements name the parties the role
//! trusts — and IAM RESOLVES each IDENTITY of its own account that it is handed there rather
//! than storing the name on trust. An `AWS` principal naming an IAM user or role of this
//! account that the account holds no record of names nobody, so the document states a trust
//! relationship that could never be exercised: the write is refused with the modelled
//! `MalformedPolicyDocumentException` (awsQuery code `MalformedPolicyDocument`, HTTP 400) and
//! the sentence [`invalid_identity_principal`], and nothing is stored. Without that refusal
//! the caller is told the role was written, every later assume by the identity it meant to
//! trust fails with an access error that names nothing, and the role's trust list reports a
//! principal that exists nowhere.
//!
//! Only the identities this account's IAM HOLDS are resolvable, so only they are resolved:
//!
//! * an IAM user or role ARN of this account ([`::service_common::iam_role::user_name`],
//!   [`::service_common::iam_role::role_name`] — the tree's one reading of which entity of
//!   this account an IAM ARN names) is looked up by the name IAM holds it under, the role
//!   under the same reading every protection in this crate resolves a role with
//!   ([`super::ComAmazonawsIamAwsidentitymanagementv20100508State::addressed_role_path`]), so
//!   a role a service keeps provisioned is a role that resolves;
//! * everything else is resolved by nothing here and refuses nothing: the `*` wildcard, a
//!   whole account by its id or its root ARN, an identity of ANOTHER account (whose IAM this
//!   emulator is not the one to ask), an STS session, a service principal, a federated or
//!   canonical-user principal, and a value that is no ARN at all — which FORMS the policy
//!   language admits under a principal type is the shared reading beside this one
//!   (`::service_common::iam_policy_principals`).
//!
//! The reading is shared by every operation that WRITES the member — `CreateRole`,
//! `UpdateAssumeRolePolicy`, and `AcquireRole`, which renders a trust policy from a role
//! template — so one document is judged the same way whichever of them was handed it, and it
//! is the same shape the services that store a resource-based policy of their own resolve a
//! grantee with (`::service_common::iam_principal`, which asks IAM over the in-process
//! transport; this service IS that IAM, so it reads its own store). A trust document IAM
//! itself writes for a service-linked role
//! ([`super::service_linked_role_trust_policy`]) is not a caller's document and is not read
//! here.

#[allow(unused_imports)]
use super::*;

/// The principal TYPE a statement names an AWS account or one of its identities under, one of
/// the four the policy language keys a `Principal`/`NotPrincipal` map by
/// (`::service_common::iam_policy_principals::PRINCIPAL_TYPES`). A value under any other type
/// names a service, an identity provider or a canonical user and is no account identity at
/// all.
const ACCOUNT_PRINCIPAL_TYPE: &str = "AWS";

/// The first identity of THIS account that `document` names and the account does not hold, as
/// the document wrote it, or `None` when every identity it names resolves.
///
/// Every statement is read, both elements that name principals, and both forms a principal
/// type's values take (one string or a list of them) — the shared walk every resolution of a
/// trust document's principals takes ([`super::named_trust_policy_principals`]) — so where in
/// the document the name sits does not change the answer. A `document` that is no policy at
/// all is `None`: that is the grammar's own clause
/// ([`super::trust_policy_document_defect`]), which the operations read first.
///
/// A READ of the account: the store is only consulted.
#[must_use]
pub fn unresolvable_identity_principal(
    store: &ComAmazonawsIamAwsidentitymanagementv20100508State,
    document: &str,
) -> Option<String> {
    super::named_trust_policy_principals(document, ACCOUNT_PRINCIPAL_TYPE)
        .into_iter()
        .find(|named| identity_is_absent(store, named))
}

/// Whether `principal` names an IAM identity of THIS account that the account holds no record
/// of.
///
/// `false` — nothing to refuse — for every value that names no identity of this account,
/// because a refusal has to be grounded in a lookup that actually answered: a whole account,
/// another account's identity, a session, a service principal, a wildcard, a value that is no
/// IAM ARN. A user and a role are different entities even under the same name, so each ARN is
/// resolved under its own entity type.
fn identity_is_absent(
    store: &ComAmazonawsIamAwsidentitymanagementv20100508State,
    principal: &str,
) -> bool {
    if let Some(role_name) = ::service_common::iam_role::role_name(principal) {
        return store.addressed_role_path(role_name).is_none();
    }
    if let Some(user_name) = ::service_common::iam_role::user_name(principal) {
        return !store.users.contains_key(&super::name_key(user_name));
    }
    false
}

/// How IAM refuses a trust policy naming an identity of this account that the account does not
/// hold: the shared invalid-principal sentence
/// ([`super::invalid_policy_principal`]) over the account-principal type —
/// `Invalid principal in policy: "AWS":"<the rejected value>"`.
///
/// The one variable part is the rejected value, taken from the request exactly as it was
/// written, so a caller whose trust document carries a typo is told which principal was
/// refused.
#[must_use]
pub fn invalid_identity_principal(principal: &str) -> String {
    super::invalid_policy_principal(ACCOUNT_PRINCIPAL_TYPE, principal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The account every ARN this emulator mints belongs to.
    fn account() -> &'static str {
        ::engine::emulator_account_id()
    }

    /// A trust policy whose single statement names `principal` as written (a JSON fragment, so
    /// a row can write one string or a list).
    fn trust(principal: &str) -> String {
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
             \"Principal\":{{\"AWS\":{principal}}},\"Action\":\"sts:AssumeRole\"}}]}}"
        )
    }

    /// An account holding one role and one user of its own, under paths either side of the
    /// default, beside the records the account starts out with.
    fn held() -> ComAmazonawsIamAwsidentitymanagementv20100508State {
        let mut store = ComAmazonawsIamAwsidentitymanagementv20100508State::default();
        store.roles.insert(
            name_key("Deployer"),
            crate::ops::types::Role {
                path: "/team/".to_string(),
                role_name: "Deployer".to_string(),
                role_id: "AROAEXAMPLEDEPLOYER1".to_string(),
                arn: role_arn("/team/", "Deployer"),
                create_date: now(),
                assume_role_policy_document: None,
                description: None,
                max_session_duration: None,
                permissions_boundary: None,
                tags: None,
                role_last_used: None,
                source_role_template: None,
            },
        );
        store.users.insert(
            name_key("builder"),
            crate::ops::types::User {
                path: DEFAULT_PATH.to_string(),
                user_name: "builder".to_string(),
                user_id: "AIDAEXAMPLEBUILDER01".to_string(),
                arn: user_arn(DEFAULT_PATH, "builder"),
                create_date: now(),
                password_last_used: None,
                permissions_boundary: None,
                tags: None,
            },
        );
        store
    }

    /// RULE: the principal type this reading looks under is one of the four the policy
    /// language defines, and the refusal spells it in upper case.
    #[test]
    fn the_account_type_is_one_the_policy_language_defines() {
        assert!(
            ::service_common::iam_policy_principals::PRINCIPAL_TYPES
                .contains(&ACCOUNT_PRINCIPAL_TYPE),
            "expected {ACCOUNT_PRINCIPAL_TYPE:?} to be one of the principal types the language \
             defines, got {:?}",
            ::service_common::iam_policy_principals::PRINCIPAL_TYPES
        );
        let message = invalid_identity_principal("arn:aws:iam::000000000000:role/ghost");
        assert_eq!(
            message,
            "Invalid principal in policy: \"AWS\":\"arn:aws:iam::000000000000:role/ghost\"",
            "expected the refusal to spell the type in upper case, got {message:?}"
        );
    }

    /// RULE: an `AWS` principal naming an IAM user or role of this account the account does
    /// not hold is reported, whichever statement names it, under either element, in either
    /// form the value takes; every other value resolves to nothing here.
    #[test]
    fn an_identity_this_account_does_not_hold_is_the_one_reported() {
        let store = held();
        let account = account();
        // The rows differ along what the rule generalises over: the entity type, whether the
        // account holds it, the path the ARN carries, the form the value takes (a string, a
        // list, the second entry of a list), the element it sits under, the statement
        // position, the form `Statement` takes, the casing of the name, the account the ARN
        // names, and the value classes the rule does not resolve.
        let rows: [(&str, String, Option<String>); 16] = [
            (
                "a role the account holds",
                trust(&format!("\"{}\"", role_arn("/team/", "Deployer"))),
                None,
            ),
            (
                "the same role in another casing, since a name is not distinguished by case",
                trust(&format!("\"{}\"", role_arn("/team/", "DEPLOYER"))),
                None,
            ),
            (
                "a user the account holds",
                trust(&format!("\"{}\"", user_arn(DEFAULT_PATH, "builder"))),
                None,
            ),
            (
                "the whole account by its id",
                trust(&format!("\"{account}\"")),
                None,
            ),
            (
                "the account's root",
                trust(&format!("\"arn:aws:iam::{account}:root\"")),
                None,
            ),
            ("every principal", trust("\"*\""), None),
            (
                "an identity of another account, whose IAM this is not",
                trust("\"arn:aws:iam::210987654321:role/ghost\""),
                None,
            ),
            (
                "a session of this account, which no record of an entity answers for",
                trust(&format!(
                    "\"arn:aws:sts::{account}:assumed-role/Deployer/build\""
                )),
                None,
            ),
            (
                "a service principal, under its own type",
                "{\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"ec2.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}"
                    .to_string(),
                None,
            ),
            ("not a policy at all", "not a policy".to_string(), None),
            (
                "a role the account does not hold",
                trust(&format!("\"{}\"", role_arn(DEFAULT_PATH, "ghost"))),
                Some(role_arn(DEFAULT_PATH, "ghost")),
            ),
            (
                "a user the account does not hold",
                trust(&format!("\"{}\"", user_arn(DEFAULT_PATH, "ghost"))),
                Some(user_arn(DEFAULT_PATH, "ghost")),
            ),
            (
                "a user ARN carrying the name of a role the account holds",
                trust(&format!("\"{}\"", user_arn(DEFAULT_PATH, "Deployer"))),
                Some(user_arn(DEFAULT_PATH, "Deployer")),
            ),
            (
                "the second value of a list, beside a role the account holds",
                trust(&format!(
                    "[\"{}\",\"{}\"]",
                    role_arn("/team/", "Deployer"),
                    role_arn("/team/", "ghost")
                )),
                Some(role_arn("/team/", "ghost")),
            ),
            (
                "the complement element names it",
                format!(
                    "{{\"Statement\":[{{\"Effect\":\"Deny\",\
                     \"NotPrincipal\":{{\"AWS\":\"{}\"}},\
                     \"Action\":\"sts:AssumeRole\"}}]}}",
                    role_arn(DEFAULT_PATH, "ghost")
                ),
                Some(role_arn(DEFAULT_PATH, "ghost")),
            ),
            (
                "the single statement object the grammar admits names it",
                format!(
                    "{{\"Statement\":{{\"Effect\":\"Allow\",\
                     \"Principal\":{{\"AWS\":\"{}\"}},\
                     \"Action\":\"sts:AssumeRole\"}}}}",
                    user_arn("/eng/", "ghost")
                ),
                Some(user_arn("/eng/", "ghost")),
            ),
        ];

        for (case, document, expected) in rows {
            let found = unresolvable_identity_principal(&store, &document);
            assert_eq!(
                found.as_deref(),
                expected.as_deref(),
                "case {case:?}: expected {expected:?}, got {found:?} for {document}"
            );
        }
    }
}
