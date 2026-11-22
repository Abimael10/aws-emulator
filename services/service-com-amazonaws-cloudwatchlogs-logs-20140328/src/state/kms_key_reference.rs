// SPDX-License-Identifier: CC0-1.0

//! Whether the KMS key a request names is a key CloudWatch Logs could encrypt a resource's
//! data with at all: the FORM of the identifier, and what KMS answers about the key it names.
//!
//! `AssociateKmsKey` states the gate and the error class it answers with: "If you attempt to
//! associate a KMS key with a log group but the KMS key does not exist or the KMS key is
//! disabled, you receive an InvalidParameterException error", and `CreateLogGroup` states the
//! same for the key a group is created under. It also states what the member may carry — "The
//! Amazon Resource Name (ARN) of the KMS key to use when encrypting log data" — so an
//! identifier that is not a key's ARN names no key this member takes, and the two conditions
//! the gate refuses are the two that sentence names: there is no such key
//! ([`KEY_NOT_FOUND_MESSAGE`]), or KMS reports it in a state it cannot be used in
//! ([`KEY_DISABLED_MESSAGE`]).
//!
//! A key is resolved through the endpoint the request reached, and a KMS key ARN addresses one
//! key in one Region, so a key ARN naming another Region names no key findable here
//! ([`names_another_region`]) — it is the first condition, decided from the identifier alone,
//! before anything is asked of KMS.
//!
//! This is a different question from the one [`super::kms_key_grant`] asks: that one is about
//! the key's own key POLICY — whether the key may be used for THIS resource — and it can only
//! be asked of a key KMS reports. This module asks whether there is such a key.
//!
//! KMS is reached the way the framework documents a handler reaching another emulated service
//! ([`smithy_server_runtime::OperationContext::callback`]): an AWS SDK client over the
//! in-process callback's HTTP client and credentials, so nothing here opens a socket or leaves
//! the host, and the only caller-supplied value that reaches the client's authority is the
//! signing Region, narrowed first ([`service_common::arn::region_label`]).
//!
//! An answer that is not KMS reporting on the key — an emulator with no KMS mounted beside
//! this service, a fault on KMS's side — establishes NOTHING about it, so it refuses nothing:
//! a refusal has to be grounded in what KMS answered.

use super::callback_client::callback_client;

/// The ARN vendor of a KMS key.
const KMS_VENDOR: &str = "kms";

/// The ARN resource type of a KEY, which is what tells a key's ARN from an ALIAS's: an alias
/// is a re-targetable name for a key rather than a key, so it is not what this member takes.
const KEY_RESOURCE_TYPE: &str = "key/";

/// The error codes KMS answers for an identifier it reports no key for: one it holds no key
/// under, and one it cannot read as a key identifier at all. Either is KMS stating that there
/// is no such key, which is the condition this gate refuses.
const NO_SUCH_KEY_CODES: [&str; 2] = ["NotFoundException", "InvalidArnException"];

/// Whether the key ARN `value` names a Region OTHER than `region`, the Region the key is being
/// resolved in.
///
/// A KMS key is a regional resource — its ARN "addresses one key in one Region" — and a
/// `kmsKeyId` is resolved through the endpoint this request reached, so a key ARN carrying
/// another Region's label names no key that can be found here, whatever KMS holds in that
/// other Region.
///
/// Only a Region LABEL is read as naming a Region ([`::service_common::arn::region_of`]): an
/// ARN whose Region field is empty or is not a label names no Region to compare, so it is left
/// to KMS to report on rather than refused here.
#[must_use]
pub fn names_another_region(value: &str, region: &str) -> bool {
    let resolved_in = ::service_common::arn::region_label(region).unwrap_or(super::DEFAULT_REGION);
    ::service_common::arn::region_of(value, KMS_VENDOR)
        .is_some_and(|named| named != resolved_in)
}

/// Whether `value` is the ARN of a KMS KEY — the form `kmsKeyId` documents ("The Amazon
/// Resource Name (ARN) of the KMS key to use when encrypting log data").
///
/// The partition, Region and account are not matched: a key's ARN is one whichever of those it
/// names, and which key it is, is KMS's answer rather than a reading taken here.
#[must_use]
pub fn is_key_arn(value: &str) -> bool {
    super::arn_service_and_resource(value)
        .is_some_and(|(service, resource)| service == KMS_VENDOR && {
            // `key/<key-id>`: the resource type, then an id. An ARN carrying the type and no
            // id names no key.
            resource
                .strip_prefix(KEY_RESOURCE_TYPE)
                .is_some_and(|key_id| !key_id.is_empty())
        })
}

