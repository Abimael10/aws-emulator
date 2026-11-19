// SPDX-License-Identifier: CC0-1.0

//! The first-party exchange of a workload identity token for a resource's API key: which
//! credential provider a `resourceCredentialProviderName` names, and the key that provider
//! keeps.
//!
//! An API key credential provider belongs to this account's TOKEN VAULT, which the AgentCore
//! CONTROL plane owns — this plane creates none, and the only identifier a request carries
//! for one is the provider's name — so the name is resolved against the component that OWNS
//! the provider ([`api_key_secret_of_provider`]) rather than against any record of this
//! plane, the same idiom the browser and payments planes use for a resource of that plane
//! ([`super::browser::browser_availability`], [`super::payment_manager_resolved`]). A name
//! that plane holds no provider under is refused with one sentence
//! ([`RESOURCE_CREDENTIAL_PROVIDER_NOT_FOUND_MESSAGE`]).
//!
//! # Where the key itself is
//!
//! Neither plane holds it: a provider carries the Secrets Manager secret its key lives in —
//! the secret the create put the caller's key into for a provider whose secret the service
//! manages, or the caller's own secret an external provider references — so the key is read
//! from the service that HOLDS it ([`api_key_held_for`], `GetSecretValue` over the
//! in-process callback into this same emulator). That is what makes the key this call
//! answers the key the create really stored: nothing is recomputed here, and a provider
//! whose secret nothing holds yields no key and therefore no success.

use ::service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as control_plane;

/// How this plane words the refusal of a call naming a credential provider the account does
/// not hold: fixed text that carries nothing of the request — the provider name is NOT
/// interpolated, because the remedy does not depend on which name was asked for (the caller
/// creates the provider or corrects the name).
///
/// One sentence for every resource-credential call of this plane, so the same condition
/// cannot be worded two ways. The CONTROL plane's own reads of a provider word their 404
/// their own way ("ApiKeyCredentialProvider not found for <name>"): that is a different
/// API's sentence for a different request, and neither is derived from the other.
pub const RESOURCE_CREDENTIAL_PROVIDER_NOT_FOUND_MESSAGE: &str =
    "Resource Credential Provider not found";

/// The resource a call addressing the API key credential provider `name` acts on: the
/// provider as the plane that owns the token vault addresses it, in this account's default
/// vault.
///
/// Built from the REQUEST alone — the name it carried and the region it was signed for — so
/// the authorization decision taken on it discloses nothing about whether that provider
/// exists.
pub fn api_key_credential_provider_resource(region: &str, name: &str) -> String {
    control_plane::state::api_key_credential_provider_arn(
        region,
        control_plane::state::DEFAULT_TOKEN_VAULT_ID,
        name,
    )
}

/// Where one provider's API key lives, as the plane that owns the provider recorded it: the
/// secret's own ARN and the JSON key the value is extracted under, when the caller's own
/// `SecretReference` named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeySecretLocation {
    pub secret_arn: String,
    pub json_key: Option<String>,
}

impl ApiKeySecretLocation {
    /// The JSON key this secret's document carries the API key under: the one the provider
    /// reports, else the key a secret the SERVICE manages is written with, whose layout the
    /// caller never named ([`control_plane::state::MANAGED_SECRET_JSON_KEY`]).
    #[must_use]
    pub fn effective_json_key(&self) -> &str {
        self.json_key
            .as_deref()
            .unwrap_or(control_plane::state::MANAGED_SECRET_JSON_KEY)
    }
}

/// Why the API key of the provider a request named cannot be answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceApiKeyProblem {
    /// The account holds no API key credential provider of that name, so there is no key to
    /// report. The modelled `ResourceNotFoundException`.
    NoSuchProvider,
    /// The store of the plane that owns credential providers could not be reached, which is
    /// this emulator's fault and not the request's.
    OwnerStoreUnavailable,
    /// The provider exists, but the secret its key lives in could not be read. Carries what
    /// came back, for the trace; a handler answers a modelled server error, because the
    /// provider the request named is there.
    SecretUnreadable(String),
}

