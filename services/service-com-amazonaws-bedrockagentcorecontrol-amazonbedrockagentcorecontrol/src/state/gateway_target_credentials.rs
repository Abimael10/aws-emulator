// SPDX-License-Identifier: CC0-1.0

//! The credential provider configuration a gateway target carries: what a
//! request that defines none leaves the target with, and which target
//! configurations have to be given one at all.
//!
//! Re-exported from [`crate::state`], so a handler names `crate::state::…` for
//! these exactly as it does for the target record they are about.

use super::*;

/// The credential a gateway target reports when the CREATE configured none: the
/// gateway's own IAM role.
///
/// `credentialProviderConfigurations` is optional on `CreateGatewayTarget` and
/// `@required` with at least one entry on every response that reports a target,
/// so the stored target carries this one rather than an empty list the wire
/// cannot render.
pub fn effective_credential_provider_configurations(
    requested: Option<Vec<CredentialProviderConfiguration>>,
) -> Vec<CredentialProviderConfiguration> {
    match requested {
        Some(configured) if !configured.is_empty() => configured,
        _ => vec![CredentialProviderConfiguration {
            credential_provider_type: CREDENTIAL_PROVIDER_GATEWAY_IAM_ROLE.to_string(),
            credential_provider: None,
        }],
    }
}

/// What a request DEFINED for `credentialProviderConfigurations`: the list it
/// carried, or `None` for a request that defined none.
///
/// A list with no entry defines nothing — the member's own `@length` is exactly
/// one entry (`smithy_query shape=CredentialProviderConfigurations`), so an
/// empty one never reaches a handler, and reading it as "defined" would store a
/// list the wire cannot render.
pub fn defined_credential_provider_configurations(
    requested: Option<Vec<CredentialProviderConfiguration>>,
) -> Option<Vec<CredentialProviderConfiguration>> {
    requested.filter(|configured| !configured.is_empty())
}

/// Whether a target configuration has to be given a credential provider
/// configuration of its own, so a request that defines none for it is refused
/// ([`member_not_defined`]) rather than left to carry the target's stored one
/// forward.
///
/// An MCP target is one the GATEWAY ITSELF calls: it invokes the API the
/// configuration describes — an OpenAPI schema, a Smithy model, a Lambda
/// function, an MCP server, an API Gateway stage, a connector — so it has to be
/// told what to authenticate to that API as, and this service's recorded
/// refusals show exactly that request turned down with the member's own
/// sentence.
///
/// The other two configuration shapes authenticate nothing of their own: an
/// HTTP target "forwards requests directly to an external HTTP endpoint" or
/// routes them to an AgentCore Runtime of this account, and an inference target
/// "routes requests to a large language model (LLM) provider" through the
/// connector or provider its own configuration names
/// (`smithy_query shape=HttpTargetConfiguration,InferenceTargetConfiguration`),
/// so a request that defines no credential provider for one of those is served
/// and the target keeps the configuration it already carries.
pub fn credential_provider_configurations_must_be_defined(
    configuration: &TargetConfiguration,
) -> bool {
    matches!(configuration, TargetConfiguration::Mcp(_))
}
