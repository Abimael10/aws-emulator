// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for two refusals this one Query front door — Amazon RDS and the Amazon
//! DocumentDB and Amazon Neptune control planes it also serves — words for every operation
//! that raises them.
//!
//! Two rules, each asserted over a TABLE spanning what it generalises over rather than over
//! one request:
//!
//! * a request naming an ENDPOINT PORT outside the range this engine family publishes for one
//!   ("Valid Values: 1150-65535", `crate::state::PORT_RANGE`) is refused HTTP 400
//!   `InvalidParameterValue` with the endpoint-port sentence — the port it would not accept
//!   and then the range an endpoint port has to fall in — whichever member of whichever
//!   operation carried the value: a cluster's `Port`, an instance's `Port`, and the
//!   `DBPortNumber` a modification moves an instance to. The refusal states no member name
//!   and echoes no identifier: its value comes from the request and its bounds from the
//!   published range, so each bound of that range is accepted and the value one past it is
//!   not;
//! * a delete of a DB cluster that still holds member DB instances in a non-deleting state is
//!   refused the modelled 400 `InvalidDBClusterStateFault` with one FIXED sentence that names
//!   neither the cluster nor its members, leaves the cluster as it was, and takes no final DB
//!   cluster snapshot — whichever engine's cluster it is and however many members hold it.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so the assertions
//! read the XML the emulator itself answered with, and every wait is for the CONDITION
//! through the one poll loop this test target shares.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    CLUSTER_STILL_HAS_MEMBERS_MESSAGE, PORT_RANGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};
use crate::lifecycle_poll_loops::poll_until;

/// The Query API error code AWS answers a parameter value it does not accept with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The modelled fault a cluster whose state forbids the call is refused with.
const INVALID_CLUSTER_STATE: &str = "InvalidDBClusterStateFault";

/// The instance class a member of a graph (Amazon Neptune) cluster is ordered with.
const GRAPH_CLASS: &str = "db.r5.large";

/// The cluster the modification and the point-in-time restore rows address.
const STORED_CLUSTER: &str = "emu-port-stored-cl";

/// The instance the instance-level modification and restore rows address.
const STORED_INSTANCE: &str = "emu-port-stored-db";

/// The endpoint-port sentence, built from the range the engine family publishes rather than
/// written out, so the refusal and the range cannot drift. The trailing space closes the
/// sentence.
fn endpoint_port_sentence(port: &str) -> String {
    format!(
        "Invalid endpoint port {port}. Valid range is: {}-{} ",
        PORT_RANGE.start(),
        PORT_RANGE.end()
    )
}

/// One operation that carries an endpoint port: the member it carries it in, and the other
/// members the call needs to be read at all.
struct PortCall {
    case: &'static str,
    action: &'static str,
    /// The member this operation configures the endpoint port with.
    member: &'static str,
    /// Everything else the call carries — its `@required` members, and the source a restore
    /// names. A value refusal is answered before any of those references is resolved, so a
    /// row needs no stored source to be refused for its port.
    rest: &'static [(&'static str, &'static str)],
}

