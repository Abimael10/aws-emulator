// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for this service's CUSTOM CLUSTER ENDPOINT slice:
//! `CreateDBClusterEndpoint`, the `DescribeDBClusterEndpoints` report that reads what it
//! persisted, `ModifyDBClusterEndpoint` and `DeleteDBClusterEndpoint`.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it (through `db_instances::emulator`, the one mounting
//! helper this test target has), and each request is a real awsQuery form POST carrying the
//! SigV4 credential scope the router keys on. The assertions read the XML the emulator
//! answers with, so a handler that computed nothing could not pass them.
//!
//! The rules under test:
//!
//! 1. A custom endpoint a create persisted is reported by every later read of the region it
//!    was created in — with the type the request asked for, the membership the create
//!    resolved and the cluster it hangs off — until a delete takes it out; it is reported as
//!    an endpoint of type `CUSTOM` whichever type the request asked for, with that requested
//!    type beside it, and the cluster it belongs to reports its address among its own
//!    `CustomEndpoints`.
//! 2. The report also carries the two endpoints a cluster has without a create, of type
//!    `WRITER` and `READER`, and every one of the filter names the operation publishes
//!    selects on the member it names, without regard to case.
//! 3. A request whose identifier, type, cluster or membership cannot be served is refused
//!    with the modelled fault for that cause and leaves nothing behind, and a second create
//!    of one identifier is refused however many creates race for it.
//!
//! The tables below span what those rules generalise over: each published `EndpointType`
//! and its spelling in another case, a membership given as static members, as exclusions and
//! as neither, one and two members, an identifier at one character and at the longest length
//! this service admits plus one past it, the identifier shapes the identifier rule refuses,
//! endpoints read back by cluster, by identifier and by each published filter name, the
//! cluster's own allowance plus one create past it, the region scope, and each state a
//! modification and a delete are answered in.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    CUSTOM_ENDPOINT_TYPES, DB_CLUSTER_ENDPOINT_QUOTA_PER_CLUSTER,
    MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// A second region, so the regional scope of a custom endpoint is observable.
const OTHER_REGION: &str = "eu-west-1";

/// The engine the clusters below run: `CreateDBClusterEndpoint` "applies only to Aurora DB
/// clusters".
const AURORA: &str = "aurora-postgresql";

/// An engine whose clusters are Multi-AZ DB clusters rather than Aurora ones, so the
/// operation does not apply to them at all.
const MULTI_AZ_ENGINE: &str = "postgres";

/// The DB cluster every endpoint in the round-trip table is created for, and the second
/// one its later rows use — one cluster cannot hold more custom endpoints than the
/// per-cluster allowance.
const CLUSTER: &str = "emu-ce-cluster";
const SECOND_CLUSTER: &str = "emu-ce-cluster-b";

/// Two member DB instances of that cluster, so a membership can name one and two of them.
const FIRST_MEMBER: &str = "emu-ce-member-one";
const SECOND_MEMBER: &str = "emu-ce-member-two";

/// The status a custom endpoint reports from an accepted create until the transition this
/// service owes it comes due.
const CREATING: &str = "creating";

/// The status it reports once that transition has settled.
const AVAILABLE: &str = "available";

/// The status an accepted modification reports.
const MODIFYING: &str = "modifying";

/// The status an accepted delete reports.
const DELETING: &str = "deleting";

/// `EndpointType` as every endpoint a caller creates is reported.
const CUSTOM: &str = "CUSTOM";

/// `CreateDBCluster` for an Aurora cluster the endpoints below hang off.
async fn create_cluster(router: &MultiServiceRouter, identifier: &str, engine: &str) -> (u16, String) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
        ("MasterUsername", "emuadmin".to_string()),
        ("MasterUserPassword", "emu-Secret-99".to_string()),
    ];
    if engine == MULTI_AZ_ENGINE {
        // "AllocatedStorage, DBClusterInstanceClass, Iops, StorageType are required to
        // create a Multi-AZ DB cluster", which the Aurora engines take none of.
        params.extend([
            ("AllocatedStorage", "100".to_string()),
            ("DBClusterInstanceClass", "db.m6gd.large".to_string()),
            ("Iops", "1000".to_string()),
            ("StorageType", "io1".to_string()),
        ]);
    }
    call(router, "CreateDBCluster", &params).await
}

