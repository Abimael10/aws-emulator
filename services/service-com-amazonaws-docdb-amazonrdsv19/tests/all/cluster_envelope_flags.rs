// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the members Amazon DocumentDB's cluster and instance envelopes
//! ALWAYS carry beyond what the shipped model shape declares: on the cluster
//! `AutoMinorVersionUpgrade`, `ActivityStreamStatus`, `DBActivityStreamStatus`,
//! `DocdbAnalyticsEnabled`, `CustomDataGroupPresent`, `VPCNetworkingEnabled` and
//! `InternetAccessGatewayEnabled`; on the instance the attribute set at the end of this
//! file.
//!
//! The rule: every member the cluster envelope always carries is projected
//! unconditionally, out of the cluster record, by the one projection every path shares
//! (`crate::state::cluster_to_shape`) — and where no input member of this service can set
//! one, the record is born at the value the service reports for a cluster created without
//! it: automatic minor version upgrade on, the DocumentDB-only capability flags off, VPC
//! networking on (a cluster is reachable only from inside a virtual private cloud) with no
//! internet access gateway, and the database activity stream in its stopped steady state.
//! The constants in `src/state.rs` cite each one.
//!
//! The rule generalises over the cluster's inputs (none of them touches these seven
//! members) and over the operation whose envelope carries the cluster, so the table below
//! spans BOTH: each row is a differently shaped `CreateDBCluster`, and every row is read
//! through the create, describe, modify and delete envelopes in turn.
//!
//! Every test drives the service over its real wire protocol (awsQuery form bodies in, XML
//! out) through the same [`MultiServiceRouter`](smithy_server_runtime::MultiServiceRouter)
//! the production router uses, with the wire harness this target shares
//! (`tests/all/cluster_lifecycle.rs`).

use crate::cluster_lifecycle::{
    count, let_the_clock_catch_up, opt_text, text, texts, Docdb, REGION,
};

/// The members and the value each must carry for a cluster created without any input that
/// could change it. Source: the `DBCluster` members of this service's on-disk model (added
/// there from the recorded AWS responses) and the defaults cited on
/// `state::DEFAULT_CLUSTER_AUTO_MINOR_VERSION_UPGRADE`, `state::ACTIVITY_STREAM_STOPPED`,
/// `state::DEFAULT_DOCDB_ANALYTICS_ENABLED`, `state::DEFAULT_CUSTOM_DATA_GROUP_PRESENT`,
/// `state::DEFAULT_VPC_NETWORKING_ENABLED` and
/// `state::DEFAULT_INTERNET_ACCESS_GATEWAY_ENABLED`.
const ENVELOPE_FLAGS: [(&str, &str); 7] = [
    ("AutoMinorVersionUpgrade", "true"),
    ("ActivityStreamStatus", "stopped"),
    ("DBActivityStreamStatus", "stopped"),
    ("DocdbAnalyticsEnabled", "false"),
    ("CustomDataGroupPresent", "false"),
    ("VPCNetworkingEnabled", "true"),
    ("InternetAccessGatewayEnabled", "false"),
];

/// Assert that `xml` carries every element of [`ENVELOPE_FLAGS`] with its value — asserted
/// on the raw XML, so a member the serializer omits fails rather than reading as a default.
fn assert_envelope_flags(case: &str, xml: &str) {
    for (member, expected) in ENVELOPE_FLAGS {
        let actual = opt_text(xml, member);
        assert_eq!(
            actual.as_deref(),
            Some(expected),
            "case {case:?}: the cluster envelope must carry {member}={expected}, \
             got {actual:?} in {xml}"
        );
    }
}

/// One row of the table: a case name and the extra parameters its `CreateDBCluster` sends
/// beyond the identifier and the engine.
struct CreateCase {
    label: &'static str,
    identifier: &'static str,
    params: Vec<(&'static str, &'static str)>,
}

