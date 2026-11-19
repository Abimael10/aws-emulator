// SPDX-License-Identifier: CC0-1.0

//! The API key credential providers of this account's token vault: the record a
//! create persists, the ARNs it mints, and the Secrets Manager secret the API key
//! itself lives in.
//!
//! One concern, kept beside the token vault rather than inside one `ops/` file:
//! the provider's own ARN, the secret its key is stored under and the response
//! envelopes are shared by the create and by every later read, so two operations
//! cannot report one provider two ways.
//!
//! ## Where the API key lives
//!
//! The key is NOT held in this service's own store. A provider carries a
//! `MANAGED` or an `EXTERNAL` secret source:
//!
//! * `MANAGED` — "the secret is managed by the service" — so the create itself
//!   puts the caller's key into a secret in Secrets Manager, under a name derived
//!   from the vault and the provider ([`managed_api_key_secret_name`]), as the
//!   JSON document a secret's value is ([`managed_api_key_secret_value`]), and
//!   reports that secret's ARN — and no JSON key, the document's layout being
//!   the service's own rather than a secret reference the caller supplied. The
//!   ARN is part of the create's own answer, so it names a secret Secrets
//!   Manager really holds: a client that reads the provider and then reads the
//!   secret finds the key there;
//! * `EXTERNAL` — "you manage the secret yourself in Amazon Web Services Secrets
//!   Manager" — so the create READS the secret the request's `apiKeySecretConfig`
//!   names and reports the ARN Secrets Manager answered with, together with the
//!   `jsonKey` the caller extracts the key with. Nothing is written into it: the
//!   key is the caller's. The read is `GetSecretValue`, which DECRYPTS: a secret
//!   whose KMS key will not decrypt is the modelled `DecryptionFailure`, because
//!   a key the provider cannot read is of no use to it.
//!
//! Secrets Manager is reached the way this crate reaches every other emulated
//! service — through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]) — so this crate names
//! no other service crate.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::{
    CreateApiKeyCredentialProviderOutput, GetApiKeyCredentialProviderOutput,
    ResourceNotFoundException, Secret, SecretReference, UpdateApiKeyCredentialProviderOutput,
    ValidationException,
};

use super::{field_validation_failed, timestamp, DEFAULT_REGION};

/// The ARN path segment an API key credential provider of a token vault is
/// addressed under: `ApiKeyCredentialProviderArnType` nests the provider under
/// the vault holding it, `token-vault/<vaultId>/apikeycredentialprovider/<name>`
/// (`smithy_query shape=ApiKeyCredentialProviderArnType`).
pub const API_KEY_CREDENTIAL_PROVIDER_ARN_TYPE: &str = "apikeycredentialprovider";

/// The resource noun [`super::resource_not_found`] names for a SECRET this
/// service could not resolve for a provider.
pub const RESOURCE_SECRET: &str = "secret";

/// How the READ route words the 404 for an API key credential provider this
/// account does not hold: the resource type and the name the request addressed
/// it by ("ApiKeyCredentialProvider not found for <name>").
///
/// The DELETE route answers no 404 for a merely absent name — its empty 204
/// success is satisfied by the provider's absence — so this sentence belongs to
/// the routes that have to resolve the provider to answer at all.
///
/// The credential-provider routes do NOT take the shared
/// [`super::resource_not_found`] sentence — they name the resource type and echo
/// the `name` the caller asked for, which is the identifier those routes address
/// a provider by. Kept here rather than in one `ops/` file so a route that words
/// it this way cannot drift from another; the UPDATE
/// route words it differently
/// ([`api_key_credential_provider_not_found_on_update`]).
pub fn api_key_credential_provider_not_found(name: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("ApiKeyCredentialProvider not found for {name}")),
    }
}

/// How the UPDATE route words the same miss: the resource type spelled as that
/// route spells it, and that it is not there.
///
/// Entirely fixed text — nothing of the request is echoed, because the remedy
/// does not depend on which name was asked for: the caller creates the provider
/// or corrects the name. The routes of this service do not share one sentence
/// for a missing resource — the read and the delete above echo the `name` they
/// could not resolve, while this one states the resource type alone, as the
/// payment credential provider's routes do
/// ([`super::PAYMENT_CREDENTIAL_PROVIDER_NOT_FOUND_MESSAGE`]) — so each route's
/// wording lives beside the resource it is about rather than being guessed from
/// another's.
pub const API_KEY_CREDENTIAL_PROVIDER_NOT_FOUND_ON_UPDATE_MESSAGE: &str =
    "ApiKey CredentialProvider not found";