/// The secret the API key credential provider `name` keeps its key in, from the store of the
/// plane that owns the account's token vault.
///
/// A READ of that plane's store: it answers exactly what `GetApiKeyCredentialProvider` would
/// answer at this instant, with no transition left to materialise, so a provider that plane
/// has deleted stops resolving here at the same moment.
pub fn api_key_secret_of_provider<Eng: Send + Sync + 'static>(
    engine: &Eng,
    name: &str,
) -> Result<ApiKeySecretLocation, ResourceApiKeyProblem> {
    let Some(control) = control_plane::state::service_state(engine) else {
        return Err(ResourceApiKeyProblem::OwnerStoreUnavailable);
    };
    let store = control.lock();
    let held = store
        .api_key_credential_providers
        .get(name)
        .ok_or(ResourceApiKeyProblem::NoSuchProvider)?;
    Ok(ApiKeySecretLocation {
        secret_arn: held.secret.secret_arn.clone(),
        json_key: held.secret.json_key.clone(),
    })
}

/// The ARN namespace of the service holding the secret a credential provider's key lives in
/// — the vendor field of `SecretArn`, `arn:…:secretsmanager:…:secret:…` (source:
/// smithy_query query="shape" shape="Secret" of the control plane's model).
const SECRETS_MANAGER_VENDOR: &str = "secretsmanager";

/// The API key a provider keeps, read from the service that holds the secret it lives in.
///
/// `request_region` addresses that service when the secret's own ARN names no region; the
/// ARN's region is preferred, because that is the Region holding the secret.
///
/// Call it with NO state guard held — it awaits.
pub async fn api_key_held_for(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    secret: &ApiKeySecretLocation,
) -> Result<String, ResourceApiKeyProblem> {
    let region = holding_region(&secret.secret_arn, request_region);
    let answer = secrets_manager_client(ctx.callback(), region)
        .get_secret_value()
        .secret_id(&secret.secret_arn)
        .send()
        .await
        .map_err(|error| {
            ResourceApiKeyProblem::SecretUnreadable(
                ::aws_smithy_types::error::display::DisplayErrorContext(&error).to_string(),
            )
        })?;
    let Some(held) = answer.secret_string() else {
        return Err(ResourceApiKeyProblem::SecretUnreadable(
            "the secret holding the API key carries no string value".to_string(),
        ));
    };
    api_key_in_document(held, secret.effective_json_key())
}

/// The Region the service holding `secret_arn` is addressed in: the one the secret's own ARN
/// names, because that is the Region holding it, else the Region the request was signed for.
///
/// A Region read for this is not just data — it ends up in the authority of the endpoint the
/// in-process call is addressed to — so an ARN field outside the Region grammar names no
/// Region at all ([`::service_common::arn::region_of`]).
fn holding_region<'value>(secret_arn: &'value str, request_region: &'value str) -> &'value str {
    ::service_common::arn::region_of(secret_arn, SECRETS_MANAGER_VENDOR).unwrap_or(request_region)
}

/// The API key a secret's value carries: a secret's value is a JSON document, and the key
/// sits in it under the JSON key the provider is read with.
///
/// A value that is no such document, or a document stating no such key, carries no API key:
/// the output member is `@required`, so there is nothing to answer rather than a key to
/// invent.
fn api_key_in_document(value: &str, json_key: &str) -> Result<String, ResourceApiKeyProblem> {
    let document: ::serde_json::Value = ::serde_json::from_str(value).map_err(|_defect| {
        ResourceApiKeyProblem::SecretUnreadable(
            "the secret holding the API key is not a JSON document".to_string(),
        )
    })?;
    match document.get(json_key) {
        Some(::serde_json::Value::String(text)) if !text.is_empty() => Ok(text.clone()),
        _ => Err(ResourceApiKeyProblem::SecretUnreadable(format!(
            "the secret holding the API key states no value under its JSON key {json_key}"
        ))),
    }
}

