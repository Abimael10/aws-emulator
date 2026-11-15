// SPDX-License-Identifier: CC0-1.0

//! The Kubernetes USERNAME a cluster authenticates an IAM principal as, and the
//! placeholders Amazon EKS resolves in one.
//!
//! `AccessEntry$username` is "the name of a user that can authenticate to your
//! cluster", and "we recommend not specifying a username and letting Amazon EKS
//! specify it for you" (`CreateAccessEntryRequest$username`) — the value it
//! specifies is a TEMPLATE for every principal type whose name depends on the
//! credential the caller presents rather than on the entry alone: a self-managed
//! node is named by its private DNS name, a Fargate Pod and an assumed role by
//! the session they authenticate with.
//!
//! An IAM ROLE cannot present itself: the credential a caller signs with is a
//! session OF that role, so the identity the API server sees is that session —
//! the `arn:<partition>:sts::<account>:assumed-role/<role>/<session>` principal
//! `sts:GetCallerIdentity` reports for the same credential. That is why the
//! username EKS specifies for a role principal carries
//! [`SESSION_NAME_PLACEHOLDER`], and why the name a request is authorized,
//! refused and audited under is the template RESOLVED against the caller's own
//! session rather than the template itself: the stored entry keeps the template
//! (it is what `DescribeAccessEntry` reports), and one request's identity is what
//! that template says about that request's credential.

use super::*;

/// The placeholder Amazon EKS resolves to the name of the session the caller's
/// credential was minted for.
///
/// The same placeholder a Fargate access entry's published username carries
/// ([`DEFAULT_USERNAME_FARGATE`]), so one resolution serves both.
pub const SESSION_NAME_PLACEHOLDER: &str = "{{SessionName}}";

/// The placeholder Amazon EKS resolves to the account the authenticated
/// principal belongs to.
pub const ACCOUNT_ID_PLACEHOLDER: &str = "{{AccountID}}";

/// The username Amazon EKS specifies for an access entry whose principal is an
/// IAM ROLE: the assumed-role form of that role, with the session name left as
/// [`SESSION_NAME_PLACEHOLDER`] for the request that presents one to fill in.
///
/// `None` for a principal that is not a role — an IAM user presents its own
/// long-term credential, so it authenticates under its own ARN and there is no
/// session in the name.
///
/// An assumed-role ARN names the role by its NAME alone: a role under a path is
/// addressed `role/<path>/<name>` while a session of it is
/// `assumed-role/<name>/<session>`, so the path is not part of the session
/// principal.
pub fn assumed_role_session_username(principal: &IamPrincipalArn<'_>) -> Option<String> {
    if principal.kind != "role" {
        return None;
    }
    let role_name = principal
        .name
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())?;
    Some(format!(
        "arn:{}:sts::{}:assumed-role/{role_name}/{SESSION_NAME_PLACEHOLDER}",
        principal.partition, principal.account
    ))
}

/// `template` with the placeholders Amazon EKS resolves per request filled in
/// from the credential the request presented: the session it was minted for and
/// the account the principal belongs to.
///
/// A placeholder this caller cannot answer is LEFT AS WRITTEN rather than filled
/// with nothing: a long-term credential names no session, so a username that
/// asks for one says nothing about that caller, and reporting the template is
/// what shows which name was not resolved. `{{EC2PrivateDNSName}}` is likewise
/// left alone here — it is answered by the node a kubelet presents, not by a
/// credential.
pub fn resolved_username(
    template: &str,
    session_name: Option<&str>,
    account_id: &str,
) -> String {
    let mut resolved = template.to_string();
    if let Some(session_name) = session_name.filter(|name| !name.is_empty()) {
        resolved = resolved.replace(SESSION_NAME_PLACEHOLDER, session_name);
    }
    if !account_id.is_empty() {
        resolved = resolved.replace(ACCOUNT_ID_PLACEHOLDER, account_id);
    }
    resolved
}

/// The name of the session an STS assumed-role principal ARN names, or `None`
/// for an ARN that is not one.
///
/// `arn:<partition>:sts::<account>:assumed-role/<role name>/<session name>` is
/// the principal `sts:GetCallerIdentity` reports for a role session, and the
/// session name is what follows the role name. Read through the workspace's ONE
/// reader of that form, whose ARN parse also holds the value to the `sts` vendor
/// and to this account, so this service and every other one that resolves a
/// session principal read it alike.
pub fn assumed_role_session_name(principal_arn: &str) -> Option<&str> {
    ::service_common::iam_role::assumed_role_session_name(principal_arn)
}

/// The account an ARN names, or an empty string for one that names none.
///
/// The account field is what [`ACCOUNT_ID_PLACEHOLDER`] stands for, read off the
/// ARN through the workspace's one ARN parse rather than by splitting the value
/// again here.
pub fn arn_account_id(arn: &str) -> &str {
    ::service_common::arn::parse(arn)
        .map(|parsed| parsed.account)
        .unwrap_or_default()
}
