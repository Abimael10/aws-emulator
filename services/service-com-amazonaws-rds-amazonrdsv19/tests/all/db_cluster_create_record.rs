// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests pinning that a `CreateDBCluster` acknowledgement IS the record the
//! operation stored, and that each cluster is minted an identity of its own.
//!
//! Every call goes over the wire, through the same `MultiServiceRouter` mounting helper the
//! sibling suites use, as a real awsQuery form POST carrying the SigV4 credential scope the
//! router keys on.
//!
//! The rules under test:
//!
//! * A create ANSWERS WITH THE RECORD IT PERSISTED: the whole `DBCluster` envelope the
//!   create returns is, element for element, the one a later `DescribeDBClusters` reports
//!   for the same cluster — the only member that may differ is the `Status` the clock
//!   advances on its own. Checking the ENVELOPES against each other rather than a list of
//!   members covers every member the create derived (the ARN, the minted identity, the
//!   endpoints, the resolved engine version, the defaults, the timestamps, the tags and the
//!   zones) in one assertion, so a member computed at response time instead of being stored
//!   fails here whether or not any suite names it.
//! * An identity RDS mints is the resource's own: the `DbClusterResourceId` and the
//!   server-assigned endpoint label are of their published forms, no two clusters share
//!   them, and no later read changes them.
//!
//! The table spans what those rules generalise over: both cluster TYPES `CreateDBCluster`
//! documents (Aurora DB clusters and Multi-AZ DB clusters), three engine families
//! `Engine` publishes (an Aurora engine, the Neptune engine this front door also serves,
//! and an RDS engine with a cluster form), a request that names nothing beyond the two
//! required members and requests that name list-, structure- and boolean-valued ones, and
//! an identifier given in mixed case.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_clusters::{describe_one, the_cluster};
use crate::db_instances::{call, emulator, expect_ok, text, texts, wait_for_settle, REGION};

/// One cluster of the round trip.
struct RecordCase {
    case: &'static str,
    /// The `DBClusterIdentifier` the create names.
    identifier: &'static str,
    /// What that identifier is stored and reported as: "this parameter is stored as a
    /// lowercase string".
    stored_identifier: &'static str,
    engine: &'static str,
    /// The parameters this row adds beyond `DBClusterIdentifier` and `Engine`.
    extra: &'static [(&'static str, &'static str)],
    /// The elements the answer must carry because the REQUEST named them, as
    /// `(element, text)` — so a row cannot pass on an envelope that dropped what it sent.
    named: &'static [(&'static str, &'static str)],
}

/// The clusters the round trip creates and reads back.
const RECORD_CASES: &[RecordCase] = &[
    RecordCase {
        case: "aurora-mysql, nothing beyond the two required members",
        identifier: "emu-record-aurora-mysql",
        stored_identifier: "emu-record-aurora-mysql",
        engine: "aurora-mysql",
        extra: &[],
        named: &[],
    },
    RecordCase {
        case: "aurora-postgresql, mixed case, serverless v2 capacity, advanced insights, \
               tags and security groups",
        identifier: "EMU-Record-Aurora-PG",
        stored_identifier: "emu-record-aurora-pg",
        engine: "aurora-postgresql",
        extra: &[
            ("MasterUsername", "pgadmin"),
            ("MasterUserPassword", "a-strong-password"),
            ("DatabaseName", "recorddb"),
            ("StorageEncrypted", "true"),
            ("ServerlessV2ScalingConfiguration.MinCapacity", "2.0"),
            ("ServerlessV2ScalingConfiguration.MaxCapacity", "64.0"),
            // "If you set this value to advanced, you must also set the
            // PerformanceInsightsEnabled parameter to true and the
            // PerformanceInsightsRetentionPeriod parameter to 465."
            ("DatabaseInsightsMode", "advanced"),
            ("EnablePerformanceInsights", "true"),
            ("PerformanceInsightsRetentionPeriod", "465"),
            ("VpcSecurityGroupIds.VpcSecurityGroupId.1", "sg-00000001"),
            ("VpcSecurityGroupIds.VpcSecurityGroupId.2", "sg-00000002"),
            ("Tags.Tag.1.Key", "team"),
            ("Tags.Tag.1.Value", "emulator"),
        ],
        named: &[
            ("DatabaseName", "recorddb"),
            ("MasterUsername", "pgadmin"),
            ("StorageEncrypted", "true"),
            ("DatabaseInsightsMode", "advanced"),
            ("PerformanceInsightsEnabled", "true"),
            ("PerformanceInsightsRetentionPeriod", "465"),
            ("MinCapacity", "2.0"),
            ("MaxCapacity", "64.0"),
            ("VpcSecurityGroupId", "sg-00000002"),
            ("Key", "team"),
            ("Value", "emulator"),
        ],
    },
    RecordCase {
        case: "neptune, an Aurora-style cluster of another engine family",
        identifier: "emu-record-neptune",
        stored_identifier: "emu-record-neptune",
        engine: "neptune",
        extra: &[
            ("BackupRetentionPeriod", "7"),
            ("PreferredBackupWindow", "02:00-02:30"),
            ("DeletionProtection", "true"),
            // The one log type the Neptune engine publishes as exportable.
            ("EnableCloudwatchLogsExports.member.1", "audit"),
        ],
        named: &[
            ("BackupRetentionPeriod", "7"),
            ("PreferredBackupWindow", "02:00-02:30"),
            ("DeletionProtection", "true"),
            // The entries of `EnabledCloudwatchLogsExports` carry no element name of their
            // own in the model, so each is a `<member>` of the list.
            ("member", "audit"),
        ],
    },
    RecordCase {
        case: "postgres, a Multi-AZ DB cluster with the four settings it requires",
        identifier: "emu-record-postgres",
        stored_identifier: "emu-record-postgres",
        engine: "postgres",
        extra: &[
            ("AllocatedStorage", "400"),
            ("DBClusterInstanceClass", "db.m5d.large"),
            // "Multi-AZ DB clusters - io1 | io2 | gp3", and `Iops` "must be a multiple
            // between .5 and 50 of the storage amount".
            ("StorageType", "gp3"),
            ("Iops", "3000"),
            ("PubliclyAccessible", "true"),
            ("MonitoringInterval", "60"),
            ("CopyTagsToSnapshot", "true"),
        ],
        named: &[
            ("AllocatedStorage", "400"),
            ("DBClusterInstanceClass", "db.m5d.large"),
            ("StorageType", "gp3"),
            ("Iops", "3000"),
            ("PubliclyAccessible", "true"),
            ("MonitoringInterval", "60"),
            ("CopyTagsToSnapshot", "true"),
        ],
    },
];