/// The modelled not-found error the UPDATE route answers with for an API key
/// credential provider this account does not hold.
pub fn api_key_credential_provider_not_found_on_update() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(API_KEY_CREDENTIAL_PROVIDER_NOT_FOUND_ON_UPDATE_MESSAGE.to_string()),
    }
}

/// The sentence a credential-provider create is refused with when the `name` it
/// carries is one this account already holds a provider under ("Credential
/// provider with name: <name> already exists").
///
/// A `CredentialProviderName` "must be unique within your account"
/// (`smithy_query doc shape=CreateApiKeyCredentialProvider member=name`), and
/// the token vault reports a repeated one as a failure of the REQUEST — the 400
/// `ValidationException` — rather than as a conflict with the stored provider:
/// nothing about the held provider changes, the caller's input is what cannot be
/// served. The refusal carries this sentence and nothing besides, which is why
/// the handlers answer it through [`crate::scaffolded::Scaffolded::validation`]
/// rather than through the modelled shape, whose `@required` `reason` would add
/// a member the identity plane does not report.
///
/// The text around the name is fixed; the name is the one the request carried.
/// Shared by every credential-provider create of the vault — the API key, the
/// OAuth2 and the payment plane all address a provider by that one name — so one
/// taken name cannot be refused in three different sentences.
pub fn credential_provider_name_taken(name: &str) -> String {
    format!("Credential provider with name: {name} already exists")
}

/// `SecretSourceType` values. The enum's full value list is `MANAGED | EXTERNAL`
/// (`smithy_query shape=SecretSourceType`).
pub const SECRET_SOURCE_MANAGED: &str = "MANAGED";
pub const SECRET_SOURCE_EXTERNAL: &str = "EXTERNAL";

/// The request members whose rules the model states in prose, so the framework's
/// constraint walk cannot refuse them and a handler must.
pub const API_KEY_MEMBER: &str = "apiKey";
pub const API_KEY_SECRET_CONFIG_MEMBER: &str = "apiKeySecretConfig";

/// How many API key credential providers this emulator will hold.
///
/// `CreateApiKeyCredentialProvider` models `ServiceQuotaExceededException` (HTTP
/// 402) for exceeding the real service's quota; this number is not a fidelity
/// claim about that quota but the emulator's own ceiling, set generously so no
/// realistic workflow meets it, so a caller looping on create cannot grow this
/// process's memory without bound.
pub const MAX_API_KEY_CREDENTIAL_PROVIDERS: usize = 1_000;

/// The prefix the service holds its OWN managed secrets under in the caller's
/// Secrets Manager, so a managed secret is recognisable as this service's and
/// cannot collide with a name a client chose for a secret of its own.
pub const MANAGED_SECRET_PREFIX: &str = "bedrock-agentcore-identity";

/// The segment of a managed secret's name that says what is in it.
const MANAGED_SECRET_KIND: &str = "apikey";

/// The JSON key the service's own managed secret holds the API key under: a
/// secret's value is a JSON document, so the key sits in it under a name of its
/// own. That name is the layout of a secret this SERVICE manages rather than a
/// secret reference the caller supplied, so it is NOT what the provider reports
/// as its `apiKeySecretJsonKey` — see
/// [`StoredApiKeySecret::json_key`].
pub const MANAGED_SECRET_JSON_KEY: &str = "apiKey";

/// The value the secret holding a `MANAGED` provider's key is stored with: the
/// JSON document carrying the caller's key under [`MANAGED_SECRET_JSON_KEY`].
pub fn managed_api_key_secret_value(api_key: &str) -> String {
    ::serde_json::json!({ MANAGED_SECRET_JSON_KEY: api_key }).to_string()
}

/// The name the secret holding a `MANAGED` provider's API key is created under:
/// the service's own prefix, the token vault the provider belongs to, and the
/// provider's name, so the name is a function of the resource rather than of
/// anything else the caller sent.
///
/// Every character of it is one Secrets Manager admits in a secret name (ASCII
/// letters, numbers and `/_+=.@-`, which `CreateSecret` refuses anything outside
/// of), and a `CredentialProviderName` is `^[a-zA-Z0-9\-_]+$` of at most 128
/// characters (`smithy_query shape=CredentialProviderName`), so the whole name
/// stays well inside that operation's 1..512 length.
pub fn managed_api_key_secret_name(token_vault_id: &str, provider_name: &str) -> String {
    format!("{MANAGED_SECRET_PREFIX}/{token_vault_id}/{MANAGED_SECRET_KIND}/{provider_name}")
}

