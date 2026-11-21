// SPDX-License-Identifier: CC0-1.0

//! What a PRIVILEGED ROOT session is decided from: which value may name the member account
//! such a session is launched on, which task policy may scope it, and whether the
//! organization the caller's account belongs to grants that caller a root session at all.
//!
//! One concern, in one file, because all three questions are asked by the one operation that
//! launches such a session (`AssumeRoot`) and none of them is asked anywhere else: the
//! sentences AWS refuses a malformed target principal and a task policy outside the published
//! root-task family with are stated here once, beside the grant the launch needs.
//!
//! The facts the grant is read from are the ORGANIZATION's, and the organization is IAM's
//! record ([`super::IamState`]): "you must use credentials from an Organizations management
//! account or a delegated administrator account for IAM to call AssumeRoot", and "before you
//! can launch a privileged session, you must have centralized root access in your
//! organization" (`AssumeRoot`, this service's model). Nothing here invents any of them — a
//! process whose engine carries no organization grants no root session, which is the position
//! an account outside an organization is in.

use super::*;

/// The organization the account belongs to, as IAM holds it: the record every centralized
/// root access decision of this account is read from, re-exported here because the grant a
/// privileged session needs is read off it.
pub use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::Organization;

/// The path AWS publishes the task policies a privileged root session may be scoped by
/// under: "you must use one of following Amazon Web Services managed policies to scope root
/// session actions" (`AssumeRootRequest$TaskPolicyArn`), the family the IAM catalog carries
/// under this path.
pub const ROOT_TASK_POLICY_PATH: &str = "/root-task/";

/// `AssumeRoot`'s session length: "the value can range from 0 seconds up to the maximum
/// session duration of 900 seconds (15 minutes) … By default, the value is set to 900
/// seconds" (`AssumeRootRequest$DurationSeconds`), which is both the default and the
/// maximum the model's own range admits.
pub const ROOT_SESSION_DEFAULT_DURATION_SECONDS: i32 = 900;

/// The centralized root access feature a privileged session needs enabled for the
/// organization — the `FeatureType` value IAM's own
/// `EnableOrganizationsRootSessions` / `ListOrganizationsFeatures` name it by, so the
/// feature this service reads is the one that service writes and reports.
pub const ROOT_SESSIONS_FEATURE: &str = "RootSessions";

/// The IAM action a privileged root session is launched by, as a refusal names it.
pub const ASSUME_ROOT_ACTION: &str = "sts:AssumeRoot";

/// The sentence AWS refuses a `TargetPrincipal` that names no member account with: fixed
/// text, carrying nothing of the request — the value is the caller's own and the refusal
/// says only that it is not a target principal.
pub const INVALID_TARGET_PRINCIPAL_MESSAGE: &str = "Request must contain a valid target principal";

/// The sentence AWS refuses a `TaskPolicyArn` outside the published root-task family with:
/// fixed text, for the same reason as [`INVALID_TARGET_PRINCIPAL_MESSAGE`].
pub const INVALID_TASK_POLICY_ARN_MESSAGE: &str =
    "Request must contain valid AWS managed task policy ARN";

/// The account a `TargetPrincipal` names, or `None` for a value that names no account.
///
/// "The member account principal ARN or account ID" (`AssumeRootRequest$TargetPrincipal`),
/// so both spellings of ONE account are read here: the bare 12-digit id, and that account's
/// root principal ARN (`arn:<partition>:iam::<account>:root`), which is the ARN form of the
/// same principal ([`::service_common::iam_policy_principals::account_root_arn`]). Any other
/// value — an identity's ARN, an ARN of another vendor, prose long enough to pass the
/// member's own `@length` minimum — names no account.
#[must_use]
pub fn target_principal_account(target_principal: &str) -> Option<String> {
    if is_account_id(target_principal) {
        return Some(target_principal.to_string());
    }
    let parsed = ::service_common::arn::parse(target_principal)?;
    let names_account_root = parsed.vendor == "iam"
        && parsed.resource == "root"
        && parsed.region.is_empty()
        && is_account_id(parsed.account);
    names_account_root.then(|| parsed.account.to_string())
}

