// SPDX-License-Identifier: CC0-1.0

//! Whether the customer managed key a dataset association names can be used by this service at
//! all: the preconditions taken before the association is written.
//!
//! "Before completing the association, Amazon CloudWatch validates the key by performing a
//! series of dry-run KMS operations. Service-principal checks run first to verify that the key
//! policy grants the required access to Amazon CloudWatch. These checks include kms:DescribeKey,
//! kms:GenerateDataKey, kms:Encrypt, kms:Decrypt, and kms:ReEncrypt*."
//! (`AssociateDatasetKmsKey`) — so the association is a WRITE conditioned on what AWS KMS says
//! about the key, and none of those facts belongs to this store:
//!
//! * whether a key of that ARN exists at all, and what state it is in — "It must be enabled and
//!   not pending deletion" — which is `DescribeKey`'s answer;
//! * whether it is the kind of key data can be sealed under — "It must be a symmetric encryption
//!   KMS key (key spec SYMMETRIC_DEFAULT, key usage ENCRYPT_DECRYPT)" — the same answer's key
//!   spec and usage;
//! * whether its KEY POLICY grants this service's principal the operations the dry run performs,
//!   which is `GetKeyPolicy`'s answer decided by [`::service_common::iam_authorization`], the
//!   tree's one evaluation of policy documents against `(action, resource, principal)`.
//!
//! Each question is answered by the service that OWNS it, reached the way any emulated service
//! reaches another ([`super::kms_in_process`], the client the key behind an alarm action's
//! encrypted topic is read through as well).
//!
//! The decision over the key policy is the one KMS takes for a SERVICE principal: the key's own
//! policy is the only authority over the use, because such a principal holds no identity-based
//! policies and a grant cannot name one. So a key carrying only the policy `CreateKey` writes for
//! the account root admits this service nothing until a statement names its principal.
//!
//! An answer that is NOT KMS stating the key's own facts (no KMS to ask, a key whose policy could
//! not be read) establishes nothing, so it withholds nothing: the association is then written as
//! it was before any of this was asked, and nothing is invented for a question that went
//! unanswered.

/// What KMS states about the key a dataset association names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCondition {
    /// The key exists, is in a state that admits cryptographic operations, is the kind data is
    /// sealed under, and its policy grants this service the operations the association needs.
    Usable,
    /// KMS answered that no key of that ARN exists.
    NotFound,
    /// The key exists but this service cannot use it, worded as the refusal AWS answers: the
    /// sentence names what has to be repaired on the key.
    Unusable(String),
    /// Nothing KMS stated decides the association: see the module docs.
    Unknown,
}

/// The operations the dry run performs on the key as this service's own principal, each spelled
/// as the IAM action a key policy names it by.
///
/// The documented list also names `kms:ReEncrypt*`, which is a permission NAME carrying a
/// wildcard rather than an operation a dry run calls, so the concrete four are what the policy is
/// asked about; a policy that grants the whole set (or `kms:*`) allows every one of them.
const DRY_RUN_OPERATIONS: [&str; 4] = [
    "kms:DescribeKey",
    "kms:GenerateDataKey",
    "kms:Encrypt",
    "kms:Decrypt",
];

/// The condition key naming the service a KMS request was made THROUGH, whose value is that
/// service's regional endpoint name: the key is reached by Amazon CloudWatch sealing the dataset's
/// data, so a key policy scoped by it is written against this service's endpoint.
const VIA_SERVICE_CONDITION_KEY: &str = "kms:ViaService";

/// The ARN namespace of this service, which is the name its regional endpoint is built from.
const CLOUDWATCH_ENDPOINT_VENDOR: &str = "cloudwatch";

