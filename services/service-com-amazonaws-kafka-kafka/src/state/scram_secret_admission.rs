// SPDX-License-Identifier: CC0-1.0

//! Whether ONE secret a SCRAM-secret batch names can serve as a cluster's SASL/SCRAM
//! credential: a fact this service RESOLVES against AWS Secrets Manager rather than
//! holds.
//!
//! A cluster's SASL/SCRAM credentials live in Secrets Manager secrets, so whether one
//! of them can be associated is a question about that SECRET, not about the cluster or
//! the request: Secrets Manager has to hold it in the endpoint's own Region, and it has
//! to be encrypted with a key of its own — a CUSTOMER MANAGED KMS key — because the
//! Amazon Web Services managed key `aws/secretsmanager` cannot be shared with the
//! service principal that reads the credential. Neither fact is this service's to keep,
//! which is why this module sits beside the state rather than in it: the secrets belong
//! to Secrets Manager, and it is asked (`secretsmanager:DescribeSecret`) the way any
//! emulated service reaches another — through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]).
//!
//! The answer is PER SECRET, which is what the batch APIs' `UnprocessedScramSecret`
//! reports ("Error info for scram secret associate/disassociate failure"): the call
//! answers 200, the rest of the batch is applied, and every secret the service could
//! not act on comes back named, with a code and a message, and is NOT stored — so a
//! later listing of the cluster's secrets does not report it. The ENTRY is built here
//! too ([`unprocessed_secret`]), for both batch operations: the DISASSOCIATION decides
//! admissibility against the cluster's own stored set rather than against Secrets
//! Manager (an entry that names no association of the cluster is what it cannot act
//! on), and reports what it could not act on with the same one code and sentence.
//!
//! Every entry the ASSOCIATION reports is GROUNDED in what Secrets Manager ANSWERED. An
//! emulator with no Secrets Manager mounted knows nothing about the account's secrets,
//! and nothing is what it reports: a secret is never called unprocessable because the
//! service that holds it could not be reached.

use std::borrow::Cow;
use std::collections::BTreeSet;

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use ::service_com_amazonaws_secretsmanager_secretsmanager::state::envelope::customer_managed_key;

use crate::ops::types::UnprocessedScramSecret;

/// The service the secrets belong to, as the endpoint the callback reaches it at names
/// it.
const SECRETS_MANAGER: &str = "secretsmanager";

/// The error code Secrets Manager answers for a `SecretId` it holds no secret for.
///
/// `ResourceNotFoundException` is that service's modelled error for the condition
/// ("Secrets Manager can't find the resource that you asked for"), and it is the one
/// answer of its own that says something about the secret: any other failure is this
/// emulator failing to ask.
const SECRET_NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The `UnprocessedScramSecret$ErrorCode` for EVERY secret the SCRAM batch surface
/// cannot act on: one this endpoint's Secrets Manager holds no secret for (never
/// created, already deleted, or of another Region or account), one encrypted with the
/// Amazon Web Services managed Secrets Manager key rather than with a customer managed
/// KMS key, and — on the disassociation — one the cluster holds no association for.
///
/// ONE code for the whole class, because that is what the service answers: Amazon MSK
/// classifies every such entry as a secret ARN it could not act on and says WHICH
/// condition it met in the entry's `ErrorMessage`, so a per-condition code of this
/// emulator's own would be a value no client can get from the service. The model
/// declares `ErrorCode` and `ErrorMessage` as plain strings, so the code and the
/// sentences live here rather than in the handlers and the two batch operations cannot
/// word one condition two ways.
pub const UNPROCESSED_SECRET_ERROR_CODE: &str = "InvalidSecretArn";

/// The `ErrorMessage` for a secret encrypted with the Amazon Web Services managed
/// Secrets Manager key rather than with a customer managed KMS key: the requirement
/// Amazon MSK states for a secret it is to read a SASL/SCRAM credential out of, which
/// is what distinguishes this condition from the one
/// [`super::SECRET_ARN_INVALID_MESSAGE`] states — the ARN resolves to a secret, and
/// that secret's encryption key is the reason it cannot serve.
///
/// Fixed text: the ARN at fault is reported in the entry's own `SecretArn`, so it is
/// not interpolated into the sentence.
pub const SECRET_ON_MANAGED_KEY_MESSAGE: &str =
    "The provided secret ARN is encrypted with the default AWS KMS key. Amazon MSK requires a \
     secret encrypted with a customer managed AWS KMS key.";

/// The `UnprocessedScramSecrets` entry naming `secret_arn`, carrying
/// [`UNPROCESSED_SECRET_ERROR_CODE`] and the sentence the condition states.
///
/// The single place either batch operation builds such an entry, so an association and
/// a disassociation report one condition identically.
pub fn unprocessed_secret(secret_arn: &str, error_message: &str) -> UnprocessedScramSecret {
    UnprocessedScramSecret {
        error_code: Some(UNPROCESSED_SECRET_ERROR_CODE.to_string()),
        error_message: Some(error_message.to_string()),
        secret_arn: Some(secret_arn.to_string()),
    }
}

