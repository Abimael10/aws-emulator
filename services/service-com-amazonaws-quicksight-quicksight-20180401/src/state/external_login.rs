// SPDX-License-Identifier: CC0-1.0

//! The external login provider a QuickSight user federates through.
//!
//! A user's external login is described by the same three members wherever it is set —
//! the provider type, the URL of a custom provider, and the user's identity id at that
//! provider — so the rule that reads them lives here once and every handler that writes
//! a user renders the same refusals: the provider type fixes the URL for Amazon Cognito,
//! a custom OpenID Connect provider carries its own, and a member that carries only
//! whitespace names nothing at all.
//!
//! Which provider types an operation admits is the operation's own: only the update of a
//! user can clear what was saved before, so the admitted types travel in from the caller
//! and the refusal names them.

use super::*;

/// The `NONE` provider type: the value that clears every external login value saved for
/// a user. Only the update of a user admits it.
/// Source: `smithy_query(query="doc", shape="UpdateUserRequest",
/// member="ExternalLoginFederationProviderType")`.
pub const EXTERNAL_LOGIN_PROVIDER_NONE: &str = "NONE";

/// What a request's external login members ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalLoginChange {
    /// The request names no provider type: it asks for no external login at all, and
    /// asks for nothing about one already saved to change.
    Unnamed,
    /// The provider the user federates through, and the URL that provider is reached at.
    Provider {
        provider_type: String,
        provider_url: String,
    },
    /// Every external login value saved for the user is cleared.
    Cleared,
}

/// What this service reads as a NAMED member: a value carrying something other than
/// whitespace. A member carrying only spaces names nothing rather than naming an empty
/// provider.
pub fn named_member(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The external login the request asks for, or the message the refusal reads for members
/// that do not go together.
///
/// `supported` is the provider types the calling operation admits, in the order its
/// refusal names them; [`EXTERNAL_LOGIN_PROVIDER_NONE`] among them is what lets that
/// operation clear a saved external login.
pub fn external_login_change(
    provider_type: Option<&str>,
    custom_url: Option<&str>,
    supported: &[&str],
) -> Result<ExternalLoginChange, String> {
    let custom_url = named_member(custom_url);
    let Some(provider_type) = named_member(provider_type) else {
        if custom_url.is_some() {
            return Err(custom_federation_provider_url_message());
        }
        return Ok(ExternalLoginChange::Unnamed);
    };

    if !supported.contains(&provider_type) {
        return Err(format!(
            "ExternalLoginFederationProviderType {provider_type} is not supported. \
             Supported types are {}.",
            conjunction(supported)
        ));
    }

    match provider_type {
        EXTERNAL_LOGIN_PROVIDER_COGNITO => {
            if custom_url.is_some() {
                return Err(custom_federation_provider_url_message());
            }
            Ok(ExternalLoginChange::Provider {
                provider_type: provider_type.to_string(),
                provider_url: COGNITO_FEDERATION_PROVIDER_URL.to_string(),
            })
        }
        EXTERNAL_LOGIN_PROVIDER_CUSTOM_OIDC => match custom_url {
            Some(url) => Ok(ExternalLoginChange::Provider {
                provider_type: provider_type.to_string(),
                provider_url: url.to_string(),
            }),
            None => Err(format!(
                "CustomFederationProviderUrl is required when \
                 ExternalLoginFederationProviderType is {EXTERNAL_LOGIN_PROVIDER_CUSTOM_OIDC}."
            )),
        },
        // The clearing type saves no provider, so it carries no URL of its own either.
        _ => {
            if custom_url.is_some() {
                return Err(custom_federation_provider_url_message());
            }
            Ok(ExternalLoginChange::Cleared)
        }
    }
}

/// The refusal a `CustomFederationProviderUrl` named for anything but a custom OpenID
/// Connect provider reads: that parameter belongs to that provider type alone.
pub fn custom_federation_provider_url_message() -> String {
    format!(
        "CustomFederationProviderUrl is only used when ExternalLoginFederationProviderType \
         is {EXTERNAL_LOGIN_PROVIDER_CUSTOM_OIDC}."
    )
}

/// `values` as a sentence lists them: separated by commas, with the last joined by
/// "and", so a refusal naming what it admits reads as prose.
fn conjunction(values: &[&str]) -> String {
    match values {
        [] => String::new(),
        [only] => (*only).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