/// `CreateDBInstance` for one member of that cluster.
async fn create_member(router: &MultiServiceRouter, identifier: &str, cluster: &str) -> (u16, String) {
    call(
        router,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", identifier),
            ("Engine", AURORA),
            ("DBInstanceClass", "db.r5.large"),
            ("DBClusterIdentifier", cluster),
        ],
    )
    .await
}

/// `CreateDBClusterEndpoint` with its three required members plus `extra`.
async fn create_endpoint(
    router: &MultiServiceRouter,
    identifier: &str,
    cluster: &str,
    endpoint_type: &str,
    extra: &[(&str, String)],
) -> (u16, String) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterEndpointIdentifier", identifier.to_string()),
        ("DBClusterIdentifier", cluster.to_string()),
        ("EndpointType", endpoint_type.to_string()),
    ];
    params.extend(extra.iter().cloned());
    call(router, "CreateDBClusterEndpoint", &params).await
}

/// `DescribeDBClusterEndpoints` with the members one read names.
async fn describe_endpoints(
    router: &MultiServiceRouter,
    params: &[(&str, String)],
) -> (u16, String) {
    call(router, "DescribeDBClusterEndpoints", params).await
}

/// The `<DBClusterEndpointList>` subtrees a report carries, in the order it reports them.
fn reported(body: &str) -> Vec<String> {
    subtrees(body, "DBClusterEndpointList")
}

/// The one endpoint a report carries for `identifier`, polled until it reports `status`.
///
/// The read is `DescribeDBClusterEndpoints` and nothing else — a REPORTING call — so only a
/// transition this service's clock owes can end the loop.
async fn poll_endpoint_until(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    status: &str,
) -> String {
    let params = vec![("DBClusterEndpointIdentifier", identifier.to_string())];
    let read = || describe_endpoints(router, &params);
    let (body, _) = poll_until(
        case,
        &format!("{identifier} reaching {status}"),
        read,
        |http_status, body| {
            http_status == 200
                && reported(body).first().is_some_and(|endpoint| {
                    optional_text(endpoint, "Status").as_deref() == Some(status)
                })
        },
    )
    .await;
    reported(&body)
        .into_iter()
        .next()
        .expect("the poll settled on a report carrying the endpoint")
}

/// Poll `DescribeDBClusterEndpoints` until it reports no endpoint under `identifier` at all.
async fn poll_endpoint_until_gone(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let params = vec![("DBClusterEndpointIdentifier", identifier.to_string())];
    let read = || describe_endpoints(router, &params);
    poll_until(
        case,
        &format!("{identifier} leaving the store"),
        read,
        |http_status, body| http_status == 200 && reported(body).is_empty(),
    )
    .await;
}

/// The `CustomEndpoints` the cluster itself reports, as `DescribeDBClusters` renders them.
async fn cluster_custom_endpoints(router: &MultiServiceRouter, cluster: &str) -> Vec<String> {
    let body = expect_ok(
        "the cluster's own report",
        call(
            router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", cluster)],
        )
        .await,
    );
    let listed = subtrees(&body, "CustomEndpoints");
    listed
        .first()
        .map(|list| texts(list, "member"))
        .unwrap_or_default()
}

/// How a row names the membership of the endpoint it creates.
#[derive(Clone, Copy)]
enum Membership {
    /// Neither list: "all other eligible instances are reachable through the custom
    /// endpoint".
    None,
    /// `StaticMembers`: the instances that are part of the group.
    Static(&'static [&'static str]),
    /// `ExcludedMembers`: the instances that are not.
    Excluded(&'static [&'static str]),
}

impl Membership {
    /// The request parameters this membership is sent as.
    fn params(self) -> Vec<(&'static str, String)> {
        let (member, named) = match self {
            Self::None => return Vec::new(),
            Self::Static(named) => ("StaticMembers", named),
            Self::Excluded(named) => ("ExcludedMembers", named),
        };
        named
            .iter()
            .enumerate()
            .map(|(position, identifier)| {
                (
                    match (member, position) {
                        ("StaticMembers", 0) => "StaticMembers.member.1",
                        ("StaticMembers", _) => "StaticMembers.member.2",
                        (_, 0) => "ExcludedMembers.member.1",
                        (_, _) => "ExcludedMembers.member.2",
                    },
                    (*identifier).to_string(),
                )
            })
            .collect()
    }

    /// The `StaticMembers` the endpoint must report.
    fn static_members(self) -> &'static [&'static str] {
        match self {
            Self::Static(named) => named,
            _ => &[],
        }
    }

    /// The `ExcludedMembers` it must report.
    fn excluded_members(self) -> &'static [&'static str] {
        match self {
            Self::Excluded(named) => named,
            _ => &[],
        }
    }
}

