// SPDX-License-Identifier: CC0-1.0

//! The Amazon Web Services Secrets Manager secret Amazon AppFlow MANAGES for one connector
//! profile's credentials.
//!
//! `ConnectorProfile$credentialsArn` is "the Amazon Resource Name (ARN) of the connector profile
//! credentials" (`smithy_query(query="shape", shape="ConnectorProfile")`), and the credentials a
//! `CreateConnectorProfile` carries are never reported back: what a reader of the profile gets is
//! that ARN. So the credentials go into a secret Secrets Manager HOLDS, and the ARN the profile
//! reports is the one that service allocated — never one this crate built, because a handle a
//! client reads has to resolve where the client would look it up:
//!
//! 1. STORE. [`mint_connector_profile_credentials_secret`] calls that service's own
//!    `CreateSecret` under the name AppFlow gives a secret of its own
//!    ([`connector_profile_credentials_secret_name`]), whose value is the connector-specific
//!    credential document the request carried. `GetSecretValue` on the ARN this answers reads
//!    those credentials back and `DescribeSecret` resolves it.
//! 2. RE-VALUE. An `UpdateConnectorProfile` that restates the credentials puts a new version into
//!    that same secret ([`store_connector_profile_credentials`]) — the profile keeps the handle it
//!    was created with, which is what makes `credentialsArn` stable across an update.
//! 3. RELEASE. `DeleteConnectorProfile` deletes the secret
//!    ([`release_connector_profile_credentials_secret`]), so nothing is left holding credentials
//!    nothing can reach.
//!
//! Every call is made as Amazon AppFlow's OWN service principal acting for the profile
//! ([`super::ServicePrincipalCall`]): that is what lets the secret be named under the reserved
//! prefix no caller can mint and be reported with `OwningService`. A call that cannot be made
//! that way cannot create that secret AT ALL, so it is reported as this emulator failing rather
//! than quietly creating an ordinary secret of the account — or, worse, reporting an ARN nothing
//! holds.
//!
//! Secrets Manager is reached the way the rest of this emulator reaches another service: the AWS
//! SDK driven over the emulator's own in-process transport, through the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so no socket is opened and no request
//! leaves the process.

/// The prefix every secret Amazon AppFlow MANAGES is named under: the service's own id and the
/// character AWS reserves for a secret an Amazon Web Services service owns.
///
/// A secret in this form is not a caller's to create — the mark is not one of the characters a
/// secret name may carry, so a caller's `CreateSecret` cannot name one at all — which is what
/// makes the name itself say the secret is the service's. It is accepted from the service whose
/// own principal is making the create.
pub const MANAGED_SECRET_NAME_PREFIX: &str = "appflow!";

/// The name of the secret Amazon AppFlow manages for one connector profile's credentials:
/// `appflow!<secret id>`.
///
/// AWS names a secret of its own under the reserved prefix every such secret carries
/// ([`MANAGED_SECRET_NAME_PREFIX`]) followed by an opaque id of the account's own. That id is
/// server-minted ([`super::new_credentials_secret_id`]), carries nothing of the caller's input and
/// never changes for the life of the profile, so two profiles can never be given one name and a
/// profile renamed by nothing keeps its secret. It is also not what the secret is addressed by:
/// every later call reads the ARN Secrets Manager allocated, which the profile record stores.
#[must_use]
pub fn connector_profile_credentials_secret_name(secret_id: &str) -> String {
    format!("{MANAGED_SECRET_NAME_PREFIX}{secret_id}")
}

/// Mint the secret that holds `credentials` for the connector profile `profile_arn` names, and
/// answer the ARN Secrets Manager allocated for it — the `credentialsArn` the profile reports.
///
/// `Err` carries the sentence to report the failure with. Every failure here is a failure of this
/// EMULATOR to do what the service does — Secrets Manager not mounted beside this service, or a
/// store that would not take the secret — never anything the request carried.
pub async fn mint_connector_profile_credentials_secret<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    profile_arn: &str,
    secret_id: &str,
    credentials: &str,
) -> Result<String, String> {
    let acting = super::ServicePrincipalCall::begin(engine, ctx, profile_arn)
        .ok_or_else(|| unactable_principal("created", profile_arn))?;
    let created = acting
        .secrets_manager_client(ctx, region)?
        .create_secret()
        .name(connector_profile_credentials_secret_name(secret_id))
        .client_request_token(fresh_version_token())
        .secret_string(credentials)
        .send()
        .await
        .map_err(|error| failed_call("created", &error))?;
    created.arn().map(str::to_string).ok_or_else(|| {
        "Amazon Web Services Secrets Manager created the connector profile credentials secret \
         without an ARN"
            .to_string()
    })
}