/// `CreateDBCluster` for one row of the table.
async fn create_case(router: &MultiServiceRouter, row: &RecordCase) -> (u16, String) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", row.identifier.to_string()),
        ("Engine", row.engine.to_string()),
    ];
    params.extend(
        row.extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    call(router, "CreateDBCluster", &params).await
}

/// One `<DBCluster>` subtree split at the CLUSTER's own `<Status>`: what precedes it, its
/// text, and what follows it.
///
/// The cluster's status is the first `<Status>` of the envelope — none of the members the
/// modelled shape orders ahead of it carries a nested one — and it is the one member the
/// clock moves between a create and a later read, so splitting there is what lets the rest
/// of the two envelopes be compared whole.
fn split_at_status(case: &str, cluster: &str) -> (String, String, String) {
    const OPEN: &str = "<Status>";
    const CLOSE: &str = "</Status>";
    let open = cluster
        .find(OPEN)
        .unwrap_or_else(|| panic!("case {case:?}: expected a cluster <Status> in {cluster}"));
    let content = open + OPEN.len();
    let close = content
        + cluster[content..]
            .find(CLOSE)
            .unwrap_or_else(|| panic!("case {case:?}: expected a closed <Status> in {cluster}"));
    (
        cluster[..open].to_string(),
        cluster[content..close].to_string(),
        cluster[close + CLOSE.len()..].to_string(),
    )
}

/// Assert the envelope carries every element the request named, and the identifier it is
/// stored under.
fn assert_named(case: &str, label: &str, cluster: &str, row: &RecordCase) {
    let case = format!("{case} ({label})");
    let identifier = text(cluster, "DBClusterIdentifier");
    assert_eq!(
        identifier, row.stored_identifier,
        "case {case:?}: expected the folded identifier {:?}, got {identifier:?} in {cluster}",
        row.stored_identifier
    );
    let engine = text(cluster, "Engine");
    assert_eq!(
        engine, row.engine,
        "case {case:?}: expected <Engine> {:?}, got {engine:?} in {cluster}",
        row.engine
    );
    for (element, expected) in row.named {
        let found = texts(cluster, element);
        assert!(
            found.iter().any(|text| text == expected),
            "case {case:?}: expected <{element}> {expected:?}, got {found:?} in {cluster}"
        );
    }
}

