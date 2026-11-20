// SPDX-License-Identifier: CC0-1.0

//! The ONE mapping from an IAM identity to the DATABASE USER an Amazon Redshift
//! warehouse runs that identity's work as.
//!
//! Two services answer with that user and must not answer differently: Amazon
//! Redshift mints temporary credentials for the identity a request is signed with
//! (`GetClusterCredentialsWithIAM`, whose `DbUser` "is mapped 1:1 to the source IAM
//! identity"), and the Redshift Data API reports the user a statement it accepted
//! really ran as, for a submission that names no database user of its own.
//!
//! The user's name is the IDENTITY's own name, prefixed by which KIND of identity
//! it is: `GetClusterCredentials` documents the `IAM:` prefix an identity logs on
//! under, and the Amazon Redshift identity-to-database-user mapping gives a ROLE
//! its own prefix. So the name after the prefix always appears in the ARN
//! `sts:GetCallerIdentity` answers for the same credential, and two credentials of
//! one identity map to one database user — which is what makes the mapping 1:1.
//!
//! Nothing here reaches the clock or the network: the user is a pure function of
//! the identity's ARN.

/// The prefix the database user of an IAM USER — or of any identity that is not a
/// role — carries, as `GetClusterCredentials` documents it for an identity that
/// logs on without being created on the way in.
pub const IAM_IDENTITY_DB_USER_PREFIX: &str = "IAM:";

/// The prefix the database user of an IAM ROLE identity carries, beside
/// [`IAM_IDENTITY_DB_USER_PREFIX`]: the prefix says which KIND of IAM identity the
/// database user stands for, and the name after it is that identity's own name.
pub const IAM_ROLE_DB_USER_PREFIX: &str = "IAMR:";

/// The resource type an assumed-role identity's ARN carries
/// (`arn:aws:sts::<account>:assumed-role/<role>/<session>`), which is what makes it
/// a ROLE identity rather than a user.
const ASSUMED_ROLE_RESOURCE_TYPE: &str = "assumed-role";

/// The database user the IAM identity `caller_identity_arn` is mapped to.
///
/// The identity's own name is the name field of its ARN — the user name of
/// `user/<path…><name>` or of `federated-user/<name>`, the ROLE name of
/// `assumed-role/<role>/<session>`, whose session name a caller chooses per session
/// and which therefore names no database user. A value that is not a well-formed
/// ARN is read as the name it stands for rather than replaced.
#[must_use]
pub fn iam_identity_db_user(caller_identity_arn: &str) -> String {
    let resource = crate::arn::resource(caller_identity_arn).unwrap_or(caller_identity_arn);
    let (resource_type, rest) = resource.split_once('/').unwrap_or(("", resource));
    if resource_type == ASSUMED_ROLE_RESOURCE_TYPE {
        let role_name = rest.split('/').find(|segment| !segment.is_empty());
        return format!("{IAM_ROLE_DB_USER_PREFIX}{}", role_name.unwrap_or(rest));
    }
    // Every other identity's name is the LAST segment of its resource field: an
    // IAM user's path (`user/team/airflow/analyst`) belongs to where the identity
    // sits, not to its name.
    let name = rest
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(rest);
    format!("{IAM_IDENTITY_DB_USER_PREFIX}{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One identity per KIND the ARN forms carry, and the database user each maps
    /// to: the prefix says which kind it is and the name after it is the
    /// identity's own, with a path and a role-session name belonging to neither.
    #[test]
    fn every_identity_maps_to_the_user_its_kind_and_name_name() {
        let cases = [
            (
                "arn:aws:iam::000000000000:user/emulator",
                "IAM:emulator",
            ),
            (
                "arn:aws:iam::000000000000:user/team/airflow/analyst",
                "IAM:analyst",
            ),
            (
                "arn:aws:sts::000000000000:assumed-role/RsWarehouseAdmin/session-7",
                "IAMR:RsWarehouseAdmin",
            ),
            (
                "arn:aws:sts::000000000000:federated-user/dana",
                "IAM:dana",
            ),
            // A value that is not an ARN at all stands for the name it carries.
            ("analyst", "IAM:analyst"),
        ];
        for (arn, expected) in cases {
            let mapped = iam_identity_db_user(arn);
            assert_eq!(
                mapped, expected,
                "case {arn:?}: expected the database user {expected:?}, got {mapped:?}"
            );
        }
    }
}