/// `arn:aws:bedrock-agentcore:{region}:{account}:token-vault/{tokenVaultId}/apikeycredentialprovider/{name}`
/// — the provider nested under the token vault that holds it, ending in the name
/// the caller chose, in the ARN namespace of the service that holds the vault
/// ([`super::ARN_SERVICE`]): the namespace every other resource of this service
/// is addressed in, and the one a consumer of a credential provider's ARN
/// requires — `ConsentPortalIdpConfig.credentialProviderArn`'s pattern is
/// `arn:…:bedrock-agentcore:…:token-vault/…/oauth2credentialprovider/…`
/// (`smithy_query shape=ConsentPortalIdpConfig`).
///
/// `ApiKeyCredentialProviderArnType`'s own pattern still spells the legacy `acps`
/// namespace, which the service no longer issues.
pub fn api_key_credential_provider_arn(
    region: &str,
    token_vault_id: &str,
    name: &str,
) -> String {
    ::engine::emulator_arn(
        super::ARN_SERVICE,
        region,
        &format!("token-vault/{token_vault_id}/{API_KEY_CREDENTIAL_PROVIDER_ARN_TYPE}/{name}"),
    )
}

/// One API key credential provider of this account, as
/// `CreateApiKeyCredentialProvider` persisted it.
///
/// The name is the identifier: the read, the update and the delete all address a
/// provider by `name`, and it "must be unique within your account"
/// (`smithy_query doc shape=CreateApiKeyCredentialProviderRequest member=name`),
/// so the store is keyed by it.
#[derive(Debug, Clone)]
pub struct ApiKeyCredentialProviderRecord {
    pub name: String,
    pub credential_provider_arn: String,
    /// The secret the provider's API key lives in, as its owner reported it.
    pub secret: StoredApiKeySecret,
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl ApiKeyCredentialProviderRecord {
    /// This provider as `CreateApiKeyCredentialProvider`'s response envelope.
    pub fn create_output(&self) -> CreateApiKeyCredentialProviderOutput {
        CreateApiKeyCredentialProviderOutput {
            api_key_secret_arn: Secret {
                secret_arn: self.secret.secret_arn.clone(),
            },
            api_key_secret_json_key: self.secret.json_key.clone(),
            api_key_secret_source: Some(self.secret.source.clone()),
            name: self.name.clone(),
            credential_provider_arn: self.credential_provider_arn.clone(),
        }
    }

    /// This provider as `GetApiKeyCredentialProvider`'s response envelope — the
    /// members [`Self::create_output`] renders plus the two instants the read
    /// reports, off the one record, so a create and a later read cannot disagree.
    pub fn get_output(&self) -> GetApiKeyCredentialProviderOutput {
        let created = self.create_output();
        GetApiKeyCredentialProviderOutput {
            api_key_secret_arn: created.api_key_secret_arn,
            api_key_secret_json_key: created.api_key_secret_json_key,
            api_key_secret_source: created.api_key_secret_source,
            name: created.name,
            credential_provider_arn: created.credential_provider_arn,
            created_time: timestamp(self.created_at),
            last_updated_time: timestamp(self.updated_at),
        }
    }

    /// Replace the secret this provider's API key lives in with the one an
    /// update supplied, and record the instant it happened.
    ///
    /// An update carries the whole secret configuration — the new key "replaces
    /// the existing API key", and the source and the reference describe where it
    /// is to live — so the provider afterwards carries exactly the secret the
    /// update asked for, with nothing of the previous one left mixed in. What
    /// the update does NOT touch is what identifies the provider rather than
    /// configures it: the name it is addressed by, the ARN minted from that
    /// name, the tags it carries and the instant it was created.
    pub fn apply_update(&mut self, secret: StoredApiKeySecret, at: SystemTime) {
        self.secret = secret;
        self.updated_at = at;
    }

    /// This provider as `UpdateApiKeyCredentialProvider`'s response envelope —
    /// the same members the read reports, off the one record the update has just
    /// written, so the update and a later read cannot disagree.
    pub fn update_output(&self) -> UpdateApiKeyCredentialProviderOutput {
        let read = self.get_output();
        UpdateApiKeyCredentialProviderOutput {
            api_key_secret_arn: read.api_key_secret_arn,
            api_key_secret_json_key: read.api_key_secret_json_key,
            api_key_secret_source: read.api_key_secret_source,
            name: read.name,
            credential_provider_arn: read.credential_provider_arn,
            created_time: read.created_time,
            last_updated_time: read.last_updated_time,
        }
    }
}

/// The API key secret of one provider, as every response that reports the
/// provider carries it: who manages it, the ARN its owner holds it at and the
/// JSON key the value is extracted under. The KEY itself is not here — it is in
/// Secrets Manager.
#[derive(Debug, Clone)]
pub struct StoredApiKeySecret {
    /// The ARN Secrets Manager answered with: the secret this service minted for
    /// a `MANAGED` provider, or the one it resolved for the secret an `EXTERNAL`
    /// provider references.
    pub secret_arn: String,
    /// The JSON key the API key is extracted from that secret with, as every
    /// response reporting this provider carries it: the one the caller's own
    /// `SecretReference` named, and NONE for a secret the service manages, whose
    /// document layout the caller never named.
    pub json_key: Option<String>,
    /// The EFFECTIVE [`SECRET_SOURCE_MANAGED`] / [`SECRET_SOURCE_EXTERNAL`].
    pub source: String,
}

/// The API key secret ONE request asks a provider to carry: where the key is to
/// live, and therefore what the request has to do about it.
///
/// Shared by the create and the update, which describe the secret with the same
/// three members and so decide it by the same rule: neither can accept a secret
/// configuration the other would refuse, and a provider's secret is reported one
/// way whichever operation wrote it.
#[derive(Debug, Clone)]
pub enum RequestedApiKeySecret {
    /// The service manages the secret, so the request's own key is what goes
    /// into it.
    Managed(String),
    /// The caller manages the secret, which the request references by id and
    /// extracts the key from under `json_key`.
    External { secret_id: String, json_key: String },
}

impl RequestedApiKeySecret {
    /// What the request asks for the provider's secret, or the modelled
    /// validation failure its members are refused for.
    ///
    /// Two rules the model states in PROSE on the members themselves, so the
    /// framework's constraint walk cannot refuse them and this must: the secret
    /// reference "is required when apiKeySecretSource is set to EXTERNAL"
    /// (`smithy_query doc shape=CreateApiKeyCredentialProviderRequest
    /// member=apiKeySecretConfig`), and a service-managed secret is the place
    /// the key is "encrypted and stored securely" — the value member has a
    /// modelled default of `""`, so a request that leaves it out is
    /// indistinguishable from one that sends it empty, and either way there is
    /// nothing to store.
    pub fn from_input(
        api_key: Option<&str>,
        requested_source: Option<&str>,
        config: Option<&SecretReference>,
    ) -> Result<Self, ValidationException> {
        let source = effective_secret_source(requested_source, config.is_some());
        if source == SECRET_SOURCE_EXTERNAL {
            let reference = config.ok_or_else(|| {
                field_validation_failed(
                    API_KEY_SECRET_CONFIG_MEMBER,
                    &format!("is required when apiKeySecretSource is {SECRET_SOURCE_EXTERNAL}"),
                )
            })?;
            return Ok(Self::External {
                secret_id: reference.secret_id.clone(),
                json_key: reference.json_key.clone(),
            });
        }
        let api_key = api_key.unwrap_or_default();
        if api_key.is_empty() {
            return Err(field_validation_failed(
                API_KEY_MEMBER,
                &format!("is required when apiKeySecretSource is {SECRET_SOURCE_MANAGED}"),
            ));
        }
        Ok(Self::Managed(api_key.to_string()))
    }

