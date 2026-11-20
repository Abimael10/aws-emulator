// SPDX-License-Identifier: CC0-1.0

//! The catalogue entries of the Amazon Bedrock AgentCore family: which provisioner owns each
//! `AWS::BedrockAgentCore::*` resource type a template may declare, beside that family's own
//! type names.
//!
//! One file of [`super`]'s catalogue, reached from [`super::provisioner_for`] for every type
//! its own arms do not name, so adding a type of this family is a change to THIS file and the
//! arms it points at ([`super::bedrock_agentcore`], [`super::bedrock_agentcore_gateways`])
//! rather than to the module root. The names live here too because the registry of recognised
//! types, the return-value catalogue and the primary-identifier table each key an entry by
//! them, and a name and the arm that owns it belong together.

use super::*;

/// The Amazon Bedrock AgentCore resource types a template declares, each created in that
/// control plane by its own API. Every one of them is addressed by an identifier its create
/// MINTS — an `agentRuntimeId`, a `memoryId`, a `gatewayId`, a `targetId` beside its gateway,
/// a `codeInterpreterId` or a `browserId` — which is why
/// [`super::super::template::identity_is_service_assigned`] names all six.
///
/// (`AWS::BedrockAgentCore::WorkloadIdentity` is not here: the name IS its handle, so it is
/// the physical id the naming rules already compute, and its own arms live beside the rest of
/// that service's platform resources — [`super::BEDROCK_AGENTCORE_WORKLOAD_IDENTITY`].)
pub(in crate::state) const BEDROCK_AGENTCORE_RUNTIME: &str = "AWS::BedrockAgentCore::Runtime";
pub(in crate::state) const BEDROCK_AGENTCORE_MEMORY: &str = "AWS::BedrockAgentCore::Memory";
pub(in crate::state) const BEDROCK_AGENTCORE_GATEWAY: &str = "AWS::BedrockAgentCore::Gateway";
pub(in crate::state) const BEDROCK_AGENTCORE_GATEWAY_TARGET: &str =
    "AWS::BedrockAgentCore::GatewayTarget";
pub(in crate::state) const BEDROCK_AGENTCORE_CODE_INTERPRETER: &str =
    "AWS::BedrockAgentCore::CodeInterpreter";
pub(in crate::state) const BEDROCK_AGENTCORE_BROWSER: &str = "AWS::BedrockAgentCore::Browser";

/// The provisioner that owns one `AWS::BedrockAgentCore::*` resource type, or `None` for a
/// type that is not one of this family's — which is what leaves every other type to the
/// answer [`super::provisioner_for`] already gave it.
///
/// Each sandbox value names which of the two sandbox types it is, so the one pair of arms
/// serves both; the catalogue still lists every provisioned type here.
pub(super) fn agentcore_provisioner_for(resource_type: &str) -> Option<Provisioner> {
    match resource_type {
        BEDROCK_AGENTCORE_RUNTIME => Some(Provisioner::BedrockAgentCoreRuntime),
        BEDROCK_AGENTCORE_MEMORY => Some(Provisioner::BedrockAgentCoreMemory),
        BEDROCK_AGENTCORE_GATEWAY => Some(Provisioner::BedrockAgentCoreGateway),
        BEDROCK_AGENTCORE_GATEWAY_TARGET => Some(Provisioner::BedrockAgentCoreGatewayTarget),
        BEDROCK_AGENTCORE_CODE_INTERPRETER => Some(Provisioner::BedrockAgentCoreSandbox(
            bedrock_agentcore_gateways::Sandbox::CodeInterpreter,
        )),
        BEDROCK_AGENTCORE_BROWSER => Some(Provisioner::BedrockAgentCoreSandbox(
            bedrock_agentcore_gateways::Sandbox::Browser,
        )),
        _ => None,
    }
}
