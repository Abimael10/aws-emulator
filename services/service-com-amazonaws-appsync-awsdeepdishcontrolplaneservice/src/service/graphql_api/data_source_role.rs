// SPDX-License-Identifier: CC0-1.0

//! Whether the ROLE a data source names may make the call a resolver step makes against it.
//!
//! "The system assumes this role when accessing the data source"
//! (`DataSource$serviceRoleArn`), so a step that calls another service acts as a SESSION of
//! that role and the permissions in force on it decide the call — whatever the caller of the
//! GraphQL request may do. One reading of that question for every data-source type that calls
//! something ([`super::event_bridge`], [`super::dynamodb`]): they differ only in the IAM
//! action the call performs and the resource it acts on, so a refusal of one reads exactly
//! like a refusal of the other.
//!
//! Nothing here writes: it asks IAM what it holds about the role and states a sentence.

use super::PlaneContext;

/// The sentence AWS refuses a data-source call with when the permissions in force on the data
/// source's role do not allow `action` on `resource`, or `None` when nothing is known against
/// it.
///
/// Which answer counts is `::service_common::iam_role`'s own rule: a role that STATES
/// identity-based policies is the authority on what it may do, so it has to be ALLOWED the
/// call; a role IAM holds no policy for says nothing about the action — a data source's
/// service role is routinely provisioned out of band — so only a statement that DENIES it
/// holds the call back. Everything that is not an answer (no IAM to ask, a value naming no
/// role of this account, a failed hop) withholds nothing.
pub async fn withheld(
    context: &PlaneContext,
    role_arn: &str,
    action: &str,
    resource: &str,
) -> Option<String> {
    let states_permissions = ::service_common::iam_role::role_states_permissions_through(
        &context.callback,
        &context.region,
        role_arn,
    )
    .await;
    let withheld = if states_permissions {
        ::service_common::iam_role::role_withholds_action_through(
            &context.callback,
            &context.region,
            role_arn,
            action,
            resource,
        )
        .await
    } else {
        ::service_common::iam_role::role_denies_action_through(
            &context.callback,
            &context.region,
            role_arn,
            action,
            resource,
        )
        .await
    };
    withheld.then(|| {
        let reason = if states_permissions {
            ::service_common::iam_authorization::no_identity_policy_allows(action)
        } else {
            ::service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
        };
        ::service_common::iam_authorization::not_authorized_by_identity_policy(
            &assumed_role_principal(role_arn),
            action,
            resource,
            &reason,
        )
    })
}

/// The principal AppSync makes such a call as: a SESSION of the role the data source names
/// (`arn:<partition>:sts::<account>:assumed-role/<role name>/<session name>`), which is the
/// identity an authorization refusal is about — a role is never acted as directly.
///
/// The partition and the account are the role's own, read off its ARN through the tree's one
/// ARN parse, so a role of another partition or account is named as that role's session rather
/// than as this emulator's.
pub fn assumed_role_principal(role_arn: &str) -> String {
    let parsed = ::service_common::arn::parse(role_arn);
    let partition = parsed.map_or(DEFAULT_PARTITION, |arn| arn.partition);
    let account = parsed.map_or("", |arn| arn.account);
    // The role's own name is the last segment of its resource, so a role held under a path
    // (`role/service-role/<name>`) is named the way IAM holds it, and a value that names no
    // role at all is named as it stands.
    let role_name = parsed
        .map(|arn| arn.resource)
        .and_then(|resource| resource.strip_prefix(ROLE_RESOURCE_PREFIX))
        .and_then(|name| name.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or(role_arn);
    format!("arn:{partition}:sts::{account}:assumed-role/{role_name}/{ROLE_SESSION_NAME}")
}

/// The resource type an IAM role ARN carries, which its name follows.
const ROLE_RESOURCE_PREFIX: &str = "role/";

/// The name of the role SESSION AppSync makes a data source's calls under, which a refusal
/// naming the session principal carries.
const ROLE_SESSION_NAME: &str = "APPSYNC_ASSUME_ROLE";

/// The partition a role ARN this emulator cannot parse is named under.
const DEFAULT_PARTITION: &str = "aws";

/// The `errorType` a step reports for a request its own data source cannot act on, whatever
/// the data source's type: the failure is the resolver's own definition — the `code` or the
/// template that produced the request — rather than anything the called service answered, so
/// it is reported as one of that CODE. Stated here, beside the authorization rule every
/// calling data source shares, so two of them cannot spell it differently.
pub const REQUEST_FAULT_ERROR_TYPE: &str = "Code";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the refusal of a call a role withholds names the role SESSION AppSync makes it
    /// under, not the role itself — a role is acted as through a session. The rows differ along
    /// how the role is held: directly, under a path, and in another partition.
    #[test]
    fn the_refusal_names_the_sessions_of_the_data_sources_role() {
        for (case, role_arn, expected) in [
            (
                "a role of this account",
                "arn:aws:iam::123456789012:role/appsync-eventbridge",
                "arn:aws:sts::123456789012:assumed-role/appsync-eventbridge/APPSYNC_ASSUME_ROLE",
            ),
            (
                "a role under a path",
                "arn:aws:iam::123456789012:role/service-role/appsync-eb",
                "arn:aws:sts::123456789012:assumed-role/appsync-eb/APPSYNC_ASSUME_ROLE",
            ),
            (
                "a role of another partition",
                "arn:aws-us-gov:iam::210987654321:role/published",
                "arn:aws-us-gov:sts::210987654321:assumed-role/published/APPSYNC_ASSUME_ROLE",
            ),
        ] {
            let named = assumed_role_principal(role_arn);
            assert_eq!(
                named, expected,
                "case {case:?}: expected {expected:?}, got {named:?}"
            );
        }
    }
}
