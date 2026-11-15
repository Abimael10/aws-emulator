// SPDX-License-Identifier: CC0-1.0

//! The AWS Secrets Manager secret a deployment parameter IS.
//!
//! `PutDeploymentParameter` takes "the deployment parameter targeted to the acceptor of
//! an agreement for which to create the AWS Secret Manager resource", and the text it
//! carries is "the text to encrypt and store in the secret"
//! (`smithy_query(query="shape",shape="DeploymentParameterInput")`) — so the WRITE this
//! operation performs is a secret in Secrets Manager, in the account the agreement's
//! acceptor holds, which is what lets a Quick Launch CloudFormation template resolve the
//! value as a Secrets Manager dynamic reference. Secrets Manager owns secret values, so
//! the value lives THERE and nowhere else: this service's own record keeps the identity
//! of that secret ([`super::DeploymentParameterRecord::secret_arn`]) and no copy of what
//! is in it.
//!
//! The three things that follow from one secret per deployment parameter, each applied
//! here:
//!
//! 1. a CREATE puts the text into a new secret, named for the resource this service
//!    minted ([`deployment_parameter_secret_name`]), and records the ARN Secrets Manager
//!    allocated;
//! 2. a repeat write for the same identity stakes a NEW VERSION of that same secret
//!    (`PutSecretValue`), so the value a client reads back is always the one last
//!    written and no stale secret is left standing;
//! 3. the deletion `expirationDate` makes due takes the secret with the record
//!    ([`release_expired_secrets`]): "the date when deployment parameters expire and are
//!    scheduled for deletion" is a deadline for the whole parameter, and the value it
//!    held is the parameter.
//!
//! Secrets Manager is reached the way every other emulated service here reaches another:
//! the AWS SDK driven over the emulator's own in-process transport, through the operation
//! callback ([`smithy_server_runtime::OperationContext::callback`]). No socket is opened,
//! no name is resolved and nothing outside this process is reachable from it; the
//! endpoint URL below is addressing for that transport rather than a host this process
//! connects to, and the Region in it is read as a Region LABEL
//! ([`::service_common::arn::region_label`]) so caller-supplied text can never name an
//! authority of its own.
//!
//! Nothing here may be called with this service's store guard held: every call awaits.

use super::InternalServerException;

/// The name every secret this service manages is held under: the service's own ARN
/// namespace ([`super::ARN_NAMESPACE`]) and the resource kind inside it.
///
/// AWS publishes no name form for the secret a deployment parameter is delivered in, so
/// the form is this emulator's OWN — chosen so that the name says which service manages
/// the secret and which resource of it the secret belongs to, and so that it cannot
/// collide with a name a buyer chose for a secret of their own.
pub const SECRET_NAME_PREFIX: &str = "aws-marketplace/deployment-parameter";

/// The name the secret holding one deployment parameter's text is created under:
/// the managed prefix and the `deploymentParameterId` this service minted for the
/// resource.
///
/// A FUNCTION of the resource, not of the request: the id is the one the response
/// reports, so a client that has made the call can name the secret the call wrote, and a
/// repeat write for the same identity — which keeps the same id — names the same secret
/// and so stakes another version of it rather than a second secret.
///
/// Every character is one Secrets Manager admits in a secret name ("ASCII letters,
/// numbers, and the following characters: /_+=.@-", `CreateSecretRequest$Name`), and a
/// `DeploymentParameterResourceIdentifier` is `^dp-[a-zA-Z0-9]+$` of at most 32
/// characters, so the whole name stays well inside that member's `length 1..512`.
#[must_use]
pub fn deployment_parameter_secret_name(deployment_parameter_id: &str) -> String {
    format!("{SECRET_NAME_PREFIX}/{deployment_parameter_id}")
}

/// What the managed secret's `Description` says: which deployment parameter of which
/// product and agreement the secret holds, so a buyer reading their secrets can tell
/// whose it is.
///
/// The parameter's NAME is here rather than in the secret's name because it is the
/// identifier a Quick Launch template's parameter matches, and it may be as long as 400
/// characters — far past what a secret reports as its own `Name`, and well inside
/// `CreateSecretRequest$Description`'s `length 0..2048`.
#[must_use]
pub fn secret_description(
    catalog: &str,
    product_id: &str,
    agreement_id: &str,
    name: &str,
) -> String {
    format!(
        "AWS Marketplace deployment parameter {name} for product {product_id} under \
         agreement {agreement_id} in catalog {catalog}."
    )
}

