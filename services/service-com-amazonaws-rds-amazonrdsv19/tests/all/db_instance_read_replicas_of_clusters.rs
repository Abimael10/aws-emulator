// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CreateDBInstanceReadReplica` with the OTHER source it serves: a
//! Multi-AZ DB cluster (`SourceDBClusterIdentifier`), the "Migrating from a Multi-AZ DB
//! cluster to a DB instance using a read replica" path.
//!
//! The rules under test:
//!
//! * a replica of a cluster is a DB INSTANCE that is a copy of that cluster — its engine,
//!   its version, the database it serves and every setting the request does not name are the
//!   cluster's — and the two report the relationship from both ends: the instance names the
//!   cluster in `ReadReplicaSourceDBClusterIdentifier` and the cluster names the instance
//!   among its `ReadReplicaIdentifiers`;
//! * a replica is built from the SOURCE's automated backups, so a cluster that is not up
//!   has none to build one from and is refused for its STATE, and the identical call is
//!   served once the cluster reports `available`;
//! * `SourceDBClusterIdentifier` "must be the identifier of an existing Multi-AZ DB
//!   cluster": a value naming no cluster is the modelled 404 and an Aurora DB cluster — the
//!   other cluster type — is a value the member does not accept;
//! * `PromoteReadReplica` ends the replication: the promoted instance is standalone and the
//!   cluster stops reporting it, and so does deleting the source cluster — no stored record
//!   names a resource this service no longer holds;
//! * the replica's own storage holds what the cluster had COMMITTED, and the replica serves
//!   no writes: a session on it is a session on a standby.
//!
//! Each rule is asserted over a TABLE spanning what it generalises over — the cluster's
//! engine (the two engines a Multi-AZ DB cluster runs), how the request names the source
//! (identifier and ARN) and which of the replica's optional members it sets — rather than
//! over one request.
//!
//! Every call goes over the wire: real awsQuery form POSTs through a `MultiServiceRouter`,
//! and every assertion reads the XML the emulator answered with. Every wait is a POLL for
//! the condition, through the suite-shared loop, so nothing here depends on how fast the
//! host runs.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    endpoint_of, mysql_login, Refusal, Transport, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents, which the ARNs below are read
/// against.
const ACCOUNT: &str = "000000000000";

/// The four settings a Multi-AZ DB cluster is required to be created with, each documented
/// "Valid for Cluster Type: Multi-AZ DB clusters only" and "required to create a Multi-AZ DB
/// cluster", with an `Iops` inside the documented ratio of the allocated storage.
const MULTI_AZ_SETTINGS: &[(&str, &str)] = &[
    ("AllocatedStorage", "400"),
    ("DBClusterInstanceClass", "db.m5d.large"),
    ("Iops", "3000"),
    ("StorageType", "gp3"),
];

/// `CreateDBCluster` for a Multi-AZ DB cluster, then polled until it reports `available` so
/// the cluster really is replicable when the replica is asked for.
async fn source_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
    retention: &str,
) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
        ("DatabaseName", DATABASE.to_string()),
        ("MasterUsername", MASTER_USER.to_string()),
        ("MasterUserPassword", MASTER_PASSWORD.to_string()),
        ("BackupRetentionPeriod", retention.to_string()),
    ];
    params.extend(
        MULTI_AZ_SETTINGS
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    expect_ok(case, call(router, "CreateDBCluster", &params).await);
    poll_cluster_until_available(router, case, identifier).await;
}

/// `DescribeDBClusters` naming one cluster, polled until it reports `available`.
async fn poll_cluster_until_available(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let (body, _) = poll_until(
        case,
        &format!("DB cluster {identifier} reaching available"),
        || read_cluster(router, identifier),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
    body
}

/// `DescribeDBClusters` for one cluster, as the HTTP status and the body.
async fn read_cluster(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "DescribeDBClusters",
        &[("DBClusterIdentifier", identifier)],
    )
    .await
}

/// `DescribeDBInstances` for one instance, as the HTTP status and the body.
async fn read_instance(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await
}