/// The table: clusters differing in the inputs `CreateDBCluster` takes — credentials,
/// port, retention, encryption, protection, logs exports, storage type, scaling, network
/// type, tags and availability zones. None of them is an input to the members under test,
/// which is exactly the rule.
fn create_cases() -> Vec<CreateCase> {
    vec![
        CreateCase {
            label: "the bare minimum",
            identifier: "flags-minimal",
            params: vec![],
        },
        CreateCase {
            label: "with master credentials",
            identifier: "flags-credentials",
            params: vec![
                ("MasterUsername", "admin"),
                ("MasterUserPassword", "secret99"),
            ],
        },
        CreateCase {
            label: "encrypted, with a key and a long retention",
            identifier: "flags-encrypted",
            params: vec![
                ("StorageEncrypted", "true"),
                ("KmsKeyId", "alias/aws/rds"),
                ("BackupRetentionPeriod", "35"),
            ],
        },
        CreateCase {
            label: "protected from deletion, on a chosen port",
            identifier: "flags-protected",
            params: vec![("DeletionProtection", "true"), ("Port", "27018")],
        },
        CreateCase {
            label: "exporting logs, copying tags, on the iopt1 storage type",
            identifier: "flags-logs",
            params: vec![
                ("EnableCloudwatchLogsExports.member.1", "audit"),
                ("CopyTagsToSnapshot", "true"),
                ("StorageType", "iopt1"),
                ("Tags.Tag.1.Key", "owner"),
                ("Tags.Tag.1.Value", "tests"),
            ],
        },
        CreateCase {
            label: "serverless, in two availability zones, on a chosen network type",
            identifier: "flags-serverless",
            params: vec![
                ("ServerlessV2ScalingConfiguration.MinCapacity", "0.5"),
                ("ServerlessV2ScalingConfiguration.MaxCapacity", "8"),
                ("AvailabilityZones.AvailabilityZone.1", "us-east-1a"),
                ("AvailabilityZones.AvailabilityZone.2", "us-east-1b"),
                ("NetworkType", "IPV4"),
            ],
        },
    ]
}

#[tokio::test]
async fn every_cluster_envelope_carries_the_added_members_whatever_the_create_asked_for() {
    let docdb = Docdb::new();

    for case in create_cases() {
        let mut params: Vec<(&str, &str)> =
            vec![("DBClusterIdentifier", case.identifier), ("Engine", "docdb")];
        params.extend(case.params.iter().copied());

        // 1. The create envelope.
        let created = docdb.ok(case.label, "CreateDBCluster", &params).await;
        assert_envelope_flags(&format!("{} / create", case.label), &created);

        let_the_clock_catch_up();

        // 2. The describe envelope, which reads the record back out of state.
        let described = docdb
            .ok(
                case.label,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", case.identifier)],
            )
            .await;
        assert_envelope_flags(&format!("{} / describe", case.label), &described);

        // 3. The modify envelope — and a modify that changes something else must leave
        //    every one of them alone.
        let modified = docdb
            .ok(
                case.label,
                "ModifyDBCluster",
                &[
                    ("DBClusterIdentifier", case.identifier),
                    ("BackupRetentionPeriod", "7"),
                    ("DeletionProtection", "false"),
                ],
            )
            .await;
        assert_eq!(
            text(&modified, "BackupRetentionPeriod"),
            "7",
            "case {:?}: the modify must really have been applied, got {modified}",
            case.label
        );
        assert_envelope_flags(&format!("{} / modify", case.label), &modified);

        // 4. The delete envelope, the last path that reports a cluster.
        let deleted = docdb
            .ok(
                case.label,
                "DeleteDBCluster",
                &[
                    ("DBClusterIdentifier", case.identifier),
                    ("SkipFinalSnapshot", "true"),
                ],
            )
            .await;
        assert_eq!(
            text(&deleted, "Status"),
            "deleting",
            "case {:?}: the delete must report the deleting status, got {deleted}",
            case.label
        );
        assert_envelope_flags(&format!("{} / delete", case.label), &deleted);
    }
}