/// Put `secret_string` into the secret this deployment parameter's value lives in, and
/// report the ARN Secrets Manager answered with.
///
/// `secret_id` is the secret this service already recorded for the parameter, when it has
/// one: that secret gets a new VERSION, which is how a repeat write for one identity
/// updates the value a client reads instead of leaving a stale one. A parameter with no
/// secret yet gets one created under `name`.
///
/// Either call may find the other side's outcome already in place — a name this account
/// already holds a secret under, a recorded secret Secrets Manager no longer holds — so
/// each falls back to the other: what is asked for is that the secret under this name
/// holds this text afterwards, whichever call gets it there.
///
/// `Err` carries the rendering of what came back, for the sentence the caller reports the
/// failure with ([`secret_write_failed`]). Every failure here is this EMULATOR failing to
/// do what AWS Marketplace does — Secrets Manager not mounted beside this service, or a
/// store that would not take the value — never anything the request carried: the values a
/// request carries are judged before this is called.
pub async fn store_secret(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
    description: &str,
    secret_id: Option<&str>,
    secret_string: &str,
) -> Result<String, String> {
    let client = secrets_manager_client(ctx, region).ok_or_else(unaskable_region)?;
    if let Some(secret_id) = secret_id {
        let staked = client
            .put_secret_value()
            .secret_id(secret_id)
            .secret_string(secret_string)
            .send()
            .await;
        match staked {
            Ok(answer) => return reported_arn(answer.arn, "PutSecretValue"),
            // The secret this service recorded is not there any more — it was deleted
            // out of band of this API — so the parameter's value is written by creating
            // it again rather than reported as unwritable.
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|service| service.is_resource_not_found_exception()) => {}
            Err(error) => return Err(rendered(&error)),
        }
    }
    let created = client
        .create_secret()
        .name(name)
        .description(description)
        .secret_string(secret_string)
        .send()
        .await;
    match created {
        Ok(answer) => reported_arn(answer.arn, "CreateSecret"),
        // The name is already taken: this parameter's own secret, standing from an
        // earlier write whose ARN this service did not record, so the text goes into it
        // as a new version.
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service| service.is_resource_exists_exception()) =>
        {
            let staked = client
                .put_secret_value()
                .secret_id(name)
                .secret_string(secret_string)
                .send()
                .await
                .map_err(|error| rendered(&error))?;
            reported_arn(staked.arn, "PutSecretValue")
        }
        Err(error) => Err(rendered(&error)),
    }
}

/// Delete the secret `secret_id` names, so the value a deployment parameter held can no
/// longer be read once the parameter is gone.
///
/// The delete goes through Secrets Manager's own `DeleteSecret`, which owns a secret's
/// lifecycle: the secret leaves service exactly as any other deleted secret does — on the
/// recovery window that operation applies by default — and nothing of this service's own
/// pretends it is gone. `Err` carries the rendering of what came back.
pub async fn release_secret(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    secret_id: &str,
) -> Result<(), String> {
    secrets_manager_client(ctx, region)
        .ok_or_else(unaskable_region)?
        .delete_secret()
        .secret_id(secret_id)
        .send()
        .await
        .map_err(|error| rendered(&error))?;
    Ok(())
}

/// Delete the secrets of the deployment parameters the clock has already taken: the
/// deletion `expirationDate` made due, carried out on the side that OWNS the value.
///
/// The queue is written by the purge itself
/// ([`super::ComAmazonawsMarketplacedeploymentAwsmpdeploymentparametersserviceState::purge_expired`]),
/// which runs under the store guard and cannot await, and is drained HERE by the call
/// whose own purge filled it. A secret the drain could not delete is reported in the
/// trace and not queued again: the answer to the caller is about its own request, and the
/// record the secret belonged to is gone either way — this is a deletion the clock owed,
/// not a step of serving anything.
///
/// Call it with NO store guard held: it awaits.
pub async fn release_expired_secrets<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let expired = state.lock().take_expired_secrets();
    for secret_id in expired {
        if let Err(detail) = release_secret(ctx, region, &secret_id).await {
            ::tracing::debug!(
                secret = %secret_id,
                %detail,
                "the secret of an expired deployment parameter was not deleted"
            );
        }
    }
}

