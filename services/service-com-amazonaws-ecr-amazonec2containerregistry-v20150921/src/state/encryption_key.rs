// SPDX-License-Identifier: CC0-1.0

//! WHICH KMS key a repository's contents are encrypted under, as the repository reports it.
//!
//! # The rule this module implements
//!
//! `EncryptionConfiguration$kmsKey` is the key a KMS-encrypted repository's contents are
//! protected with, and the request names it loosely or not at all: "the alias, key ID, or full
//! ARN of the KMS key can be specified. The key must exist in the same Region as the
//! repository. If no key is specified, the default Amazon Web Services managed KMS key for
//! Amazon ECR will be used"
//! (`smithy_query(query="shape",shape="EncryptionConfiguration")`, whose `encryptionType` doc
//! repeats it for both KMS types: "you can either use the default Amazon Web Services managed
//! KMS key for Amazon ECR, or specify your own KMS key"). An alias points at WHATEVER key it
//! currently targets and a key id names one without saying where it lives, so neither says
//! which key protects the repository — and a configuration naming nothing at all says least of
//! all. So a repository encrypted under a KMS key reports the KEY: the configuration stored for
//! it carries the ARN KMS resolves the reference to, and for a configuration that names no
//! reference that is the ARN of this account's and Region's Amazon Web Services managed key for
//! Amazon ECR, the key the predefined alias [`AWS_MANAGED_KEY_ALIAS`] stands for.
//!
//! The ARN itself is an environment value — the account and Region are the caller's, the key id
//! is whatever KMS assigned — so it is never written here: it is read from the one place that
//! holds it, which is KMS's own answer about the key.
//!
//! A repository encrypted with an Amazon S3-managed key ([`super::S3_MANAGED_ENCRYPTION_TYPE`])
//! is under no KMS key and reports none: it is encrypted "using an AES256 encryption algorithm"
//! with keys that are Amazon S3's, which no `kmsKey` identifies.
//!
//! # How KMS is reached, and what an answer establishes
//!
//! The way every emulated service reaches another: the AWS SDK over the framework's in-process
//! callback, through the tree's one construction of that client
//! (`service_common::kms_key::kms_client`) — the same hop the authorizations a KMS-encrypted
//! repository takes are made over ([`super::key_grant`]). No socket is opened, no name is
//! resolved, nothing outside this process is reachable, and the credentials are the emulator's
//! own. Every call is made with NO registry lock held.
//!
//! `DescribeKey` is the read that resolves a key reference and nothing else — it "provides
//! detailed information about a KMS key" — and it is also the call AWS documents for a
//! predefined Amazon Web Services alias, where it is what ASSOCIATES that alias with the
//! account's Amazon Web Services managed key: "Amazon Web Services services use DescribeKey to
//! create Amazon Web Services managed keys from a predefined Amazon Web Services alias with no
//! key ID"
//! (`smithy_query(query="doc",shape="DescribeKeyRequest",member="KeyId",model_path="services/service-com-amazonaws-kms-trentservice/smithy-model/com-amazonaws-kms-trentservice.json")`).
//! Only a CREATE asks it, for exactly that reason.
//!
//! An answer that does not say which key the reference names establishes nothing — an emulator
//! assembled with no KMS beside this service, a fault, a refusal of the call itself — so the
//! configuration then carries the reference the request submitted, unresolved, rather than a key
//! nothing stands behind.

/// The predefined Amazon Web Services alias of the Amazon Web Services managed KMS key for
/// Amazon ECR: the key that encrypts a repository whose KMS configuration names no key of its
/// own ("if no key is specified, the default Amazon Web Services managed KMS key for Amazon ECR
/// will be used").
///
/// Such an alias has the `aws/<service-name>` form KMS publishes for an Amazon Web Services
/// managed key, with this service's own endpoint name, and it "exists in every account and
/// Region"; asking KMS about it is what gives the account its managed key for this service, and
/// what reports the key's ARN.
pub const AWS_MANAGED_KEY_ALIAS: &str = "alias/aws/ecr";

/// Whether `encryption` encrypts the repository's contents under a KMS key at all.
///
/// Every encryption type but the Amazon S3-managed one is a KMS type — `KMS` encrypts "using
/// server-side encryption with Key Management Service key stored in KMS" and `KMS_DSSE` the same
/// "with two layers of encryption" — so the question is asked of the type that is NOT a KMS one
/// rather than of a list of those that are, and a type the model widens the enum with later is
/// read as a KMS type rather than silently as an Amazon S3-managed one.
#[must_use]
pub fn under_a_kms_key(encryption: &crate::ops::types::EncryptionConfiguration) -> bool {
    encryption.encryption_type != super::S3_MANAGED_ENCRYPTION_TYPE
}

/// The ARN of this account's and `region`'s Amazon Web Services managed key for Amazon ECR, as
/// KMS reports it — and `None` where KMS established nothing about it (see the module docs).
///
/// The key a repository encrypted under a KMS type that names no key of its own is protected
/// with. Asked only by a create, because resolving the predefined alias is what associates it
/// with the account's managed key.
pub async fn aws_managed_key(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<String> {
    resolved_arn(ctx, region, AWS_MANAGED_KEY_ALIAS).await
}

/// The ARN KMS resolves `reference` to at `region`'s endpoint — the key's own `KeyMetadata$Arn`,
/// whichever form the reference took — and `None` for every answer that does not say which key
/// it names.
async fn resolved_arn(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    reference: &str,
) -> Option<String> {
    let client = ::service_common::kms_key::kms_client(ctx, region)?;
    let error = match client.describe_key().key_id(reference).send().await {
        Ok(described) => {
            return described
                .key_metadata
                .and_then(|metadata| metadata.arn)
                .filter(|arn| !arn.is_empty());
        }
        Err(error) => error,
    };
    // Visible to an operator: an answer nothing can be read from leaves the configuration
    // naming the reference the request submitted, so without this line that would be
    // indistinguishable from a key KMS resolved. The reference is rendered in its escaped
    // `Debug` form, so a value carrying a newline cannot forge a log line.
    ::tracing::warn!(
        kms_key = ?reference,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
        "KMS did not say which key this encryption configuration names, so the repository \
         reports the reference the request submitted"
    );
    None
}