#[tokio::test]
async fn a_multi_cluster_page_carries_the_flags_on_every_cluster_it_lists() {
    let docdb = Docdb::new();
    let identifiers = ["page-one", "page-two", "page-three"];
    for identifier in identifiers {
        docdb
            .ok(
                identifier,
                "CreateDBCluster",
                &[("DBClusterIdentifier", identifier), ("Engine", "docdb")],
            )
            .await;
    }
    let_the_clock_catch_up();

    let page = docdb.ok("whole page", "DescribeDBClusters", &[]).await;
    assert_eq!(
        count(&page, "DBCluster"),
        identifiers.len(),
        "case \"whole page\": expected every cluster, got {page}"
    );
    for (member, expected) in ENVELOPE_FLAGS {
        assert_eq!(
            count(&page, member),
            identifiers.len(),
            "case \"whole page\": every listed cluster must carry one {member}, got {page}"
        );
        for (index, value) in texts(&page, member).into_iter().enumerate() {
            assert_eq!(
                value, expected,
                "case \"whole page\" (cluster {index}): expected {member}={expected}, \
                 got {value:?} in {page}"
            );
        }
    }
}

#[tokio::test]
async fn an_instance_keeps_its_own_documented_minor_version_upgrade_default() {
    // The cluster-level flag is a different member from the instance-level one:
    // `CreateDBInstanceMessage$AutoMinorVersionUpgrade` documents "Default: false" and is
    // a real input, so an instance created without it reports false while its cluster
    // reports true.
    let docdb = Docdb::new();
    docdb
        .ok(
            "host cluster",
            "CreateDBCluster",
            &[("DBClusterIdentifier", "flags-host"), ("Engine", "docdb")],
        )
        .await;
    for (case, identifier, sent, expected) in [
        ("no input", "flags-member-default", None, "false"),
        (
            "explicit false",
            "flags-member-off",
            Some("false"),
            "false",
        ),
        ("explicit true", "flags-member-on", Some("true"), "true"),
    ] {
        let mut params: Vec<(&str, &str)> = vec![
            ("DBInstanceIdentifier", identifier),
            ("DBInstanceClass", "db.r5.large"),
            ("Engine", "docdb"),
            ("DBClusterIdentifier", "flags-host"),
        ];
        if let Some(sent) = sent {
            params.push(("AutoMinorVersionUpgrade", sent));
        }
        let created = docdb.ok(case, "CreateDBInstance", &params).await;
        assert_eq!(
            opt_text(&created, "AutoMinorVersionUpgrade").as_deref(),
            Some(expected),
            "case {case:?}: an instance reports the flag it was created with, got {created}"
        );
    }

    let_the_clock_catch_up();
    let cluster = docdb
        .ok(
            "host cluster describe",
            "DescribeDBClusters",
            &[("DBClusterIdentifier", "flags-host")],
        )
        .await;
    assert_envelope_flags("host cluster describe", &cluster);
}

// ---------------------------------------------------------------------------
// The INSTANCE envelope: the attribute set every Amazon DocumentDB instance carries
// ---------------------------------------------------------------------------

/// The members an instance envelope carries with the same value whatever the instance is,
/// and the value each one has for this engine.
///
/// Sources: `DBInstance$PubliclyAccessible` states "Amazon DocumentDB does not currently
/// support public endpoints. The value of `PubliclyAccessible` is always `false`"; the
/// others are what the shared Amazon RDS wire API this control plane is fronted by reports
/// for an instance whose storage, port and master user belong to its CLUSTER — no storage
/// size, no provisioned throughput and no port of the instance's own, the one license
/// model this engine publishes, VPC-only networking, and no HTTP endpoint — each cited on
/// the constant in `src/state.rs` that projects it.
const INSTANCE_ATTRIBUTES: [(&str, &str); 8] = [
    ("AllocatedStorage", "1"),
    ("StorageThroughput", "0"),
    ("DbInstancePort", "0"),
    ("PubliclyAccessible", "false"),
    ("VPCNetworkingEnabled", "true"),
    ("HttpEndpointEnabled", "false"),
    ("LicenseModel", "na"),
    // Reported on every instance, one subelement per pending change: an instance with
    // nothing pending carries the element with nothing inside it.
    ("PendingModifiedValues", ""),
];