/// The modelled `InternalServerException` (HTTP 500) for a write whose value this
/// emulator could not put into Secrets Manager.
///
/// The deployment parameter IS the secret, so a value that is nowhere readable is not a
/// deployment parameter: the write is reported as having failed rather than answered with
/// a resource whose value no caller can get at. `detail` is the rendering of what Secrets
/// Manager came back with, which is the only thing that distinguishes one such failure
/// from another.
#[must_use]
pub fn secret_write_failed(detail: &str) -> InternalServerException {
    InternalServerException {
        message: format!(
            "the deployment parameter's secret could not be written to AWS Secrets \
             Manager: {detail}"
        ),
    }
}

/// The sentence a Region this emulator cannot ask Secrets Manager in is reported with.
///
/// The Region comes from the request's own SigV4 credential scope, and it lands inside an
/// endpoint URL's authority, so a value that is not a Region label names no peer service
/// to ask.
fn unaskable_region() -> String {
    "the request names no Region this emulator can reach AWS Secrets Manager in".to_string()
}

/// What came back from a call, rendered for a trace or a sentence — the whole error
/// chain, which is what says which failure this was.
fn rendered<E: ::std::error::Error>(error: &E) -> String {
    ::aws_smithy_types::error::display::DisplayErrorContext(error).to_string()
}

/// The ARN Secrets Manager answered with, or the failure of an answer that carried none:
/// the recorded ARN is always the one that service allocated, never a value this service
/// built, so a record can never name a secret nothing holds.
fn reported_arn(arn: Option<String>, call: &str) -> Result<String, String> {
    arn.filter(|arn| !arn.is_empty())
        .ok_or_else(|| format!("AWS Secrets Manager answered {call} with no secret ARN"))
}

/// The Secrets Manager client this service drives, addressed to `region`.
///
/// Secrets Manager holds its secrets per Region, so `region` is the Region the request
/// this call serves was signed for — the secret is created where the deployment parameter
/// it belongs to lives. `None` for a value that is not a Region label, for the reason
/// [`unaskable_region`] gives.
///
/// The transport is in-process, so everything a network client needs and this one cannot
/// have is disabled: retries, timeouts, stalled-stream protection and identity caching
/// each need a sleep implementation the callback transport is not given, and there is
/// nothing behind it to retry. The credentials are the emulator's OWN — the caller's
/// `Authorization` header is never forwarded.
fn secrets_manager_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_secretsmanager::Client> {
    let region = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_secretsmanager::Config::builder()
        .behavior_version(::aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(::aws_sdk_secretsmanager::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://secretsmanager.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        .retry_config(::aws_sdk_secretsmanager::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_secretsmanager::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_secretsmanager::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_secretsmanager::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    Some(::aws_sdk_secretsmanager::Client::from_conf(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A managed secret's name is the prefix and the resource's own id, and every
    /// character of it is one Secrets Manager admits in a secret name: the rows differ
    /// along the id's length and its digits and letters.
    #[test]
    fn a_managed_secret_is_named_for_the_resource_it_belongs_to() {
        // The charset `CreateSecretRequest$Name` documents, as the Secrets Manager
        // emulator states it (`state::SECRET_NAME_EXTRA_CHARS`).
        const EXTRA: &str = "/_+=.@-";
        for case in [
            "dp-0000000000000001",
            "dp-00000000000000ff",
            "dp-abcdefABCDEF0123",
            "dp-1",
        ] {
            let name = deployment_parameter_secret_name(case);
            let expected = format!("{SECRET_NAME_PREFIX}/{case}");
            assert_eq!(
                name, expected,
                "case {case:?}: expected {expected:?}, got {name:?}"
            );
            assert!(
                name.len() <= 512 && !name.is_empty(),
                "case {case:?}: expected a name inside CreateSecret's 1..512, got {} \
                 characters",
                name.len()
            );
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || EXTRA.contains(c)),
                "case {case:?}: expected only characters a secret name admits, got {name:?}"
            );
        }
    }

    /// The description names the parameter and its target, and stays inside
    /// `CreateSecretRequest$Description`'s `length 0..2048` even for the longest
    /// `DeploymentParameterName` the model admits (`length 1..400`).
    #[test]
    fn the_description_names_the_parameter_and_fits_the_member() {
        for (case, name) in [
            ("a short name", "DbPassword".to_string()),
            ("the longest name the model admits", "n".repeat(400)),
        ] {
            let description = secret_description(
                "AWSMarketplace",
                &"p".repeat(64),
                &"a".repeat(64),
                &name,
            );
            assert!(
                description.contains(&name),
                "case {case:?}: expected the description to name the parameter, got \
                 {description:?}"
            );
            assert!(
                description.len() <= 2048,
                "case {case:?}: expected a description inside 0..2048, got {} characters",
                description.len()
            );
        }
    }
}
