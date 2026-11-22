// SPDX-License-Identifier: CC0-1.0

//! Whether the ROLE a scheduled query's destination configuration names may make the write
//! that delivery performs.
//!
//! Each destination of a scheduled query names the role its delivery is made under:
//! `S3Configuration$roleArn` is "the ARN of the IAM role that grants permissions to write
//! query results to the specified Amazon S3 destination", and
//! `LookupTableConfiguration$roleArn` "the ARN of the IAM role that grants permissions to
//! create or update the lookup table with query results". So the delivery is that ROLE's
//! write rather than this service's: it happens only where the permissions in force on the
//! role allow the action the delivery performs on the resource it writes, whatever the
//! client that read the history may do. A role those permissions do not allow writes
//! NOTHING, and the destination entry carries the refusal instead of a result
//! ([`super::scheduled_query_executions`]).
//!
//! The decision itself is IAM's, put to IAM over the in-process hop — the one route in this
//! tree to what IAM answers about a role's permissions
//! ([`::service_common::iam_role::role_action_decision_through`]) — so the documents in force
//! (the inline ones, the attached ones and the ceiling a permissions boundary sets) are
//! collected and decided by the service that holds them, exactly as this service's
//! subscription deliveries are ([`super::subscription_delivery`]). Everything that is NOT an
//! answer withholds nothing: a value that names no role of this account, a role this
//! emulator's IAM holds nothing under, an answer that could not be read, and an emulator with
//! no IAM mounted at all. A refusal has to be grounded in what IAM said.
//!
//! The ROLE is never acted as directly: a service that takes one on makes its calls as a
//! session of it ([`::service_common::iam_role::assumed_role_session_arn`]), so the refusal
//! names that session, and the role's own name is the segment of it a client goes and fixes
//! the grant on.
//!
//! Nothing here reads or writes this service's store: it asks IAM what it holds about the role
//! and states a sentence.

use ::smithy_server_runtime::EmbeddedEmulator;

use ::service_common::iam_authorization::Decision;

/// The IAM action an Amazon S3 delivery performs: ONE object holding the run's result set is
/// written at the destination URI ([`super::scheduled_query_executions`]), and `s3:PutObject`
/// is the action Amazon S3 authorizes that write under.
pub const S3_DELIVERY_ACTION: &str = "s3:PutObject";

/// The IAM action a lookup-table delivery performs when this account holds no such table yet:
/// the member's own documentation states the two calls the role is given permissions for —
/// "create or update the lookup table with query results" — and a table that is not there is
/// CREATED by the refresh, which is the action this service publishes `CreateLookupTable`
/// under.
pub const LOOKUP_TABLE_CREATE_ACTION: &str = "logs:CreateLookupTable";

/// The IAM action a lookup-table delivery performs when the account already holds the table:
/// the refresh REPLACES its content, the other half of "create or update the lookup table with
/// query results".
pub const LOOKUP_TABLE_UPDATE_ACTION: &str = "logs:UpdateLookupTable";

/// The error class a refused delivery's `errorMessage` leads with, as the service states the
/// class of a destination that was not authorized — the bracketed prefix is the fixed part of
/// that message, and what follows it is the call that was refused.
const ACCESS_DENIED_CLASS: &str = "AccessDenied";

/// The name of the SESSION this service delivers a scheduled query's results under: a role is
/// acted as through a session, so this is the identity a refusal of the delivery is about.
const DELIVERY_SESSION_NAME: &str = "CloudWatchLogsScheduledQueryDelivery";

/// Amazon S3's ARN vendor: `arn:<partition>:s3:::<bucket>/<key>` IS the object, S3 being a
/// service whose ARNs carry neither Region nor account.
const S3_ARN_NAMESPACE: &str = "s3";

/// The partition an authorization decision about a role this emulator cannot read an ARN out
/// of is taken in, which is the one this emulator serves.
const DEFAULT_PARTITION: &str = "aws";

/// The ARN of the object a delivery writes, which is the resource the write is authorized
/// against: `arn:<partition>:s3:::<bucket>/<key>`.
///
/// The partition is the DELIVERY ROLE's own, read through the tree's one ARN parse, so a role
/// of another partition is judged against a resource of that same partition rather than
/// against one of this emulator's.
pub fn object_arn(role_arn: &str, bucket: &str, key: &str) -> String {
    let partition = ::service_common::arn::parse(role_arn)
        .map_or(DEFAULT_PARTITION, |parsed| parsed.partition);
    format!("arn:{partition}:{S3_ARN_NAMESPACE}:::{bucket}/{key}")
}

/// Why the role `role_arn` may not perform `action` on `resource`, or `None` where nothing
/// withholds it.
///
/// The decision is IAM's and the refusal is worded the way an authorization failure of an
/// identity-based decision is worded everywhere in this tree
/// ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]), led by the
/// error class a refused delivery is reported under: which principal (the session the
/// delivery is made as, whose role name is the grant a client fixes), which action, which
/// resource, and which of IAM's rules withheld it — an explicit deny, or no grant at all.
pub async fn withheld(
    callback: &EmbeddedEmulator,
    region: &str,
    role_arn: &str,
    action: &str,
    resource: &str,
) -> Option<String> {
    let decision = ::service_common::iam_role::role_action_decision_through(
        callback, region, role_arn, action, resource, &[],
    )
    .await?;
    let reason = match decision {
        // The role is allowed the write, so the delivery is its to make.
        Decision::Allowed => return None,
        Decision::Denied => {
            ::service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
        }
        Decision::NotStated => {
            ::service_common::iam_authorization::no_identity_policy_allows(action)
        }
    };
    Some(format!(
        "[{ACCESS_DENIED_CLASS}] {}",
        ::service_common::iam_authorization::not_authorized_by_identity_policy(
            &::service_common::iam_role::assumed_role_session_arn(
                role_arn,
                DELIVERY_SESSION_NAME
            ),
            action,
            resource,
            &reason,
        )
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the resource a delivery is authorized against is the OBJECT it writes, in the
    /// partition of the role the write is made under. The rows differ along the partition the
    /// role names and the shape of the key.
    #[test]
    fn the_authorized_resource_is_the_object_the_delivery_writes() {
        for (case, role_arn, bucket, key, expected) in [
            (
                "a role of this partition, a nested key",
                "arn:aws:iam::000000000000:role/delivery",
                "results",
                "history/nightly/q-1.json",
                "arn:aws:s3:::results/history/nightly/q-1.json",
            ),
            (
                "a role of another partition",
                "arn:aws-us-gov:iam::000000000000:role/delivery",
                "results",
                "q-1.json",
                "arn:aws-us-gov:s3:::results/q-1.json",
            ),
            (
                "a value that is no ARN at all",
                "delivery",
                "results",
                "q-1.json",
                "arn:aws:s3:::results/q-1.json",
            ),
        ] {
            let named = object_arn(role_arn, bucket, key);
            assert_eq!(
                named, expected,
                "case {case:?}: expected {expected:?}, got {named:?}"
            );
        }
    }
}
