// SPDX-License-Identifier: CC0-1.0

//! Resolving the KMS key a `PutEncryptionConfig` selector names, through KMS.
//!
//! `EncryptionConfig$KeyId` is "the ID of the KMS key used for encryption"
//! (`smithy_query(query="shape",shape="EncryptionConfig")`), while
//! `PutEncryptionConfig$KeyId` admits three selectors for it — an alias, a key
//! id, or the full ARN of either. Those are not the same thing: an alias points
//! at WHATEVER key it currently targets, so a configuration that reported the
//! alias back would not say which key protects the account's trace data. The
//! configuration therefore reports the KEY the selector resolves to, which is a
//! fact only KMS holds.
//!
//! So the selector is RESOLVED — through the KMS endpoint of this same emulator,
//! the way every emulated service reaches another, over the in-process transport
//! the operation callback hands out
//! ([`smithy_server_runtime::OperationContext::callback`]). No socket is opened,
//! no name is resolved and nothing outside this process is reachable: the
//! endpoint URL below is ADDRESSING for that transport, which routes the call
//! back into this emulator's own KMS service. Its only variable part is the
//! Region the request was signed for — a KMS key is regional, so the key a
//! selector names is the one in that Region — and no caller-supplied value
//! becomes a destination. The caller's `Authorization` header is never
//! forwarded; the credentials are the emulator's own.
//!
//! `DescribeKey` is the read that resolves a key selector and nothing else: it
//! "provides detailed information about a KMS key" and is the call AWS documents
//! for a predefined Amazon Web Services alias, where it is also what ASSOCIATES
//! that alias with the account's Amazon Web Services managed key. A selector KMS
//! does not resolve, and an emulator assembled WITHOUT a KMS, answer nothing
//! about the key, so the configuration keeps the qualified form of the selector
//! the request submitted ([`super::kms_key_arn`]) rather than reporting a key
//! nothing stands behind.
//!
//! What KMS DOES answer about a selector it holds no key for is a definite
//! negative, and that is not a configuration to store at all: X-Ray has to
//! create a grant on the key before it can encrypt the account's trace data
//! under it, and no grant can be created on a key that is not there. Such a
//! request is refused ([`ResolvedKey::NoSuchKey`], [`KEY_GRANT_FAILED_MESSAGE`]),
//! which is the one answer that leaves the account's stored configuration alone
//! rather than pointing it at a key ingestion could never use.

/// What X-Ray answers a request whose `KeyId` names a key KMS holds none for.
///
/// FIXED text: the refusal states the condition that was broken — the grant
/// X-Ray has to create on the key could not be created, because KMS reported no
/// such key — and interpolates nothing from the request.
pub const KEY_GRANT_FAILED_MESSAGE: &str =
    "Failed to create grant for customer master key. KMSKeyNotFound";

/// What KMS answered about one key selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedKey {
    /// KMS resolved the selector to a key, and reported it under this ARN.
    Key(String),
    /// KMS reported that this account and Region hold no key the selector
    /// resolves to.
    NoSuchKey,
    /// KMS said nothing about the key itself. Not a negative, so not a refusal.
    Unknown,
}

/// What KMS says about the key `key_id` selects at `region`'s endpoint.
///
/// The ARN a resolved key is reported under is KMS's own `KeyMetadata$Arn` — the
/// `key/<key id>` form, whichever selector was submitted — so a caller comparing
/// the configuration against `kms:DescribeKey`, a key policy or a key's audit
/// records is comparing the same identifier.
///
/// A definite negative is only KMS's own "no such key" answer, with the client
/// status it came with; see the module docs for why everything else is
/// [`ResolvedKey::Unknown`].
pub async fn resolved_key(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    key_id: &str,
) -> ResolvedKey {
    let error = match kms_client(ctx, region)
        .describe_key()
        .key_id(key_id)
        .send()
        .await
    {
        Ok(described) => {
            return match described.key_metadata.and_then(|metadata| metadata.arn) {
                Some(arn) => ResolvedKey::Key(arn),
                // A key KMS reported without an ARN is still the key the request
                // named, but nothing said WHICH key that is.
                None => ResolvedKey::Unknown,
            };
        }
        Err(error) => error,
    };
    let about_the_key = error.as_service_error().is_some_and(
        ::aws_sdk_kms::operation::describe_key::DescribeKeyError::is_not_found_exception,
    );
    // A 5xx is about KMS, not about the key, whatever code travels with it.
    let client_error = error
        .raw_response()
        .is_some_and(|response| response.status().is_client_error());
    if about_the_key && client_error {
        return ResolvedKey::NoSuchKey;
    }
    // A selector KMS did not answer about, a refusal of the call, a KMS this
    // emulator was assembled without: none of them says what key the selector
    // names, which is what the caller is left with. Visible to an operator,
    // because an answer nothing can be read from refuses nothing.
    ::tracing::debug!(
        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
        "KMS did not resolve the key the encryption configuration names"
    );
    ResolvedKey::Unknown
}

/// A KMS client over the in-process callback into this same emulator, addressed
/// to `region`'s endpoint.
///
/// The transport is in-process, so there is nothing to retry, time out or watch
/// for a stalled stream — each would need a sleep implementation this client is
/// not given — and the credentials the callback hands over are used directly
/// rather than cached.
fn kms_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_kms::Client {
    let config = ::aws_sdk_kms::Config::builder()
        .behavior_version(::aws_sdk_kms::config::BehaviorVersion::latest())
        .region(::aws_sdk_kms::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://kms.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        .retry_config(::aws_sdk_kms::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_kms::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_kms::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_kms::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_kms::Client::from_conf(config)
}