/// One row of the instance table: what its cluster was created with, what the instance
/// asked for, and the members whose value follows from those.
struct InstanceCase {
    label: &'static str,
    cluster: &'static str,
    instance: &'static str,
    /// The `MasterUsername` the cluster was created with, when it was created with one.
    master_username: Option<&'static str>,
    /// The `StorageType` the cluster named, when it named one.
    storage_type: Option<&'static str>,
    /// The `PromotionTier` the instance asked for, when it asked for one.
    promotion_tier: Option<&'static str>,
    /// The storage type the instance must report: the one its cluster manages its storage
    /// on, which is `standard` for a cluster that named none
    /// (`state::DEFAULT_STORAGE_TYPE`).
    expected_storage_type: &'static str,
    /// The promotion tier the instance must report; `CreateDBInstanceMessage$PromotionTier`
    /// documents "Default: 1 Valid values: 0-15" for one that asked for none.
    expected_promotion_tier: &'static str,
}

/// The table: instances differing along what the rule generalises over — whether their
/// cluster carries a master user, which of the engine's storage types manages their
/// storage, and which promotion tier they were created at, including both published bounds
/// of that range.
fn instance_cases() -> [InstanceCase; 4] {
    [
        InstanceCase {
            label: "a cluster with a master user, on the default storage type",
            cluster: "inst-attrs-default",
            instance: "inst-attrs-default-1",
            master_username: Some("probeadmin"),
            storage_type: None,
            promotion_tier: Some("1"),
            expected_storage_type: "standard",
            expected_promotion_tier: "1",
        },
        InstanceCase {
            label: "an instance that asks for no promotion tier",
            cluster: "inst-attrs-notier",
            instance: "inst-attrs-notier-1",
            master_username: Some("dbadmin"),
            storage_type: Some("standard"),
            promotion_tier: None,
            expected_storage_type: "standard",
            expected_promotion_tier: "1",
        },
        InstanceCase {
            label: "a cluster on the iopt1 storage type, at the top of the tier range",
            cluster: "inst-attrs-iopt1",
            instance: "inst-attrs-iopt1-1",
            master_username: Some("ioadmin"),
            storage_type: Some("iopt1"),
            promotion_tier: Some("15"),
            expected_storage_type: "iopt1",
            expected_promotion_tier: "15",
        },
        InstanceCase {
            label: "a cluster created without a master user, at the bottom of the range",
            cluster: "inst-attrs-nouser",
            instance: "inst-attrs-nouser-1",
            master_username: None,
            storage_type: None,
            promotion_tier: Some("0"),
            expected_storage_type: "standard",
            expected_promotion_tier: "0",
        },
    ]
}

/// Assert that `xml` carries the whole instance attribute set — the members fixed for this
/// engine ([`INSTANCE_ATTRIBUTES`]) and the ones that follow from the row. Asserted on the
/// raw XML, so a member the serializer omits fails rather than reading as a default.
fn assert_instance_attributes(case: &str, row: &InstanceCase, xml: &str) {
    for (member, expected) in INSTANCE_ATTRIBUTES {
        let actual = opt_text(xml, member);
        assert_eq!(
            actual.as_deref(),
            Some(expected),
            "case {case:?}: the instance envelope must carry {member}={expected:?}, \
             got {actual:?} in {xml}"
        );
    }
    for (member, expected) in [
        ("StorageType", Some(row.expected_storage_type)),
        ("PromotionTier", Some(row.expected_promotion_tier)),
        ("MasterUsername", row.master_username),
    ] {
        let actual = opt_text(xml, member);
        assert_eq!(
            actual.as_deref(),
            expected,
            "case {case:?}: the instance envelope must report {member}={expected:?}, \
             got {actual:?} in {xml}"
        );
    }
}

