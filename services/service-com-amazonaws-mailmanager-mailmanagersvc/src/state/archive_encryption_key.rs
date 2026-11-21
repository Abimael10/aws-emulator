// SPDX-License-Identifier: CC0-1.0

//! Whether the customer managed KMS key an archive would be created under is one this
//! service may really use — the pre-flight `CreateArchive` runs before it writes anything.
//!
//! `CreateArchiveRequest$KmsKeyArn` is "the Amazon Resource Name (ARN) of the KMS key for
//! encrypting emails in the archive"
//! (`smithy_query(query="doc",shape="CreateArchiveRequest",member="KmsKeyArn")`), and the
//! member's `@pattern` can say only that the value is shaped like a KMS key or alias ARN —
//! not that a key exists, nor that this service may encrypt with it. Using a key is a KMS
//! operation, so what decides it is the key's own KEY POLICY, read for the service principal
//! this service calls KMS as ([`SERVICE_PRINCIPAL`]). An archive bound to a key that is not
//! there, or to one whose policy grants this service nothing, could never hold mail it can
//! encrypt and serve back, so the refusal belongs on the call that can still be corrected —
//! `CreateArchive` models `AccessDeniedException` (HTTP 403) for exactly that.
//!
//! KMS is reached the way the framework documents a handler reaching another emulated
//! service ([`smithy_server_runtime::OperationContext::callback`]): the shared in-process
//! client ([`::service_common::kms_key::kms_client`]), so nothing here opens a socket or
//! leaves the host, and the only caller-supplied value that reaches the client's authority is
//! a Region, read as a Region LABEL first ([`::service_common::arn::region_label`]). The
//! decision itself is the tree's one evaluation of a policy document
//! ([`::service_common::iam_authorization`]).
//!
//! Two reads, because the decision needs both: `DescribeKey` RESOLVES the identifier — a key
//! ARN or an alias ARN, both of which the member's pattern admits — to the key's own ARN and
//! reports who manages it, and `GetKeyPolicy` states the document. Both READ, so a refused
//! create leaves KMS exactly as the caller left it.
//!
//! An answer that is not KMS stating the key's facts — no KMS mounted beside this service, a
//! fault, a throttle, a refusal of the call itself, a key of another account, which is not
//! this emulator's to hold — establishes NOTHING about the key, so it refuses nothing
//! ([`ArchiveKey::NotJudged`]): a refusal has to be grounded in what KMS answered.

use ::service_common::iam_authorization::{self as iam, Decision};

/// The ARN namespace this service's resources and its service principal are named in; see
/// [`SERVICE_PRINCIPAL`].
const SERVICE_ARN_NAMESPACE: &str = "ses";

/// The service principal this service calls KMS as when it encrypts an archive's mail.
///
/// Mail Manager is the Amazon SES feature whose resources live in SES's own ARN namespace
/// (`smithy_query(query="service_info")` reports `arnNamespace: ses`, which is what
/// [`super::archive_arn`] builds an archive's ARN in), so a key policy written for an
/// archive names that service.
pub const SERVICE_PRINCIPAL: &str = "ses.amazonaws.com";

/// How this service words the refusal of a `KmsKeyArn` it cannot encrypt an archive under.
///
/// FIXED text for the error class, with no variable part: the value the caller sent is not
/// echoed, and the one sentence covers both ways the key is unusable — it resolves to no key,
/// or its key policy does not let this service use it — because either leaves the archive
/// with nothing to encrypt its mail with.
pub const KEY_NOT_USABLE_MESSAGE: &str = "Unable to use KMS key";

/// The KMS operations an archive needs its key for: mail arriving in the archive is sealed
/// under a data key asked of that key, and reading a message back decrypts it.
const ARCHIVE_KEY_ACTIONS: &[&str] = &["kms:GenerateDataKey", "kms:Decrypt"];

/// The condition key naming the account the service acts for.
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";

