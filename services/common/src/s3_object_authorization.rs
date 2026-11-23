// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of whether the permissions in force allow an Amazon S3 call a service makes
//! UNDER a caller-supplied role.
//!
//! Several emulated services act on an object store as a role their caller named — a Firehose
//! delivery writing its objects (`S3DestinationConfiguration$RoleARN`), a SageMaker Feature
//! Store persisting a record into its OfflineStore ("the IAM execution role used to persist
//! data into the OfflineStore"), AWS Config delivering a snapshot — and each has to settle the
//! SAME question: TWO documents are in force on such a call, the role's identity-based policies
//! and the destination bucket's own policy, and within one account IAM weighs them TOGETHER —
//! an explicit `Deny` from either side refuses, an `Allow` from EITHER grants, and a call
//! neither allows falls to the implicit deny ([`crate::iam_authorization::is_allowed`]). So a
//! bucket whose policy grants the role takes the call even where the role's own policies say
//! nothing about it, and a bucket that denies it takes nothing however much the role allows.
//!
//! What the ROLE's documents say is put to IAM itself, over the in-process hop
//! ([`crate::iam_role::role_action_decision_through`]), so the documents in force are collected
//! and decided by the service that HOLDS them; what the BUCKET's policy says is decided by this
//! crate's one evaluation of a policy document ([`crate::iam_authorization`]), handed the
//! document the object store itself holds. Only what was ANSWERED establishes anything: a value
//! naming no role of this account, a role this emulator's IAM holds nothing under, a bucket with
//! no policy and any failure of either hop leave the question OPEN rather than refused, because
//! such a role is routinely provisioned out of band and refusing on evidence this emulator does
//! not have would deny a call AWS permits.
//!
//! The per-service copies that predate this reading —
//! `service-com-amazonaws-firehose-firehose-20150804`'s `state::delivery::write_authorization`,
//! `service-com-amazonaws-sagemaker-sagemaker`'s `state::s3_access` and
//! `service-com-amazonaws-configservice-starlingdoveservice`'s `state::delivery_authorization`
//! — are to be MIGRATED onto it, each in the change that owns its service, so this stays the
//! one place the rule is stated.
//!
//! Nothing here holds a store guard: every question is awaited.

/// What the policy on the bucket `bucket` says about `action` on `resource` for the role
/// `role_arn`, combined with what IAM answers about that role — and `None` when NOTHING about
/// the call was established (see the module docs).
///
/// `principals` is every spelling the bucket's policy may name the caller by BESIDE the role's
/// own ARN: the assumed-role session the call is really made under, or the service principal a
/// policy written to open a bucket to the service names. The ACCOUNT's own spellings are added
/// to the deny side only ([`bucket_policy_decision`]).
pub async fn call_decision(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    role_arn: &str,
    principals: &[String],
    document: Option<&str>,
    action: &str,
    resource: &str,
) -> Option<crate::iam_authorization::Decision> {
    let from_bucket = bucket_policy_decision(document, role_arn, principals, action, resource);
    let from_role = crate::iam_role::role_action_decision_through(
        callback,
        region,
        role_arn,
        action,
        resource,
        // The call carries no condition context of its own: nothing is guessed for a key it
        // does not state, which is the fail-closed reading every clause this emulator cannot
        // decide is given.
        &[],
    )
    .await;
    let said = match from_role {
        Some(said) => said,
        // With nothing established about the role, the bucket's own policy is the only thing
        // in force: what it SAYS decides, and a bucket that says nothing establishes nothing.
        None if from_bucket == crate::iam_authorization::Decision::NotStated => return None,
        None => return Some(from_bucket),
    };
    Some(
        if said == crate::iam_authorization::Decision::Denied
            || from_bucket == crate::iam_authorization::Decision::Denied
        {
            crate::iam_authorization::Decision::Denied
        } else if crate::iam_authorization::is_allowed(said, from_bucket) {
            crate::iam_authorization::Decision::Allowed
        } else {
            crate::iam_authorization::Decision::NotStated
        },
    )
}

