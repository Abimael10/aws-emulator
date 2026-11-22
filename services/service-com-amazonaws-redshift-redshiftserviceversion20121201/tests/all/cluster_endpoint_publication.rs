// SPDX-License-Identifier: CC0-1.0

//! When a cluster publishes its connection endpoint, end to end over the
//! awsQuery wire.
//!
//! The rule under test: **a cluster reports `Cluster$Endpoint` only once it is
//! serving — while its `ClusterStatus` is `creating` the member is absent
//! altogether, and from the moment the cluster is `available` it carries the
//! address a client connects to, the port its database is listening on and the
//! VPC interface endpoint it is reached through.** That is what stops a client
//! which polls `DescribeClusters` and connects as soon as an endpoint appears
//! from dialling a cluster that is not up yet — and what it then dials is the
//! socket this emulator serves that cluster's database on, which
//! [`crate::cluster_engine_endpoint`] opens a session against.
//!
//! The address and the port are the LISTENER's own — the core owns the port
//! range its listeners are bound in, so what a cluster publishes is the port its
//! database engine really answers on rather than the one a request asked for —
//! and the endpoint is proved by dialling it, not by recomputing it.
//!
//! Every row drives a real HTTP request through the same `MultiServiceRouter` the
//! production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! The rows differ along what the rule generalises over: the cluster's shape
//! (single-node and multi-node), whether the request names the port or takes the
//! default, and whether it names the Availability Zone the endpoint's interface
//! then lives in. The endpoint's own VALUES are computed from the row's
//! declaration through the service's published derivations, never read back out of
//! the answer being checked.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Emulator, REGION};

/// The members of a `Cluster$Endpoint` — the whole of what a cluster that is not
/// serving must report NONE of.
///
/// Source: `smithy_query(query="shape", shape="Endpoint")` (`Address`, `Port`,
/// `VpcEndpoints`), with the two elements a `VpcEndpoint` and its
/// `NetworkInterface` carry of their own.
const ENDPOINT_ELEMENTS: [&str; 5] = [
    "Address",
    "Port",
    "VpcEndpointId",
    "NetworkInterfaceId",
    "PrivateIpAddress",
];

/// One row: the cluster the case creates, and what its endpoint must carry once
/// the cluster is serving.
struct Row {
    case: &'static str,
    identifier: &'static str,
    /// The members beyond the three required ones.
    params: Vec<(&'static str, &'static str)>,
    /// The Availability Zone its endpoint's network interface must be in, or
    /// `None` where the service chooses the zone.
    zone: Option<&'static str>,
}