/// One row of the create/describe round trip.
struct EndpointCase {
    case: &'static str,
    /// The cluster the endpoint is created on.
    cluster: &'static str,
    /// The `DBClusterEndpointIdentifier` the create names, in the case it names it.
    named: &'static str,
    /// The identifier the endpoint is stored and reported under: the same value lowercased.
    identifier: &'static str,
    /// The `EndpointType` the create names.
    endpoint_type: &'static str,
    /// The `CustomEndpointType` the endpoint reports for it.
    custom_type: &'static str,
    membership: Membership,
    /// The tags the create assigns.
    tags: &'static [(&'static str, &'static str)],
}

#[tokio::test]
async fn a_created_custom_endpoint_is_reported_by_every_later_read_of_its_region() {
    // One character is the shortest identifier the rule admits, and the longest is
    // `MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN` characters.
    const LONGEST: &str = "emu-ce-identifier-at-the-longest-length-this-service-admits-abc";
    let table: &[EndpointCase] = &[
        EndpointCase {
            case: "a reader endpoint over two static members",
            cluster: CLUSTER,
            named: "emu-ce-reader",
            identifier: "emu-ce-reader",
            endpoint_type: "READER",
            custom_type: "READER",
            membership: Membership::Static(&[FIRST_MEMBER, SECOND_MEMBER]),
            tags: &[("owner", "platform")],
        },
        EndpointCase {
            case: "a writer endpoint with no membership at all",
            cluster: CLUSTER,
            named: "emu-ce-writer",
            identifier: "emu-ce-writer",
            endpoint_type: "WRITER",
            custom_type: "WRITER",
            membership: Membership::None,
            tags: &[],
        },
        EndpointCase {
            case: "an any endpoint with one excluded member",
            cluster: CLUSTER,
            named: "emu-ce-any",
            identifier: "emu-ce-any",
            endpoint_type: "ANY",
            custom_type: "ANY",
            membership: Membership::Excluded(&[FIRST_MEMBER]),
            tags: &[("owner", "platform"), ("tier", "gold")],
        },
        EndpointCase {
            case: "a type named in another case",
            cluster: SECOND_CLUSTER,
            named: "emu-ce-lowercase-type",
            identifier: "emu-ce-lowercase-type",
            endpoint_type: "reader",
            custom_type: "READER",
            membership: Membership::Static(&[SECOND_MEMBER]),
            tags: &[],
        },
        EndpointCase {
            case: "an identifier named in mixed case, stored lowercased",
            cluster: SECOND_CLUSTER,
            named: "EMU-CE-Mixed-Case",
            identifier: "emu-ce-mixed-case",
            endpoint_type: "ANY",
            custom_type: "ANY",
            membership: Membership::None,
            tags: &[],
        },
        EndpointCase {
            case: "an identifier at the longest length this service admits",
            cluster: SECOND_CLUSTER,
            named: LONGEST,
            identifier: LONGEST,
            endpoint_type: "ANY",
            custom_type: "ANY",
            membership: Membership::None,
            tags: &[],
        },
    ];
    assert_eq!(
        LONGEST.len(),
        MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN,
        "the longest-identifier row must sit exactly on the bound, not near it"
    );

    let router = emulator();
    for cluster in [CLUSTER, SECOND_CLUSTER] {
        expect_ok(cluster, create_cluster(&router, cluster, AURORA).await);
    }
    for member in [FIRST_MEMBER, SECOND_MEMBER] {
        expect_ok(member, create_member(&router, member, CLUSTER).await);
    }

    for row in table {
        let case = row.case;
        let mut extra = row.membership.params();
        for (position, (key, value)) in row.tags.iter().enumerate() {
            let index = position + 1;
            extra.push((
                if index == 1 {
                    "Tags.member.1.Key"
                } else {
                    "Tags.member.2.Key"
                },
                (*key).to_string(),
            ));
            extra.push((
                if index == 1 {
                    "Tags.member.1.Value"
                } else {
                    "Tags.member.2.Value"
                },
                (*value).to_string(),
            ));
        }
        let created = expect_ok(
            case,
            create_endpoint(&router, row.named, row.cluster, row.endpoint_type, &extra).await,
        );

        // The create answers with the endpoint it stored: the identifier folded as the
        // member documents, the cluster it hangs off, the type a custom endpoint reports
        // with the requested type beside it, and the status an accepted create reports.
        assert_eq!(
            text(&created, "DBClusterEndpointIdentifier"),
            row.identifier,
            "case {case:?}: the identifier is stored as a lowercase string, got {created}"
        );
        assert_eq!(
            text(&created, "DBClusterIdentifier"),
            row.cluster,
            "case {case:?}: expected the cluster the endpoint was created on, got {created}"
        );
        assert_eq!(
            text(&created, "EndpointType"),
            CUSTOM,
            "case {case:?}: an endpoint a create made is of type {CUSTOM}, got {created}"
        );
        assert_eq!(
            text(&created, "CustomEndpointType"),
            row.custom_type,
            "case {case:?}: expected the requested type, got {created}"
        );
        assert_eq!(
            text(&created, "Status"),
            CREATING,
            "case {case:?}: an accepted create reports {CREATING:?}, got {created}"
        );
        assert_eq!(
            text(&created, "DBClusterEndpointArn"),
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster-endpoint:{}", row.identifier),
            "case {case:?}: expected the endpoint's own ARN, got {created}"
        );
        let resource_id = text(&created, "DBClusterEndpointResourceIdentifier");
        assert!(
            resource_id.starts_with("cluster-endpoint-") && resource_id.len() > "cluster-endpoint-".len(),
            "case {case:?}: expected a minted endpoint resource id, got {resource_id:?}"
        );

        // The record was PERSISTED: the read after the transition this service owes has
        // come due reports the same endpoint, with the membership the create RESOLVED.
        let described = poll_endpoint_until(&router, case, row.identifier, AVAILABLE).await;
        for (tag, expected) in [
            ("DBClusterEndpointIdentifier", row.identifier),
            ("DBClusterIdentifier", row.cluster),
            ("EndpointType", CUSTOM),
            ("CustomEndpointType", row.custom_type),
        ] {
            assert_eq!(
                optional_text(&described, tag).as_deref(),
                Some(expected),
                "case {case:?}: the stored endpoint must keep its {tag}, got {described}"
            );
        }
        assert_eq!(
            optional_text(&described, "DBClusterEndpointResourceIdentifier").as_deref(),
            Some(resource_id.as_str()),
            "case {case:?}: the minted id \"remains the same for the whole life of the \
             endpoint\", got {described}"
        );
        assert_eq!(
            texts(&described, "member"),
            row.membership
                .static_members()
                .iter()
                .chain(row.membership.excluded_members())
                .map(|identifier| (*identifier).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the membership the create resolved, got {described}"
        );

        // And the CLUSTER reports the endpoint's address among its own custom endpoints,
        // which is the same address the endpoint reports as its `Endpoint`.
        let address = text(&described, "Endpoint");
        assert!(
            !address.is_empty(),
            "case {case:?}: a custom endpoint publishes an address, got {described}"
        );
        assert!(
            cluster_custom_endpoints(&router, row.cluster)
                .await
                .contains(&address),
            "case {case:?}: the cluster must report {address:?} among its CustomEndpoints"
        );
    }

    // Every endpoint the table created on a cluster is reported for that cluster, beside
    // the two the cluster has without a create — and in no other region.
    for cluster in [CLUSTER, SECOND_CLUSTER] {
        let created_here = table.iter().filter(|row| row.cluster == cluster).count();
        let all = expect_ok(
            cluster,
            describe_endpoints(&router, &[("DBClusterIdentifier", cluster.to_string())]).await,
        );
        assert_eq!(
            reported(&all).len(),
            created_here + 2,
            "case {cluster:?}: the report must carry every custom endpoint of the cluster plus \
             its own two, got {all}"
        );
    }
    let elsewhere = expect_ok(
        "another region",
        call_in(&router, OTHER_REGION, "DescribeDBClusterEndpoints", &[] as &[(&str, String)])
            .await,
    );
    assert!(
        reported(&elsewhere).is_empty(),
        "a region holding no cluster reports no endpoint, got {elsewhere}"
    );
}

#[tokio::test]
async fn the_report_carries_the_clusters_own_endpoints_and_every_published_filter() {
    let router = emulator();
    expect_ok("the cluster", create_cluster(&router, CLUSTER, AURORA).await);
    expect_ok(
        "the custom endpoint",
        create_endpoint(&router, "emu-ce-filtered", CLUSTER, "READER", &[]).await,
    );
    poll_endpoint_until(&router, "the custom endpoint", "emu-ce-filtered", AVAILABLE).await;

    // The two endpoints a cluster has without a create: each reports the cluster, its
    // address, its state and its type, and carries no identifier, since no create named
    // one.
    let body = expect_ok(
        "the cluster's endpoints",
        describe_endpoints(&router, &[("DBClusterIdentifier", CLUSTER.to_string())]).await,
    );
    let builtin: Vec<String> = reported(&body)
        .into_iter()
        .filter(|endpoint| optional_text(endpoint, "DBClusterEndpointIdentifier").is_none())
        .collect();
    assert_eq!(
        builtin
            .iter()
            .map(|endpoint| text(endpoint, "EndpointType"))
            .collect::<Vec<String>>(),
        vec!["WRITER".to_string(), "READER".to_string()],
        "the cluster's own writer and reader endpoints must be reported, got {body}"
    );
    for endpoint in &builtin {
        assert_eq!(
            text(endpoint, "DBClusterIdentifier"),
            CLUSTER,
            "the cluster's own endpoint names its cluster, got {endpoint}"
        );
        assert_eq!(
            text(endpoint, "Status"),
            AVAILABLE,
            "an endpoint of an available cluster is available, got {endpoint}"
        );
        assert!(
            !text(endpoint, "Endpoint").is_empty(),
            "the cluster's own endpoint publishes an address, got {endpoint}"
        );
        assert!(
            optional_text(endpoint, "DBClusterEndpointArn").is_none(),
            "an endpoint no create made carries no ARN, got {endpoint}"
        );
    }

    // Each filter name the operation publishes selects on the member it names, and the
    // published values are matched without regard to case.
    let filtered: &[(&str, &str, &str, usize)] = &[
        ("the custom endpoints", "db-cluster-endpoint-type", "custom", 1),
        ("the writer endpoint", "db-cluster-endpoint-type", "WRITER", 1),
        ("the reader endpoints", "db-cluster-endpoint-type", "reader", 1),
        ("the custom type", "db-cluster-endpoint-custom-type", "reader", 1),
        ("another custom type", "db-cluster-endpoint-custom-type", "any", 0),
        ("one identifier", "db-cluster-endpoint-id", "emu-ce-filtered", 1),
        ("an identifier nothing carries", "db-cluster-endpoint-id", "emu-ce-absent", 0),
        ("the available endpoints", "db-cluster-endpoint-status", "AVAILABLE", 3),
        ("a status nothing is in", "db-cluster-endpoint-status", "deleting", 0),
    ];
    for (case, name, value, expected) in filtered {
        let body = expect_ok(
            case,
            describe_endpoints(
                &router,
                &[
                    ("Filters.Filter.1.Name", (*name).to_string()),
                    ("Filters.Filter.1.Values.Value.1", (*value).to_string()),
                ],
            )
            .await,
        );
        assert_eq!(
            reported(&body).len(),
            *expected,
            "case {case:?}: filter {name}={value} must select {expected} endpoint(s), got {body}"
        );
    }

    // A filter name the report publishes no meaning for is refused rather than ignored,
    // and so is a `MaxRecords` outside the published range.
    let refused = expect_error(
        "an unsupported filter name",
        describe_endpoints(
            &router,
            &[
                ("Filters.Filter.1.Name", "db-cluster-id".to_string()),
                ("Filters.Filter.1.Values.Value.1", CLUSTER.to_string()),
            ],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
    assert_eq!(
        text(&refused, "Message"),
        "Unrecognized filter name: db-cluster-id",
        "the refusal names the filter the request carried, got {refused}"
    );
    // "Constraints: Minimum 20, maximum 100": each published bound is served and one value
    // past each is refused.
    for (case, max_records, served) in [
        ("the published minimum", "20", true),
        ("the published maximum", "100", true),
        ("one below the minimum", "19", false),
        ("one above the maximum", "101", false),
    ] {
        let answer = describe_endpoints(
            &router,
            &[("MaxRecords", max_records.to_string())],
        )
        .await;
        if served {
            expect_ok(case, answer);
        } else {
            expect_error(case, answer, 400, "InvalidParameterValue");
        }
    }

    // A `DBClusterIdentifier` that names no cluster of this region is the one fault the
    // report models.
    let missing = expect_error(
        "a cluster nothing created",
        describe_endpoints(&router, &[("DBClusterIdentifier", "emu-ce-nothing".to_string())]).await,
        404,
        "DBClusterNotFoundFault",
    );
    assert_eq!(
        text(&missing, "Message"),
        "The source cluster could not be found or cannot be accessed: emu-ce-nothing",
        "the not-found fault names the cluster the request asked for, got {missing}"
    );
}

/// One row of the refusal table: what the create carries, and how it is answered.
struct RefusalCase {
    case: &'static str,
    identifier: &'static str,
    cluster: &'static str,
    endpoint_type: &'static str,
    extra: &'static [(&'static str, &'static str)],
    status: u16,
    code: &'static str,
}

#[tokio::test]
async fn a_create_that_cannot_be_served_is_refused_and_leaves_nothing_behind() {
    // One character past the longest identifier this service admits.
    const TOO_LONG: &str = "emu-ce-identifier-one-character-past-the-longest-admitted-abcdef";
    let table: &[RefusalCase] = &[
        RefusalCase {
            case: "a cluster nothing created",
            identifier: "emu-ce-no-cluster",
            cluster: "emu-ce-absent-cluster",
            endpoint_type: "ANY",
            extra: &[],
            status: 404,
            code: "DBClusterNotFoundFault",
        },
        RefusalCase {
            case: "a Multi-AZ DB cluster, which the operation does not apply to",
            identifier: "emu-ce-multi-az",
            cluster: "emu-ce-multi-az-cluster",
            endpoint_type: "ANY",
            extra: &[],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a type outside the published set",
            identifier: "emu-ce-bad-type",
            cluster: CLUSTER,
            endpoint_type: "PRIMARY",
            extra: &[],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "an identifier one character past the longest admitted",
            identifier: TOO_LONG,
            cluster: CLUSTER,
            endpoint_type: "ANY",
            extra: &[],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "an identifier that does not start with a letter",
            identifier: "9-emu-ce-leading-digit",
            cluster: CLUSTER,
            endpoint_type: "ANY",
            extra: &[],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "an identifier with two consecutive hyphens",
            identifier: "emu--ce-doubled-hyphen",
            cluster: CLUSTER,
            endpoint_type: "ANY",
            extra: &[],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a static member naming no DB instance",
            identifier: "emu-ce-absent-static",
            cluster: CLUSTER,
            endpoint_type: "ANY",
            extra: &[("StaticMembers.member.1", "emu-ce-no-instance")],
            status: 404,
            code: "DBInstanceNotFound",
        },
        RefusalCase {
            case: "both membership lists named at once",
            identifier: "emu-ce-both-lists",
            cluster: CLUSTER,
            endpoint_type: "ANY",
            extra: &[
                ("StaticMembers", ""),
                ("ExcludedMembers", ""),
            ],
            status: 400,
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "an excluded member naming no DB instance",
            identifier: "emu-ce-absent-excluded",
            cluster: CLUSTER,
            endpoint_type: "ANY",
            extra: &[("ExcludedMembers.member.1", "emu-ce-no-instance")],
            status: 404,
            code: "DBInstanceNotFound",
        },
    ];

    let router = emulator();
    expect_ok("the cluster", create_cluster(&router, CLUSTER, AURORA).await);
    expect_ok(
        "the Multi-AZ cluster",
        create_cluster(&router, "emu-ce-multi-az-cluster", MULTI_AZ_ENGINE).await,
    );

    for row in table {
        let case = row.case;
        let extra: Vec<(&str, String)> = row
            .extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string()))
            .collect();
        expect_error(
            case,
            create_endpoint(&router, row.identifier, row.cluster, row.endpoint_type, &extra).await,
            row.status,
            row.code,
        );
        // A refused create leaves nothing behind: the identifier it named is still one
        // nothing created.
        let after = expect_ok(
            case,
            describe_endpoints(
                &router,
                &[("DBClusterEndpointIdentifier", row.identifier.to_string())],
            )
            .await,
        );
        assert!(
            reported(&after).is_empty(),
            "case {case:?}: a refused create stores no endpoint, got {after}"
        );
    }

    // A second create of one identifier is the modelled already-exists fault, and the
    // endpoint the FIRST create stored is untouched.
    // A second create of one identifier is the modelled already-exists fault — naming the
    // identifier that collided and the ADDRESS the endpoint already holding it publishes, so
    // a client creating several endpoints in one flow can tell WHICH one it collided with —
    // and the endpoint the FIRST create stored is untouched.
    let first = expect_ok(
        "the first create",
        create_endpoint(&router, "emu-ce-taken", CLUSTER, "ANY", &[]).await,
    );
    let holder_address = text(&first, "Endpoint");
    for case in ["a second create of one identifier", "a third one"] {
        let refused = expect_error(
            case,
            create_endpoint(&router, "emu-ce-taken", CLUSTER, "READER", &[]).await,
            400,
            "DBClusterEndpointAlreadyExistsFault",
        );
        assert_eq!(
            text(&refused, "Message"),
            format!(
                "An endpoint with the identifier emu-ce-taken already exists: {holder_address}"
            ),
            "case {case:?}: expected the already-exists sentence naming the identifier and the \
             endpoint holding it, got {refused}"
        );
    }
    let kept = poll_endpoint_until(&router, "the stored endpoint", "emu-ce-taken", AVAILABLE).await;
    assert_eq!(
        text(&kept, "CustomEndpointType"),
        "ANY",
        "the refused create must not have overwritten the stored endpoint, got {kept}"
    );

    // The cluster's own allowance: the creates up to it are served and the one past it is
    // the modelled quota fault.
    for index in 1..DB_CLUSTER_ENDPOINT_QUOTA_PER_CLUSTER {
        let identifier = format!("emu-ce-quota-{index}");
        expect_ok(
            &identifier,
            create_endpoint(&router, &identifier, CLUSTER, "ANY", &[]).await,
        );
    }
    expect_error(
        "one create past the cluster's allowance",
        create_endpoint(&router, "emu-ce-quota-past", CLUSTER, "ANY", &[]).await,
        403,
        "DBClusterEndpointQuotaExceededFault",
    );
    // And the allowance is per CLUSTER: another cluster can still take one.
    expect_ok(
        "another cluster",
        create_cluster(&router, "emu-ce-second-cluster", AURORA).await,
    );
    expect_ok(
        "a create on another cluster",
        create_endpoint(&router, "emu-ce-other-cluster", "emu-ce-second-cluster", "ANY", &[]).await,
    );
    assert_eq!(
        CUSTOM_ENDPOINT_TYPES.len(),
        3,
        "the published type list is what the rows above span"
    );

    // A member DB instance whose delete has been accepted is on its way out, so it is not
    // in a state it can join a custom endpoint group in: the second fault the operation
    // models for a membership.
    expect_ok(
        "the member being deleted",
        create_member(&router, "emu-ce-leaving", CLUSTER).await,
    );
    expect_ok(
        "the delete of that member",
        call(
            &router,
            "DeleteDBInstance",
            &[("DBInstanceIdentifier", "emu-ce-leaving")],
        )
        .await,
    );
    let leaving = expect_error(
        "a member whose delete has been accepted",
        create_endpoint(
            &router,
            "emu-ce-leaving-member",
            CLUSTER,
            "ANY",
            &[("StaticMembers.member.1", "emu-ce-leaving".to_string())],
        )
        .await,
        400,
        "InvalidDBInstanceState",
    );
    assert_eq!(
        text(&leaving, "Message"),
        "The specified database instance is currently in deleting state, which is a \
         terminal state.",
        "the refusal names the state the instance is in, got {leaving}"
    );
}

#[tokio::test]
async fn a_modification_and_a_delete_act_on_the_stored_endpoint() {
    let router = emulator();
    expect_ok("the cluster", create_cluster(&router, CLUSTER, AURORA).await);
    for member in [FIRST_MEMBER, SECOND_MEMBER] {
        expect_ok(member, create_member(&router, member, CLUSTER).await);
    }
    expect_ok(
        "the endpoint",
        create_endpoint(
            &router,
            "emu-ce-modified",
            CLUSTER,
            "READER",
            &[("StaticMembers.member.1", FIRST_MEMBER.to_string())],
        )
        .await,
    );
    poll_endpoint_until(&router, "the endpoint", "emu-ce-modified", AVAILABLE).await;

    // Each property the modification names is applied and read back; the ones it does not
    // name are left as they were.
    let modified = expect_ok(
        "a modification of the type alone",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "emu-ce-modified"),
                ("EndpointType", "ANY"),
            ],
        )
        .await,
    );
    assert_eq!(
        text(&modified, "CustomEndpointType"),
        "ANY",
        "the modification reports the type it applied, got {modified}"
    );
    assert_eq!(
        text(&modified, "Status"),
        MODIFYING,
        "an accepted modification reports {MODIFYING:?}, got {modified}"
    );
    assert_eq!(
        texts(&modified, "member"),
        vec![FIRST_MEMBER.to_string()],
        "a membership the request did not name is left alone, got {modified}"
    );
    // A second modification while the first is still being applied is the modelled
    // endpoint-state fault, which is what makes two of them serialise.
    expect_error(
        "a modification while one is being applied",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "emu-ce-modified"),
                ("EndpointType", "READER"),
            ],
        )
        .await,
        400,
        "InvalidDBClusterEndpointStateFault",
    );
    let settled = poll_endpoint_until(&router, "the modification", "emu-ce-modified", AVAILABLE).await;
    assert_eq!(
        text(&settled, "CustomEndpointType"),
        "ANY",
        "the applied modification is what every later read reports, got {settled}"
    );

    // A membership the modification names REPLACES the stored one.
    let replaced = expect_ok(
        "a membership replacement",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "emu-ce-modified"),
                ("StaticMembers.member.1", SECOND_MEMBER),
            ],
        )
        .await,
    );
    assert_eq!(
        texts(&replaced, "member"),
        vec![SECOND_MEMBER.to_string()],
        "the named membership replaces the stored one, got {replaced}"
    );
    poll_endpoint_until(&router, "the replacement", "emu-ce-modified", AVAILABLE).await;

    // The refusals a modification and a delete answer: a request that asks for no change, a
    // membership naming no DB instance, and an identifier nothing created.
    let nothing = expect_error(
        "a modification that asks for nothing",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", "emu-ce-modified")],
        )
        .await,
        400,
        "InvalidParameterCombination",
    );
    let probe = expect_error(
        "both membership lists named at once",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "emu-ce-modified"),
                ("StaticMembers.member.1", FIRST_MEMBER),
                ("ExcludedMembers.member.1", SECOND_MEMBER),
            ],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
    assert_eq!(
        text(&probe, "Message"),
        "You can't specify both static and excluded members",
        "the refusal states that only one kind of membership may be given, got {probe}"
    );
    // An empty list is a membership the caller STATED: it clears the stored one rather
    // than being refused or ignored.
    let cleared = expect_ok(
        "a membership named with no DB instance in it",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "emu-ce-modified"),
                ("StaticMembers", ""),
            ],
        )
        .await,
    );
    assert!(
        texts(&cleared, "member").is_empty(),
        "an empty membership list clears the stored one, got {cleared}"
    );
    poll_endpoint_until(&router, "the cleared membership", "emu-ce-modified", AVAILABLE).await;
    assert_eq!(
        text(&nothing, "Message"),
        "No modifications were requested",
        "the refusal states that nothing was asked for, got {nothing}"
    );
    expect_error(
        "a membership naming no DB instance",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", "emu-ce-modified"),
                ("StaticMembers.member.1", "emu-ce-no-instance"),
            ],
        )
        .await,
        404,
        "DBInstanceNotFound",
    );
    for action in ["ModifyDBClusterEndpoint", "DeleteDBClusterEndpoint"] {
        let refused = expect_error(
            action,
            call(
                &router,
                action,
                &[
                    ("DBClusterEndpointIdentifier", "emu-ce-nothing"),
                    ("EndpointType", "ANY"),
                ],
            )
            .await,
            400,
            "DBClusterEndpointNotFoundFault",
        );
        assert_eq!(
            text(&refused, "Message"),
            "DBClusterEndpoint emu-ce-nothing not found",
            "case {action:?}: expected the endpoint the request named in the sentence, \
             got {refused}"
        );
    }

    // The delete takes the endpoint out: it reports `deleting`, the cluster stops reporting
    // its address, and every later read of the identifier reports nothing.
    let deleted = expect_ok(
        "the delete",
        call(
            &router,
            "DeleteDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", "emu-ce-modified")],
        )
        .await,
    );
    assert_eq!(
        text(&deleted, "Status"),
        DELETING,
        "an accepted delete reports {DELETING:?}, got {deleted}"
    );
    expect_error(
        "a second delete inside the span the first owes",
        call(
            &router,
            "DeleteDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", "emu-ce-modified")],
        )
        .await,
        400,
        "InvalidDBClusterEndpointStateFault",
    );
    poll_endpoint_until_gone(&router, "the delete", "emu-ce-modified").await;
    assert!(
        cluster_custom_endpoints(&router, CLUSTER).await.is_empty(),
        "a deleted endpoint is no longer one of the cluster's custom endpoints"
    );
}