/// Whether `arn` names one of the AWS-managed task policies a privileged root session may
/// be scoped by.
///
/// Decided against the catalog of AWS-managed policies the IAM store in the same engine
/// publishes, narrowed to the path AWS files that family under
/// ([`ROOT_TASK_POLICY_PATH`]) — so the set this service admits is the set IAM's own
/// `ListPolicies` reports under that path, and a policy AWS publishes elsewhere (an
/// `AdministratorAccess`, a job-function policy) is not one of them.
#[must_use]
pub fn is_root_task_policy_arn(arn: &str) -> bool {
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::aws_managed_policy(arn)
        .is_some_and(|published| published.path == ROOT_TASK_POLICY_PATH)
}

/// Whether `organization` grants the account `caller_account` a privileged session on the
/// member account `target_account`.
///
/// Every standing requirement `AssumeRoot` documents, read off the organization's own
/// record:
///
/// * there is an organization at all, with "All features" enabled and trusted access
///   enabled for IAM in it — the two facts every centralized root access decision of this
///   account rests on (`Organization`, the IAM store);
/// * the caller is the account that administers that access — "you must use credentials
///   from an Organizations management account or a delegated administrator account for IAM
///   to call AssumeRoot" — which is the organization's own answer
///   ([`Organization::serves_root_access_to`]);
/// * centralized root access is actually turned on for root sessions —  "before you can
///   launch a privileged session, you must have centralized root access in your
///   organization" — which is the [`ROOT_SESSIONS_FEATURE`] feature being enabled; and
/// * the target is a MEMBER account — "privileged tasks on a member account in your
///   organization" — so the account that administers the access is not a target of its own.
///
/// A PURE function of the record, so the handler reads the organization out of the IAM store
/// once and decides afterwards, never holding that store's lock and the session registry's at
/// the same time.
#[must_use]
pub fn organization_grants_root_session(
    organization: Option<&Organization>,
    caller_account: &str,
    target_account: &str,
) -> bool {
    let Some(organization) = organization else {
        return false;
    };
    organization.all_features_enabled
        && organization.iam_trusted_access_enabled
        && organization.serves_root_access_to(caller_account)
        && organization
            .enabled_root_access_features
            .contains(ROOT_SESSIONS_FEATURE)
        && !organization.serves_root_access_to(target_account)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a target principal is a 12-digit account id or that account's root principal
    /// ARN, and nothing else — over the forms the member's own `@length` 12..2048 admits.
    #[test]
    fn a_target_principal_names_an_account_in_either_spelling() {
        // (case, the submitted value, the account it names)
        let rows: [(&str, &str, Option<&str>); 9] = [
            ("the emulator's own account id", "000000000000", Some("000000000000")),
            ("another account id", "123456789012", Some("123456789012")),
            (
                "that account's root principal ARN",
                "arn:aws:iam::123456789012:root",
                Some("123456789012"),
            ),
            (
                "a root principal ARN in another partition",
                "arn:aws-cn:iam::210987654321:root",
                Some("210987654321"),
            ),
            ("prose long enough to pass the length bound", "not-an-account-id-but-long-enough", None),
            ("an account id with a non-digit", "00000000000a", None),
            ("an IAM user's ARN", "arn:aws:iam::123456789012:user/alice", None),
            ("a root ARN of another vendor", "arn:aws:sts::123456789012:root", None),
            (
                "a root ARN carrying a region",
                "arn:aws:iam:us-east-1:123456789012:root",
                None,
            ),
        ];

        for (case, value, expected) in rows {
            let named = target_principal_account(value);
            assert_eq!(
                named.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {named:?}"
            );
        }
    }

    /// RULE: a task policy is one of the AWS-managed policies published under the root-task
    /// path, read from the catalog rather than from a list written out here.
    #[test]
    fn only_a_published_root_task_policy_scopes_a_root_session() {
        let published: Vec<String> = ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::AWS_MANAGED_POLICIES
            .iter()
            .filter(|entry| entry.path == ROOT_TASK_POLICY_PATH)
            .map(::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::AwsManagedPolicy::arn)
            .collect();
        assert!(
            published.len() >= 5,
            "expected the published root-task family, got {published:?}"
        );
        for arn in &published {
            assert!(
                is_root_task_policy_arn(arn),
                "case {arn:?}: a published root-task policy scopes a root session"
            );
        }
        // Rows spanning what a task policy ARN is NOT: an AWS-managed policy outside the
        // family, a root-task name filed under another path, an account's own policy of
        // that name, and values that are no policy ARN at all.
        let outside = [
            "arn:aws:iam::aws:policy/AdministratorAccess",
            "arn:aws:iam::aws:policy/job-function/SystemAdministrator",
            "arn:aws:iam::aws:policy/S3UnlockBucketPolicy",
            "arn:aws:iam::000000000000:policy/root-task/S3UnlockBucketPolicy",
            "S3UnlockBucketPolicy",
            "",
        ];
        for arn in outside {
            assert!(
                !is_root_task_policy_arn(arn),
                "case {arn:?}: expected no root-task policy, got one"
            );
        }
    }

    /// One organization, meeting every standing requirement, which a row then changes the
    /// one fact it is about.
    fn granting(management_account_id: &str) -> Organization {
        Organization {
            organization_id: "o-rootsessions01".to_string(),
            management_account_id: management_account_id.to_string(),
            all_features_enabled: true,
            iam_trusted_access_enabled: true,
            root_access_delegated_administrator_ids: ::std::collections::BTreeSet::new(),
            enabled_root_access_features: ::std::collections::BTreeSet::from([
                ROOT_SESSIONS_FEATURE.to_string()
            ]),
        }
    }

    /// RULE: a privileged session is granted only where the organization meets every
    /// standing requirement, has root sessions enabled, serves the caller and the target is
    /// a member account.
    #[test]
    fn a_root_session_is_granted_only_by_an_organization_that_serves_the_caller() {
        let management = "000000000000";
        let member = "123456789012";
        let delegated = "210987654321";

        // (case, the organization, the calling account, the target, granted)
        let mut rows: Vec<(&str, Option<Organization>, &str, &str, bool)> = vec![
            ("no organization at all", None, management, member, false),
            (
                "every requirement met",
                Some(granting(management)),
                management,
                member,
                true,
            ),
            (
                "a delegated administrator calling",
                Some(Organization {
                    root_access_delegated_administrator_ids:
                        ::std::collections::BTreeSet::from([delegated.to_string()]),
                    ..granting(management)
                }),
                delegated,
                member,
                true,
            ),
            (
                "the target is the management account",
                Some(granting(management)),
                management,
                management,
                false,
            ),
            (
                "a member account calling",
                Some(granting(management)),
                member,
                delegated,
                false,
            ),
            (
                "root sessions not enabled",
                Some(Organization {
                    enabled_root_access_features: ::std::collections::BTreeSet::new(),
                    ..granting(management)
                }),
                management,
                member,
                false,
            ),
            (
                "only the other centralized root access feature enabled",
                Some(Organization {
                    enabled_root_access_features: ::std::collections::BTreeSet::from([
                        "RootCredentialsManagement".to_string(),
                    ]),
                    ..granting(management)
                }),
                management,
                member,
                false,
            ),
            (
                "the organization is not in all-features mode",
                Some(Organization {
                    all_features_enabled: false,
                    ..granting(management)
                }),
                management,
                member,
                false,
            ),
            (
                "trusted access for IAM is not enabled",
                Some(Organization {
                    iam_trusted_access_enabled: false,
                    ..granting(management)
                }),
                management,
                member,
                false,
            ),
        ];

        for (case, organization, caller, target, expected) in rows.drain(..) {
            let granted =
                organization_grants_root_session(organization.as_ref(), caller, target);
            assert_eq!(
                granted, expected,
                "case {case:?}: expected granted={expected}, got {granted}"
            );
        }
    }
}