/// A Secrets Manager client over the in-process callback into this same emulator.
///
/// The callback is the framework's seam for calling another emulated service: it routes in
/// process, so no socket is opened and no real account is reached, and only secrets this
/// process holds resolve. The configuration is the one every callback client of this
/// workspace uses — nothing to retry, time out or watch for a stalled stream, because the
/// transport is in process, and the identity is handed over directly.
fn secrets_manager_client(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_secretsmanager::Client {
    let config = ::aws_sdk_secretsmanager::Config::builder()
        .behavior_version(::aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(::aws_sdk_secretsmanager::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://secretsmanager.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_secretsmanager::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_secretsmanager::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_secretsmanager::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_secretsmanager::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_secretsmanager::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the key is the value the secret's document states under the provider's JSON key;
    /// a document that states none carries no key.
    #[test]
    fn the_key_is_what_the_secrets_document_states_under_its_json_key() {
        let managed = ApiKeySecretLocation {
            secret_arn: "arn:aws:secretsmanager:us-east-1:000000000000:secret:managed-AbCdEf"
                .to_string(),
            json_key: None,
        };
        let external = ApiKeySecretLocation {
            secret_arn: "arn:aws:secretsmanager:eu-west-1:000000000000:secret:partner-AbCdEf"
                .to_string(),
            json_key: Some("partnerKey".to_string()),
        };
        assert_eq!(
            managed.effective_json_key(),
            "apiKey",
            "case \"a secret the service manages\": expected the layout the service writes"
        );
        assert_eq!(
            external.effective_json_key(),
            "partnerKey",
            "case \"a secret the caller references\": expected the caller's own JSON key"
        );

        let cases: Vec<(&str, &str, &str, Result<String, ()>)> = vec![
            (
                "the managed layout",
                r#"{"apiKey":"sk-live-0001"}"#,
                managed.effective_json_key(),
                Ok("sk-live-0001".to_string()),
            ),
            (
                "the caller's own key beside other fields",
                r#"{"other":"x","partnerKey":"pk-0002"}"#,
                external.effective_json_key(),
                Ok("pk-0002".to_string()),
            ),
            (
                "a document stating no such key",
                r#"{"somethingElse":"x"}"#,
                managed.effective_json_key(),
                Err(()),
            ),
            (
                "an empty value under the key",
                r#"{"apiKey":""}"#,
                managed.effective_json_key(),
                Err(()),
            ),
            (
                "a value that is no JSON document",
                "not-json",
                managed.effective_json_key(),
                Err(()),
            ),
            (
                "a JSON value that is not an object",
                "[1,2,3]",
                managed.effective_json_key(),
                Err(()),
            ),
        ];

        for (case, value, json_key, expected) in cases {
            let read = api_key_in_document(value, json_key);
            match &expected {
                Ok(key) => assert_eq!(
                    read.as_ref().ok(),
                    Some(key),
                    "case {case:?}: expected {key:?}, got {read:?}"
                ),
                Err(()) => assert!(
                    read.is_err(),
                    "case {case:?}: expected no key at all, got {read:?}"
                ),
            }
        }
    }

    /// RULE: the service holding a provider's secret is addressed in the Region that secret's
    /// own ARN names, and in the request's Region when the ARN names none.
    #[test]
    fn the_secrets_own_region_is_where_it_is_read_from() {
        for (case, secret_arn, expected) in [
            (
                "an ARN naming its Region",
                "arn:aws:secretsmanager:eu-west-1:000000000000:secret:partner-AbCdEf",
                "eu-west-1",
            ),
            (
                "an ARN of another partition",
                "arn:aws-us-gov:secretsmanager:us-gov-west-1:000000000000:secret:partner-AbCdEf",
                "us-gov-west-1",
            ),
            (
                "an ARN whose Region field is empty",
                "arn:aws:secretsmanager::000000000000:secret:partner-AbCdEf",
                "us-east-1",
            ),
            (
                "an ARN of another vendor",
                "arn:aws:kms:eu-west-1:000000000000:key/abcd",
                "us-east-1",
            ),
            ("a value that is no ARN at all", "partner-secret", "us-east-1"),
        ] {
            let read = holding_region(secret_arn, "us-east-1");
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the provider a call addresses is the one the owning plane holds under that
    /// name, in this account's default token vault.
    #[test]
    fn the_addressed_resource_is_the_providers_own_arn() {
        for (case, region, name) in [
            ("a us region", "us-east-1", "partner-api"),
            ("another region", "eu-west-1", "partner_api-2"),
        ] {
            let addressed = api_key_credential_provider_resource(region, name);
            let owners = control_plane::state::api_key_credential_provider_arn(
                region,
                control_plane::state::DEFAULT_TOKEN_VAULT_ID,
                name,
            );
            assert_eq!(
                addressed, owners,
                "case {case:?}: expected the ARN the owning plane mints, got {addressed:?}"
            );
            assert!(
                addressed.contains(&format!(":{region}:")) && addressed.ends_with(name),
                "case {case:?}: expected the region and the name the request carried, got \
                 {addressed:?}"
            );
        }
    }
}
