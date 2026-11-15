// SPDX-License-Identifier: CC0-1.0

//! Whether the KMS key an encrypted EBS volume is to be created under will serve it.
//!
//! An encrypted volume is not encrypted by Amazon EBS alone: "Amazon EBS … requests a data
//! key from KMS" for the volume and that request is subject to the key itself — a key that
//! is disabled, pending deletion, in a state KMS will not use, or that names no key of the
//! account at all cannot produce the volume's data key, so the volume cannot come into
//! existence under it. The judgement is KMS's and is asked of KMS, over the in-process
//! callback every emulated service reaches another through
//! ([`smithy_server_runtime::OperationContext::callback`]): this crate names no other
//! service's crate, no socket is opened and nothing outside this process is reachable. The
//! endpoint URL below is ADDRESSING for that transport, whose only variable part is the
//! Region the request was signed for; no caller-supplied value becomes a destination, and
//! the caller's `Authorization` header is never forwarded — the credentials are the
//! emulator's own.
//!
//! The ask is the one Amazon EBS makes, `kms:GenerateDataKeyWithoutPlaintext` for a
//! 256-bit symmetric data key: that call, and not a read of the key's metadata, is what a
//! key's STATE refuses, which is the whole point of pointing a volume at a customer managed
//! key.
//!
//! An emulator assembled WITHOUT a KMS can judge nothing: the transport's own refusal says
//! nothing about the key, and withdrawing every encrypted volume where no KMS is mounted
//! would make encrypted volumes unusable without one. Such a call therefore leaves the key
//! as given, exactly as the sibling readings in this tree do
//! ([`::service_com_amazonaws_ebs_ebs`]'s own key reference resolution).

use ::aws_sdk_kms::error::{ProvideErrorMetadata, SdkError};
use ::aws_smithy_runtime_api::client::orchestrator::HttpResponse;

/// The error codes the in-process TRANSPORT answers when this emulator holds no KMS at the
/// endpoint that was called, or when more than one of its services claims that request.
/// Either way the call reached no KMS, so it says nothing about the key.
const TRANSPORT_REFUSALS: [&str; 2] = ["UnknownService", "AmbiguousService"];

/// The data key spec Amazon EBS wraps a volume's data with: a 256-bit symmetric key
/// (`DataKeySpec`'s `AES_256`).
const VOLUME_DATA_KEY_SPEC: ::aws_sdk_kms::types::DataKeySpec =
    ::aws_sdk_kms::types::DataKeySpec::Aes256;

/// What the service that owns the key SAID when it refused to produce a volume's data key.
///
/// Kept whole — the code and the message, as that service worded them — because the two
/// have different readers: the code is the debug record a withdrawn volume leaves, and the
/// message is the CAUSE the failure is reported under to anything watching the volume
/// ([`crate::state::VolumeNotification`]). Neither reaches the caller of the create: EC2
/// answers a create under an unusable key by taking the volume back rather than by relaying
/// KMS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeKeyRefusal {
    /// The error code the key's own service answered with.
    pub code: String,
    /// What that service said about the key, when it said anything at all.
    pub message: Option<String>,
}

impl VolumeKeyRefusal {
    /// WHY the volume could not be created, in the words of the service that owns the key —
    /// which is what names the key and the state it is in ("… is disabled.").
    ///
    /// A refusal that carried no message at all leaves the code as the only thing that was
    /// said, rather than an invented sentence.
    #[must_use]
    pub fn cause(&self) -> String {
        self.message.clone().unwrap_or_else(|| self.code.clone())
    }
}

impl ::std::fmt::Display for VolumeKeyRefusal {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match &self.message {
            Some(message) => write!(formatter, "({}) {message}", self.code),
            None => write!(formatter, "({})", self.code),
        }
    }
}

/// Why the key `kms_key_id` names cannot encrypt a volume, as KMS stated it, or `None` when
/// it will serve — and also when nothing was said about it, for the reason this module's
/// docs give.
pub async fn volume_key_refusal(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kms_key_id: &str,
) -> Option<VolumeKeyRefusal> {
    match kms_client(ctx, region)
        .generate_data_key_without_plaintext()
        .key_id(kms_key_id)
        .key_spec(VOLUME_DATA_KEY_SPEC)
        .send()
        .await
    {
        Ok(_) => None,
        Err(error) => stated_refusal(&error),
    }
}

/// The refusal KMS stated, or `None` for a failure that says nothing about the key: one the
/// transport refused ([`TRANSPORT_REFUSALS`]), and one that reached no answer at all.
fn stated_refusal<E: ProvideErrorMetadata + ::std::error::Error + 'static>(
    error: &SdkError<E, HttpResponse>,
) -> Option<VolumeKeyRefusal> {
    let SdkError::ServiceError(context) = error else {
        ::tracing::debug!(
            error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
            "KMS did not answer whether this volume's key would encrypt it"
        );
        return None;
    };
    let refused = context.err();
    let code = refused.code()?;
    if TRANSPORT_REFUSALS.contains(&code) {
        ::tracing::debug!(code, "no KMS answered this volume's encryption key");
        return None;
    }
    Some(VolumeKeyRefusal {
        code: code.to_string(),
        message: refused.message().map(str::to_string),
    })
}

/// A KMS client over the in-process callback into this same emulator.
///
/// `region` is the Region the create was signed for, so the key is asked about where the
/// volume that names it is created. The transport is in-process, so there is nothing to
/// retry, time out or watch for a stalled stream — each would need a sleep implementation
/// this client is not given — and the credentials the callback hands over are used directly
/// rather than cached.
fn kms_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
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
