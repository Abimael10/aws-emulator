// SPDX-License-Identifier: CC0-1.0

//! The ESTATE the Reachability Analyzer suites analyse paths over: one resource of every kind
//! an end of a path may name, held under the ids those suites write out.
//!
//! `CreateNetworkInsightsPath` resolves each end it is given — "the ID or ARN of the source.
//! If the resource is in another account, you must specify an ARN"
//! (`CreateNetworkInsightsPathRequest$Source`), so a bare id names a resource of THIS account
//! and has to be one the account holds — and the suites that drive the path surface name their
//! ends by literal ids rather than by whatever a create happened to mint. This module makes
//! the account hold a resource under each of those ids, so those suites assert what they are
//! about (the path, its members, its ARNs, its idempotency) rather than the estate behind it;
//! `super::network_insights_path_endpoints` is the suite that owns the resolution rule itself.
//!
//! The kinds are the catalog this crate screens an end against
//! ([`NETWORK_INSIGHTS_ENDPOINT_TYPES`]) rather than a list written out here, so a kind added
//! to that catalog is held too. Each record is stored through the store the service itself
//! keeps that kind in, carrying the id it is keyed by and nothing else a path reads.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, TransitGatewayAttachmentRecord,
    TransitGatewayAttachmentView, VpcPeeringConnectionRecord, VpnGatewayRecord,
    NETWORK_INSIGHTS_ENDPOINT_TYPES,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::ops::types::{
    InternetGateway, NetworkInterface, ServiceConfiguration, TransitGateway,
    TransitGatewayPeeringAttachment, VpcEndpoint, VpcPeeringConnection, VpnGateway,
};

use super::ec2_query::{call, elements};
use super::wire::param;

/// The AMI the seeded launch names. The launch path stores the id it is given, so an id of the
/// long form is all a launch needs.
const AMI: &str = "ami-0123456789abcdef0";

/// The id suffixes the path suites name their ends by: the two long-form values those tables
/// write out, and the short id form EC2 has also issued ids in.
const SUFFIXES: [&str; 3] = ["0123456789abcdef0", "0fedcba9876543210", "01234567"];

/// Launch one instance in `region` and answer its id — the one record of a kind this module
/// cannot write by hand, so it is minted by the operation that mints instances and then held
/// under each instance id the suites name.
async fn launch_one(router: &MultiServiceRouter, region: &str) -> String {
    let (status, body) = call(
        router,
        region,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("MinCount", "1"),
            param("MaxCount", "1"),
            param("InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the estate's launch must succeed, got {status} with body {body}"
    );
    elements(&body, "instanceId")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the launch must report an instance id: {body}"))
}

/// Make the account hold one resource of every kind an end of a path may name, under every id
/// the path suites name an end by.
pub(super) async fn hold_path_endpoints(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    region: &str,
) {
    let launched = launch_one(router, region).await;
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let instance = guard
        .instances
        .get(&launched)
        .expect("the instance the estate just launched is in the store")
        .clone();

    for suffix in SUFFIXES {
        for (resource_type, prefix) in NETWORK_INSIGHTS_ENDPOINT_TYPES {
            let id = format!("{prefix}-{suffix}");
            match resource_type {
                "instance" => {
                    let mut held = instance.clone();
                    held.instance_id = id.clone();
                    guard.instances.insert(id, held);
                }
                "network-interface" => {
                    guard.network_interfaces.insert(
                        id.clone(),
                        NetworkInterface {
                            network_interface_id: Some(id),
                            ..Default::default()
                        },
                    );
                }
                "internet-gateway" => {
                    guard.internet_gateways.insert(
                        id.clone(),
                        InternetGateway {
                            internet_gateway_id: Some(id),
                            attachments: Some(Vec::new()),
                            ..Default::default()
                        },
                    );
                }
                "vpn-gateway" => {
                    guard.vpn_gateways.insert(
                        id.clone(),
                        VpnGatewayRecord {
                            view: VpnGateway {
                                vpn_gateway_id: Some(id),
                                vpc_attachments: Some(Vec::new()),
                                ..Default::default()
                            },
                            pending: None,
                        },
                    );
                }
                "vpc-endpoint" => {
                    guard.vpc_endpoints.insert(
                        id.clone(),
                        VpcEndpoint {
                            vpc_endpoint_id: Some(id),
                            ..Default::default()
                        },
                    );
                }
                "vpc-endpoint-service" => {
                    guard.vpc_endpoint_services.insert(
                        id.clone(),
                        ServiceConfiguration {
                            service_id: Some(id),
                            ..Default::default()
                        },
                    );
                }
                "vpc-peering-connection" => {
                    guard.vpc_peering_connections.insert(
                        id.clone(),
                        VpcPeeringConnectionRecord {
                            view: VpcPeeringConnection {
                                vpc_peering_connection_id: Some(id),
                                ..Default::default()
                            },
                            pending: None,
                        },
                    );
                }
                "transit-gateway" => {
                    guard.transit_gateways.insert(
                        id.clone(),
                        TransitGateway {
                            transit_gateway_id: Some(id),
                            ..Default::default()
                        },
                    );
                }
                "transit-gateway-attachment" => {
                    guard.transit_gateway_attachments.insert(
                        id.clone(),
                        TransitGatewayAttachmentRecord {
                            view: TransitGatewayAttachmentView::Peering(
                                TransitGatewayPeeringAttachment {
                                    transit_gateway_attachment_id: Some(id),
                                    ..Default::default()
                                },
                            ),
                            pending: None,
                        },
                    );
                }
                other => panic!(
                    "the estate holds no resource of the kind {other:?}, which the endpoint \
                     catalog names"
                ),
            }
        }
    }
}
