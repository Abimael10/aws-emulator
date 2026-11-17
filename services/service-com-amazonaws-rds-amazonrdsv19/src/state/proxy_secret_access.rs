// SPDX-License-Identifier: CC0-1.0

//! Whether a proxy can read the Secrets Manager secrets its `Auth` names — the credentials
//! it logs in to a registered database with.
//!
//! "The Amazon Resource Name (ARN) of the IAM role that the proxy uses to access secrets in
//! Amazon Web Services Secrets Manager" (`CreateDBProxy$RoleArn`) and "the Amazon Resource
//! Name representing the secret that the proxy uses to authenticate to the RDS DB instance
//! or Aurora DB cluster" (`UserAuthConfig$SecretArn`): a proxy presents the credentials in
//! that secret, and it reads the secret with that role. So a role whose permissions do not
//! allow the read leaves the proxy with nothing to log in with, and the target it was
//! registered against never becomes healthy — which is what `AUTH_FAILURE`
//! (`TargetHealthReason`) reports.
//!
//! The question is put to IAM itself, over the in-process hop every service here asks a
//! role question through ([`::service_common::iam_role::role_withholds_action`]), so the
//! documents in force on the role are collected and decided by the service that holds them.
//! Nothing is established — and so nothing is reported unhealthy — where that answer cannot
//! be had: a value that names no role of this account, a role IAM holds no record of, or an
//! emulator with no IAM mounted beside this one. That is the same deliberate non-refusal
//! every other role reading in this crate takes ([`super::export_access`]).

use crate::ops::types::UserAuthConfigInfo;

/// The IAM action a proxy performs on the secret its `Auth` names: it reads the credentials
/// out of it, which is the `secretsmanager:GetSecretValue` that read requires ("Required
/// permissions: `secretsmanager:GetSecretValue`", Secrets Manager's `GetSecretValue`).
pub const PROXY_SECRET_READ_ACTION: &str = "secretsmanager:GetSecretValue";

/// The first secret among `auth` that IAM ANSWERED the role `role_arn` names may not read.
///
/// `None` where every secret the list names is readable, where the list names none at all
/// (a proxy authenticating end to end with IAM reads no secret), and where nothing could be
/// established about the role.
pub async fn unreadable_auth_secret(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
    auth: &[UserAuthConfigInfo],
) -> Option<String> {
    for secret_arn in secrets_named_in(auth) {
        if ::service_common::iam_role::role_withholds_action(
            ctx,
            region,
            role_arn,
            PROXY_SECRET_READ_ACTION,
            secret_arn,
        )
        .await
        {
            return Some(secret_arn.to_string());
        }
    }
    None
}

/// The distinct secrets a proxy's `Auth` names, in the order it names them: the entries
/// that state one, with the repetitions dropped so one secret is asked about once however
/// many users authenticate with it.
///
/// The list is already bounded when it enters the store
/// ([`super::check_user_auth_config`]), so the number of questions one call asks is bounded
/// by that allowance and not by anything a request chooses now.
fn secrets_named_in(auth: &[UserAuthConfigInfo]) -> Vec<&str> {
    let mut named: Vec<&str> = Vec::new();
    for entry in auth {
        let Some(secret_arn) = entry.secret_arn.as_deref().filter(|arn| !arn.is_empty()) else {
            continue;
        };
        if !named.contains(&secret_arn) {
            named.push(secret_arn);
        }
    }
    named
}