/// Every operation of this front door that takes an endpoint port, with the member each one
/// takes it in. The rows span the cluster and instance surfaces and all three of the shapes
/// that carry a port — a create, a modification and a restore — because the sentence belongs
/// to the port, not to the operation.
const PORT_CALLS: &[PortCall] = &[
    PortCall {
        case: "a graph cluster create",
        action: "CreateDBCluster",
        member: "Port",
        rest: &[
            ("DBClusterIdentifier", "emu-port-graph-cl"),
            ("Engine", "neptune"),
        ],
    },
    PortCall {
        case: "an Aurora cluster create",
        action: "CreateDBCluster",
        member: "Port",
        rest: &[
            ("DBClusterIdentifier", "emu-port-aurora-cl"),
            ("Engine", "aurora-mysql"),
        ],
    },
    PortCall {
        case: "a cluster modification",
        action: "ModifyDBCluster",
        member: "Port",
        rest: &[("DBClusterIdentifier", STORED_CLUSTER)],
    },
    PortCall {
        case: "an instance create",
        action: "CreateDBInstance",
        member: "Port",
        rest: &[
            ("DBInstanceIdentifier", "emu-port-db"),
            ("Engine", "postgres"),
            ("DBInstanceClass", "db.m5.large"),
        ],
    },
    PortCall {
        case: "an instance modification",
        action: "ModifyDBInstance",
        member: "DBPortNumber",
        rest: &[("DBInstanceIdentifier", STORED_INSTANCE)],
    },
    PortCall {
        case: "a read replica create",
        action: "CreateDBInstanceReadReplica",
        member: "Port",
        rest: &[
            ("DBInstanceIdentifier", "emu-port-replica"),
            ("SourceDBInstanceIdentifier", STORED_INSTANCE),
        ],
    },
    PortCall {
        case: "a cluster restore from a snapshot",
        action: "RestoreDBClusterFromSnapshot",
        member: "Port",
        rest: &[
            ("DBClusterIdentifier", "emu-port-restored-cl"),
            ("SnapshotIdentifier", "emu-port-snap"),
            ("Engine", "aurora-mysql"),
        ],
    },
    PortCall {
        case: "a cluster restore to a point in time",
        action: "RestoreDBClusterToPointInTime",
        member: "Port",
        rest: &[
            ("DBClusterIdentifier", "emu-port-pitr-cl"),
            ("SourceDBClusterIdentifier", STORED_CLUSTER),
            // A restore to a point in time has to name the instant it rewinds to, which is a
            // refusal of its own: the row carries one so the PORT is what it is refused for.
            ("UseLatestRestorableTime", "true"),
        ],
    },
    PortCall {
        case: "an instance restore from a snapshot",
        action: "RestoreDBInstanceFromDBSnapshot",
        member: "Port",
        rest: &[
            ("DBInstanceIdentifier", "emu-port-restored-db"),
            ("DBSnapshotIdentifier", "emu-port-db-snap"),
        ],
    },
    PortCall {
        case: "an instance restore to a point in time",
        action: "RestoreDBInstanceToPointInTime",
        member: "Port",
        rest: &[
            ("TargetDBInstanceIdentifier", "emu-port-pitr-db"),
            ("SourceDBInstanceIdentifier", STORED_INSTANCE),
            ("UseLatestRestorableTime", "true"),
        ],
    },
];

/// Ports OUTSIDE the published range, spanning what the range generalises over: the value one
/// below the floor, the smallest ports there are, a value no port can be, and the values at
/// and past the top of the 16-bit port space.
const PORTS_REFUSED: &[(&str, &str)] = &[
    ("one below the floor", "1149"),
    ("the lowest port there is", "1"),
    ("no port at all", "0"),
    ("a negative value", "-1"),
    ("one past the ceiling", "65536"),
    ("far past the ceiling", "70000"),
];

