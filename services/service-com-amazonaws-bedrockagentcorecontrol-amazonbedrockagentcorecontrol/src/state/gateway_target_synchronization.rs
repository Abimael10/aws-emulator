// SPDX-License-Identifier: CC0-1.0

//! Synchronizing a gateway target's tool definitions: which targets have
//! definitions to fetch at all, how a target that has none is refused, and the
//! `GatewayTarget` envelope `SynchronizeGatewayTargets` reports one in.
//!
//! Re-exported from [`crate::state`], so a handler names `crate::state::…` for
//! these exactly as it does for the target record they are about.

use super::*;
use crate::ops::types::GatewayTarget;

/// `TargetStatus` for a target whose tool definitions are being fetched from its
/// endpoint. The enum's full value list is
/// [`crate::ops::types::TARGET_STATUS_VALUES`].
pub const STATUS_SYNCHRONIZING: &str = "SYNCHRONIZING";

/// `TargetStatus` for a target an attempted fetch of its tool definitions left
/// unusable: the status a target whose endpoint could not be read reports, with
/// the reason in `statusReasons`.
pub const STATUS_FAILED: &str = "FAILED";

/// Whether this target's tool definitions live AT its endpoint, so a
/// synchronization has something to fetch from there.
///
/// `SynchronizeGatewayTargets` "synchronizes the gateway targets by fetching the
/// latest tool definitions from the target endpoints"
/// (`smithy_query doc shape=SynchronizeGatewayTargets`), so it applies to the one
/// target shape whose tools are discovered from the endpoint rather than declared
/// in the target's own configuration: an MCP SERVER target, which the gateway
/// lists tools from. Every other target type carries its tool definitions in its
/// configuration — an OpenAPI schema, a Smithy model, a Lambda tool schema, an
/// API Gateway tool configuration, a connector's prebuilt tools — or serves no
/// tool listing at all (an HTTP or an inference target), so a fetch from the
/// endpoint would have nothing to read.
///
/// An MCP server is the exception to its own rule when it is pinned to a STATIC
/// tool list, which the model spells out for that one member: "Dynamic tool
/// discovery/synchronization will be disabled when a target is configured with
/// mcpToolSchema"
/// (`smithy_query doc shape=McpServerTargetConfiguration member=mcpToolSchema`).
pub fn target_discovers_tool_definitions(configuration: &TargetConfiguration) -> bool {
    match configuration {
        TargetConfiguration::Mcp(McpTargetConfiguration::mcpServer(server)) => {
            server.mcp_tool_schema.is_none()
        }
        _ => false,
    }
}

/// How a `SynchronizeGatewayTargets` naming a target with no tool definitions to
/// fetch is refused: the modelled `ValidationException` (HTTP 400), worded for
/// whichever of the two reasons applies.
///
/// A target pinned to a static tool list is refused in the words the model
/// documents for exactly that case, remedy included; every other target is
/// refused by the TARGET TYPE it is ([`target_type`]), which is what decides
/// whether synchronization applies to it at all. Nothing is wrong with the
/// request's members — the stored target's own configuration refuses the call —
/// so `reason` is [`VALIDATION_REASON_RESOURCE_CONFLICT`] and there is no
/// `fieldList`.
pub fn target_discovers_no_tool_definitions(
    configuration: &TargetConfiguration,
) -> ValidationException {
    let message = match configuration {
        TargetConfiguration::Mcp(McpTargetConfiguration::mcpServer(_)) => {
            "You cannot synchronize a target that has a static tool schema (mcpToolSchema) \
             configured. Remove the static schema through an UpdateGatewayTarget call to \
             enable dynamic tool synchronization."
                .to_string()
        }
        other => format!(
            "Target type {} is not supported for synchronization",
            target_type(other)
        ),
    };
    ValidationException {
        message,
        reason: VALIDATION_REASON_RESOURCE_CONFLICT.to_string(),
        field_list: None,
    }
}

impl GatewayTargetRecord {
    /// This target as one entry of `SynchronizeGatewayTargets`'s
    /// `targets` list (`GatewayTarget`) — the same members the create, the update
    /// and the read render, off the same record, so the synchronization's own
    /// answer cannot differ from a later read of the target it synchronized.
    pub fn gateway_target(&self, gateway_arn: &str, now: SystemTime) -> GatewayTarget {
        let created = self.create_output(gateway_arn, now);
        GatewayTarget {
            gateway_arn: created.gateway_arn,
            target_id: created.target_id,
            created_at: created.created_at,
            updated_at: created.updated_at,
            status: created.status,
            status_reasons: created.status_reasons,
            name: created.name,
            description: created.description,
            target_configuration: created.target_configuration,
            credential_provider_configurations: created.credential_provider_configurations,
            last_synchronized_at: created.last_synchronized_at,
            metadata_configuration: created.metadata_configuration,
            private_endpoint: created.private_endpoint,
            private_endpoint_managed_resources: created.private_endpoint_managed_resources,
            authorization_data: created.authorization_data,
            protocol_type: created.protocol_type,
            certificate_configurations: created.certificate_configurations,
        }
    }

    /// Record an attempt to fetch this target's tool definitions at `now`: the
    /// instant the fetch was attempted, and the status the attempt puts the target
    /// in.
    ///
    /// The fetch is a LIVE read of the target's own endpoint
    /// ([`fetch_tool_definitions`], whose outcome `fetched` is), and that outcome
    /// is owed by the clock rather than by this call: the target reports
    /// `SYNCHRONIZING` until the span passes, so a caller acting straight after
    /// still sees the fetch under way, and then reports what the fetch achieved —
    /// `READY` for an endpoint that served its tool definitions, `FAILED` carrying
    /// the reason for one that did not, never tool definitions nothing read.
    ///
    /// `lastSynchronizedAt` is the instant of the attempt itself, which is what
    /// the member reports ("The last synchronization time"), and `updatedAt` is
    /// untouched: a synchronization refreshes what the target serves rather than
    /// reconfiguring the target, which is why the shape carries the two instants
    /// separately.
    pub fn synchronize(&mut self, now: SystemTime, fetched: ToolDefinitionFetch) {
        self.last_synchronized_at = Some(now);
        self.transient_status = STATUS_SYNCHRONIZING.to_string();
        self.settled_status = fetched.settled_status.to_string();
        self.settled_status_reasons = fetched.settled_status_reasons;
        self.settles_at = now + GATEWAY_SETTLE_DELAY;
    }
}
