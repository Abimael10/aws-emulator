// SPDX-License-Identifier: CC0-1.0

//! Whether the customer managed key a write would bind a pipe to EXISTS — the question
//! every operation that accepts a `KmsKeyIdentifier` asks before it writes anything.
//!
//! `KmsKeyIdentifier` is constrained only by length and pattern ("The identifier can be the
//! key Amazon Resource Name (ARN), KeyId, key alias, or key alias ARN",
//! `smithy_query(query="doc",shape="CreatePipeRequest",member="KmsKeyIdentifier")`), so
//! whether the identifier names a key at all is not a boundary check: it is resolved against
//! KMS, the component that OWNS keys, and an identifier KMS holds no key for is refused with
//! the modelled `ValidationException` before any pipe record is written. A pipe bound to a
//! key KMS does not know could never encrypt anything, and the refusal belongs on the call
//! that can still be corrected.
//!
//! KMS is reached the way the framework documents a handler reaching another emulated
//! service ([`smithy_server_runtime::OperationContext::callback`]): an AWS SDK client over
//! the in-process callback's HTTP client and credentials, so nothing here opens a socket or
//! leaves the host, and the only caller-supplied value that reaches the client's authority is
//! a Region, read as a Region LABEL first ([`::service_common::arn::region_label`]).
//!
//! Nothing is refused on evidence this emulator does not have, which is the same bar
//! [`super::bring_up`] holds a pipe's source and role to: an answer that is not KMS stating
//! that the identifier resolves to no key — no KMS beside this service, a fault, a throttle,
//! a refusal about the call, a key of another account, which is not this emulator's to hold —
//! shows nothing about the key, and the write goes through.

use crate::ops::types::ValidationException;

/// The sentence AWS refuses a pipe write naming a key KMS holds none for with: FIXED text
/// for the error class, with no variable part — the identifier the caller sent is not echoed.
pub const KEY_NOT_FOUND_MESSAGE: &str = "The provided KMS Key does not exist.";

/// The `ValidationException` (HTTP 400) a write naming a key that does not exist is refused
/// with.
///
/// No `fieldList`: this is not a member whose value failed a constraint — the value is well
/// formed and names nothing — so the exception carries only the sentence for the class.
#[must_use]
pub fn key_not_found() -> ValidationException {
    ValidationException {
        message: Some(KEY_NOT_FOUND_MESSAGE.to_string()),
        field_list: None,
    }
}

/// Whether KMS answers that the account holds no key the identifier `identifier` resolves
/// to, in which case a write that would bind a pipe to it is refused with
/// [`key_not_found`].
///
/// `request_region` is the Region the call itself was signed for, which is where a key named
/// by an id or an alias — an identifier carrying no Region of its own — lives; a key named by
/// an ARN is resolved in the Region THAT ARN names, because that is where KMS holds it.
///
/// `false` for everything that is not KMS stating the key is not there: the empty identifier
/// (which is how a caller says "no customer managed key" rather than naming one), a key of
/// another account, and any answer that establishes nothing about the key. See the module
/// docs.
pub async fn is_absent(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    identifier: &str,
) -> bool {
    if identifier.is_empty() {
        return false;
    }
    let parsed = ::service_common::arn::parse(identifier);
    // A key of another account is not this emulator's to hold, so its absence here shows
    // nothing about it.
    if parsed.is_some_and(|arn| !arn.account.is_empty() && arn.account != ::engine::emulator_account_id())
    {
        return false;
    }
    let region = parsed
        .and_then(|arn| ::service_common::arn::region_label(arn.region))
        .unwrap_or(request_region);
    let Some(client) = kms_client(ctx, region) else {
        return false;
    };
    let error = match client.describe_key().key_id(identifier).send().await {
        Ok(_) => return false,
        Err(error) => error,
    };
    // `NotFoundException` is the ONE answer that says this account and Region hold no key
    // the identifier resolves to; a 5xx is about KMS rather than about the key, whatever
    // code travels with it.
    let about_the_key = error.as_service_error().is_some_and(
        ::aws_sdk_kms::operation::describe_key::DescribeKeyError::is_not_found_exception,
    );
    let client_error = error
        .raw_response()
        .is_some_and(|response| response.status().is_client_error());
    if about_the_key && client_error {
        return true;
    }
    // Visible to an operator: an answer nothing can be read from refuses nothing, so without
    // this line it would be indistinguishable from a key KMS resolved. The caller's own value
    // is rendered in its escaped `Debug` form, so an identifier carrying a newline cannot
    // forge a log line.
    ::tracing::warn!(
        kms_key = ?identifier,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
        "KMS gave no answer about this key, so nothing is refused on its existence"
    );
    false
}

/// The KMS client every question about a key is put through, for the Region the key lives
/// in.
///
/// That Region is CALLER-SUPPLIED text and here it ends up in an endpoint URL's AUTHORITY,
/// so it is read as a Region LABEL rather than interpolated as given: `None` for a value that
/// would name an authority the caller chose, in which case no lookup is made at all and
/// nothing is refused on it.
///
/// The endpoint url names KMS's public host so the request is signed and routed as KMS's, but
/// the TRANSPORT is this emulator's own in-process connector, so no socket is opened and no
/// name is resolved. Being in-process, the client disables what a network client needs and
/// this one cannot have: retries, timeouts, stalled-stream protection and identity caching
/// each need a sleep implementation the callback transport is not given.
fn kms_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_kms::Client> {
    let region = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_kms::Config::builder()
        .behavior_version(::aws_sdk_kms::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_kms::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://kms.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_kms::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_kms::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_kms::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_kms::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_kms::Client::from_conf(config))
}