/// The condition key naming the resource the service acts for — the archive itself, which is
/// how a key policy is confined to one archive.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The condition key naming the service making the KMS request on the account's behalf,
/// whose value is that service's regional endpoint name.
const VIA_SERVICE_KEY: &str = "kms:ViaService";

/// The condition key naming the account whose credentials the KMS request was made under.
const CALLER_ACCOUNT_KEY: &str = "kms:CallerAccount";

/// What KMS answered about the key a create names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveKey {
    /// Nothing is established about a key, so nothing is refused on it: the request named
    /// none at all (an archive with no customer managed key), the key is one Amazon Web
    /// Services manages — whose policy Amazon Web Services writes for the service that uses
    /// the key, so no statement of the caller's is asked for ("KMS keys in your Amazon Web
    /// Services account are either customer managed or Amazon Web Services managed",
    /// `KeyMetadata$KeyManager`) — or KMS stated no facts about it (see the module docs).
    NotJudged,
    /// KMS answered that no key this account and Region hold resolves from the identifier.
    Absent,
    /// The key policy KMS holds for the key, beside the key's own ARN — which is what the
    /// decision names as its resource, since "in a key policy, the value of the Resource
    /// element is `*`, which means this KMS key" (KMS Developer Guide, *Key policies*).
    Governed { key_arn: String, policy: String },
}

impl ArchiveKey {
    /// Whether an archive must NOT be created under this key.
    ///
    /// `archive_arn` is the ARN of the archive the create would make, which is the value
    /// every KMS request this service makes for that archive carries in [`SOURCE_ARN_KEY`]
    /// and therefore the value a key policy confined to one archive is written against;
    /// `region` is the Region the call was signed for, which is the regional endpoint
    /// [`VIA_SERVICE_KEY`] names.
    #[must_use]
    pub fn refuses_archive(&self, region: &str, archive_arn: &str) -> bool {
        match self {
            Self::NotJudged => false,
            Self::Absent => true,
            Self::Governed { key_arn, policy } => {
                !permits_archive(policy, key_arn, region, archive_arn)
            }
        }
    }
}

/// What KMS holds for the key `kms_key_arn` names, read from this emulator's own KMS.
///
/// `request_region` is the Region the call itself was signed for, which is where a key named
/// without a Region of its own lives; a key named by an ARN is resolved in the Region THAT
/// ARN names, because that is the endpoint holding it.
pub async fn resolve(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    kms_key_arn: Option<&str>,
) -> ArchiveKey {
    let Some(identifier) = kms_key_arn.map(str::trim).filter(|value| !value.is_empty()) else {
        return ArchiveKey::NotJudged;
    };
    let parsed = ::service_common::arn::parse(identifier);
    // A key of another account is not this emulator's to hold, so its absence here shows
    // nothing about it.
    if parsed.is_some_and(|arn| {
        !arn.account.is_empty() && arn.account != ::engine::emulator_account_id()
    }) {
        return ArchiveKey::NotJudged;
    }
    let region = parsed
        .and_then(|arn| ::service_common::arn::region_label(arn.region))
        .unwrap_or(request_region);
    let Some(client) = ::service_common::kms_key::kms_client(ctx, region) else {
        return ArchiveKey::NotJudged;
    };
    let described = match client.describe_key().key_id(identifier).send().await {
        Ok(described) => described,
        Err(failure) => {
            return match failure.as_service_error() {
                // The two answers that say this account and Region hold no key the
                // identifier resolves to: there is no such key, or the identifier is not one
                // KMS can resolve at all.
                Some(
                    ::aws_sdk_kms::operation::describe_key::DescribeKeyError::NotFoundException(_)
                    | ::aws_sdk_kms::operation::describe_key::DescribeKeyError::InvalidArnException(
                        _,
                    ),
                ) => ArchiveKey::Absent,
                _ => not_judged(identifier, &failure),
            }
        }
    };
    let metadata = described.key_metadata();
    if metadata.and_then(::aws_sdk_kms::types::KeyMetadata::key_manager)
        == Some(&::aws_sdk_kms::types::KeyManagerType::Aws)
    {
        return ArchiveKey::NotJudged;
    }
    // The key policy is asked for under the ARN KMS RESOLVED, not the identifier the caller
    // sent: an alias ARN — which this member's pattern admits — names the key only through
    // that resolution.
    let Some(key_arn) = metadata
        .and_then(::aws_sdk_kms::types::KeyMetadata::arn)
        .map(str::to_string)
    else {
        return ArchiveKey::NotJudged;
    };
    match client.get_key_policy().key_id(&key_arn).send().await {
        Ok(answer) => match answer.policy {
            Some(policy) => ArchiveKey::Governed { key_arn, policy },
            None => ArchiveKey::NotJudged,
        },
        // Including KMS not stating a policy for the very ARN it just resolved: that is KMS
        // disagreeing with itself, which is no evidence about the key, and a refusal has to be
        // grounded in what KMS answered.
        Err(failure) => not_judged(identifier, &failure),
    }
}

