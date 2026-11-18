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
//!
//! WHAT A FAILED DRY RUN SAYS is one sentence for every outcome but the key's own state
//! ([`Unusable`]): the dry run does not report which of its checks failed, so a key that resolves
//! to nothing, one whose policy admits this service nothing, one that is not a symmetric
//! encryption key and one the caller may not use are all stated as a key this service could not
//! validate. Only a key KMS holds in a state that admits no cryptographic operation is named on
//! its own.

/// What KMS states about the key a dataset association names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCondition {
    /// The key exists, is in a state that admits cryptographic operations, is the kind data is
    /// sealed under, and its policy grants this service and the caller the operations the
    /// association needs.
    Usable,
    /// The dry run did not pass: the key cannot be used for this association, in the one way
    /// the refusal tells them apart ([`Unusable`]).
    Unusable(Unusable),
    /// Nothing KMS stated decides the association: see the module docs.
    Unknown,
}

/// Why the dry run did not pass — the two refusals this service words differently.
///
/// Everything a dry run can find about a key other than its STATE is answered with one
/// sentence, because the dry run does not report which of its checks failed: a key that
/// resolves to nothing, one whose key policy grants this service nothing, one that is not the
/// kind data can be sealed under and one the caller may not use are a key this service could
/// not validate ([`unable_to_validate_message`]). A key KMS holds in a state that admits no
/// cryptographic operation is the one condition the refusal names on its own
/// ([`key_disabled_message`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// The key could not be validated for this association.
    NotValidated,
    /// KMS holds the key in a state that admits no cryptographic operation.
    Disabled,
    /// The key's own policy denies the CALLING principal the operation the association needs.
    ///
    /// Not a judgement of the key but of the caller's authority over it, which is why it is the
    /// one outcome of the dry run answered in the class an authorization failure carries.
    CallerDenied,
}

impl Unusable {
    /// The sentence this refusal carries about the key `key_arn` the request named, for the
    /// `caller` the dry run was run with.
    #[must_use]
    pub fn message(self, key_arn: &str, caller: &str) -> String {
        match self {
            Self::NotValidated => unable_to_validate_message(key_arn),
            Self::Disabled => key_disabled_message(key_arn),
            Self::CallerDenied => caller_denied_message(caller, key_arn),
        }
    }