    /// Carry out what the request asked for, against the service that owns the
    /// secrets, and report the secret the provider is to carry.
    ///
    /// An EXTERNAL provider's secret is the CALLER's: nothing is written into
    /// it, so an `apiKey` sent alongside the reference is not stored anywhere.
    /// The model states no rule making the two members exclusive, so a request
    /// carrying both is served rather than refused over it.
    ///
    /// Call it with NO state guard held — it awaits.
    pub async fn resolve(
        self,
        ctx: &::smithy_server_runtime::OperationContext<'_>,
        region: &str,
        token_vault_id: &str,
        provider_name: &str,
    ) -> Result<StoredApiKeySecret, ApiKeySecretProblem> {
        match self {
            Self::Managed(api_key) => Ok(StoredApiKeySecret {
                secret_arn: store_managed_api_key(
                    ctx,
                    region,
                    token_vault_id,
                    provider_name,
                    &api_key,
                )
                .await?,
                // A secret the SERVICE manages carries the key under a JSON key
                // of the service's own choosing, which no `SecretReference` of
                // the request named: there is no caller-supplied JSON key to
                // report, so the optional member is omitted.
                json_key: None,
                source: SECRET_SOURCE_MANAGED.to_string(),
            }),
            Self::External {
                secret_id,
                json_key,
            } => Ok(StoredApiKeySecret {
                secret_arn: referenced_external_secret_arn(ctx, region, &secret_id).await?,
                json_key: Some(json_key),
                source: SECRET_SOURCE_EXTERNAL.to_string(),
            }),
        }
    }
}

/// The secret source a request asks for: the one it named, else the one its
/// members imply.
///
/// A request that references a secret of its own
/// (`apiKeySecretConfig`, "Required when apiKeySecretSource is set to EXTERNAL")
/// is asking for that secret to be used; anything else is the service managing
/// the secret it stores the caller's `apiKey` in.
pub fn effective_secret_source(requested: Option<&str>, has_secret_config: bool) -> &str {
    match requested {
        Some(SECRET_SOURCE_EXTERNAL) => SECRET_SOURCE_EXTERNAL,
        Some(SECRET_SOURCE_MANAGED) => SECRET_SOURCE_MANAGED,
        _ if has_secret_config => SECRET_SOURCE_EXTERNAL,
        _ => SECRET_SOURCE_MANAGED,
    }
}

/// The message this service refuses a credential provider with when a secret
/// the caller references cannot be decrypted.
///
/// Fixed text: nothing of the request or of the state is interpolated, because
/// the remedy does not depend on which secret or which key was at fault — the
/// caller fixes the key's permissions and retries. Shared by every credential
/// provider plane of this crate, so one undecryptable secret is reported in one
/// sentence whichever provider referenced it.
pub const SECRET_UNDECRYPTABLE_MESSAGE: &str =
    "Unable to decrypt the provided secret. Verify KMS key permissions.";

/// Why the secret an API key credential provider's key lives in could not be
/// reported.
#[derive(Debug)]
pub enum ApiKeySecretProblem {
    /// Secrets Manager answered that this account holds no such secret, so the
    /// request references a resource that does not exist.
    Absent,
    /// Secrets Manager will not decrypt the secret: the state of its own KMS
    /// key. The secret is there, but its value cannot be read, so a credential
    /// provider over it would carry a credential nothing can use.
    Undecryptable,
    /// Secrets Manager could not be asked, or refused the call: nothing is known
    /// about the secret, so there is no ARN to report. Carries the rendering of
    /// what came back, for the trace.
    Unavailable(String),
}

/// Put `api_key` into the Secrets Manager secret a `MANAGED` provider's key lives
/// in, and report that secret's ARN.
///
/// The secret is created under [`managed_api_key_secret_name`], carrying the key
/// as the JSON document [`managed_api_key_secret_value`] builds; the store itself
/// is the shared one every credential-provider plane of this crate uses
/// ([`store_managed_secret`]), so the ARN reported is the one Secrets Manager
/// answered with.
///
/// Call it with NO state guard held — it awaits.
pub async fn store_managed_api_key(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    token_vault_id: &str,
    provider_name: &str,
    api_key: &str,
) -> Result<String, ApiKeySecretProblem> {
    store_managed_secret(
        ctx,
        region,
        &managed_api_key_secret_name(token_vault_id, provider_name),
        &managed_api_key_secret_value(api_key),
    )
    .await
}

/// Put `value` into the Secrets Manager secret named `name`, which is a secret
/// this SERVICE manages for a credential provider, and report that secret's ARN.
///
/// A secret already standing under that name is the provider's own — the name is
/// derived from the provider — so the value is put into it as a new version
/// rather than refused. Shared by the credential-provider stores of this crate
/// (the API key provider above and the payment provider beside it), so no two of
/// them differ about how a managed secret is written or which ARN is reported
/// for it.
///
/// Call it with NO state guard held — it awaits.
pub(crate) async fn store_managed_secret(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
    value: &str,
) -> Result<String, ApiKeySecretProblem> {
    let client = secrets_manager_client(ctx, region);
    let created = client
        .create_secret()
        .name(name)
        .secret_string(value)
        .send()
        .await;
    match created {
        Ok(answer) => arn_of(answer.arn, "CreateSecret"),
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service| service.is_resource_exists_exception()) =>
        {
            let put = client
                .put_secret_value()
                .secret_id(name)
                .secret_string(value)
                .send()
                .await
                .map_err(|error| {
                    unavailable(::aws_smithy_types::error::display::DisplayErrorContext(&error))
                })?;
            arn_of(put.arn, "PutSecretValue")
        }
        Err(error) => Err(unavailable(
            ::aws_smithy_types::error::display::DisplayErrorContext(&error),
        )),
    }
}