#[tokio::test]
async fn every_instance_envelope_carries_the_documentdb_instance_attribute_set() {
    let docdb = Docdb::new();

    for row in instance_cases() {
        let mut cluster_params: Vec<(&str, &str)> =
            vec![("DBClusterIdentifier", row.cluster), ("Engine", "docdb")];
        if let Some(user) = row.master_username {
            cluster_params.push(("MasterUsername", user));
            cluster_params.push(("MasterUserPassword", "secret99"));
        }
        if let Some(storage) = row.storage_type {
            cluster_params.push(("StorageType", storage));
        }
        docdb
            .ok(row.label, "CreateDBCluster", &cluster_params)
            .await;

        let mut instance_params: Vec<(&str, &str)> = vec![
            ("DBInstanceIdentifier", row.instance),
            ("DBInstanceClass", "db.r5.large"),
            ("Engine", "docdb"),
            ("DBClusterIdentifier", row.cluster),
        ];
        if let Some(tier) = row.promotion_tier {
            instance_params.push(("PromotionTier", tier));
        }

        // 1. The create envelope.
        let created = docdb
            .ok(row.label, "CreateDBInstance", &instance_params)
            .await;
        assert_instance_attributes(&format!("{} / create", row.label), &row, &created);

        // 2. The describe envelope, which reads the record back out of state: the same
        //    projection, so the read reports exactly what the create announced. The
        //    attribute set is a property of the record and its cluster, so it does not wait
        //    on the status the clock owes (`cluster_lifecycle.rs` pins that transition).
        let described = docdb
            .ok(
                row.label,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", row.instance)],
            )
            .await;
        assert_instance_attributes(&format!("{} / describe", row.label), &row, &described);
    }

    // 3. The delete envelope, the last path that reports an instance. A delete is refused
    //    while the instance is still coming up, so the clock is let past ONCE here rather
    //    than once per row.
    let_the_clock_catch_up();
    for row in instance_cases() {
        let deleted = docdb
            .ok(
                row.label,
                "DeleteDBInstance",
                &[("DBInstanceIdentifier", row.instance)],
            )
            .await;
        assert_instance_attributes(&format!("{} / delete", row.label), &row, &deleted);
    }
}

#[tokio::test]
async fn a_multi_instance_page_carries_the_attribute_set_on_every_instance_it_lists() {
    let docdb = Docdb::new();
    docdb
        .ok(
            "host cluster",
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "inst-page-host"),
                ("Engine", "docdb"),
                ("MasterUsername", "pageadmin"),
                ("MasterUserPassword", "secret99"),
            ],
        )
        .await;
    // The cluster's writer and two replicas: the attribute set is the same on every member,
    // whatever its place in the cluster.
    let identifiers = ["inst-page-1", "inst-page-2", "inst-page-3"];
    for identifier in identifiers {
        docdb
            .ok(
                identifier,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", identifier),
                    ("DBInstanceClass", "db.r5.large"),
                    ("Engine", "docdb"),
                    ("DBClusterIdentifier", "inst-page-host"),
                ],
            )
            .await;
    }

    let page = docdb.ok("whole page", "DescribeDBInstances", &[]).await;
    assert_eq!(
        count(&page, "DBInstance"),
        identifiers.len(),
        "case \"whole page\": expected every instance, got {page}"
    );
    for (member, expected) in INSTANCE_ATTRIBUTES {
        assert_eq!(
            count(&page, member),
            identifiers.len(),
            "case \"whole page\": every listed instance must carry one {member}, got {page}"
        );
        for (index, value) in texts(&page, member).into_iter().enumerate() {
            assert_eq!(
                value, expected,
                "case \"whole page\" (instance {index}): expected {member}={expected:?}, \
                 got {value:?} in {page}"
            );
        }
    }
    for member in ["MasterUsername", "StorageType"] {
        assert_eq!(
            count(&page, member),
            identifiers.len(),
            "case \"whole page\": every listed instance must carry one {member}, got {page}"
        );
    }
}

// ---------------------------------------------------------------------------
// The host an instance announces
// ---------------------------------------------------------------------------

