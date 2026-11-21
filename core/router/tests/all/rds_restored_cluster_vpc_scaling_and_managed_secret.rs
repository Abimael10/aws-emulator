// SPDX-License-Identifier: CC0-1.0

//! An Aurora DB cluster RESTORED from a cluster snapshot, driven the way a client that placed
//! the source cluster in its own VPC drives it, on the ASSEMBLED emulator.
//!
//! RULE: every status an Amazon RDS resource is answered in while the service is still working
//! on it is a TRANSIENT status the clock settles, so each one reaches its terminal value in
//! finite time with no further call and WHATEVER optional members the request that started it
//! carried — a DB cluster snapshot reaches `available`, a restored DB cluster reaches
//! `available`, the managed master user secret `ModifyDBCluster(ManageMasterUserPassword)`
//! asks for reaches `active`, a DB instance added to the restored cluster reaches `available`,
//! and the Data API answers on the restored cluster once `EnableHttpEndpoint` has turned it
//! on. Every member the restore named is the restored cluster's own configuration from then
//! on — not the source's — and the data the restored cluster serves is the data its snapshot
//! held.
//!
//! `rds_cluster_snapshot_restore_lifecycle.rs` drives the same lifecycle with the bare
//! required members and in one fixed order. This suite is the same rule over the REST of the
//! inputs a client sends: a cluster placed on a DB subnet group over subnets Amazon EC2
//! allocated and in an EC2 security group, restored into a DIFFERENT subnet group and security
//! group with a DIFFERENT capacity range, deletion protection and tags of its own, with the
//! writer added before or after the Data API endpoint is turned on. None of those members may
//! leave a status short of its terminal value, and each one the restore named — rather than
//! the source's — is what the restored cluster reports. The calls and the polls both suites
//! drive the lifecycle by live in [`crate::rds_cluster_lifecycle_calls`].
//!
//! The suite lives beside the router because the lifecycle spans four services: Amazon EC2
//! allocates the VPC, the subnets and the security groups the clusters are placed in, Amazon
//! RDS holds the cluster, the snapshot and the instance, Secrets Manager holds the managed
//! master user credential, and the RDS Data API owns the rows read back. A crate-local suite
//! can only ever see one of the four.
//!
//! The rows differ along what the rule generalises over: which optional members the restore
//! carries (the DB subnet group and VPC security group it is placed in, its Aurora Serverless
//! v2 capacity range, deletion protection, tags — or none of them), and the ORDER the client
//! drives the post-restore sequence in (the writer DB instance before `EnableHttpEndpoint`, or
//! after it).
//!
//! Sources: `smithy_query(query="shape", shape="RestoreDBClusterFromSnapshotMessage")` on the
//! rds model for the members a restore carries (`DBSubnetGroupName`, `VpcSecurityGroupIds`,
//! `ServerlessV2ScalingConfiguration`, `DeletionProtection`, `Tags`),
//! `smithy_query(query="shape", shape="MasterUserSecret")` for `SecretStatus`, and
//! `smithy_query(query="operation", operation="ExecuteStatement")` on the rdsdata model for
//! the call that reads the rows back.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, blocks_of, element, opt_element};
use crate::rds_cluster_lifecycle_calls::{
    add_writer, authorization, await_available_cluster, await_settled_cluster, build_router,
    create_source_cluster, query, served, take_snapshot, Addressed, DATABASE, ENGINE, MASTER_USER,
    REGION, SOURCE_MAX_CAPACITY, SOURCE_MIN_CAPACITY, TABLE,
};

/// The Aurora Serverless v2 capacity range a restore that names one is given: DIFFERENT from
/// the range the source cluster carries ([`SOURCE_MIN_CAPACITY`] / [`SOURCE_MAX_CAPACITY`]), so
/// what the restored cluster reports tells the member the restore NAMED from the one it would
/// have inherited from the snapshot.
const MIN_CAPACITY: &str = "1.0";
const MAX_CAPACITY: &str = "4.0";

