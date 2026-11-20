// SPDX-License-Identifier: CC0-1.0

//! `Action$s3`, "Write to an Amazon S3 bucket": the object one selected message is stored as,
//! and the hop the rules engine stores it over.
//!
//! The object's BODY is the message the rule's statement selected and its KEY is "the object
//! key" the action names (`S3Action$key`) — a parameter AWS IoT renders from the message, so a
//! key carrying `${…}` is computed per message
//! ([`super::substitution_template`]) and two messages of one rule reach two objects. The
//! bucket is the one the action names, resolved the same way, and `cannedAcl` — "the Amazon S3
//! canned ACL that controls access to the object identified by the object key" — is stored on
//! the object by Amazon S3 itself when the action names one.
//!
//! What decides whether the object is written at all is the action's role: `s3` names "the ARN
//! of the IAM role that grants access", AWS IoT acts AS that role, and the call it makes is
//! [`PUT_OBJECT_ACTION`] on the object the rendered key addresses
//! ([`super::super::rule_action_role`]). A role that does not admit this service principal or
//! does not grant that call is an object NOT written, which is the only observable the
//! permission boundary has, and which the rule's `errorAction` reports.
//!
//! Amazon S3's own refusal of the write — a bucket the account does not hold, a key its own
//! rules refuse, an ACL the bucket's ownership setting bars — is the action FAILING, reported in
//! Amazon S3's own words ([`super::refusal_of`]) rather than in a sentence worded here.
//!
//! The call goes over the emulator's own IN-PROCESS transport, through the one construction of
//! the object-store client this tree has ([`::service_common::s3_client`]), so the bucket a
//! rule names reaches only what this emulator serves itself: no socket is opened and no host
//! name is resolved.

use crate::ops::types::S3Action;

/// The IAM action an `s3` rule action performs on the object it addresses: `PutObject`
/// "requires permission for the `s3:PutObject` action".
pub const PUT_OBJECT_ACTION: &str = "s3:PutObject";

/// The shape of the action this engine names in a rule-failure report, as the model names it.
pub const FAILED_ACTION: &str = "S3Action";

/// The ARN of the object `key` in the bucket `bucket`, which is the resource the action's role
/// has to be granted the write on.
///
/// Amazon S3's ARNs carry neither a Region nor an account id — `arn:aws:s3:::<bucket>/<key>`,
/// the form every policy in this tree names an object by — because a bucket name is global.
#[must_use]
pub fn object_arn(bucket: &str, key: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{key}")
}

/// Store one object holding the selected message `message` under `key` in `bucket`.
///
/// `Err` is why the object was NOT stored, worded by whatever refused it — Amazon S3's own
/// message, which is what the rule's `errorAction` reports.
pub async fn put_object(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    bucket: &str,
    key: &str,
    action: &S3Action,
    message: &[u8],
) -> Result<(), String> {
    let stored = ::service_common::s3_client::s3_client(ctx.callback(), region)
        .put_object()
        .bucket(bucket)
        .key(key)
        .set_acl(
            action
                .canned_acl
                .as_deref()
                .map(::aws_sdk_s3::types::ObjectCannedAcl::from),
        )
        .body(::aws_sdk_s3::primitives::ByteStream::from(
            message.to_vec(),
        ))
        .send()
        .await;
    stored.map(|_| ()).map_err(|error| super::refusal_of(&error))
}