#[tokio::test]
async fn an_endpoint_port_outside_the_published_range_is_refused_with_the_endpoint_port_sentence() {
    let router = emulator();
    stored_cluster_and_instance(&router).await;
    // What the two stored resources report BEFORE any refused call, so the rows below can be
    // held to having changed nothing.
    let cluster_port_before = reported_cluster_port(&router).await;
    let instance_port_before = reported_instance_port(&router).await;

    for row in PORT_CALLS {
        for (value_case, port) in PORTS_REFUSED {
            let case = format!("{}, {value_case}", row.case);
            let mut params: Vec<(&str, &str)> = row.rest.to_vec();
            params.push((row.member, port));
            let body = expect_error(
                &case,
                call(&router, row.action, &params).await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            let message = text(&body, "Message");
            let expected = endpoint_port_sentence(port);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            // The sentence states the port and the range and nothing of the resource: a
            // caller cannot be handed back the identifier it sent.
            for named in row.rest {
                assert!(
                    !message.contains(named.1),
                    "case {case:?}: the refusal must not carry {:?}, got {message:?}",
                    named.1
                );
            }
        }
    }

    // The refused modifications changed nothing: both resources still report the port they
    // reported before them.
    let cluster_port_after = reported_cluster_port(&router).await;
    let instance_port_after = reported_instance_port(&router).await;
    assert_eq!(
        cluster_port_after, cluster_port_before,
        "case \"the cluster the refused modifications named\": expected the port to be \
         unchanged at {cluster_port_before}, got {cluster_port_after}"
    );
    assert_eq!(
        instance_port_after, instance_port_before,
        "case \"the instance the refused modifications named\": expected the port to be \
         unchanged at {instance_port_before}, got {instance_port_after}"
    );
}

#[tokio::test]
async fn each_published_bound_of_the_endpoint_port_range_is_accepted() {
    let router = emulator();

    // Both published bounds and the value one past each of them, on the cluster member and
    // the instance member alike: the bound itself is a port a resource can be created on, and
    // one step outside it is the refusal above.
    let floor = *PORT_RANGE.start();
    let ceiling = *PORT_RANGE.end();
    let rows: [(&str, i32, bool); 4] = [
        ("the floor of the range", floor, true),
        ("one below the floor", floor - 1, false),
        ("the ceiling of the range", ceiling, true),
        ("one past the ceiling", ceiling + 1, false),
    ];

    for (index, (case, port, accepted)) in rows.into_iter().enumerate() {
        let cluster = format!("emu-portbound-cl-{index}");
        let instance = format!("emu-portbound-db-{index}");
        let port = port.to_string();
        let cluster_response = call(
            &router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", cluster.as_str()),
                ("Engine", "neptune"),
                ("Port", port.as_str()),
            ],
        )
        .await;
        let instance_response = call(
            &router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", instance.as_str()),
                ("Engine", "postgres"),
                ("DBInstanceClass", "db.m5.large"),
                ("Port", port.as_str()),
            ],
        )
        .await;

        if accepted {
            expect_ok(case, cluster_response);
            expect_ok(case, instance_response);
            // The accepted create stored the resource: it is there to be read back.
            let described = expect_ok(
                case,
                call(
                    &router,
                    "DescribeDBClusters",
                    &[("DBClusterIdentifier", cluster.as_str())],
                )
                .await,
            );
            assert_eq!(
                text(&described, "DBClusterIdentifier"),
                cluster,
                "case {case:?}: expected the created cluster to be reported, got {described}"
            );
        } else {
            for (surface, response) in [
                ("the cluster create", cluster_response),
                ("the instance create", instance_response),
            ] {
                let case = format!("{case}, {surface}");
                let body = expect_error(&case, response, 400, INVALID_PARAMETER_VALUE);
                let message = text(&body, "Message");
                let expected = endpoint_port_sentence(&port);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
            }
            // Nothing outside the range was created.
            expect_error(
                case,
                call(
                    &router,
                    "DescribeDBClusters",
                    &[("DBClusterIdentifier", cluster.as_str())],
                )
                .await,
                404,
                "DBClusterNotFoundFault",
            );
        }
    }
}

/// One row of the busy-cluster table: the engine whose cluster is deleted, the class its
/// members are ordered with, and how many members hold it.
struct BusyCluster {
    case: &'static str,
    engine: &'static str,
    class: &'static str,
    members: usize,
}

/// The rows the refusal is asserted over: the engines of the control planes this door serves
/// whose clusters keep their members across a delete, and the membership counts that make a
/// cluster busy — one member and more than one.
const BUSY_CLUSTERS: &[BusyCluster] = &[
    BusyCluster {
        case: "a graph cluster with one member",
        engine: "neptune",
        class: GRAPH_CLASS,
        members: 1,
    },
    BusyCluster {
        case: "a graph cluster with two members",
        engine: "neptune",
        class: GRAPH_CLASS,
        members: 2,
    },
    BusyCluster {
        case: "an Aurora cluster with one member",
        engine: "aurora-postgresql",
        class: "db.serverless",
        members: 1,
    },
    BusyCluster {
        case: "a document cluster with one member",
        engine: "docdb",
        class: "db.r5.large",
        members: 1,
    },
];