/// `cluster` with the members the CLOCK fills removed.
///
/// The point-in-time restore window is "the earliest and latest time a database can be
/// restored to" AS OF the moment of the read, so it differs between any two reads of one
/// record — exactly as `Status` does while the transition the create owes is still
/// outstanding, which is why this comparison already splits at that member.
/// `PendingModifiedValues` is the same kind of member: a cluster still being provisioned
/// with a master credential has that credential pending, and the clock clears it when the
/// cluster comes up.
fn without_clock_derived_members(cluster: &str) -> String {
    let mut stripped = cluster.to_string();
    for member in [
        "EarliestRestorableTime",
        "LatestRestorableTime",
        "PendingModifiedValues",
    ] {
        let close = format!("</{member}>");
        while let Some(start) = stripped.find(&format!("<{member}>")) {
            let Some(end) = stripped[start..]
                .find(&close)
                .map(|offset| start + offset + close.len())
            else {
                break;
            };
            stripped.replace_range(start..end, "");
        }
    }
    stripped
}

#[tokio::test]
async fn the_record_a_create_acknowledges_is_the_record_describe_reports() {
    let router = emulator();

    for row in RECORD_CASES {
        let created = expect_ok(row.case, create_case(&router, row).await);
        let created = without_clock_derived_members(&the_cluster(row.case, &created));
        assert_named(row.case, "CreateDBCluster", &created, row);
        let (created_head, created_status, created_tail) = split_at_status(row.case, &created);
        assert_eq!(
            created_status, "creating",
            "case {:?}: a newly created cluster is not usable yet, got {created}",
            row.case
        );

        // The same cluster, read back out of the store once the transition the clock owed
        // it has come due.
        wait_for_settle();
        let read_back = without_clock_derived_members(
            &describe_one(&router, row.case, row.identifier).await,
        );
        assert_named(row.case, "DescribeDBClusters", &read_back, row);
        let (read_head, read_status, read_tail) = split_at_status(row.case, &read_back);
        assert_eq!(
            read_status, "available",
            "case {:?}: expected the cluster to have reached available on its own, got \
             {read_back}",
            row.case
        );

        // Everything but that status: the create's acknowledgement and the later report are
        // one record.
        assert_eq!(
            created_head, read_head,
            "case {:?}: the create answered members before <Status> that the stored record \
             does not report: {created} versus {read_back}",
            row.case
        );
        assert_eq!(
            created_tail, read_tail,
            "case {:?}: the create answered members after <Status> that the stored record \
             does not report: {created} versus {read_back}",
            row.case
        );
    }
}

/// One cluster of the identity test.
struct IdentityCase {
    identifier: &'static str,
    engine: &'static str,
    /// Whether this emulator serves the engine's own wire, which decides what the cluster
    /// publishes as its writer endpoint: the address that wire is answered at, or the name the
    /// cluster mints under the `cluster-` label.
    served: bool,
    /// The parameters this row adds beyond `DBClusterIdentifier` and `Engine`.
    extra: &'static [(&'static str, &'static str)],
}

/// The clusters the identity test creates.
///
/// Two of them share an engine, so an identity that varied only with the engine would not
/// pass; one is a Multi-AZ DB cluster, whose identity is minted the same way; and the last
/// is an engine this emulator serves no wire for, whose endpoints sit in its own control
/// plane's DNS domain rather than in the zone a served cluster is reached in.
const IDENTITY_CASES: &[IdentityCase] = &[
    IdentityCase {
        identifier: "emu-identity-one",
        engine: "aurora-mysql",
        served: true,
        extra: &[],
    },
    IdentityCase {
        identifier: "emu-identity-two",
        engine: "aurora-mysql",
        served: true,
        extra: &[],
    },
    IdentityCase {
        identifier: "emu-identity-three",
        engine: "aurora-postgresql",
        served: true,
        extra: &[],
    },
    IdentityCase {
        identifier: "emu-identity-four",
        engine: "postgres",
        served: true,
        extra: &[
            ("AllocatedStorage", "100"),
            ("DBClusterInstanceClass", "db.m6gd.xlarge"),
            ("StorageType", "io2"),
            ("Iops", "1000"),
        ],
    },
    IdentityCase {
        identifier: "emu-identity-five",
        // An engine whose wire this emulator serves no data plane for, so its cluster's
        // endpoints sit in its own control plane's DNS domain.
        engine: "docdb",
        served: false,
        extra: &[],
    },
];

/// What one cluster envelope reports as the cluster's OWN identity: the values RDS mints
/// for it rather than taking from the request.
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    resource_id: String,
    /// The DNS name the cluster's reader endpoint carries the server-assigned label in,
    /// which is minted per cluster.
    reader_endpoint: String,
    arn: String,
}