/// Put `credentials` into the secret `secret_arn` names as a new version, for an update that
/// restated a profile's credentials.
///
/// The profile keeps the handle it was created with: a client that read `credentialsArn` before
/// the update reads the same secret after it, now holding what the update stated.
pub async fn store_connector_profile_credentials<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    profile_arn: &str,
    secret_arn: &str,
    credentials: &str,
) -> Result<(), String> {
    let acting = super::ServicePrincipalCall::begin(engine, ctx, profile_arn)
        .ok_or_else(|| unactable_principal("updated", profile_arn))?;
    acting
        .secrets_manager_client(ctx, region)?
        .put_secret_value()
        .secret_id(secret_arn)
        .client_request_token(fresh_version_token())
        .secret_string(credentials)
        .send()
        .await
        .map_err(|error| failed_call("updated", &error))?;
    Ok(())
}

/// Delete the secret a profile's credentials were held in: the profile is gone, so the
/// credentials it held can no longer be read.
///
/// The delete goes through Secrets Manager's own `DeleteSecret`, which owns the secret's
/// lifecycle, made as the service's own principal acting for the profile for the reason the
/// create is: a caller's own `DeleteSecret` on such a secret is refused. It goes WITHOUT a
/// recovery window, because the secret is the service's own and nobody but the service could take
/// it back out of one.
pub async fn release_connector_profile_credentials_secret<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    profile_arn: &str,
    secret_arn: &str,
) -> Result<(), String> {
    let acting = super::ServicePrincipalCall::begin(engine, ctx, profile_arn)
        .ok_or_else(|| unactable_principal("deleted", profile_arn))?;
    acting
        .secrets_manager_client(ctx, region)?
        .delete_secret()
        .secret_id(secret_arn)
        .force_delete_without_recovery(true)
        .send()
        .await
        .map_err(|error| failed_call("deleted", &error))?;
    Ok(())
}

/// Release a secret no connector profile reports any more, so nothing is left behind holding
/// credentials nothing can reach: the secret of a profile this call deleted, or one minted for a
/// profile the call then could not store.
///
/// Best effort by design: the answer to the caller is already decided — the profile is gone, or
/// the refusal that orphaned the secret stands — so a Secrets Manager call that fails here cannot
/// change it. It is recorded and the answer stands.
pub async fn release_orphaned_credentials_secret<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    profile_arn: &str,
    secret_arn: &str,
) {
    if let Err(message) = release_connector_profile_credentials_secret(
        engine, ctx, region, profile_arn, secret_arn,
    )
    .await
    {
        ::tracing::debug!(
            secret = secret_arn,
            %message,
            "the credentials secret of a connector profile no caller can reach any more was not \
             deleted"
        );
    }
}

/// The document one connector profile's secret holds: the connector-specific credentials the
/// write carried, rendered by this service's own codec, or an empty document for a profile
/// created without credentials (every member of `ConnectorProfileCredentials` is optional, and a
/// connector whose credentials are configured out of band needs none).
///
/// Rendered through the codec rather than written by hand, exactly as the comparison of two stored
/// configurations is ([`super::rendered_shape`]), so what the secret holds is what the model says
/// the member is.
#[must_use]
pub fn credentials_document(
    credentials: Option<&crate::ops::types::ConnectorProfileCredentials>,
) -> String {
    credentials
        .and_then(|credentials| {
            super::rendered_shape(
                &crate::ops::types::ConnectorProfileCredentials::SCHEMA,
                credentials,
            )
        })
        .unwrap_or_else(|| ::serde_json::Value::Object(::serde_json::Map::new()))
        .to_string()
}

