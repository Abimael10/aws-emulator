// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of a caller-supplied AWS KMS key identifier: whether this account and
//! Region hold a key it resolves to.
//!
//! Several emulated services take a key identifier in a request — Aurora DSQL's
//! `kmsEncryptionKey`, EventBridge Pipes' `KmsKeyIdentifier`, Amazon Kinesis's `KeyId`,
//! ElastiCache's `KmsKeyId` — and each has to decide the same thing before it stores an
//! encryption configuration: the member's `@length` and `@pattern` cannot say whether a key
//! EXISTS, and the identifier may be an ARN, a key id, an alias or an alias ARN, so the
//! question is KMS's to answer and the answer has to mean the same thing wherever it is
//! asked.
//!
//! KMS is reached the way the framework documents a handler reaching another emulated service
//! ([`smithy_server_runtime::OperationContext::callback`]): the AWS SDK driven over the
//! emulator's own in-process transport, so no socket is opened, no name is resolved and
//! nothing outside this process is reachable from it. The endpoint URL is addressing for that
//! transport rather than a host this process connects to, and the only caller-supplied value
//! that reaches its authority is a Region, read as a Region LABEL first
//! ([`crate::arn::region_label`]).
//!
//! The copies that predate this module — `state::kms_key` in
//! `service-com-amazonaws-pipes-pipes`, `service-com-amazonaws-kinesis-kinesis-20131202` and
//! `service-com-amazonaws-elasticache-amazonelasticachev9`, and the per-crate `kms_client`
//! builders beside them — are to be MIGRATED onto it, each in the change that owns its
//! service. Nothing new should build the client or re-decide the answer by hand: the client
//! itself is public ([`kms_client`]), so a service with another question for KMS — the
//! authorization a grant records, a key policy — asks it over the same hop.

/// Whether KMS ANSWERED that this account and Region hold no key the identifier `identifier`
/// resolves to.
///
/// `request_region` is the Region the call was signed for, which is where a key named by an id
/// or an alias — an identifier carrying no Region of its own — lives; a key named by an ARN is
/// resolved in the Region THAT ARN names, because that is the endpoint holding it.
///
/// `false` — establish nothing, refuse nothing — for everything that is NOT KMS stating the
/// key is not there: the empty identifier (which names no key rather than a missing one), a key
/// of another account, which is not this emulator's to hold, an answer that is about KMS rather
/// than about the key (a fault, a throttle, a refusal of the call itself), an answer this
/// emulator could not read, and an emulator with no KMS mounted to ask. A service refuses a
/// write on this only where it is grounded in what KMS said.
pub async fn key_is_absent(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    identifier: &str,
) -> bool {
    if identifier.is_empty() {
        return false;
    }
    let parsed = crate::arn::parse(identifier);
    if parsed.is_some_and(|arn| {
        !arn.account.is_empty() && arn.account != ::engine::emulator_account_id()
    }) {
        return false;
    }
    let region = parsed
        .and_then(|arn| crate::arn::region_label(arn.region))
        .unwrap_or(request_region);
    let Some(client) = kms_client(ctx, region) else {
        return false;
    };
    let error = match client.describe_key().key_id(identifier).send().await {
        Ok(_) => return false,
        Err(error) => error,
    };
    // `NotFoundException` is the ONE answer that says this account and Region hold no key the
    // identifier resolves to; a 5xx is about KMS rather than about the key, whatever code
    // travels with it.
    let about_the_key = error.as_service_error().is_some_and(
        ::aws_sdk_kms::operation::describe_key::DescribeKeyError::is_not_found_exception,
    );
    let client_error = error
        .raw_response()
        .is_some_and(|response| response.status().is_client_error());
    if about_the_key && client_error {
        return true;
    }
    // Visible to an operator: an answer nothing can be read from establishes nothing, so
    // without this line it would be indistinguishable from a key KMS resolved. The caller's own
    // value is rendered in its escaped `Debug` form, so an identifier carrying a newline cannot
    // forge a log line.
    ::tracing::warn!(
        kms_key = ?identifier,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
        "KMS gave no answer about this key, so nothing is established about its existence"
    );
    false
}

/// The KMS client every question about a key, and every authorization taken on one, is put
/// through — for the Region the key lives in.
///
/// That Region is CALLER-SUPPLIED text and here it ends up in an endpoint URL's AUTHORITY, so
/// it is read as a Region LABEL rather than interpolated as given: `None` for a value that
/// would name an authority the caller chose, in which case no lookup is made at all.
///
/// The endpoint URL names KMS's public host so the request is signed and routed as KMS's, but
/// the TRANSPORT is the emulator's own in-process connector. Being in-process, the client
/// disables what a network client needs and this one cannot have: retries, timeouts, stalled-
/// stream protection and identity caching each need a sleep implementation the callback
/// transport is not given. The credentials the callback hands over are the emulator's own: the
/// caller's `Authorization` header is never forwarded.
#[must_use]
pub fn kms_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_kms::Client> {
    let region = crate::arn::region_label(region)?;
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