/// A batch of secret ARNs split into the ones that can be associated and the ones that
/// come back unprocessed.
#[derive(Debug, Clone)]
pub struct Admission<'a> {
    /// The secrets to associate, in the order the batch named them: the batch itself
    /// while nothing of it is unprocessable, so the common answer borrows the submitted
    /// list rather than copying a caller-sized one.
    pub associable: Cow<'a, [String]>,
    /// One entry per DISTINCT secret the service could not act on, in that same order:
    /// `UnprocessedScramSecrets` as the response reports it. At most
    /// [`super::MAX_SCRAM_SECRETS_PER_CLUSTER`] of them.
    pub unprocessed: Vec<UnprocessedScramSecret>,
}

impl<'a> Admission<'a> {
    /// The batch as submitted, with nothing asked and therefore nothing unprocessable:
    /// what an endpoint with no Secrets Manager to ask answers, and what a batch past
    /// the ceiling below answers, since the association refuses such a batch whole.
    fn unasked(secret_arns: &'a [String]) -> Self {
        Self {
            associable: Cow::Borrowed(secret_arns),
            unprocessed: Vec::new(),
        }
    }
}

/// Split `secret_arns` into the secrets that can serve as this endpoint's SASL/SCRAM
/// credentials and the ones that cannot, by asking Secrets Manager about each distinct
/// ARN once.
///
/// BOUNDED BY [`super::MAX_SCRAM_SECRETS_PER_CLUSTER`] before anything grows: the one
/// pass over the batch stops as soon as it has seen that many distinct ARNs, so every
/// collection built here, and the number of secrets asked about, is capped by that
/// ceiling rather than by the request — and a batch carrying more distinct ARNs than
/// that is not asked about at all, because the association refuses such a batch whole
/// and stores none of it. The pass itself is one set insertion per submitted entry, as
/// the handler's own pass over the same list already is; the ceiling is read against the
/// DISTINCT ARNs rather than the submitted length, so a batch padded with repetitions of
/// one ARN is still resolved rather than waved through.
pub async fn admit_for_association<'a>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    secret_arns: &'a [String],
) -> Admission<'a> {
    // Each distinct ARN is resolved once, in the order the batch first named it: a
    // batch may repeat one, and one secret is one outcome however many times it was
    // named. The set decides membership, so neither this pass nor what it holds is
    // sized by how many times the batch repeats an ARN.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut distinct: Vec<&String> = Vec::new();
    for arn in secret_arns {
        if !seen.insert(arn.as_str()) {
            continue;
        }
        if distinct.len() == super::MAX_SCRAM_SECRETS_PER_CLUSTER {
            return Admission::unasked(secret_arns);
        }
        distinct.push(arn);
    }

    // No Region label, so no endpoint to ask Secrets Manager at: the same answer an
    // unreachable Secrets Manager gets, which grounds no unprocessed entry.
    let Some(endpoint) = super::network::peer_endpoint(SECRETS_MANAGER, region) else {
        return Admission::unasked(secret_arns);
    };
    let client = super::network::callback_client!(aws_sdk_secretsmanager, ctx, endpoint, region);

    let mut unprocessed: Vec<UnprocessedScramSecret> = Vec::new();
    let mut refused: BTreeSet<&str> = BTreeSet::new();
    for arn in distinct {
        if let Some(error_message) = inadmissible(&client, arn).await {
            refused.insert(arn.as_str());
            unprocessed.push(unprocessed_secret(arn, error_message));
        }
    }
    if refused.is_empty() {
        return Admission {
            associable: Cow::Borrowed(secret_arns),
            unprocessed,
        };
    }
    Admission {
        // The batch minus the secrets reported above, in the order it named them.
        associable: Cow::Owned(
            secret_arns
                .iter()
                .filter(|arn| !refused.contains(arn.as_str()))
                .cloned()
                .collect(),
        ),
        unprocessed,
    }
}

/// The sentence the secret `secret_arn` comes back unprocessed with, or `None` when it
/// can be associated.
///
/// Two conditions, both of them Secrets Manager's to answer: whether it holds a secret
/// under that ARN, and — from the secret it reports — whether the secret is encrypted
/// with a key of its own. The second reading is Secrets Manager's own
/// (`envelope::customer_managed_key`, where the rule that an absent, empty or
/// `aws/secretsmanager`-aliased `KmsKeyId` all name the managed key lives), so the two
/// services cannot drift apart about which secrets are on a key a caller can grant on.
async fn inadmissible(
    client: &::aws_sdk_secretsmanager::Client,
    secret_arn: &str,
) -> Option<&'static str> {
    let described = match client.describe_secret().secret_id(secret_arn).send().await {
        Ok(described) => described,
        Err(error) => {
            let code = error
                .as_service_error()
                .and_then(ProvideErrorMetadata::code)
                .unwrap_or_default()
                .to_string();
            if code == SECRET_NOT_FOUND_CODE {
                return Some(super::SECRET_ARN_INVALID_MESSAGE);
            }
            // Any other answer is this emulator failing to ask rather than Secrets
            // Manager reporting on the secret, so it grounds no entry — and it is worth
            // a warning, because a deployment with no Secrets Manager to ask is one
            // where this admission does not run.
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a SCRAM secret could not be resolved against Secrets Manager"
            );
            return None;
        }
    };
    customer_managed_key(described.kms_key_id())
        .is_none()
        .then_some(SECRET_ON_MANAGED_KEY_MESSAGE)
}