/// The ARN of the secret an `EXTERNAL` credential provider references, as
/// Secrets Manager reports it for the `secretId` the caller named — which may be
/// the secret's name or its whole ARN, both of which that service resolves.
///
/// Nothing is written: the secret is the caller's. The read is `GetSecretValue`,
/// the call that DECRYPTS the value, because a credential the provider cannot
/// read is of no use to it: a secret whose KMS key will not decrypt is
/// [`ApiKeySecretProblem::Undecryptable`] and a `secretId` Secrets Manager holds
/// no secret for is [`ApiKeySecretProblem::Absent`]. The ARN reported is the one
/// Secrets Manager answered with, never the string the request carried, so a
/// provider is never recorded against a secret nothing holds.
///
/// Shared by every credential-provider plane of this crate, so none of them can
/// accept a secret another would refuse.
///
/// Call it with NO state guard held — it awaits.
pub async fn referenced_external_secret_arn(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    secret_id: &str,
) -> Result<String, ApiKeySecretProblem> {
    match secrets_manager_client(ctx, region)
        .get_secret_value()
        .secret_id(secret_id)
        .send()
        .await
    {
        Ok(answer) => arn_of(answer.arn, "GetSecretValue"),
        Err(error) => {
            let service = error.as_service_error();
            if service.is_some_and(|failure| failure.is_decryption_failure()) {
                return Err(ApiKeySecretProblem::Undecryptable);
            }
            if service.is_some_and(|failure| failure.is_resource_not_found_exception()) {
                return Err(ApiKeySecretProblem::Absent);
            }
            Err(unavailable(
                ::aws_smithy_types::error::display::DisplayErrorContext(&error),
            ))
        }
    }
}