    /// Whether this refusal is a judgement of the CALLER's authority rather than of the key:
    /// the dry run AWS runs "with the caller's credentials", which is refused in the class an
    /// authorization failure carries rather than in the request's own.
    #[must_use]
    pub fn is_the_callers_authority(self) -> bool {
        matches!(self, Self::CallerDenied)
    }
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

/// The one operation the dry run performs with the CALLER's own credentials: "The calling
/// principal must have kms:Decrypt permission on the key." (`AssociateDatasetKmsKey`).
const CALLER_DRY_RUN_OPERATION: &str = "kms:Decrypt";

/// What KMS holds for the key `key_arn` names, asked of this emulator's own KMS in `region`.
///
/// Two reads, because the decision needs both: what the key IS and what state it is in
/// (`DescribeKey`) and the key policy itself (`GetKeyPolicy`).
///
/// `caller_principals` are the spellings of the principal that MADE this request — the one
/// the caller's own `kms:Decrypt` dry run is taken for: "After those succeed, a kms:Decrypt
/// dry-run is run with the caller's credentials to verify that the calling principal can use
/// the new key." (`AssociateDatasetKmsKey`).
pub async fn key_condition(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    key_arn: &str,
    caller_principals: &[String],
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
        return KeyCondition::Unusable(Unusable::Disabled);
    }
    // "It must be a symmetric encryption KMS key (key spec SYMMETRIC_DEFAULT, key usage
    // ENCRYPT_DECRYPT). Asymmetric keys, HMAC keys, and key material types other than
    // SYMMETRIC_DEFAULT are not supported." (the same member) — a key of any other spec or usage
    // takes none of the dry-run operations the association performs.
    if metadata.key_spec() != Some(&::aws_sdk_kms::types::KeySpec::SymmetricDefault)
        || metadata.key_usage() != Some(&::aws_sdk_kms::types::KeyUsageType::EncryptDecrypt)
    {
        return KeyCondition::Unusable(Unusable::NotValidated);
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
    let prepared = ::service_common::iam_authorization::PreparedDocument::read(&policy);
    if let Some(withheld) = withheld_by(&prepared, &resolved_arn, region) {
        // WHICH operation the document withheld, and how, is diagnostic: what a caller
        // repairs is the grant itself, which is what the refusal states.
        ::tracing::debug!(
            action = withheld.action,
            reading = withheld.reading,
            "the key policy withholds this service's use of the key a dataset association names"
        );
        return KeyCondition::Unusable(Unusable::NotValidated);
    }
    // "After those succeed, a kms:Decrypt dry-run is run with the caller's credentials to
    // verify that the calling principal can use the new key." — a key policy that denies the
    // calling principal that operation outright is a key this caller cannot use, whatever the
    // policy grants this service ([`withheld_from_caller`]).
    if let Some(withheld) = withheld_from_caller(&prepared, &resolved_arn, caller_principals) {
        ::tracing::debug!(
            action = withheld.action,
            reading = withheld.reading,
            "the key policy withholds the caller's use of the key a dataset association names"
        );
        return KeyCondition::Unusable(Unusable::CallerDenied);
    }
    KeyCondition::Usable
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

/// The operation of the dry run the key's own policy does not admit for THIS SERVICE, or `None`
/// when the policy admits every one of them.
///
/// A document silent about this service's principal states no grant for it, because the grant a
/// service principal holds on a key is one the key's owner writes into the key policy. A statement
/// whose only bearing on the access hinges on a condition key this request carries no value for
/// decides nothing, and so withholds nothing.
fn withheld_by(
    prepared: &::service_common::iam_authorization::PreparedDocument,
    key_arn: &str,
    region: &str,
) -> Option<Withheld> {
    let principals = [super::alarm_actions::CLOUDWATCH_SERVICE_PRINCIPAL.to_string()];
    let context = [(
        VIA_SERVICE_CONDITION_KEY.to_string(),
        format!("{CLOUDWATCH_ENDPOINT_VENDOR}.{region}.amazonaws.com"),
    )];
    let published: Vec<&str> = context.iter().map(|(key, _)| key.as_str()).collect();
    DRY_RUN_OPERATIONS.iter().find_map(|action| {
        let request =
            ::service_common::iam_authorization::Request::new(action, key_arn, &principals)
                .with_context(&context)
                .with_published_keys(&published);
        withheld_reading(prepared, &request, action)
    })
}

/// The dry run the key's own policy EXPLICITLY DENIES the caller, or `None` when it denies it
/// nothing.
///
/// One operation: "The calling principal must have kms:Decrypt permission on the key."
/// (`AssociateDatasetKmsKey`). The decision is taken over every spelling of the calling
/// principal, the account's own delegation included, because a key policy that grants the key to
/// the account root — the statement `CreateKey` writes, through which an identity-based policy
/// can grant the key at all — is what admits an IAM principal of that account to it.
///
/// Only an explicit DENY withholds the association. Unlike a service principal, an IAM principal
/// reaches a key under TWO authorities — the key policy and the identity-based policies the
/// account's delegation lets grant it — and this evaluation holds only the first, so a document
/// that merely states nothing about the caller establishes nothing about what the caller may do
/// (the module docs' rule: an answer that establishes nothing withholds nothing). A statement
/// that denies it outright decides it whatever else either authority allows.
fn withheld_from_caller(
    prepared: &::service_common::iam_authorization::PreparedDocument,
    key_arn: &str,
    caller_principals: &[String],
) -> Option<Withheld> {
    if caller_principals.is_empty() {
        // Nothing to decide the caller's own dry run over, so it withholds nothing.
        return None;
    }
    let request = ::service_common::iam_authorization::Request::new(
        CALLER_DRY_RUN_OPERATION,
        key_arn,
        caller_principals,
    );
    let outcome =
        ::service_common::iam_authorization::resource_outcome_prepared(prepared, &request);
    matches!(
        outcome.decision,
        ::service_common::iam_authorization::Decision::Denied
    )
    .then_some(Withheld {
        action: CALLER_DRY_RUN_OPERATION,
        reading: EXPLICIT_DENY,
    })
}

/// How `prepared` reads on `request`, as a withheld dry-run operation or `None` when the
/// document admits it — the one reading both halves of the dry run are judged by.
fn withheld_reading(
    prepared: &::service_common::iam_authorization::PreparedDocument,
    request: &::service_common::iam_authorization::Request<'_>,
    action: &'static str,
) -> Option<Withheld> {
    let outcome =
        ::service_common::iam_authorization::resource_outcome_prepared(prepared, request);
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
            return KeyCondition::Unusable(Unusable::NotValidated);
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
            return KeyCondition::Unusable(Unusable::NotValidated);
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

/// How this service states a key the dry run could not validate for the association: the key
/// the request named, and what to repair on it.
///
/// ONE sentence for every outcome of the dry run other than the key's own state, because the
/// refusal does not report which of its checks failed — a key that resolves to nothing is
/// stated exactly as one whose policy grants no principal the operations the association
/// performs (see [`Unusable`]). The key ARN is the only variable part, taken from the request.
///
/// The WORDING is the recorded one, not this service's invention, and not the paragraph the
/// modelled `KmsAccessDeniedException` / `KmsKeyNotFoundException` shapes document (which is
/// shape DOCUMENTATION rather than a wire message): the behavioural corpus compares this
/// sentence exactly on six of this operation's checks — a key KMS holds nothing for, a key whose
/// policy grants this service nothing, the same presented as a re-association, and the duplicate
/// and access-error rows — each of which passes with this text and failed with the shapes'
/// documentation. The resolution note of
/// `issues/things-that-now-work/20261031T235853Z-cloudwatch-7-net-new-root-causes-associatedatasetkmskey-validates.md`
/// records it with the checks it is quoted from.
#[must_use]
pub fn unable_to_validate_message(key_arn: &str) -> String {
    format!(
        "Unable to validate KMS key {key_arn}. Verify the key exists and the key policy grants \
         kms:DescribeKey to the CloudWatch service principal."
    )
}

/// How it states a key KMS holds in a state that admits no cryptographic operation — "It must be
/// enabled and not pending deletion." (`AssociateDatasetKmsKeyInput$KmsKeyArn`) — with the key the
/// request named as its one variable part.
///
/// The recorded wording again, which the corpus states literally in the assertion of both
/// disabled-key checks (`assert message == f"KMS key {key_arn} is disabled."`): the sentence
/// names the key a caller has to re-enable, where `KmsKeyDisabledException`'s documentation
/// paragraph names none.
#[must_use]
pub fn key_disabled_message(key_arn: &str) -> String {
    format!("KMS key {key_arn} is disabled.")
}

/// How it states the dry run that refused the CALLER its use of the key.
///
/// The caller's own `kms:Decrypt` dry run is the one AWS runs "with the caller's credentials"
/// (`AssociateDatasetKmsKey`), so what refuses the association is an AUTHORIZATION refusal over
/// that principal and that key rather than a statement about the key's own condition. The
/// sentence is the tree's one rendering of such a refusal
/// ([`::service_common::iam_authorization`]), closed by the clause for the reading this service
/// refuses on — an explicit `Deny` in the key's own policy, which is a RESOURCE-based one
/// ([`withheld_from_caller`], the only reading that withholds the caller's dry run here), so
/// the sentence states the decision the code took and nothing else. Another service of this
/// tree is recorded relaying the same sentence for a refused `kms:Decrypt`, with the clause of
/// the reading ITS decision rested on
/// (`issues/things-that-now-work/20261028T021654Z-sfn-4-net-new-behavioral-root-causes-asl.md`).
#[must_use]
pub fn caller_denied_message(caller: &str, key_arn: &str) -> String {
    format!(
        "{} {}",
        ::service_common::iam_authorization::not_authorized_to_perform(
            caller,
            CALLER_DRY_RUN_OPERATION,
            key_arn,
        ),
        ::service_common::iam_authorization::RESOURCE_POLICY_EXPLICIT_DENY,
    )
}

/// How it states a `KmsKeyArn` whose account segment is not the calling identity's own account:
/// both accounts, and nothing of the key or the dataset.
///
/// A key of another account is not a key this association can be validated against at all, which
/// is a judgement of the REQUEST rather than of the key — so it is refused as the invalid
/// parameter it is, naming the two accounts a caller has to reconcile.
#[must_use]
pub fn key_account_mismatch_message(key_account: &str, caller_account: &str) -> String {
    format!("KMS key account {key_account} does not match caller account {caller_account}")
}