/// Note that KMS stated no facts about the key, and so that the key is judged against no
/// policy.
///
/// Worth a line to an operator, because a deployment with no KMS to ask is one where this
/// pre-flight does not run, and without it that would be indistinguishable from a key KMS
/// reported as usable. The caller's own value is rendered in its escaped `Debug` form, so an
/// identifier carrying a newline cannot forge a log line.
fn not_judged<E: ::std::error::Error + 'static, R: ::std::fmt::Debug>(
    identifier: &str,
    failure: &::aws_sdk_kms::error::SdkError<E, R>,
) -> ArchiveKey {
    ::tracing::warn!(
        kms_key = ?identifier,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(failure),
        "KMS stated nothing about the key this request names, so the archive's key is judged \
         against no policy"
    );
    ArchiveKey::NotJudged
}

/// Whether `policy` lets this service encrypt the archive `archive_arn`'s mail under the key
/// `key_arn` names.
///
/// Permitted when the document withholds NONE of [`ARCHIVE_KEY_ACTIONS`]: a key policy that
/// states nothing about this service principal grants it nothing, and an explicit deny
/// refuses whatever else the document allows. A statement whose only bar to deciding is a
/// condition key this emulator carries no value for withholds nothing — that is this
/// evaluation lacking a value, not KMS refusing the use — which is why the four keys such a
/// grant is normally scoped by travel with the request as its context.
fn permits_archive(policy: &str, key_arn: &str, region: &str, archive_arn: &str) -> bool {
    let principals = [SERVICE_PRINCIPAL.to_string()];
    let account = ::engine::emulator_account_id().to_string();
    let context = [
        (SOURCE_ACCOUNT_KEY.to_string(), account.clone()),
        (SOURCE_ARN_KEY.to_string(), archive_arn.to_string()),
        (VIA_SERVICE_KEY.to_string(), via_service(region)),
        (CALLER_ACCOUNT_KEY.to_string(), account),
    ];
    let published = [
        SOURCE_ACCOUNT_KEY,
        SOURCE_ARN_KEY,
        VIA_SERVICE_KEY,
        CALLER_ACCOUNT_KEY,
    ];
    let prepared = iam::PreparedDocument::read(policy);
    ARCHIVE_KEY_ACTIONS.iter().all(|action| {
        let request = iam::Request::new(action, key_arn, &principals)
            .with_context(&context)
            .with_published_keys(&published);
        let outcome = iam::resource_outcome_prepared(&prepared, &request);
        match outcome.decision {
            Decision::Allowed => true,
            Decision::Denied => false,
            // Nothing in the document is about this use. That withholds the action, unless
            // the only statements that were about it hinged on a condition key this request
            // carries no value for, in which case nothing was decided at all.
            Decision::NotStated => !outcome.required_context_keys.is_empty(),
        }
    })
}

/// The regional endpoint name a key policy scoped to this service names in
/// [`VIA_SERVICE_KEY`]. Built from this service's ARN namespace, the same one
/// [`super::archive_arn`] uses.
fn via_service(region: &str) -> String {
    format!("{SERVICE_ARN_NAMESPACE}.{region}.amazonaws.com")
}