/// The refusal CloudWatch Logs answers for a `kmsKeyId` that names no key it can find: a value
/// that is not a key's ARN at all, an alias (which is a name for a key rather than a key), a
/// key ARN of another Region ([`names_another_region`]), and a well-formed key ARN of this
/// Region KMS holds no key under. Fixed text for the class, carrying nothing of the request.
pub const KEY_NOT_FOUND_MESSAGE: &str = "The specified KMS Key Id could not be found.";

/// The refusal it answers for a key KMS does report, in a state it cannot encrypt with: "if you
/// attempt to associate a KMS key with a log group but [...] the KMS key is disabled, you
/// receive an InvalidParameterException error". A key is in that state exactly when KMS reports
/// it as not enabled — "specifies whether the KMS key is enabled. When KeyState is Enabled this
/// value is true, otherwise it is false" (`KeyMetadata$Enabled`).
pub const KEY_DISABLED_MESSAGE: &str = "The specified KMS key is marked as disabled.";

/// What KMS answers about the key a `kmsKeyId` names, as the two conditions
/// `AssociateKmsKey` states are told apart from each other and from the FORM of the
/// identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyUsability {
    /// The value is not a key's ARN, so it names no key this member takes: nothing was asked
    /// of KMS, because there is nothing to ask about.
    NotAKeyReference,
    /// The value is a key's ARN naming another Region than the one the key is resolved in, so
    /// no key it could name is findable here ([`names_another_region`]): nothing was asked of
    /// KMS, because this endpoint serves no key of that Region.
    OutOfRegion,
    /// KMS reports no key under the ARN: it holds none, or it cannot read the ARN as a key's.
    NoSuchKey,
    /// KMS reports the key in a state it cannot encrypt in.
    Disabled,
    /// KMS reports the key as usable — or did not report on it at all, which establishes
    /// nothing (see the module docs).
    NotRefused,
}

/// What KMS answers about the key `kms_key_id` names.
///
/// The caller decides how each answer is refused, because the operations differ: a key
/// CloudWatch Logs cannot use for the resource it was asked to encrypt is an authorization
/// refusal naming that resource on the association path, while the identifier's own form is a
/// bad parameter wherever it is named.
pub async fn usability(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kms_key_id: &str,
) -> KeyUsability {
    // Only the key's own ARN names a key here, so no other identifier form is even taken to
    // KMS: it names no key this member accepts.
    if !is_key_arn(kms_key_id) {
        return KeyUsability::NotAKeyReference;
    }
    // A key of another Region is not a key this endpoint can resolve at all, so it is told apart
    // from what KMS answers about the keys it does serve.
    if names_another_region(kms_key_id, region) {
        return KeyUsability::OutOfRegion;
    }
    let narrowed = ::service_common::arn::region_label(region).unwrap_or(super::DEFAULT_REGION);
    let client = callback_client!(aws_sdk_kms, ctx.callback(), "kms", narrowed);
    match client.describe_key().key_id(kms_key_id).send().await {
        Ok(answer) => {
            // An answer carrying no metadata states nothing about the key.
            let Some(metadata) = answer.key_metadata() else {
                return KeyUsability::NotRefused;
            };
            if is_enabled(metadata) {
                KeyUsability::NotRefused
            } else {
                KeyUsability::Disabled
            }
        }
        Err(error) => {
            let code = ::aws_smithy_types::error::metadata::ProvideErrorMetadata::code(&error);
            if code.is_some_and(|code| NO_SUCH_KEY_CODES.contains(&code)) {
                return KeyUsability::NoSuchKey;
            }
            // Not KMS reporting on the key, so nothing is established about it and nothing is
            // refused. Worth a debug line, because a deployment with no KMS to ask is one
            // where this gate does not run; the caller's own value is rendered in its escaped
            // `Debug` form, so a key identifier carrying a newline cannot forge a log line.
            ::tracing::debug!(
                kms_key = ?kms_key_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "KMS did not report on the key this request names, so the key is not judged"
            );
            KeyUsability::NotRefused
        }
    }
}