#[tokio::test]
async fn a_cluster_publishes_its_endpoint_only_once_it_is_serving() {
    let emulator = Emulator::new();

    let rows: Vec<Row> = vec![
        Row {
            case: "a single-node cluster on the default port",
            identifier: "endpoint-default",
            params: vec![("ClusterType", "single-node")],
            zone: None,
        },
        Row {
            case: "a multi-node cluster on a port the request names",
            identifier: "endpoint-ported",
            params: vec![
                ("ClusterType", "multi-node"),
                ("NumberOfNodes", "2"),
                ("Port", "5455"),
            ],
            zone: None,
        },
        Row {
            case: "a cluster in a zone the request names",
            identifier: "endpoint-zoned",
            params: vec![
                ("ClusterType", "single-node"),
                ("Port", "8192"),
                ("AvailabilityZone", "us-west-2c"),
            ],
            zone: Some("us-west-2c"),
        },
    ];

    // CREATING: not one element of the endpoint is reported, by the create itself
    // or by a read taken while the cluster is still coming up.
    for row in &rows {
        let mut params: Vec<(&str, &str)> = vec![
            ("ClusterIdentifier", row.identifier),
            ("NodeType", "ra3.xlplus"),
            ("MasterUsername", "admin"),
        ];
        params.extend(row.params.iter().copied());
        let created = emulator.call("CreateCluster", &params).await;
        assert_eq!(
            created.status, 200,
            "case {}: expected 200, got {} with body {}",
            row.case, created.status, created.body
        );
        assert_eq!(
            created.text("ClusterStatus").as_deref(),
            Some(state::STATUS_CREATING),
            "case {}: expected the transient creating status, got {:?}",
            row.case,
            created.text("ClusterStatus")
        );
        for element in ENDPOINT_ELEMENTS {
            assert_eq!(
                created.text(element),
                None,
                "case {}: expected no {element} while the cluster is creating, got {:?}",
                row.case,
                created.text(element)
            );
        }
        let described = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", row.identifier)])
            .await;
        if described.text("ClusterStatus").as_deref() == Some(state::STATUS_CREATING) {
            for element in ENDPOINT_ELEMENTS {
                assert_eq!(
                    described.text(element),
                    None,
                    "case {}: expected a read of a creating cluster to report no {element}, got {:?}",
                    row.case,
                    described.text(element)
                );
            }
        }
    }

    // SERVING: every row's cluster reaches `available` and reports the whole
    // endpoint — the same values a second read reports, because the endpoint was
    // decided when the cluster was allocated. Each row waits for the CONDITION
    // (the status a client has to see before it connects) through the harness's
    // own poll loop rather than for a fixed time.
    for row in &rows {
        let (serving, _polls) = emulator
            .poll_until(
                row.case,
                "DescribeClusters",
                &[("ClusterIdentifier", row.identifier)],
                |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
            )
            .await;
        assert_eq!(
            serving.text("ClusterStatus").as_deref(),
            Some(state::STATUS_AVAILABLE),
            "case {}: expected available once the cluster is up, got {:?}",
            row.case,
            serving.text("ClusterStatus")
        );
        // The address and port a cluster publishes are the ones this emulator
        // SERVES its database on: the core binds that listener and assigns its
        // port, so what the endpoint carries is an address a client of this
        // emulator can reach rather than a name only AWS resolves.
        assert_eq!(
            serving.text("Address").as_deref(),
            Some("127.0.0.1"),
            "case {}: expected the address this emulator serves the cluster on, got {:?}",
            row.case,
            serving.text("Address")
        );
        let port = serving
            .text("Port")
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or_else(|| {
                panic!(
                    "case {}: expected the endpoint port once serving, got body {}",
                    row.case, serving.body
                )
            });
        assert!(
            port != 0,
            "case {}: expected the port the cluster's listener is bound to, got {port}",
            row.case
        );
        // A client can open a connection there, which is what publishing an
        // endpoint promises; what the engine answers on it is
        // [`crate::cluster_engine_endpoint`]'s subject.
        let dialled = ::tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
        assert!(
            dialled.is_ok(),
            "case {}: expected the published endpoint 127.0.0.1:{port} to accept a connection, \
             got {dialled:?}",
            row.case
        );
        // The VPC interface endpoint the cluster is reached through, in the
        // cluster's own VPC and zone. The VPC is the one the cluster's subnet
        // group reports — the VPC its subnets are in, as the service that owns
        // subnets answered for them — asked of that group rather than computed
        // here.
        let group = emulator
            .call(
                "DescribeClusterSubnetGroups",
                &[(
                    "ClusterSubnetGroupName",
                    state::DEFAULT_CLUSTER_SUBNET_GROUP,
                )],
            )
            .await;
        let vpc_id = group.text("VpcId").unwrap_or_else(|| {
            panic!(
                "case {}: the default subnet group must report its VPC, got body {}",
                row.case, group.body
            )
        });
        assert_eq!(
            serving.text("VpcId").as_deref(),
            Some(vpc_id.as_str()),
            "case {}: expected the cluster in its subnet group's VPC {vpc_id:?}, got {:?}",
            row.case,
            serving.text("VpcId")
        );
        let zone = row.zone.map(str::to_string).unwrap_or_else(|| {
            state::default_availability_zone(row.identifier, REGION)
        });
        let expected = state::cluster_vpc_endpoints(row.identifier, &vpc_id, &zone);
        let expected_endpoint = expected
            .first()
            .expect("the service derives one VPC endpoint per cluster");
        assert_eq!(
            serving.text("VpcEndpointId"),
            expected_endpoint.vpc_endpoint_id,
            "case {}: expected the derived VPC endpoint id, got {:?}",
            row.case,
            serving.text("VpcEndpointId")
        );
        let zones = all_text(&serving.body, "AvailabilityZone");
        assert_eq!(
            zones,
            vec![zone.clone(), zone.clone()],
            "case {}: expected the endpoint's interface to be in the cluster's own zone, got {zones:?}",
            row.case
        );

        let again = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", row.identifier)])
            .await;
        assert_eq!(
            (again.text("Address"), again.text("Port")),
            (serving.text("Address"), serving.text("Port")),
            "case {}: expected two reads to report one endpoint, got {:?} then {:?}",
            row.case,
            (serving.text("Address"), serving.text("Port")),
            (again.text("Address"), again.text("Port"))
        );
    }
}