/// Poll `DescribeDBInstances` until the instance reports `available`, answering the body it
/// settled on.
async fn poll_instance_until_available(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let (body, _) = poll_until(
        case,
        &format!("DB instance {identifier} reaching available"),
        || read_instance(router, identifier),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;
    body
}

/// One row of the cluster-source replica table: the cluster's engine, how the request names
/// it, and the replica's own members.
struct ClusterReplicaCase {
    case: &'static str,
    engine: &'static str,
    cluster: &'static str,
    replica: &'static str,
    /// How this row names the source cluster: `{name}` for the identifier, `{arn}` for its
    /// ARN.
    names_source_by: &'static str,
    /// The members beyond the two identifiers.
    extra: &'static [(&'static str, &'static str)],
}

/// The class the rule generalises over: the two engines a Multi-AZ DB cluster runs
/// (`CreateDBCluster$Engine` names `mysql` and `postgres` for that cluster type), the two
/// forms a source may be named by, and a different set of the replica's optional members in
/// each row.
const CLUSTER_REPLICAS: &[ClusterReplicaCase] = &[
    ClusterReplicaCase {
        case: "a mysql Multi-AZ DB cluster named by identifier, nothing else set",
        engine: "mysql",
        cluster: "cluster-src-mysql",
        replica: "replica-of-cluster-mysql",
        names_source_by: "{name}",
        extra: &[],
    },
    ClusterReplicaCase {
        case: "a postgres Multi-AZ DB cluster named by ARN, with its own class and tag copying",
        engine: "postgres",
        cluster: "cluster-src-postgres",
        replica: "replica-of-cluster-postgres",
        names_source_by: "{arn}",
        extra: &[
            ("DBInstanceClass", "db.m5d.xlarge"),
            ("CopyTagsToSnapshot", "true"),
        ],
    },
    ClusterReplicaCase {
        case: "a mysql Multi-AZ DB cluster, with the replica's own zone and storage",
        engine: "mysql",
        cluster: "cluster-src-mysql-2",
        replica: "replica-of-cluster-mysql-2",
        names_source_by: "{name}",
        extra: &[
            ("AvailabilityZone", "us-west-2c"),
            ("AllocatedStorage", "500"),
            ("PubliclyAccessible", "true"),
        ],
    },
];

#[tokio::test]
async fn a_replica_of_a_cluster_copies_it_and_both_report_the_relationship() {
    let router = emulator();

    for row in CLUSTER_REPLICAS {
        let case = row.case;
        source_cluster(&router, case, row.cluster, row.engine, "1").await;
        let cluster_arn = format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:{}", row.cluster);
        let named = row
            .names_source_by
            .replace("{name}", row.cluster)
            .replace("{arn}", &cluster_arn);

        let described_cluster = read_cluster(&router, row.cluster).await.1;
        let engine_version = text(&described_cluster, "EngineVersion");

        let mut params: Vec<(&str, String)> = vec![
            ("DBInstanceIdentifier", row.replica.to_string()),
            ("SourceDBClusterIdentifier", named),
        ];
        for (name, value) in row.extra {
            params.push((name, (*value).to_string()));
        }
        let created = expect_ok(
            case,
            call(&router, "CreateDBInstanceReadReplica", &params).await,
        );

        // The replica runs the CLUSTER's engine and version — the operation has no `Engine`
        // member — serves the cluster's database, and reports the cluster it replicates and
        // no source DB instance, because it replicates no instance.
        assert_eq!(
            (
                text(&created, "DBInstanceIdentifier"),
                text(&created, "Engine"),
                text(&created, "EngineVersion"),
                text(&created, "DBName"),
                text(&created, "ReadReplicaSourceDBClusterIdentifier"),
                optional_text(&created, "ReadReplicaSourceDBInstanceIdentifier"),
                text(&created, "DBInstanceStatus"),
                text(&created, "DBInstanceArn"),
            ),
            (
                row.replica.to_string(),
                row.engine.to_string(),
                engine_version.clone(),
                DATABASE.to_string(),
                row.cluster.to_string(),
                None,
                "creating".to_string(),
                format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{}", row.replica),
            ),
            "case {case:?}: got {created}"
        );
        // A replica keeps no automated backup of its own until one is turned on for it,
        // whichever kind of source it was built from, and it belongs to no DB cluster: it is
        // a standalone instance that REPLICATES one.
        assert_eq!(
            (
                text(&created, "BackupRetentionPeriod"),
                optional_text(&created, "DBClusterIdentifier"),
            ),
            ("0".to_string(), None),
            "case {case:?}: got {created}"
        );
        // Replicating is what `StatusInfos` reports for a read replica, and a replica of a
        // cluster is one.
        assert_eq!(
            (
                text(&created, "StatusType"),
                text(&created, "Normal"),
                optional_text(&created, "Message"),
            ),
            (
                "read replication".to_string(),
                "true".to_string(),
                None
            ),
            "case {case:?}: got {created}"
        );

        // The members the request named are the replica's own; the ones it did not are the
        // cluster's.
        for (member, value) in row.extra {
            let reported = match *member {
                "DBInstanceClass" => text(&created, "DBInstanceClass"),
                "CopyTagsToSnapshot" => text(&created, "CopyTagsToSnapshot"),
                "AvailabilityZone" => text(&created, "AvailabilityZone"),
                "AllocatedStorage" => text(&created, "AllocatedStorage"),
                "PubliclyAccessible" => text(&created, "PubliclyAccessible"),
                other => panic!("case {case:?}: no projection asserted for {other}"),
            };
            assert_eq!(
                reported, *value,
                "case {case:?}: the replica must carry the {member} the request named, got \
                 {created}"
            );
        }
        if !row
            .extra
            .iter()
            .any(|(member, _)| *member == "DBInstanceClass")
        {
            // `DBInstanceClass` "Default: Inherits the value from the source": the class a
            // DB instance of this cluster runs is its `DBClusterInstanceClass`.
            assert_eq!(
                text(&created, "DBInstanceClass"),
                text(&described_cluster, "DBClusterInstanceClass"),
                "case {case:?}: a member the request did not name is the cluster's, got \
                 {created}"
            );
        }
        if !row
            .extra
            .iter()
            .any(|(member, _)| *member == "AllocatedStorage")
        {
            assert_eq!(
                text(&created, "AllocatedStorage"),
                text(&described_cluster, "AllocatedStorage"),
                "case {case:?}: a member the request did not name is the cluster's, got \
                 {created}"
            );
        }

        // The replica reaches `available` on its own, and the read-back is what proves it was
        // persisted rather than echoed.
        let described_replica = poll_instance_until_available(&router, case, row.replica).await;
        assert_eq!(
            text(&described_replica, "ReadReplicaSourceDBClusterIdentifier"),
            row.cluster,
            "case {case:?}: got {described_replica}"
        );
        // Its endpoint is its OWN, never the cluster's: that is what lets a client point
        // readers at the replica while writers stay on the cluster. The two addresses are
        // compared whole — host and port — because the emulator serves every endpoint it
        // publishes on this machine.
        assert_ne!(
            (
                text(&described_replica, "Address"),
                text(&described_replica, "Port"),
            ),
            (
                text(&described_cluster, "Endpoint"),
                text(&described_cluster, "Port"),
            ),
            "case {case:?}: the replica must serve its own endpoint, got {described_replica}"
        );

        // The CLUSTER reports the replica from its own end.
        let described_cluster = read_cluster(&router, row.cluster).await.1;
        assert!(
            texts(&described_cluster, "ReadReplicaIdentifier")
                .contains(&row.replica.to_string()),
            "case {case:?}: the cluster must report the replica it has, got {described_cluster}"
        );

        // Deleting the replica ends the relationship: the cluster reports no replica it does
        // not have.
        expect_ok(
            case,
            call(
                &router,
                "DeleteDBInstance",
                &[
                    ("DBInstanceIdentifier", row.replica),
                    ("SkipFinalSnapshot", "true"),
                ],
            )
            .await,
        );
        let (described_cluster, _) = poll_until(
            case,
            &format!("DB cluster {} dropping the deleted replica", row.cluster),
            || read_cluster(&router, row.cluster),
            |status, body| {
                status == 200
                    && !texts(body, "ReadReplicaIdentifier").contains(&row.replica.to_string())
            },
        )
        .await;
        assert!(
            !texts(&described_cluster, "ReadReplicaIdentifier")
                .contains(&row.replica.to_string()),
            "case {case:?}: a deleted replica is not one the cluster has, got \
             {described_cluster}"
        );
    }
}

#[tokio::test]
async fn a_promoted_replica_of_a_cluster_is_standalone_and_the_cluster_stops_reporting_it() {
    let router = emulator();
    // Both engines, so the rule is not one engine's.
    for (case, engine, cluster, replica) in [
        (
            "a mysql Multi-AZ DB cluster",
            "mysql",
            "promote-cluster-mysql",
            "promote-replica-mysql",
        ),
        (
            "a postgres Multi-AZ DB cluster",
            "postgres",
            "promote-cluster-postgres",
            "promote-replica-postgres",
        ),
    ] {
        source_cluster(&router, case, cluster, engine, "3").await;
        expect_ok(
            case,
            call(
                &router,
                "CreateDBInstanceReadReplica",
                &[
                    ("DBInstanceIdentifier", replica),
                    ("SourceDBClusterIdentifier", cluster),
                ],
            )
            .await,
        );
        poll_instance_until_available(&router, case, replica).await;

        let promoted = expect_ok(
            case,
            call(
                &router,
                "PromoteReadReplica",
                &[("DBInstanceIdentifier", replica)],
            )
            .await,
        );
        // The promotion ends the replication: the instance replicates nothing from now on,
        // so it reports neither source and no replication status at all.
        assert_eq!(
            (
                optional_text(&promoted, "ReadReplicaSourceDBClusterIdentifier"),
                optional_text(&promoted, "ReadReplicaSourceDBInstanceIdentifier"),
                optional_text(&promoted, "StatusType"),
            ),
            (None, None, None),
            "case {case:?}: got {promoted}"
        );

        let settled = poll_instance_until_available(&router, case, replica).await;
        assert_eq!(
            optional_text(&settled, "ReadReplicaSourceDBClusterIdentifier"),
            None,
            "case {case:?}: a promoted replica reports no source, got {settled}"
        );
        // A promoted instance keeps automated backups of its own, which is what a standalone
        // DB instance has.
        let retention: i32 = text(&settled, "BackupRetentionPeriod").parse().unwrap_or(0);
        assert!(
            retention > 0,
            "case {case:?}: a promoted replica keeps its own automated backups, got {settled}"
        );
        // And the cluster stops reporting it: the replicas a cluster has are the instances
        // that still name it.
        let described_cluster = read_cluster(&router, cluster).await.1;
        assert!(
            !texts(&described_cluster, "ReadReplicaIdentifier").contains(&replica.to_string()),
            "case {case:?}: a promoted replica is not one the cluster has, got \
             {described_cluster}"
        );
        // Promoting it again is refused: it is no read replica any more.
        expect_error(
            case,
            call(
                &router,
                "PromoteReadReplica",
                &[("DBInstanceIdentifier", replica)],
            )
            .await,
            400,
            "InvalidDBInstanceState",
        );
    }
}

#[tokio::test]
async fn a_source_cluster_this_operation_cannot_replicate_is_refused_and_stores_nothing() {
    let router = emulator();
    // An Aurora DB cluster: the other cluster type, whose replicas are its own member
    // instances.
    expect_ok(
        "the Aurora source",
        call(
            &router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "guard-aurora-cluster"),
                ("Engine", "aurora-mysql"),
                ("MasterUsername", "admin"),
                ("MasterUserPassword", "a-password-9"),
                ("BackupRetentionPeriod", "1"),
            ],
        )
        .await,
    );
    poll_cluster_until_available(&router, "the Aurora source", "guard-aurora-cluster").await;

    // A Multi-AZ DB cluster that is still coming up: a cluster reports `creating` until the
    // transition its create owes is settled, and a replica of a cluster that is not up is
    // refused for the cluster's STATE. Asked for straight after the create, as a caller
    // that does not wait would.
    let case = "a source cluster that is still being created";
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", "guard-creating-cluster".to_string()),
        ("Engine", "postgres".to_string()),
        ("MasterUsername", "primary".to_string()),
        ("MasterUserPassword", "a-master-password-1".to_string()),
        ("BackupRetentionPeriod", "1".to_string()),
    ];
    params.extend(
        MULTI_AZ_SETTINGS
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    let created_cluster = expect_ok(case, call(&router, "CreateDBCluster", &params).await);
    assert_eq!(
        text(&created_cluster, "Status"),
        "creating",
        "case {case:?}: got {created_cluster}"
    );
    let refused = expect_error(
        case,
        call(
            &router,
            "CreateDBInstanceReadReplica",
            &[
                ("DBInstanceIdentifier", "cluster-replica-guard-4"),
                ("SourceDBClusterIdentifier", "guard-creating-cluster"),
            ],
        )
        .await,
        400,
        "InvalidDBClusterStateFault",
    );
    assert_eq!(
        text(&refused, "Message"),
        "DbCluster guard-creating-cluster is in creating state but expected it to be one of \
         available.",
        "case {case:?}: got {refused}"
    );

    /// One refusal row: the parameters of a replica this operation cannot serve, and the
    /// status and wire code it answers with.
    struct Guard {
        case: &'static str,
        params: &'static [(&'static str, &'static str)],
        status: u16,
        code: &'static str,
    }

    let rows: [Guard; 4] = [
        Guard {
            case: "a source cluster identifier nothing created",
            params: &[
                ("DBInstanceIdentifier", "cluster-replica-guard-1"),
                ("SourceDBClusterIdentifier", "no-such-cluster"),
            ],
            status: 404,
            code: "DBClusterNotFoundFault",
        },
        Guard {
            case: "a source cluster ARN nothing created",
            params: &[
                ("DBInstanceIdentifier", "cluster-replica-guard-2"),
                (
                    "SourceDBClusterIdentifier",
                    "arn:aws:rds:us-west-2:000000000000:cluster:no-such-cluster",
                ),
            ],
            status: 404,
            code: "DBClusterNotFoundFault",
        },
        Guard {
            case: "an Aurora DB cluster as the source",
            params: &[
                ("DBInstanceIdentifier", "cluster-replica-guard-3"),
                ("SourceDBClusterIdentifier", "guard-aurora-cluster"),
            ],
            status: 400,
            code: "InvalidParameterValue",
        },
        Guard {
            case: "a source cluster and a source instance at once",
            params: &[
                ("DBInstanceIdentifier", "cluster-replica-guard-5"),
                ("SourceDBClusterIdentifier", "guard-aurora-cluster"),
                ("SourceDBInstanceIdentifier", "no-such-source"),
            ],
            status: 400,
            code: "InvalidParameterCombination",
        },
    ];

    for row in rows {
        let case = row.case;
        expect_error(
            case,
            call(&router, "CreateDBInstanceReadReplica", row.params).await,
            row.status,
            row.code,
        );
        let identifier = row
            .params
            .iter()
            .find(|(member, _)| *member == "DBInstanceIdentifier")
            .map(|(_, value)| *value)
            .unwrap_or_default();
        // The refused call left no instance behind.
        expect_error(
            case,
            call(
                &router,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", identifier)],
            )
            .await,
            404,
            "DBInstanceNotFound",
        );
    }

    // The cluster-not-found wording is the SOURCE lookup's own sentence, carrying the value
    // the request wrote.
    let refused = expect_error(
        "the not-found wording",
        call(
            &router,
            "CreateDBInstanceReadReplica",
            &[
                ("DBInstanceIdentifier", "cluster-replica-guard-6"),
                ("SourceDBClusterIdentifier", "still-no-such-cluster"),
            ],
        )
        .await,
        404,
        "DBClusterNotFoundFault",
    );
    assert_eq!(
        text(&refused, "Message"),
        "The source cluster could not be found or cannot be accessed: still-no-such-cluster",
        "case \"the not-found wording\": got {refused}"
    );
    // The Aurora refusal names the member, the cluster and the engine that decides which
    // cluster type it is — the three things a caller needs to fix the request.
    let refused = expect_error(
        "the Aurora wording",
        call(
            &router,
            "CreateDBInstanceReadReplica",
            &[
                ("DBInstanceIdentifier", "cluster-replica-guard-7"),
                ("SourceDBClusterIdentifier", "guard-aurora-cluster"),
            ],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
    assert_eq!(
        text(&refused, "Message"),
        "invalid value guard-aurora-cluster for SourceDBClusterIdentifier: it must be the \
         identifier of an existing Multi-AZ DB cluster, and DB cluster guard-aurora-cluster \
         runs engine aurora-mysql",
        "case \"the Aurora wording\": got {refused}"
    );

    // Once the cluster is up, the IDENTICAL call that was refused for its state is served:
    // the refusal was about the state the source was in, not about replicating a cluster.
    poll_cluster_until_available(&router, case, "guard-creating-cluster").await;
    let served = expect_ok(
        "the cluster once it is up",
        call(
            &router,
            "CreateDBInstanceReadReplica",
            &[
                ("DBInstanceIdentifier", "cluster-replica-guard-4"),
                ("SourceDBClusterIdentifier", "guard-creating-cluster"),
            ],
        )
        .await,
    );
    assert_eq!(
        optional_text(&served, "ReadReplicaSourceDBClusterIdentifier").as_deref(),
        Some("guard-creating-cluster"),
        "case \"the cluster once it is up\": got {served}"
    );
    // One DB instance was created, and it is the replica this call answered with.
    let reported = subtrees(
        &poll_instance_until_available(
            &router,
            "the cluster once it is up",
            "cluster-replica-guard-4",
        )
        .await,
        "DBInstance",
    );
    assert_eq!(
        reported.len(),
        1,
        "case \"the cluster once it is up\": exactly one DB instance must be reported, got \
         {reported:?}"
    );
}

/// What MySQL answers a write on a server its `read_only` setting applies to with: one
/// sentence for every statement it refuses, under `ER_OPTION_PREVENTS_STATEMENT` (1290).
fn mysql_read_only_refusal() -> Refusal {
    Refusal {
        code: "1290".to_string(),
        message: "The MySQL server is running with the --read-only option so it cannot \
                  execute this statement"
            .to_string(),
    }
}

#[tokio::test]
async fn deleting_the_source_cluster_promotes_the_replicas_it_leaves_behind() {
    let router = emulator();
    let case = "a deleted source cluster";
    source_cluster(&router, case, "doomed-src-cluster", "postgres", "1").await;
    expect_ok(
        case,
        call(
            &router,
            "CreateDBInstanceReadReplica",
            &[
                ("DBInstanceIdentifier", "replica-of-doomed-cluster"),
                ("SourceDBClusterIdentifier", "doomed-src-cluster"),
            ],
        )
        .await,
    );
    poll_instance_until_available(&router, case, "replica-of-doomed-cluster").await;

    // A replica of a cluster is no member of it, so it is not terminated with the cluster:
    // it is promoted, which is what keeps it from naming a cluster this service no longer
    // holds.
    expect_ok(
        case,
        call(
            &router,
            "DeleteDBCluster",
            &[
                ("DBClusterIdentifier", "doomed-src-cluster"),
                ("SkipFinalSnapshot", "true"),
            ],
        )
        .await,
    );
    let standalone =
        poll_instance_until_available(&router, case, "replica-of-doomed-cluster").await;
    assert_eq!(
        (
            optional_text(&standalone, "ReadReplicaSourceDBClusterIdentifier"),
            optional_text(&standalone, "StatusType"),
        ),
        (None, None),
        "case {case:?}: a replica of a deleted cluster is standalone, got {standalone}"
    );
}

#[tokio::test]
async fn a_replica_of_a_cluster_serves_the_rows_the_cluster_committed_and_no_writes() {
    let router = emulator();
    let case = "a mysql Multi-AZ DB cluster's replica";
    source_cluster(&router, case, "data-src-cluster", "mysql", "1").await;

    // Rows the CLUSTER committed, over its own published endpoint, before the replica exists.
    let described_cluster = poll_cluster_until_available(&router, case, "data-src-cluster").await;
    let cluster_port: u16 = text(&described_cluster, "Port")
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: the cluster must publish a port"));
    let mut writer = mysql_login(
        &text(&described_cluster, "Endpoint"),
        cluster_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case:?}: the master user must be admitted on the cluster, got {refusal:?}")
    });
    for statement in [
        "CREATE TABLE ledger (id integer PRIMARY KEY, note text)",
        "INSERT INTO ledger (id, note) VALUES (1, 'committed by the cluster')",
    ] {
        writer.query(statement).await.unwrap_or_else(|refusal| {
            panic!("case {case:?}: {statement:?} must run on the cluster, got {refusal:?}")
        });
    }

    expect_ok(
        case,
        call(
            &router,
            "CreateDBInstanceReadReplica",
            &[
                ("DBInstanceIdentifier", "data-replica-of-cluster"),
                ("SourceDBClusterIdentifier", "data-src-cluster"),
            ],
        )
        .await,
    );
    let described_replica =
        poll_instance_until_available(&router, case, "data-replica-of-cluster").await;
    let (host, port) = endpoint_of(&described_replica);
    let mut reader = mysql_login(
        &host,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case:?}: the master user must be admitted on the replica, got {refusal:?}")
    });

    // THE REPLICATION: the replica's own storage holds what the cluster had committed, and
    // the master credential a login on it is verified against is the cluster's.
    let read_back = reader
        .query("SELECT note FROM ledger WHERE id = 1")
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: the replica must serve the cluster's rows, got {refusal:?}")
        });
    assert_eq!(
        read_back.only(),
        Some("committed by the cluster"),
        "case {case:?}: expected the replica to read the cluster's committed row, got \
         {read_back:?}"
    );
    // A replica of a cluster is a standby, whatever it replicates: it reports itself
    // read-only and refuses the statements that write, with the engine's own code and
    // sentence.
    let reported = reader
        .query("SELECT @@read_only")
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: the probe must run, got {refusal:?}"));
    assert_eq!(
        reported.only(),
        Some("1"),
        "case {case:?}: expected the replica to report itself read-only, got {reported:?}"
    );
    for statement in [
        "INSERT INTO ledger (id, note) VALUES (9, 'from the replica')",
        "UPDATE ledger SET note = 'rewritten' WHERE id = 1",
        "DELETE FROM ledger WHERE id = 1",
        "CREATE TABLE replica_made (id integer)",
        "DROP TABLE ledger",
    ] {
        let refusal = reader.query(statement).await.err().unwrap_or_else(|| {
            panic!("case {case:?}: {statement:?} must be refused on the replica")
        });
        assert_eq!(
            refusal,
            mysql_read_only_refusal(),
            "case {case:?}: expected the engine's own refusal of {statement:?}, got {refusal:?}"
        );
    }
    // The replica's storage is its OWN copy: a row the CLUSTER commits after the replica was
    // created is not one the replica holds, and the refused writes changed nothing.
    writer
        .query("INSERT INTO ledger (id, note) VALUES (2, 'committed after the copy')")
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: the cluster must still write, got {refusal:?}")
        });
    let rows = reader
        .query("SELECT count(*) FROM ledger")
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: the count must run, got {refusal:?}"));
    assert_eq!(
        rows.only(),
        Some("1"),
        "case {case:?}: expected the replica to hold the one row it was created from, got \
         {rows:?}"
    );
}

