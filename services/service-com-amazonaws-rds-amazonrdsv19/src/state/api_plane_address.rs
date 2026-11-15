// SPDX-License-Identifier: CC0-1.0

//! The address a DB cluster or DB instance publishes when its DATA PLANE is an AWS API this
//! emulator serves at its OWN endpoint, rather than a native engine wire on a listener of
//! its own.
//!
//! A cluster's `Endpoint` is "the connection endpoint for the primary instance of the DB
//! cluster" and its `Port` is "the port that the database engine is listening on": the pair
//! is the one address a client connects to. For the engines whose native wire this emulator
//! speaks that address is the resource's own listener ([`super::data_plane`]). An Amazon
//! Neptune cluster's data plane is not a native wire at all: it is the graph API
//! (restJson1, signing name `neptune-db`), which has no regional endpoint of its own and is
//! addressed at the cluster's connection endpoint and nowhere else — and this emulator
//! serves that API on its own endpoint. Such a cluster therefore publishes THAT address:
//! the host this process is reached at, with the port it is served on. A name published
//! under the engine's AWS DNS domain ([`super::endpoint_domain`]) resolves to nothing here,
//! so it would be no connection endpoint at all.
//!
//! WHICH address is this emulator's own is the tree's one reading of that question
//! (`service_common::data_plane_addressing::reached_emulator_host_and_port`), so this
//! service writes no second rule for it; what lives here is only which engines are served
//! that way.

use super::{ServedEndpoint, NEPTUNE_ENGINE};

/// Whether the data plane of `engine` is an AWS API this emulator serves at its own
/// endpoint instead of a native wire of its own.
///
/// Amazon Neptune's is: `CreateDBCluster$Engine` accepts `neptune`, and a Neptune cluster's
/// graph queries, bulk loader, statistics, ML endpoints and fast reset are all operations
/// of its data-plane API, which this emulator serves.
#[must_use]
pub fn engine_data_plane_is_an_api(engine: &str) -> bool {
    engine.eq_ignore_ascii_case(NEPTUNE_ENGINE)
}

/// The address a cluster or instance running `engine` publishes, for an engine whose data
/// plane this emulator serves as an API at its own endpoint: the address this request
/// reached this emulator at.
///
/// `None` for an engine whose data plane is a native wire (the resource's own listener is
/// its address), and for an emulator served on no port at all that no request addressed
/// directly — an in-process call with no socket behind it, where there is no address of this
/// process to publish, so the resource keeps its AWS DNS-form endpoint.
#[must_use]
pub fn api_plane_endpoint(
    engine: &str,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Option<ServedEndpoint> {
    if !engine_data_plane_is_an_api(engine) {
        return None;
    }
    let (host, port) = ::service_common::data_plane_addressing::reached_emulator_host_and_port(
        ctx.parts(),
        ctx.endpoint().as_str(),
    )?;
    Some(ServedEndpoint {
        host,
        port: i32::from(port),
    })
}