/// A version id for the secret version this call stakes.
///
/// `ClientRequestToken` becomes the version's id, and "if you generate a raw HTTP request to the
/// Secrets Manager service endpoint, then you must generate a ClientRequestToken and include it in
/// the request" — which is the position this service is in, since it is Amazon AppFlow that
/// creates the secret. The canonically hyphenated v4 UUID that member documents, from the one
/// generator this crate mints an opaque id with ([`super::new_execution_id`]), so two calls never
/// stake the same version and this crate adds no second path to a uuid generator.
fn fresh_version_token() -> String {
    super::new_execution_id()
}

/// A Secrets Manager client over the IN-PROCESS callback into this same emulator: the secret
/// Amazon AppFlow manages is one this emulator holds, so creating, re-valuing and deleting it
/// never leaves the process.
///
/// `signed_as` names the credential the call presents: the service's own principal credential for
/// a call that has to be the service's ([`super::ServicePrincipalCall::secrets_manager_client`]).
///
/// `region` is the request's own signing region, so the secret is created where the profile that
/// carries it lives. The transport is in-process, so everything a network client needs and this
/// one cannot have is disabled: retries, timeouts, stalled-stream protection and identity caching
/// each need a sleep implementation the callback transport is not given, and there is nothing
/// behind it to retry.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in an endpoint
/// URL's authority, so it is read as a Region LABEL (`service_common::arn::region_label`, the one
/// place this tree states that grammar) instead of being interpolated as given: `None` for a value
/// that would name an authority the caller chose rather than the peer service this emulator asks.
pub fn secrets_manager_client_signed_as(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    signed_as: Option<::aws_sdk_secretsmanager::config::Credentials>,
) -> Option<::aws_sdk_secretsmanager::Client> {
    let region = ::service_common::arn::region_label(region)?;
    let mut config = ::aws_sdk_secretsmanager::Config::builder()
        .behavior_version(::aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(::aws_sdk_secretsmanager::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://secretsmanager.{region}.amazonaws.com"))
        .http_client(ctx.callback().http_client())
        .retry_config(::aws_sdk_secretsmanager::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_secretsmanager::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_secretsmanager::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_secretsmanager::config::StalledStreamProtectionConfig::disabled(),
        );
    config = match signed_as {
        Some(credentials) => config.credentials_provider(credentials),
        None => config.credentials_provider(ctx.callback().aws_credentials_provider()),
    };
    Some(::aws_sdk_secretsmanager::Client::from_conf(config.build()))
}

/// The sentence a managed-secret call reports when the request's signing scope names no Region
/// this emulator can ask a peer service in: a failure of this emulator to reach the service that
/// owns the secret, like the others here.
#[must_use]
pub fn unaskable_region_message() -> String {
    "the connector profile credentials secret could not be reached in Amazon Web Services Secrets \
     Manager: the request's signing scope names no Amazon Web Services Region"
        .to_string()
}

/// The sentence reported when Secrets Manager refused the call, carrying that service's own
/// diagnostic. `action` is what could not be done to the secret ("created", "updated",
/// "deleted").
fn failed_call<Error: std::error::Error>(action: &str, error: &Error) -> String {
    format!(
        "the connector profile credentials secret could not be {action} in Amazon Web Services \
         Secrets Manager: {}",
        ::aws_smithy_types::error::display::DisplayErrorContext(error)
    )
}

/// The sentence reported when this emulator could not make the call AS the Amazon AppFlow service
/// principal acting for `profile_arn` ([`super::ServicePrincipalCall::begin`]).
///
/// Such a secret is the service's own — its name is not a caller's to write, and neither its value
/// nor its lifecycle is a caller's to change — so a call that cannot be made as that principal
/// does nothing.
fn unactable_principal(action: &str, profile_arn: &str) -> String {
    format!(
        "the connector profile credentials secret could not be {action} in Amazon Web Services \
         Secrets Manager: this emulator could not make the call as the Amazon AppFlow service \
         principal acting for {profile_arn}, which is the identity that owns the secret"
    )
}
