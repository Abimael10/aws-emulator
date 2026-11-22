// SPDX-License-Identifier: CC0-1.0

//! The ENDPOINTS of an RDS proxy: the one every proxy publishes without anybody creating
//! it, and the projection the proxy-endpoint report answers with.
//!
//! "RDS automatically creates one endpoint for each DB proxy" (`DBProxyEndpoint`), and that
//! endpoint is the proxy's DEFAULT one — "default DB proxy endpoints always have read/write
//! capability" (`DBProxyEndpoint$IsDefault`). It is therefore no record of its own: every
//! member it reports is a function of the proxy it belongs to — the address that proxy
//! publishes, the network it listens on, the subnets and security groups it was placed in,
//! and the state it is in — so a proxy that was renamed, re-addressed or taken away cannot
//! leave an endpoint reporting what the proxy no longer is.

use super::*;
use crate::ops::types::{DBProxyEndpoint, D_B_PROXY_ENDPOINT_STATUS_VALUES};

/// The ARN resource type of a proxy's endpoint, as AWS writes it under the `rds` namespace.
const ENDPOINT_ARN_RESOURCE_TYPE: &str = "db-proxy-endpoint";

/// `DBProxyEndpointStatus` — "a status of `available` means the endpoint is ready to handle
/// requests".
pub const DB_PROXY_ENDPOINT_STATUS_AVAILABLE: &str = "available";

/// `DBProxyEndpointTargetRole` — "`READ_WRITE`", the role of an endpoint that "can be used
/// for read/write" operations, which is what a proxy's default endpoint always is.
pub const DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE: &str = "READ_WRITE";

/// `DBProxyEndpoint$DBProxyEndpointArn` — "the Amazon Resource Name (ARN) for the DB proxy
/// endpoint", under the resource type AWS gives a proxy endpoint.
pub fn db_proxy_endpoint_arn(region: &str, endpoint_name: &str) -> String {
    emulator_server::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{ENDPOINT_ARN_RESOURCE_TYPE}:{endpoint_name}"),
    )
}

/// The message a `DBProxyEndpointNotFoundFault` carries for a `DBProxyEndpointName` that
/// resolves no endpoint: the fault reports "the DB proxy endpoint doesn't exist", and Amazon
/// RDS words it as a statement about the resource type the API spells `DBProxyEndpoint`,
/// with the requested name in SINGLE QUOTES — the wording and quoting every not-found
/// sentence of the proxy API shares ([`db_proxy_not_found_message`]).
///
/// It lives here rather than in one handler because every operation that resolves a
/// `DBProxyEndpointName` answers the same one fault. `named` is the value the REQUEST
/// carried, which is the spelling an endpoint is reported under too (`DBProxyEndpointName`
/// is not folded).
pub fn db_proxy_endpoint_not_found_message(named: &str) -> String {
    format!("DBProxyEndpoint '{named}' not found.")
}

/// What the endpoint of a proxy in `proxy_status` reports as its own `Status`.
///
/// An endpoint's status list — "available, modifying, incompatible-network,
/// insufficient-resource-limits, creating, deleting" (`DBProxyEndpointStatus`) — is NARROWER
/// than the proxy's, so the proxy's state is read onto it: a proxy still being provisioned
/// is bringing its endpoint up with it, a proxy applying a change is applying it to the
/// endpoint clients reach it through, and a proxy whose delete was accepted takes its
/// endpoint with it, so every status the two vocabularies share is reported as it stands. A
/// proxy status the endpoint's own list does NOT carry leaves the endpoint
/// [`DB_PROXY_ENDPOINT_STATUS_AVAILABLE`], because the endpoint is the address a client
/// connects to and the proxy behind it still answers.
pub fn proxy_endpoint_status(proxy_status: &str) -> &'static str {
    D_B_PROXY_ENDPOINT_STATUS_VALUES
        .iter()
        .find(|shared| **shared == proxy_status)
        .copied()
        .unwrap_or(DB_PROXY_ENDPOINT_STATUS_AVAILABLE)
}

/// The endpoint `proxy` publishes without anybody creating one: the DEFAULT endpoint RDS
/// creates with every DB proxy.
///
/// It carries the proxy's own name, because the default endpoint is the proxy's own address
/// rather than one a caller named, and it is READ/WRITE and `IsDefault` by what
/// `DBProxyEndpoint$IsDefault` documents. Its address is exactly the one the proxy publishes
/// ([`DbProxyRecord::endpoint`]) — a client connecting to the proxy's default endpoint
/// connects to the proxy — and its network, VPC, subnets, security groups and creation
/// instant are the proxy's, since RDS created the two together.
pub fn default_proxy_endpoint(proxy: &DbProxyRecord) -> DBProxyEndpoint {
    DBProxyEndpoint {
        db_proxy_endpoint_name: Some(proxy.name.clone()),
        db_proxy_endpoint_arn: Some(db_proxy_endpoint_arn(&proxy.region, &proxy.name)),
        db_proxy_name: Some(proxy.name.clone()),
        status: Some(proxy_endpoint_status(&proxy.status).to_string()),
        vpc_id: proxy.vpc_id.clone(),
        vpc_security_group_ids: Some(proxy.vpc_security_group_ids.clone()),
        vpc_subnet_ids: Some(proxy.vpc_subnet_ids.clone()),
        endpoint: Some(proxy.endpoint()),
        created_date: Some(proxy.created_date),
        target_role: Some(DB_PROXY_ENDPOINT_TARGET_ROLE_READ_WRITE.to_string()),
        is_default: Some(true),
        endpoint_network_type: Some(proxy.endpoint_network_type.clone()),
    }
}
