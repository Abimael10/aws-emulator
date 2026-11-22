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
//! Two questions are answered here, because both are the same reading of the same identifier:
//! whether a key EXISTS ([`key_is_absent`]) and whether the key an identifier names is one KMS
//! would still perform a cryptographic operation with ([`key_use_refusal`]) — the question a
//! service asks before it stores an encryption configuration it will have to encrypt under.
//!
//! The copies that predate this module — `state::kms_key` in
//! `service-com-amazonaws-pipes-pipes`, `service-com-amazonaws-kinesis-kinesis-20131202` and
//! `service-com-amazonaws-elasticache-amazonelasticachev9`, the key-state reading and its prose
//! in `service-com-amazonaws-lambda-awsgirapiservice`'s `state::execution::environment_key`, and
//! the per-crate `kms_client` builders beside them — are to be MIGRATED onto it, each in the
//! change that owns its service. Nothing new should build the client or re-decide the answer by
//! hand: the client itself is public ([`kms_client`]), so a service with another question for
//! KMS — the authorization a grant records, a key policy — asks it over the same hop.

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
    let Some(client) = endpoint_holding(ctx, request_region, identifier) else {
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

/// The KMS endpoint the key `identifier` names is held by, as a client to ask about it — and
/// `None` where the identifier names no key of this emulator to ask about at all: the empty
/// identifier (which names no key rather than a missing one), a key of another account, which is
/// not this emulator's to hold, and a Region that would name an authority the caller chose.
///
/// The ONE reading of the identifier itself, so every question put to KMS about a caller-supplied
/// key resolves it in the same Region and withholds the same values from the lookup.
fn endpoint_holding(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    identifier: &str,
) -> Option<::aws_sdk_kms::Client> {
    if identifier.is_empty() {
        return None;
    }
    let parsed = crate::arn::parse(identifier);
    if parsed.is_some_and(|arn| {
        !arn.account.is_empty() && arn.account != ::engine::emulator_account_id()
    }) {
        return None;
    }
    let region = parsed
        .and_then(|arn| crate::arn::region_label(arn.region))
        .unwrap_or(request_region);
    kms_client(ctx, region)
}

/// The `KeyState` of a key that can perform a cryptographic operation.
pub const STATE_ENABLED: &str = "Enabled";

/// The `KeyState` values a key admits no cryptographic use in — every value of that enum other
/// than [`STATE_ENABLED`] (source: `smithy_query(query="shape",shape="KeyState")` of the key
/// service's model lists `Creating`, `Enabled`, `Disabled`, `PendingDeletion`, `PendingImport`,
/// `PendingReplicaDeletion`, `Unavailable` and `Updating`).
///
/// Named rather than taken as "anything but `Enabled`", so a state value this emulator does not
/// know is not read as evidence that a key cannot be used.
pub const STATES_THAT_ADMIT_NO_USE: &[&str] = &[
    "Creating",
    "Disabled",
    "PendingDeletion",
    "PendingImport",
    "PendingReplicaDeletion",
    "Unavailable",
    "Updating",
];

/// Why the key an identifier names cannot be used, as KMS states it — and `None` where nothing
/// KMS answered says it cannot.
///
/// A service that takes a customer managed key for a resource it will have to encrypt under asks
/// this before it stores that configuration: the member's `@length` and `@pattern` can say only
/// that a value is shaped like a key identifier, never that KMS would perform an operation with
/// the key it names. Each caller carries the sentence in its OWN modelled error, because the
/// class a refusal falls in is the asking service's; the sentence is KMS's.
///
/// Two answers say the key cannot be used, and both are KMS's own words: a `KeyState` that admits
/// no cryptographic use ([`STATES_THAT_ADMIT_NO_USE`]), reported as [`key_state_sentence`] words
/// it, and KMS refusing to resolve the identifier to a key at all, relayed as the sentence it
/// answered. Everything else establishes nothing and refuses nothing — see the module docs — as
/// does a refusal KMS stated no sentence for, since there is then nothing of KMS's to carry.
///
/// A READ: it asks and judges, and changes nothing in KMS.
pub async fn key_use_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    identifier: &str,
) -> Option<String> {
    let client = endpoint_holding(ctx, request_region, identifier)?;
    let described = match client.describe_key().key_id(identifier).send().await {
        Ok(described) => described,
        Err(error) => return relayed_refusal(identifier, &error),
    };
    let metadata = described.key_metadata();
    // The ARN a refusal names is the one KMS RESOLVED, falling back to the identifier the request
    // named when KMS reported none: either way it is the key the caller asked for.
    let key_arn = metadata
        .and_then(::aws_sdk_kms::types::KeyMetadata::arn)
        .unwrap_or(identifier);
    let state = metadata
        .and_then(::aws_sdk_kms::types::KeyMetadata::key_state)
        .map(::aws_sdk_kms::types::KeyState::as_str)?;
    // A key KMS reports as usable, and a state this emulator does not know, are both answers that
    // say nothing about the key being unusable.
    if !STATES_THAT_ADMIT_NO_USE.contains(&state) {
        return None;
    }
    Some(key_state_sentence(key_arn, state))
}