/// The cluster's READER endpoint, asserting its published form on the way.
///
/// Every cluster publishes the documented DNS form
/// `<identifier>.cluster-ro-<label>.<region>.<suffix>`, in which the 12-character
/// server-assigned label is visible: the CLUSTER mints both of its endpoint names from its
/// own identifier, and the suffix is the zone its data plane is reached in (see
/// `state::cluster_endpoint_names`). The model gives a DB cluster one `Port` for the pair, so
/// neither member carries a port of its own.
fn reader_endpoint(case: &str, identifier: &str, endpoint: &str) -> String {
    let prefix = format!("{identifier}.cluster-ro-");
    let suffix = format!(".{REGION}.");
    let label = endpoint
        .strip_prefix(&prefix)
        .and_then(|rest| rest.split_once(suffix.as_str()))
        .map(|(label, _domain)| label)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an endpoint of the form \
                 {prefix}<label>{suffix}<domain>, got {endpoint:?}"
            )
        });
    assert!(
        label.len() == 12
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
        "case {case:?}: expected a 12-character server-assigned endpoint label, got {label:?}"
    );
    endpoint.to_string()
}

/// The identity one cluster envelope reports, asserting the published form of each member
/// on the way.
fn identity_of(case: &str, identifier: &str, served: bool, cluster: &str) -> Identity {
    let resource_id = text(cluster, "DbClusterResourceId");
    // "The Amazon Web Services Region-unique, immutable identifier for the DB cluster",
    // written `cluster-` plus server-minted characters.
    let minted = resource_id.strip_prefix("cluster-").unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted resource id, got {resource_id:?}")
    });
    assert!(
        minted.len() == 26
            && minted
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()),
        "case {case:?}: expected 26 server-minted characters, got {resource_id:?}"
    );

    let endpoint = text(cluster, "Endpoint");
    // The writer endpoint is where a client OPENS a session: the address this emulator answers
    // the cluster's engine wire at for an engine whose wire it serves, and the same name under
    // the `cluster-` label — derived from the cluster's identifier too — for one it does not.
    if served {
        assert!(
            endpoint.parse::<std::net::IpAddr>().is_ok(),
            "case {case:?}: expected the address this emulator serves the cluster's engine \
             wire at as the cluster endpoint, got {endpoint:?}"
        );
    } else {
        assert!(
            endpoint.starts_with(&format!("{identifier}.cluster-")),
            "case {case:?}: expected the published DNS form of the cluster endpoint, got \
             {endpoint:?}"
        );
    }
    let reader = text(cluster, "ReaderEndpoint");

    Identity {
        resource_id,
        reader_endpoint: reader_endpoint(case, identifier, &reader),
        arn: text(cluster, "DBClusterArn"),
    }
}

#[tokio::test]
async fn every_cluster_is_minted_an_identity_of_its_own() {
    let router = emulator();

    let mut created: Vec<(&IdentityCase, Identity)> = Vec::new();
    for row in IDENTITY_CASES {
        let mut params: Vec<(&str, String)> = vec![
            ("DBClusterIdentifier", row.identifier.to_string()),
            ("Engine", row.engine.to_string()),
        ];
        params.extend(
            row.extra
                .iter()
                .map(|(name, value)| (*name, (*value).to_string())),
        );
        let body = expect_ok(
            row.identifier,
            call(&router, "CreateDBCluster", &params).await,
        );
        let cluster = the_cluster(row.identifier, &body);
        created.push((
            row,
            identity_of(row.identifier, row.identifier, row.served, &cluster),
        ));
    }

    // No two clusters share a minted identity.
    for (left_index, (left_row, left_identity)) in created.iter().enumerate() {
        let left = left_row.identifier;
        for (right_row, right_identity) in created.iter().skip(left_index + 1) {
            let right = right_row.identifier;
            assert_ne!(
                left_identity.resource_id, right_identity.resource_id,
                "clusters {left} and {right} were minted the same DbClusterResourceId {:?}",
                left_identity.resource_id
            );
            // The DNS name a cluster publishes carries a label minted for that cluster, so
            // no two of them are the same.
            assert_ne!(
                left_identity.reader_endpoint, right_identity.reader_endpoint,
                "clusters {left} and {right} publish the same reader endpoint {:?}",
                left_identity.reader_endpoint
            );
            assert_ne!(
                left_identity.arn, right_identity.arn,
                "clusters {left} and {right} report the same ARN {:?}",
                left_identity.arn
            );
        }
    }

    // An identity is minted ONCE: a later read reports what the create did, rather than a
    // value recomputed per response.
    wait_for_settle();
    for (row, identity) in &created {
        let identifier = row.identifier;
        let read_back = describe_one(&router, identifier, identifier).await;
        let read_identity = identity_of(identifier, identifier, row.served, &read_back);
        assert_eq!(
            read_identity, *identity,
            "cluster {identifier}: expected the identity the create minted {identity:?}, got \
             {read_identity:?} in {read_back}"
        );
    }
}
