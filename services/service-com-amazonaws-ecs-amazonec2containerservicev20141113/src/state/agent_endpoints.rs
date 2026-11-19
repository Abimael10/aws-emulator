// SPDX-License-Identifier: CC0-1.0

//! The endpoints `DiscoverPollEndpoint` ANNOUNCES to the Amazon ECS container agent: where it
//! polls for the work assigned to its container instance, where it submits that instance's
//! telemetry, and where it polls for Service Connect configuration.
//!
//! All three are addresses the AGENT DIALS afterwards — `DiscoverPollEndpoint` "[r]eturns an
//! endpoint for the Amazon ECS agent to poll for updates", `endpoint` is "[t]he endpoint for
//! the Amazon ECS agent to poll", `telemetryEndpoint` "[t]he telemetry endpoint for the Amazon
//! ECS agent" and `serviceConnectEndpoint` "[t]he endpoint for the Amazon ECS agent to poll for
//! Service Connect configuration". So each has to be an address THIS emulator answers on: a
//! host of the real service's own endpoint family resolves to nothing on the machine this
//! emulator runs on or, worse, to genuine AWS infrastructure, and an agent handed one leaves the
//! emulated environment carrying the credentials it holds.
//!
//! The requirement is therefore RELATIONAL — whatever address this emulator is reached at is the
//! address it announces — so the value comes from
//! [`::smithy_server_runtime::OperationContext::endpoint`], "the base URL a later request
//! addresses… Callers send normally signed requests to it", rather than from a per-Region host
//! template. The Region a request was signed for does not enter it: one process answers on one
//! address, whichever Region a caller signs for, and this emulator serves every Region's Amazon
//! ECS surface there — including the agent's own calls.
//!
//! Amazon ECS answers the three on three different hosts, which one endpoint cannot do, so each
//! member here carries that one address: the only endpoint a client on this machine can reach,
//! and the one this process actually serves. The three are kept as three readings rather than
//! one so each member's own contract is stated where it is answered.

/// The endpoint this service announces to the agent: the base URL this emulator is served at,
/// as the BASE URL every one of these members is documented and recorded as — an address the
/// agent appends its own path to, with the trailing `/` that makes it one.
///
/// `OperationContext::endpoint` reports that base as `scheme://host[:port]` and nothing else
/// (`EndpointUrl::parse` admits no path, query or fragment), so the announced URL carries the
/// scheme and authority this deployment is actually reached at and nothing is parsed out of it.
fn announced_agent_endpoint(emulator_endpoint: &str) -> String {
    format!("{}/", emulator_endpoint.trim_end_matches('/'))
}

/// "The endpoint for the Amazon ECS agent to poll", on the address this emulator is reached at
/// ([`announced_agent_endpoint`]).
pub fn agent_poll_endpoint(emulator_endpoint: &str) -> String {
    announced_agent_endpoint(emulator_endpoint)
}

/// "The telemetry endpoint for the Amazon ECS agent", on the address this emulator is reached at
/// ([`announced_agent_endpoint`]).
pub fn agent_telemetry_endpoint(emulator_endpoint: &str) -> String {
    announced_agent_endpoint(emulator_endpoint)
}

/// "The endpoint for the Amazon ECS agent to poll for Service Connect configuration", on the
/// address this emulator is reached at ([`announced_agent_endpoint`]).
pub fn agent_service_connect_endpoint(emulator_endpoint: &str) -> String {
    announced_agent_endpoint(emulator_endpoint)
}