/// What the bucket policy `document` says about `action` on `resource` made as `role_arn` —
/// [`crate::iam_authorization::Decision::NotStated`] for a bucket with no policy, no bucket, or
/// no object store to ask.
///
/// Asked TWICE, because naming an ACCOUNT in a policy's `Principal` is not naming a principal:
/// a statement that DENIES the account refuses each of its principals, while one that ALLOWS
/// the account delegates the decision to that account's own identity-based policies rather than
/// granting the call by itself ([`crate::iam_authorization::account_principals`]). So the deny
/// is decided with the account's spellings beside the principals the call is made by, and the
/// grant with those principals alone.
#[must_use]
pub fn bucket_policy_decision(
    document: Option<&str>,
    role_arn: &str,
    principals: &[String],
    action: &str,
    resource: &str,
) -> crate::iam_authorization::Decision {
    let Some(document) = document else {
        return crate::iam_authorization::Decision::NotStated;
    };
    let mut granting = vec![role_arn.to_string()];
    granting.extend(principals.iter().cloned());
    let mut denying = granting.clone();
    denying.extend(crate::iam_authorization::account_principals(role_arn));
    let denied = crate::iam_authorization::resource_decision(
        document,
        &crate::iam_authorization::Request::new(action, resource, &denying),
    );
    if denied == crate::iam_authorization::Decision::Denied {
        return denied;
    }
    crate::iam_authorization::resource_decision(
        document,
        &crate::iam_authorization::Request::new(action, resource, &granting),
    )
}

/// The policy document the bucket `bucket` carries, and `None` for a bucket with no policy, no
/// bucket, or no object store to ask — read through the in-process hop every Amazon S3 call of
/// this crate takes ([`crate::s3_client`]).
pub async fn bucket_policy_document(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    bucket: &str,
) -> Option<String> {
    crate::s3_client::s3_client(callback, region)
        .get_bucket_policy()
        .bucket(bucket)
        .send()
        .await
        .ok()?
        .policy()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iam_authorization::Decision;

    /// A one-statement bucket policy document.
    fn document(effect: &str, principal: &str, action: &str, resource: &str) -> String {
        ::serde_json::json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": effect,
                "Principal": { "AWS": principal },
                "Action": action,
                "Resource": resource,
            }],
        })
        .to_string()
    }

    /// RULE: the bucket's own policy decides the call it is ABOUT, for the principals the call
    /// is made by — a statement about another principal, action or resource states nothing, an
    /// allow naming the ACCOUNT alone delegates rather than grants, and a deny naming it
    /// refuses.
    #[test]
    fn the_bucket_policy_decides_the_call_it_is_about() {
        let role = format!("arn:aws:iam::{}:role/delivery", ::engine::emulator_account_id());
        let account_root = format!("arn:aws:iam::{}:root", ::engine::emulator_account_id());
        let session = format!(
            "arn:aws:sts::{}:assumed-role/delivery/Session",
            ::engine::emulator_account_id()
        );
        let object = "arn:aws:s3:::archive/data/record";
        let action = "s3:PutObject";
        // (case, the bucket's policy, what it says about this call)
        let rows = [
            ("a bucket carrying no policy", None, Decision::NotStated),
            (
                "a text that is no policy at all",
                Some("not a policy".to_string()),
                Decision::NotStated,
            ),
            (
                "an allow of the call naming the role",
                Some(document("Allow", &role, action, "arn:aws:s3:::archive/*")),
                Decision::Allowed,
            ),
            (
                "an allow naming the session the call is made under",
                Some(document("Allow", &session, action, "arn:aws:s3:::archive/*")),
                Decision::Allowed,
            ),
            (
                "a deny of the call naming the role",
                Some(document("Deny", &role, action, "arn:aws:s3:::archive/*")),
                Decision::Denied,
            ),
            (
                "a deny naming the role's account",
                Some(document("Deny", &account_root, action, "arn:aws:s3:::archive/*")),
                Decision::Denied,
            ),
            (
                "an allow naming the role's account alone",
                Some(document("Allow", &account_root, action, "arn:aws:s3:::archive/*")),
                Decision::NotStated,
            ),
            (
                "a deny of another action",
                Some(document("Deny", &role, "s3:GetObject", "arn:aws:s3:::archive/*")),
                Decision::NotStated,
            ),
            (
                "a deny scoped to another bucket",
                Some(document("Deny", &role, action, "arn:aws:s3:::elsewhere/*")),
                Decision::NotStated,
            ),
        ];
        let principals = [session.clone()];
        for (case, policy, expected) in rows {
            let said =
                bucket_policy_decision(policy.as_deref(), &role, &principals, action, object);
            assert_eq!(
                said, expected,
                "case {case:?}: expected {expected:?}, got {said:?}"
            );
        }
    }
}