#[tokio::test]
async fn a_cluster_that_still_holds_members_is_refused_the_delete_and_takes_no_final_snapshot() {
    let router = emulator();

    for (index, row) in BUSY_CLUSTERS.iter().enumerate() {
        let cluster = format!("emu-busy-cl-{index}");
        expect_ok(
            row.case,
            call(
                &router,
                "CreateDBCluster",
                &[
                    ("DBClusterIdentifier", cluster.as_str()),
                    ("Engine", row.engine),
                ],
            )
            .await,
        );
        poll_cluster_until_available(&router, row.case, &cluster).await;

        let members: Vec<String> = (0..row.members)
            .map(|member| format!("emu-busy-db-{index}-{member}"))
            .collect();
        for member in &members {
            expect_ok(
                row.case,
                call(
                    &router,
                    "CreateDBInstance",
                    &[
                        ("DBInstanceIdentifier", member.as_str()),
                        ("Engine", row.engine),
                        ("DBInstanceClass", row.class),
                        ("DBClusterIdentifier", cluster.as_str()),
                    ],
                )
                .await,
            );
        }

        // The delete asks for a final snapshot, which is the shape a teardown sends: the
        // cluster's state is answered before anything is snapshotted.
        let final_snapshot = format!("emu-busy-final-{index}");
        let refused = expect_error(
            row.case,
            call(
                &router,
                "DeleteDBCluster",
                &[
                    ("DBClusterIdentifier", cluster.as_str()),
                    ("SkipFinalSnapshot", "false"),
                    ("FinalDBSnapshotIdentifier", final_snapshot.as_str()),
                ],
            )
            .await,
            400,
            INVALID_CLUSTER_STATE,
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, CLUSTER_STILL_HAS_MEMBERS_MESSAGE,
            "case {:?}: expected the fixed sentence for a cluster that still holds members, \
             got {message:?}",
            row.case
        );
        for member in &members {
            assert!(
                !message.contains(member.as_str()) && !message.contains(cluster.as_str()),
                "case {:?}: the refusal must disclose neither the cluster nor the member \
                 {member}, got {message:?}",
                row.case
            );
        }

        // The refused delete left the cluster as it was …
        let described = expect_ok(
            row.case,
            call(
                &router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", cluster.as_str())],
            )
            .await,
        );
        let status = text(&described, "Status");
        assert_eq!(
            status, "available",
            "case {:?}: expected the refused delete to leave the cluster available, got \
             {status:?}",
            row.case
        );
        // … and took no final snapshot: the identifier it named holds nothing.
        expect_error(
            row.case,
            call(
                &router,
                "DescribeDBClusterSnapshots",
                &[("DBClusterSnapshotIdentifier", final_snapshot.as_str())],
            )
            .await,
            404,
            "DBClusterSnapshotNotFoundFault",
        );

        // Deleting the members is what releases the cluster, which is the order the API
        // documents: the same delete is then served.
        for member in &members {
            expect_ok(
                row.case,
                call(
                    &router,
                    "DeleteDBInstance",
                    &[
                        ("DBInstanceIdentifier", member.as_str()),
                        ("SkipFinalSnapshot", "true"),
                    ],
                )
                .await,
            );
        }
        expect_ok(
            row.case,
            call(
                &router,
                "DeleteDBCluster",
                &[
                    ("DBClusterIdentifier", cluster.as_str()),
                    ("SkipFinalSnapshot", "true"),
                ],
            )
            .await,
        );
    }
}

/// Create the cluster and the instance the modification and restore rows address, so those
/// rows are refused for their PORT rather than for a reference that resolves to nothing.
async fn stored_cluster_and_instance(router: &MultiServiceRouter) {
    expect_ok(
        STORED_CLUSTER,
        call(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", STORED_CLUSTER),
                ("Engine", "aurora-mysql"),
            ],
        )
        .await,
    );
    expect_ok(
        STORED_INSTANCE,
        call(
            router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", STORED_INSTANCE),
                ("Engine", "postgres"),
                ("DBInstanceClass", "db.m5.large"),
            ],
        )
        .await,
    );
}

/// The `Port` [`STORED_CLUSTER`] reports.
async fn reported_cluster_port(router: &MultiServiceRouter) -> i32 {
    let described = expect_ok(
        STORED_CLUSTER,
        call(
            router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", STORED_CLUSTER)],
        )
        .await,
    );
    text(&described, "Port")
        .parse()
        .unwrap_or_else(|_| panic!("the cluster reports a numeric Port, got {described}"))
}

/// The `Endpoint/Port` [`STORED_INSTANCE`] reports, once it has one.
async fn reported_instance_port(router: &MultiServiceRouter) -> i32 {
    let params = [("DBInstanceIdentifier", STORED_INSTANCE)];
    let (described, _) = poll_until(
        STORED_INSTANCE,
        "the instance's endpoint",
        || call(router, "DescribeDBInstances", &params),
        |status, body| status == 200 && !text(body, "Port").is_empty(),
    )
    .await;
    text(&described, "Port")
        .parse()
        .unwrap_or_else(|_| panic!("the instance reports a numeric Port, got {described}"))
}

/// Read `identifier` until the cluster reports `available`, through the one poll loop this
/// test target shares: the CONDITION is waited for, never a fixed span.
async fn poll_cluster_until_available(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let params = [("DBClusterIdentifier", identifier)];
    poll_until(
        case,
        &format!("the cluster {identifier}"),
        || call(router, "DescribeDBClusters", &params),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
}