/// What KMS holds for the key `key_arn` names, asked of this emulator's own KMS in `region`.
///
/// Two reads, because the decision needs both: what the key IS and what state it is in
/// (`DescribeKey`) and the key policy itself (`GetKeyPolicy`).
pub async fn key_condition(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    key_arn: &str,
) -> KeyCondition {
    let client = super::kms_in_process::client(callback, region);
    let described = match client.describe_key().key_id(key_arn).send().await {
        Ok(described) => described,
        Err(failure) => return describe_failure(key_arn, failure),
    };
    let Some(metadata) = described.key_metadata() else {
        return KeyCondition::Unknown;
    };
    // "It must be enabled and not pending deletion." (`AssociateDatasetKmsKeyInput$KmsKeyArn`) —
    // every other key state (disabled, pending deletion, pending import, unavailable) is a key no
    // cryptographic operation can be performed under right now, which is one sentence to repair.
    if metadata.key_state() != Some(&::aws_sdk_kms::types::KeyState::Enabled) {
        return KeyCondition::Unusable(KEY_DISABLED_REFUSAL.to_string());
    }
    // "It must be a symmetric encryption KMS key (key spec SYMMETRIC_DEFAULT, key usage
    // ENCRYPT_DECRYPT). Asymmetric keys, HMAC keys, and key material types other than
    // SYMMETRIC_DEFAULT are not supported." (the same member) — a key of any other spec or usage
    // takes none of the dry-run operations the association performs.
    if metadata.key_spec() != Some(&::aws_sdk_kms::types::KeySpec::SymmetricDefault)
        || metadata.key_usage() != Some(&::aws_sdk_kms::types::KeyUsageType::EncryptDecrypt)
    {
        return KeyCondition::Unusable(KEY_NOT_SYMMETRIC_REFUSAL.to_string());
    }
    // A key policy's `Resource` element is `*`, which means the key the policy is attached to, so
    // the resource the decision is taken over is the key's own ARN as KMS reports it.
    let Some(resolved_arn) = metadata.arn().map(str::to_string) else {
        return KeyCondition::Unknown;
    };
    let policy = match client.get_key_policy().key_id(key_arn).send().await {
        Ok(answered) => answered.policy,
        Err(failure) => return policy_failure(key_arn, failure),
    };
    let Some(policy) = policy else {
        return KeyCondition::Unknown;
    };
    match withheld_by(&policy, &resolved_arn, region) {
        Some(withheld) => {
            // WHICH operation the document withheld, and how, is diagnostic: what a caller
            // repairs is the grant itself, which is what the refusal states.
            ::tracing::debug!(
                action = withheld.action,
                reading = withheld.reading,
                "the key policy withholds this service's use of the key a dataset association \
                 names"
            );
            KeyCondition::Unusable(KEY_ACCESS_DENIED_REFUSAL.to_string())
        }
        None => KeyCondition::Usable,
    }
}

/// What a key policy withheld: the operation it did not admit, and the reading that withheld it.
///
/// Two borrowed names rather than a sentence, because nothing a caller sees is built from them:
/// the refusal states the grant to repair, and these two say which statement to look at.
#[derive(Debug, Clone, Copy)]
struct Withheld {
    /// The operation of the dry run the document did not admit.
    action: &'static str,
    /// Why it was not admitted: an explicit deny, or no statement allowing it.
    reading: &'static str,
}

/// The reading that withheld the use because a statement denied it outright.
const EXPLICIT_DENY: &str = "an explicit deny in the key policy";

/// The reading that withheld it because no statement allows it for this principal.
const NO_ALLOW: &str = "no statement of the key policy allows it";

/// The operation of the dry run the key's own policy does not admit, or `None` when the policy
/// admits every one of them.
///
/// A document silent about this service's principal states no grant for it, because the grant a
/// service principal holds on a key is one the key's owner writes into the key policy. A statement
/// whose only bearing on the access hinges on a condition key this request carries no value for
/// decides nothing, and so withholds nothing.
fn withheld_by(policy: &str, key_arn: &str, region: &str) -> Option<Withheld> {
    let principals = [super::alarm_actions::CLOUDWATCH_SERVICE_PRINCIPAL.to_string()];
    let context = [(
        VIA_SERVICE_CONDITION_KEY.to_string(),
        format!("{CLOUDWATCH_ENDPOINT_VENDOR}.{region}.amazonaws.com"),
    )];
    let published: Vec<&str> = context.iter().map(|(key, _)| key.as_str()).collect();
    let prepared = ::service_common::iam_authorization::PreparedDocument::read(policy);
    DRY_RUN_OPERATIONS.iter().find_map(|action| {
        let request =
            ::service_common::iam_authorization::Request::new(action, key_arn, &principals)
                .with_context(&context)
                .with_published_keys(&published);
        let outcome =
            ::service_common::iam_authorization::resource_outcome_prepared(&prepared, &request);
        match outcome.decision {
            ::service_common::iam_authorization::Decision::Allowed => None,
            ::service_common::iam_authorization::Decision::Denied => Some(Withheld {
                action,
                reading: EXPLICIT_DENY,
            }),
            ::service_common::iam_authorization::Decision::NotStated => outcome
                .required_context_keys
                .is_empty()
                .then_some(Withheld {
                    action,
                    reading: NO_ALLOW,
                }),
        }
    })
}