/// The port an Aurora PostgreSQL cluster listens on, which is what the clusters' security
/// groups admit ("RDS for PostgreSQL and Aurora PostgreSQL - 5432").
const ENGINE_PORT: &str = "5432";

/// `VpcSecurityGroupMembership$Status` of a group a cluster really belongs to.
const MEMBERSHIP_ACTIVE: &str = "active";

/// The tag a restore that names one carries.
const TAG_KEY: &str = "workload";
const TAG_VALUE: &str = "snapshot-restore";

/// One Amazon EC2 call that must be served, answered with its XML body.
async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "ec2", "2016-11-15", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} with {body}"
    );
    body
}

/// One Secrets Manager call, answered with the HTTP status and the parsed body.
async fn secrets(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Where a cluster is placed: the DB subnet group over subnets Amazon EC2 allocated, and the
/// EC2 security group admitting the engine's port.
#[derive(Debug, Clone)]
struct Placement {
    subnet_group: String,
    security_group: String,
}

/// The network the clusters below are placed in: TWO placements over one VPC, so a restore
/// names a DB subnet group and a security group the source cluster is not in.
#[derive(Debug, Clone)]
struct Network {
    source: Placement,
    restore: Placement,
}

/// Allocate one placement over `subnets`: a security group with an ingress rule on the
/// engine's port, and a DB subnet group of its own over those subnets.
async fn allocate_placement(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    subnets: &[String],
    name: &str,
) -> Placement {
    let group = ec2_ok(
        router,
        case,
        "CreateSecurityGroup",
        &[
            ("GroupName", name),
            ("GroupDescription", "a group the clusters belong to"),
            ("VpcId", vpc_id),
        ],
    )
    .await;
    let security_group = element(&group, "groupId");
    ec2_ok(
        router,
        case,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", security_group.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", ENGINE_PORT),
            ("IpPermissions.1.ToPort", ENGINE_PORT),
            ("IpPermissions.1.IpRanges.1.CidrIp", "10.61.0.0/16"),
        ],
    )
    .await;
    let subnet_group = format!("{name}-subnets");
    served(
        router,
        case,
        "CreateDBSubnetGroup",
        &[
            ("DBSubnetGroupName", subnet_group.as_str()),
            ("DBSubnetGroupDescription", "a placement for one cluster"),
            ("SubnetIds.SubnetIdentifier.1", subnets[0].as_str()),
            ("SubnetIds.SubnetIdentifier.2", subnets[1].as_str()),
        ],
    )
    .await;
    Placement {
        subnet_group,
        security_group,
    }
}

/// Allocate that network: a VPC, two subnets in different Availability Zones, and the two
/// placements over them.
async fn allocate_network(router: &MultiServiceRouter, case: &str) -> Network {
    let vpc = ec2_ok(router, case, "CreateVpc", &[("CidrBlock", "10.61.0.0/16")]).await;
    let vpc_id = element(&vpc, "vpcId");
    let mut subnets = Vec::new();
    for (index, zone) in ["us-west-2a", "us-west-2b"].iter().enumerate() {
        let subnet = ec2_ok(
            router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", vpc_id.as_str()),
                ("CidrBlock", &format!("10.61.{index}.0/24")),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&subnet, "subnetId"));
    }
    Network {
        source: allocate_placement(router, case, &vpc_id, &subnets, "emu-restore-source").await,
        restore: allocate_placement(router, case, &vpc_id, &subnets, "emu-restore-target").await,
    }
}