/// One row of the announced-host table.
struct AddressCase {
    label: &'static str,
    /// The `Host` header the request carries — the address the client is talking to — or
    /// `None` for a request that addresses no host of its own.
    host: Option<&'static str>,
    /// The URL this emulator's data port is published at, or `None` for a router served on
    /// no port.
    served_base: Option<&'static str>,
    /// The host this emulator is REACHED at in this row, which the announced address must
    /// not be: this emulator serves no Amazon DocumentDB data plane, so an instance keeps
    /// the name its own control plane publishes rather than this process's address.
    reached_host: &'static str,
    /// The `Port` the cluster is created with, when the row names one.
    cluster_port: Option<&'static str>,
    /// The port its `Endpoint$Port` must report: the port its CLUSTER publishes for the
    /// engine, whatever address the request arrived at.
    expected_port: &'static str,
}

#[tokio::test]
async fn an_instance_announces_the_dns_name_under_its_clusters_endpoint_suffix() {
    // The rows span how the emulator is addressed and how it is served: the default data
    // port, an ephemeral one, loopback by name, an IPv6 literal whose own colons live
    // inside its brackets, an authority naming no port, and the two ways a request can name
    // no address of this process while the process itself is served at one. None of them
    // changes what the instance announces — that is the rule — and the announced PORT is
    // its cluster's, which the rows vary independently.
    let cases = [
        AddressCase {
            label: "the front door on the emulator's default data port",
            host: Some("127.0.0.1:9297"),
            served_base: None,
            reached_host: "127.0.0.1",
            cluster_port: None,
            expected_port: "27017",
        },
        AddressCase {
            label: "an ephemeral data port, on a cluster that chose its engine port",
            host: Some("127.0.0.1:44903"),
            served_base: None,
            reached_host: "127.0.0.1",
            cluster_port: Some("27018"),
            expected_port: "27018",
        },
        AddressCase {
            label: "loopback by name",
            host: Some("localhost:41509"),
            served_base: None,
            reached_host: "localhost",
            cluster_port: None,
            expected_port: "27017",
        },
        AddressCase {
            label: "an IPv6 loopback literal",
            host: Some("[::1]:38001"),
            served_base: None,
            reached_host: "[::1]",
            cluster_port: None,
            expected_port: "27017",
        },
        AddressCase {
            label: "an authority that names no port of its own",
            host: Some("127.0.0.1"),
            served_base: None,
            reached_host: "127.0.0.1",
            cluster_port: None,
            expected_port: "27017",
        },
        AddressCase {
            label: "a request that named no host, on an emulator served on a port",
            host: None,
            served_base: Some("https://127.0.0.1:9297"),
            reached_host: "127.0.0.1",
            cluster_port: None,
            expected_port: "27017",
        },
        AddressCase {
            label: "a request that addressed an AWS host, on an emulator served on a port",
            host: Some("docdb.us-east-1.amazonaws.com"),
            served_base: Some("https://127.0.0.1:35117"),
            reached_host: "127.0.0.1",
            cluster_port: None,
            expected_port: "27017",
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let docdb = match case.served_base {
            Some(base) => Docdb::served_at(base),
            None => Docdb::new(),
        };
        let cluster = format!("announced-cluster-{index}");
        let instance = format!("announced-instance-{index}");
        let mut cluster_params: Vec<(&str, &str)> = vec![
            ("DBClusterIdentifier", cluster.as_str()),
            ("Engine", "docdb"),
        ];
        if let Some(port) = case.cluster_port {
            cluster_params.push(("Port", port));
        }
        let created_cluster = call(&docdb, case.host, "CreateDBCluster", &cluster_params).await;
        assert_eq!(
            created_cluster.status, 200,
            "case {:?}: CreateDBCluster must succeed, got {} with {}",
            case.label, created_cluster.status, created_cluster.xml
        );
        let created = call(
            &docdb,
            case.host,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", instance.as_str()),
                ("DBInstanceClass", "db.r5.large"),
                ("Engine", "docdb"),
                ("DBClusterIdentifier", cluster.as_str()),
            ],
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreateDBInstance must succeed, got {} with {}",
            case.label, created.status, created.xml
        );

        let described = call(
            &docdb,
            case.host,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", instance.as_str())],
        )
        .await;
        // ROUND TRIP: the describe reports the host the create announced, because the
        // create resolved it once and stored it on the record.
        for (path, xml) in [("create", &created.xml), ("describe", &described.xml)] {
            let address = text(xml, "Address");
            let port = text(xml, "Port");
            // The name this engine's control plane publishes for a member: the instance's
            // own label under the DNS suffix its cluster minted.
            assert!(
                address.starts_with(&format!("{instance}."))
                    && address.ends_with(&format!(".{REGION}.docdb.amazonaws.com")),
                "case {:?} / {path}: expected an Endpoint.Address of the form \
                 {instance}.<cluster label>.{REGION}.docdb.amazonaws.com, got {address:?} in \
                 {xml}",
                case.label
            );
            assert!(
                !address.contains(case.reached_host),
                "case {:?} / {path}: the announced address must not name the address this \
                 emulator was reached at ({:?}), which serves no data plane for this \
                 engine, got {address:?}",
                case.label,
                case.reached_host
            );
            assert!(
                !address.contains("://"),
                "case {:?} / {path}: Endpoint.Address is a host, not a URL, got {address:?}",
                case.label
            );
            assert_eq!(
                port, case.expected_port,
                "case {:?} / {path}: expected Endpoint.Port {:?}, got {port:?} in {xml}",
                case.label, case.expected_port
            );
            // The point of the pair: `<Address>:<Port>` is ONE address a client dials, so
            // it parses as a URL naming exactly the announced port.
            let dialled = format!("https://{address}:{port}/")
                .parse::<http::Uri>()
                .unwrap_or_else(|error| {
                    panic!(
                        "case {:?} / {path}: https://{address}:{port}/ must be a dialable \
                         URL: {error}",
                        case.label
                    )
                });
            assert_eq!(
                dialled.port_u16().map(|found| found.to_string()).as_deref(),
                Some(case.expected_port),
                "case {:?} / {path}: expected the dialed URL to name port {:?}, got {:?}",
                case.label,
                case.expected_port,
                dialled.port_u16()
            );
        }
    }
}

