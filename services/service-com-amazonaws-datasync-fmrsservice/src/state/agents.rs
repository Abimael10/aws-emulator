// SPDX-License-Identifier: CC0-1.0

//! The DataSync agents this account has activated, and the ONE resolution every
//! operation that names one shares.
//!
//! An agent is a resource of this account like a location or a task: it exists because an
//! activation stored it, and it stops existing when a delete removes it. Every operation
//! that addresses an agent by its `AgentArn` — the read, the rename and the delete —
//! resolves it through [`ComAmazonawsDatasyncFmrsserviceState::agent`] (or its writing
//! and removing siblings) and refuses an ARN no agent of this account answers to with
//! [`agent_not_found`], so the code and the wording of that refusal are the same wherever
//! an agent is resolved rather than restated per operation.
//!
//! What the record holds is what an activation established: the name the caller asked
//! for, how the agent reaches the service (its `EndpointType`, and the VPC service
//! endpoint members of a private-link activation), when it was activated and the tags it
//! carries. Nothing here is derived from the request's activation key, which identifies
//! the deployed agent to the real service and names no property of the stored resource.

use std::time::SystemTime;

use crate::ops::types::{InvalidRequestException, PrivateLinkConfig, TagListEntry};

use super::{emulator_arn, resource_id, ComAmazonawsDatasyncFmrsserviceState};

/// The ceiling on stored agents. See [`super::MAX_LOCATIONS`] for why each store has one.
pub const MAX_AGENTS: usize = 10_000;

/// The `AgentStatus` an agent this account activated reports.
///
/// The model documents the pair: `ONLINE` is "the agent is configured properly and ready
/// to use", and `OFFLINE` is an agent "out of contact with DataSync for five minutes or
/// longer". An agent is in this store because an activation — a call the deployed agent
/// takes part in — stored it, and nothing in this emulator takes one out of contact
/// afterwards, so a stored agent is the first of the two.
pub const AGENT_STATUS_ONLINE: &str = "ONLINE";

/// The `EndpointType` of an agent that reaches the service over its public endpoint: the
/// activation named no VPC service endpoint.
pub const ENDPOINT_TYPE_PUBLIC: &str = "PUBLIC";

/// The `EndpointType` of an agent that reaches the service over a VPC service endpoint,
/// which is what an activation naming one of the `PrivateLinkConfig` members asks for.
///
/// The remaining two values of the modelled enum describe an agent activated against a
/// FIPS service endpoint, which is a property of the ENDPOINT a request was sent to
/// rather than anything the request states, so no activation this emulator serves reaches
/// them.
pub const ENDPOINT_TYPE_PRIVATE_LINK: &str = "PRIVATE_LINK";

/// An activated DataSync agent: what `CreateAgent` stored and `DescribeAgent` reports.
#[derive(Debug, Clone)]
pub struct Agent {
    pub arn: String,
    pub name: Option<String>,
    /// How the agent reaches the service: [`ENDPOINT_TYPE_PUBLIC`] or
    /// [`ENDPOINT_TYPE_PRIVATE_LINK`].
    pub endpoint_type: String,
    /// The VPC service endpoint members of a private-link activation, and `None` for an
    /// agent that named none.
    pub private_link_config: Option<PrivateLinkConfig>,
    pub tags: Vec<TagListEntry>,
    pub creation_time: SystemTime,
    /// When the agent last communicated with the service.
    ///
    /// An activation is such a communication — it is the deployed agent that supplies the
    /// activation key — so an agent's own activation is the last contact this emulator has
    /// observed from it.
    pub last_connection_time: SystemTime,
}

impl ComAmazonawsDatasyncFmrsserviceState {
    /// Mint the next `AgentArn`: `agent/agent-<17 lowercase alphanumerics>`.
    pub fn mint_agent_arn(&mut self, region: &str) -> String {
        self.next_agent_seq += 1;
        emulator_arn(
            region,
            &format!("agent/agent-{seq:017x}", seq = self.next_agent_seq),
        )
    }

    /// The agent stored under `agent_arn`, or the refusal an ARN no agent of this account
    /// answers to earns.
    pub fn agent(&self, agent_arn: &str) -> Result<&Agent, InvalidRequestException> {
        self.agents
            .get(agent_arn)
            .ok_or_else(|| agent_not_found(agent_arn))
    }

    /// That same agent borrowed for an update to write — refused in the same words as the
    /// reading lookup, so a read and a rename agree on which ARNs resolve.
    pub fn agent_mut(&mut self, agent_arn: &str) -> Result<&mut Agent, InvalidRequestException> {
        self.agents
            .get_mut(agent_arn)
            .ok_or_else(|| agent_not_found(agent_arn))
    }

    /// Remove the agent stored under `agent_arn`, refused in the same words when this
    /// account holds none — so a delete of an agent that is already gone is refused
    /// rather than reported as having removed something.
    pub fn remove_agent(&mut self, agent_arn: &str) -> Result<Agent, InvalidRequestException> {
        self.agents
            .remove(agent_arn)
            .ok_or_else(|| agent_not_found(agent_arn))
    }
}

/// The refusal an `AgentArn` that names no agent this account holds earns, wherever an
/// operation of the agent surface resolves one.
///
/// The agent surface identifies the agent it could not resolve by its bare resource id —
/// the `agent-…` token of the ARN the request carried, the way this service identifies a
/// resource in a refusal ([`resource_id`]) — and states the two reasons an id resolves to
/// nothing: it never named an agent of this account, or the agent it named is deleted. The
/// refusal carries no class discriminator, because this service publishes none for the
/// agent kind (see [`super::tagging::TaggableKind`]).
pub fn agent_not_found(agent_arn: &str) -> InvalidRequestException {
    InvalidRequestException {
        message: Some(format!(
            "{agent} does not exist or has been deleted.",
            agent = resource_id(agent_arn)
        )),
        error_code: None,
        datasync_error_code: None,
    }
}