/// `Some(message)` when the `kmsKeyId` a request names is not a key this service could encrypt
/// with — it names no key KMS can find ([`KEY_NOT_FOUND_MESSAGE`]), or KMS reports the key in a
/// state it cannot be used in ([`KEY_DISABLED_MESSAGE`]) — and `None` when it is usable, or when
/// KMS did not report on it at all, which establishes nothing (see the module docs).
///
/// This is the answer for an operation that states ONE error class for both conditions: "If you
/// attempt to associate a KMS key with the log group but the KMS key does not exist or the KMS
/// key is disabled, you receive an InvalidParameterException error" (`CreateLogGroup`).
pub async fn refusal_for(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kms_key_id: &str,
) -> Option<String> {
    match usability(ctx, region, kms_key_id).await {
        KeyUsability::NotAKeyReference | KeyUsability::OutOfRegion | KeyUsability::NoSuchKey => {
            Some(KEY_NOT_FOUND_MESSAGE.to_string())
        }
        KeyUsability::Disabled => Some(KEY_DISABLED_MESSAGE.to_string()),
        KeyUsability::NotRefused => None,
    }
}

/// Whether the key KMS reported is ENABLED, the one state its data keys can be used in: every
/// other state — disabled, pending deletion, pending import — is a key that cannot encrypt
/// (`KeyMetadata$Enabled`, quoted on [`KEY_DISABLED_MESSAGE`]).
fn is_enabled(metadata: &::aws_sdk_kms::types::KeyMetadata) -> bool {
    matches!(
        metadata.key_state(),
        Some(::aws_sdk_kms::types::KeyState::Enabled)
    )
}

#[cfg(test)]
mod tests {
    use super::{is_key_arn, names_another_region};

    /// RULE: a key ARN resolves only in the Region it names, so one naming another Region than
    /// the request's names no key findable there; an ARN that names no Region label at all
    /// names no Region to disagree with.
    #[test]
    fn a_key_arn_resolves_only_in_the_region_it_names() {
        const REGION: &str = "eu-west-2";
        for (case, value, expected) in [
            (
                "a key ARN of the request's own Region",
                "arn:aws:kms:eu-west-2:000000000000:key/4b2f1a60",
                false,
            ),
            (
                "a key ARN of another Region",
                "arn:aws:kms:us-east-1:000000000000:key/4b2f1a60",
                true,
            ),
            (
                "a key ARN of another Region of another partition",
                "arn:aws-us-gov:kms:us-gov-west-1:000000000000:key/4b2f1a60",
                true,
            ),
            (
                "a key ARN whose Region field is empty",
                "arn:aws:kms::000000000000:key/4b2f1a60",
                false,
            ),
            (
                "a key ARN whose Region field is no Region label",
                "arn:aws:kms:EU-WEST-2:000000000000:key/4b2f1a60",
                false,
            ),
            (
                "an ARN of another vendor in another Region",
                "arn:aws:logs:us-east-1:000000000000:log-group:g",
                false,
            ),
            ("a value that is no ARN", "not-an-arn", false),
        ] {
            let actual = names_another_region(value, REGION);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected names_another_region({value:?}, {REGION:?}) == \
                 {expected}, got {actual}"
            );
        }
    }

    /// RULE: `kmsKeyId` takes the ARN of a KMS KEY and nothing else — not an alias (a
    /// re-targetable name), not a bare identifier, not another vendor's ARN.
    #[test]
    fn only_a_kms_key_arn_names_a_key() {
        for (case, value, expected) in [
            (
                "a key ARN",
                "arn:aws:kms:eu-west-2:000000000000:key/4b2f1a60-1f2a-4f26-8a3a-1f4d9f0b61c2",
                true,
            ),
            (
                "a key ARN in another partition",
                "arn:aws-us-gov:kms:us-gov-west-1:000000000000:key/abc",
                true,
            ),
            (
                "an alias ARN",
                "arn:aws:kms:eu-west-2:000000000000:alias/log-data",
                false,
            ),
            ("an alias name", "alias/log-data", false),
            ("a bare key id", "4b2f1a60-1f2a-4f26-8a3a-1f4d9f0b61c2", false),
            ("a value that is no ARN", "not-an-arn", false),
            (
                "another vendor's ARN",
                "arn:aws:logs:eu-west-2:000000000000:log-group:g",
                false,
            ),
            (
                "a key ARN naming no key",
                "arn:aws:kms:eu-west-2:000000000000:key/",
                false,
            ),
            ("the empty value", "", false),
        ] {
            let actual = is_key_arn(value);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected is_key_arn({value:?}) == {expected}, got {actual}"
            );
        }
    }
}