/// What a `DescribeKey` that did not answer with the key's facts establishes: KMS stating that
/// there is no such key, or nothing at all.
fn describe_failure(
    key_arn: &str,
    failure: ::aws_sdk_kms::error::SdkError<
        ::aws_sdk_kms::operation::describe_key::DescribeKeyError,
        ::aws_smithy_runtime_api::http::Response,
    >,
) -> KeyCondition {
    if let ::aws_sdk_kms::error::SdkError::ServiceError(ref service) = failure {
        // KMS answers an identifier naming no key with its own not-found fault, and an ARN it
        // cannot read as a key identifier with the invalid-ARN one: both are KMS stating that this
        // ARN resolves to no key it holds.
        if service.err().is_not_found_exception() || service.err().is_invalid_arn_exception() {
            return KeyCondition::NotFound;
        }
    }
    super::kms_in_process::unstated_key(key_arn, &failure, UNSTATED_KEY_NOTE);
    KeyCondition::Unknown
}

/// The same for a `GetKeyPolicy`: a policy that could not be read decides nothing, and a key KMS
/// has meanwhile stopped holding is one that resolves to no key.
fn policy_failure(
    key_arn: &str,
    failure: ::aws_sdk_kms::error::SdkError<
        ::aws_sdk_kms::operation::get_key_policy::GetKeyPolicyError,
        ::aws_smithy_runtime_api::http::Response,
    >,
) -> KeyCondition {
    if let ::aws_sdk_kms::error::SdkError::ServiceError(ref service) = failure {
        if service.err().is_not_found_exception() || service.err().is_invalid_arn_exception() {
            return KeyCondition::NotFound;
        }
    }
    super::kms_in_process::unstated_key(key_arn, &failure, UNSTATED_KEY_NOTE);
    KeyCondition::Unknown
}

/// What an operator is told when KMS stated nothing about the key: this precondition did not run,
/// so the association was written as it was before it existed.
const UNSTATED_KEY_NOTE: &str =
    "the key a dataset association names is one KMS did not state its facts for, so the key's \
     own preconditions are not applied to this association";

/// What this service refuses an association with a key whose policy does not grant it the
/// operations the association performs, in the words the fault documents: "The operation was
/// denied because either the calling principal lacks the required Amazon Web Services Key
/// Management Service (Amazon Web Services KMS) permission on the key, or the key policy does not
/// grant Amazon CloudWatch the permissions it needs to use the key. Verify that the caller has
/// kms:Decrypt permission on the key, and that the key policy grants the CloudWatch service
/// principal the kms:DescribeKey, kms:GenerateDataKey, kms:Encrypt, kms:Decrypt, and
/// kms:ReEncrypt* permissions described in AssociateDatasetKmsKey."
/// (`KmsAccessDeniedException`).
///
/// The SENTENCE is that fault's; the CODE it travels under is not. Both usability failures are
/// answered under one wire code (see the handler), so the sentence is what tells a caller which
/// of them it was.
pub const KEY_ACCESS_DENIED_REFUSAL: &str =
    "The operation was denied because either the calling principal lacks the required Amazon Web \
     Services Key Management Service (Amazon Web Services KMS) permission on the key, or the key \
     policy does not grant Amazon CloudWatch the permissions it needs to use the key. Verify that \
     the caller has kms:Decrypt permission on the key, and that the key policy grants the \
     CloudWatch service principal the kms:DescribeKey, kms:GenerateDataKey, kms:Encrypt, \
     kms:Decrypt, and kms:ReEncrypt* permissions described in AssociateDatasetKmsKey.";

/// What it refuses an association with a key no cryptographic operation can be performed under, in
/// the words that fault documents: "The specified Amazon Web Services Key Management Service
/// (Amazon Web Services KMS) key is disabled or pending deletion. Re-enable the key (or restore
/// it, if it is pending deletion) and retry the operation." (`KmsKeyDisabledException`).
pub const KEY_DISABLED_REFUSAL: &str =
    "The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key is \
     disabled or pending deletion. Re-enable the key (or restore it, if it is pending deletion) \
     and retry the operation.";

/// What it refuses an association with a key data cannot be sealed under: "It must be a symmetric
/// encryption KMS key (key spec SYMMETRIC_DEFAULT, key usage ENCRYPT_DECRYPT). Asymmetric keys,
/// HMAC keys, and key material types other than SYMMETRIC_DEFAULT are not supported."
/// (`AssociateDatasetKmsKeyInput$KmsKeyArn`).
pub const KEY_NOT_SYMMETRIC_REFUSAL: &str =
    "The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key must \
     be a symmetric encryption KMS key (key spec SYMMETRIC_DEFAULT, key usage ENCRYPT_DECRYPT). \
     Asymmetric keys, HMAC keys, and key material types other than SYMMETRIC_DEFAULT are not \
     supported.";