#[tokio::test]
async fn an_instance_endpoint_reports_the_port_its_cluster_publishes() {
    // The port the pair names is the cluster's, as its own `Port` member sets it
    // (`CreateDBClusterMessage$Port`: "Default: 27017"), because an instance of this engine
    // has no port of its own to publish.
    for (label, requested_port, expected_port) in [
        ("no Port member, so the engine default", None, "27017"),
        ("a Port member the cluster chose", Some("27018"), "27018"),
    ] {
        let docdb = Docdb::new();
        let cluster = format!("unserved-cluster-{expected_port}");
        let instance = format!("unserved-instance-{expected_port}");
        let mut cluster_params: Vec<(&str, &str)> = vec![
            ("DBClusterIdentifier", cluster.as_str()),
            ("Engine", "docdb"),
        ];
        if let Some(port) = requested_port {
            cluster_params.push(("Port", port));
        }
        docdb.ok(label, "CreateDBCluster", &cluster_params).await;
        let created = docdb
            .ok(
                label,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", instance.as_str()),
                    ("DBInstanceClass", "db.r5.large"),
                    ("Engine", "docdb"),
                    ("DBClusterIdentifier", cluster.as_str()),
                ],
            )
            .await;
        let address = text(&created, "Address");
        assert!(
            address.starts_with(&format!("{instance}."))
                && address.ends_with(&format!(".{REGION}.docdb.amazonaws.com")),
            "case {label:?}: unexpected Endpoint.Address {address:?} in {created}"
        );
        let port = text(&created, "Port");
        assert_eq!(
            port, expected_port,
            "case {label:?}: expected Endpoint.Port {expected_port:?}, got {port:?} in \
             {created}"
        );
    }
}

/// Drive one awsQuery call, with or without a `Host` header.
async fn call(
    docdb: &Docdb,
    host: Option<&str>,
    action: &str,
    params: &[(&str, &str)],
) -> crate::cluster_lifecycle::Reply {
    match host {
        Some(host) => docdb.call_addressed_to(host, action, params).await,
        None => docdb.call(action, params).await,
    }
}