/// The ARN a Secrets Manager answer reported, or [`ApiKeySecretProblem`] when it
/// reported none: the provider's own answer names that secret, so there is
/// nothing to report without it.
fn arn_of(arn: Option<String>, operation: &str) -> Result<String, ApiKeySecretProblem> {
    arn.ok_or_else(|| {
        ApiKeySecretProblem::Unavailable(format!("{operation} answered with no secret ARN"))
    })
}

/// A failed Secrets Manager call as [`ApiKeySecretProblem::Unavailable`], with
/// what came back rendered for the trace.
pub(crate) fn unavailable(detail: impl ::std::fmt::Display) -> ApiKeySecretProblem {
    ApiKeySecretProblem::Unavailable(detail.to_string())
}

/// The Secrets Manager client this crate's credential-provider stores store and
/// resolve a secret through: the in-process callback into this same emulator, at
/// the endpoint of the REQUEST's own Region, so the secret lands where the
/// provider's own ARN says the provider is.
pub(crate) fn secrets_manager_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_secretsmanager::Client {
    let region = if region.is_empty() {
        DEFAULT_REGION.to_string()
    } else {
        region.to_string()
    };
    let config = ::aws_sdk_secretsmanager::Config::builder()
        .behavior_version(::aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_secretsmanager::config::Region::new(
            region.clone(),
        ))
        .endpoint_url(format!("https://secretsmanager.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch
        // for a stalled stream, and each of those would need a sleep implementation
        // this client is not given. The same goes for caching the identity: the
        // credentials are handed over directly.
        .retry_config(::aws_sdk_secretsmanager::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_secretsmanager::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_secretsmanager::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_secretsmanager::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_secretsmanager::Client::from_conf(config)
}