/// What a `DescribeKey` KMS REFUSED says about the key: the sentence it stated, for the two
/// answers that are about the key rather than about KMS — there is no such key, and the
/// identifier is not one KMS can resolve at all.
fn relayed_refusal(
    identifier: &str,
    error: &::aws_sdk_kms::error::SdkError<
        ::aws_sdk_kms::operation::describe_key::DescribeKeyError,
        impl ::std::fmt::Debug,
    >,
) -> Option<String> {
    let answered = error
        .as_service_error()
        .filter(|refusal| refusal.is_not_found_exception() || refusal.is_invalid_arn_exception());
    if let Some(sentence) = answered.and_then(::aws_sdk_kms::error::ProvideErrorMetadata::message) {
        return Some(sentence.to_string());
    }
    // Visible to an operator: an answer nothing can be read from refuses nothing, so without this
    // line it would be indistinguishable from a key KMS reported as usable. The caller's own value
    // is rendered in its escaped `Debug` form, so an identifier carrying a newline cannot forge a
    // log line.
    ::tracing::warn!(
        kms_key = ?identifier,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
        "KMS stated no fact about the key this request names, so nothing is refused on it"
    );
    None
}

/// How KMS words a refusal caused by the key's own key state: the key's ARN, then the state as
/// prose — its `KeyState` value read with each camel-case word lowercased — and a full stop;
/// "… is disabled." for a key whose owner turned it off, "… is pending deletion." for one whose
/// deletion is scheduled.
///
/// The variable parts are the key the request named and the state KMS reported it in; the rest is
/// fixed for the class, which is why no one state has a sentence of its own here.
#[must_use]
pub fn key_state_sentence(key_arn: &str, key_state: &str) -> String {
    format!("{key_arn} is {}.", key_state_prose(key_state))
}

/// A `KeyState` value as prose: its camel-case words, lowercased and separated by spaces, so a
/// state is worded the same way without a case for it.
fn key_state_prose(key_state: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for letter in key_state.chars() {
        if letter.is_uppercase() && !word.is_empty() {
            words.push(::std::mem::take(&mut word));
        }
        word.extend(letter.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.join(" ")
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

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the sentence a key-state refusal states is the key's ARN, the state as its own
    /// lowercased words, and a full stop — one row per `KeyState` value a key can be reported
    /// in, so no state needs a sentence of its own.
    #[test]
    fn a_key_state_reads_as_its_own_words() {
        let key_arn = "arn:aws:kms:eu-west-1:000000000000:key/a-key";
        // Every value of the key service's `KeyState` enum
        // (`smithy_query(query="shape",shape="KeyState")`), each with the tail of the sentence
        // KMS words a refusal on it with.
        let rows: [(&str, &str); 8] = [
            ("Creating", "is creating."),
            ("Enabled", "is enabled."),
            ("Disabled", "is disabled."),
            ("PendingDeletion", "is pending deletion."),
            ("PendingImport", "is pending import."),
            ("PendingReplicaDeletion", "is pending replica deletion."),
            ("Unavailable", "is unavailable."),
            ("Updating", "is updating."),
        ];
        for (state, tail) in rows {
            let expected = format!("{key_arn} {tail}");
            let sentence = key_state_sentence(key_arn, state);
            assert_eq!(
                sentence, expected,
                "case {state:?}: expected {expected:?}, got {sentence:?}"
            );
        }
    }

    /// The states that admit no use are every `KeyState` value but `Enabled`, so no value of
    /// that enum is silently treated as usable and none is refused twice over.
    #[test]
    fn every_state_but_enabled_admits_no_use() {
        let all = [
            "Creating",
            "Enabled",
            "Disabled",
            "PendingDeletion",
            "PendingImport",
            "PendingReplicaDeletion",
            "Unavailable",
            "Updating",
        ];
        for state in all {
            let admits_no_use = STATES_THAT_ADMIT_NO_USE.contains(&state);
            assert_eq!(
                admits_no_use,
                state != STATE_ENABLED,
                "case {state:?}: expected admits-no-use {}, got {admits_no_use}",
                state != STATE_ENABLED
            );
        }
    }
}