/// The credentials a secret holds, as `(username, password)`.
async fn held_credentials(
    router: &MultiServiceRouter,
    case: &str,
    secret_arn: &str,
) -> (String, String) {
    let (status, read) = secrets(router, "GetSecretValue", &json!({ "SecretId": secret_arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetSecretValue({secret_arn:?}) must be served, got {status} with {read}"
    );
    let document: Value = serde_json::from_str(
        read.get("SecretString")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .unwrap_or_else(|_| {
        panic!("case {case:?}: the managed secret holds a credentials document: {read}")
    });
    let text = |member: &str| {
        document
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    (text("username"), text("password"))
}

/// The tags a resource carries, as the map a client reads them as.
async fn tags_of(router: &MultiServiceRouter, case: &str, arn: &str) -> BTreeMap<String, String> {
    let listed = served(
        router,
        case,
        "ListTagsForResource",
        &[("ResourceName", arn)],
    )
    .await;
    blocks_of(&listed, "Tag")
        .into_iter()
        .filter_map(|tag| Some((opt_element(tag, "Key")?, opt_element(tag, "Value")?)))
        .collect()
}

/// The Aurora Serverless v2 capacity range a cluster reports, as `(MinCapacity, MaxCapacity)`.
fn reported_capacity(cluster: &str) -> (Option<String>, Option<String>) {
    let scaling = block_of(cluster, "ServerlessV2ScalingConfiguration").unwrap_or_default();
    let number = |member: &str| {
        opt_element(scaling, member)
            .and_then(|text| text.parse::<f64>().ok())
            .map(|capacity| format!("{capacity:.1}"))
    };
    (number("MinCapacity"), number("MaxCapacity"))
}

/// The VPC security group memberships a cluster reports, as the id -> status map a client
/// reads them as.
fn reported_memberships(cluster: &str) -> BTreeMap<String, String> {
    blocks_of(cluster, "VpcSecurityGroupMembership")
        .into_iter()
        .filter_map(|block| {
            Some((
                opt_element(block, "VpcSecurityGroupId")?,
                opt_element(block, "Status")?,
            ))
        })
        .collect()
}

/// One row: a restore of the same snapshot, differing in the members it carries and in the
/// order the client drives the sequence that follows it.
struct Row {
    case: &'static str,
    restored: &'static str,
    writer: &'static str,
    /// Whether the restore names the DB subnet group, the VPC security group, the Aurora
    /// Serverless v2 capacity range, deletion protection and a tag of its own.
    names_placement: bool,
    /// Whether the writer DB instance is created BEFORE `EnableHttpEndpoint`, as a client that
    /// provisions the cluster's compute first does, or after it.
    writer_before_http_endpoint: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a restore that names its own placement, capacity range, deletion protection and \
               tag, with the writer created before the Data API endpoint is turned on",
        restored: "emu-restore-placed-first",
        writer: "emu-restore-placed-first-1",
        names_placement: true,
        writer_before_http_endpoint: true,
    },
    Row {
        case: "a restore that names the same members, with the Data API endpoint turned on \
               before the writer",
        restored: "emu-restore-placed-later",
        writer: "emu-restore-placed-later-1",
        names_placement: true,
        writer_before_http_endpoint: false,
    },
    Row {
        case: "a restore that names none of them, with the writer created before the Data API \
               endpoint is turned on",
        restored: "emu-restore-bare",
        writer: "emu-restore-bare-1",
        names_placement: false,
        writer_before_http_endpoint: true,
    },
];

/// Turn the cluster's RDS Data API endpoint on.
async fn enable_http_endpoint(router: &MultiServiceRouter, case: &str, arn: &str) {
    served(router, case, "EnableHttpEndpoint", &[("ResourceArn", arn)]).await;
}

/// The whole lifecycle a client drives after a restore, over the members its restore carries
/// and the order it drives them in: every status settles, the restored cluster carries exactly
/// what the restore named, its managed credential is usable, and the rows it serves are the
/// snapshot's.
#[tokio::test]
async fn a_restored_db_clusters_statuses_settle_whatever_the_restore_named() {
    let router = build_router();
    let setup = "the source cluster";
    let network = allocate_network(&router, setup).await;

    // ---- the source: a cluster in its own placement, with a writer and the Data API on ----
    let source_cluster = "emu-restore-placement-src";
    let source = create_source_cluster(
        &router,
        setup,
        source_cluster,
        &[
            ("DBSubnetGroupName", network.source.subnet_group.as_str()),
            (
                "VpcSecurityGroupIds.VpcSecurityGroupId.1",
                network.source.security_group.as_str(),
            ),
        ],
    )
    .await;
    add_writer(&router, setup, source_cluster, "emu-restore-placement-src-1").await;

    // The rows the snapshot must carry, committed before it is taken.
    for sql in [
        &format!("CREATE TABLE {TABLE} (id int PRIMARY KEY, label text)"),
        &format!("INSERT INTO {TABLE} (id, label) VALUES (1, 'before')"),
        &format!("INSERT INTO {TABLE} (id, label) VALUES (2, 'before')"),
    ] {
        source.run(&router, setup, sql).await;
    }

    let snapshot = "emu-restore-placement-snap";
    take_snapshot(&router, setup, source_cluster, snapshot).await;

    // A row committed AFTER the snapshot, which no restore of it may carry.
    source
        .run(
            &router,
            setup,
            &format!("INSERT INTO {TABLE} (id, label) VALUES (3, 'after')"),
        )
        .await;

    for row in ROWS {
        let case = row.case;
        let mut members: Vec<(&str, String)> = vec![
            ("DBClusterIdentifier", row.restored.to_string()),
            ("SnapshotIdentifier", snapshot.to_string()),
            ("Engine", ENGINE.to_string()),
        ];
        if row.names_placement {
            members.extend([
                ("DBSubnetGroupName", network.restore.subnet_group.clone()),
                (
                    "VpcSecurityGroupIds.VpcSecurityGroupId.1",
                    network.restore.security_group.clone(),
                ),
                (
                    "ServerlessV2ScalingConfiguration.MinCapacity",
                    MIN_CAPACITY.to_string(),
                ),
                (
                    "ServerlessV2ScalingConfiguration.MaxCapacity",
                    MAX_CAPACITY.to_string(),
                ),
                ("DeletionProtection", "true".to_string()),
                ("Tags.Tag.1.Key", TAG_KEY.to_string()),
                ("Tags.Tag.1.Value", TAG_VALUE.to_string()),
            ]);
        }
        let borrowed: Vec<(&str, &str)> = members
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let restored = served(&router, case, "RestoreDBClusterFromSnapshot", &borrowed).await;
        let arn = element(&restored, "DBClusterArn");
        assert_ne!(
            arn, source.arn,
            "case {case:?}: a restore creates a resource of its own, got {arn} for both"
        );

        // 1. The restored cluster reaches `available` on its own.
        await_available_cluster(&router, case, row.restored).await;

        // 2. And the managed master user password the client then asks for reaches `active`,
        //    on a cluster that is `available` at the same time.
        served(
            &router,
            case,
            "ModifyDBCluster",
            &[
                ("DBClusterIdentifier", row.restored),
                ("ManageMasterUserPassword", "true"),
                ("ApplyImmediately", "true"),
            ],
        )
        .await;
        let secret_arn = await_settled_cluster(&router, case, row.restored).await;
        assert_ne!(
            secret_arn, source.secret_arn,
            "case {case:?}: the restored cluster's managed secret is its own, got the source's \
             {secret_arn:?}"
        );
        let (username, password) = held_credentials(&router, case, &secret_arn).await;
        assert_eq!(
            username, MASTER_USER,
            "case {case:?}: expected the restored cluster's secret to hold its master user, got \
             {username:?}"
        );
        assert!(
            !password.is_empty(),
            "case {case:?}: expected the managed secret to hold a generated password"
        );

        // 3. The writer DB instance reaches `available`, and the Data API answers once
        //    `EnableHttpEndpoint` has turned it on — in whichever order the client drives them.
        if row.writer_before_http_endpoint {
            add_writer(&router, case, row.restored, row.writer).await;
            enable_http_endpoint(&router, case, &arn).await;
        } else {
            enable_http_endpoint(&router, case, &arn).await;
            add_writer(&router, case, row.restored, row.writer).await;
        }
        let addressed = Addressed {
            arn: arn.clone(),
            secret_arn: secret_arn.clone(),
        };
        addressed.await_data_api(&router, case).await;

        // 4. What the restored cluster now REPORTS is what the restore named, and what it
        //    SERVES is what its snapshot held.
        let reported = await_available_cluster(&router, case, row.restored).await;
        assert_eq!(
            opt_element(&reported, "HttpEndpointEnabled").as_deref(),
            Some("true"),
            "case {case:?}: expected the Data API endpoint to be reported on, got {reported}"
        );
        let expected_protection = if row.names_placement { "true" } else { "false" };
        assert_eq!(
            opt_element(&reported, "DeletionProtection").as_deref(),
            Some(expected_protection),
            "case {case:?}: expected DeletionProtection {expected_protection:?}, got {reported}"
        );
        let carried = tags_of(&router, case, &arn).await;
        let expected_tag = row.names_placement.then(|| TAG_VALUE.to_string());
        assert_eq!(
            carried.get(TAG_KEY).cloned(),
            expected_tag,
            "case {case:?}: expected the tag {expected_tag:?} the restore named, got {carried:?}"
        );
        if row.names_placement {
            // Each of these is a value the SOURCE cluster does not carry, so the restored
            // cluster reports it only if the restore's own member was honoured.
            assert_eq!(
                opt_element(&reported, "DBSubnetGroup").as_deref(),
                Some(network.restore.subnet_group.as_str()),
                "case {case:?}: expected the DB subnet group the restore named, got {reported}"
            );
            let memberships = reported_memberships(&reported);
            assert_eq!(
                memberships
                    .get(&network.restore.security_group)
                    .map(String::as_str),
                Some(MEMBERSHIP_ACTIVE),
                "case {case:?}: expected {:?} to be an {MEMBERSHIP_ACTIVE:?} membership, got \
                 {memberships:?}",
                network.restore.security_group
            );
            assert!(
                !memberships.contains_key(&network.source.security_group),
                "case {case:?}: expected none of the source cluster's groups, got {memberships:?}"
            );
            assert_eq!(
                reported_capacity(&reported),
                (
                    Some(MIN_CAPACITY.to_string()),
                    Some(MAX_CAPACITY.to_string())
                ),
                "case {case:?}: expected the capacity range the restore named, got {reported}"
            );
        }

        let served_ids = addressed.ids_in(&router, case, DATABASE).await;
        assert_eq!(
            served_ids,
            vec![1, 2],
            "case {case:?}: expected the rows committed before the snapshot on the restored \
             cluster, got {served_ids:?}"
        );
    }

    // Every restore above left the source cluster alone: it still reports its own placement
    // and its own capacity range, is still in its terminal status, and still serves all three
    // rows.
    let reported = await_available_cluster(&router, setup, source_cluster).await;
    assert_eq!(
        opt_element(&reported, "DBSubnetGroup").as_deref(),
        Some(network.source.subnet_group.as_str()),
        "case {setup:?}: expected the source cluster's own placement, got {reported}"
    );
    assert_eq!(
        reported_capacity(&reported),
        (
            Some(SOURCE_MIN_CAPACITY.to_string()),
            Some(SOURCE_MAX_CAPACITY.to_string())
        ),
        "case {setup:?}: expected the source cluster's own capacity range, got {reported}"
    );
    let served_ids = source.ids_in(&router, setup, DATABASE).await;
    assert_eq!(
        served_ids,
        vec![1, 2, 3],
        "case {setup:?}: expected every committed row on the source cluster, got {served_ids:?}"
    );
}
