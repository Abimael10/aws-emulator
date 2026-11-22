// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RECORDS `DescribeDBClusterEndpoints` serves: the ORDER the
//! report puts a cluster's endpoints in, the ADDRESS each one publishes, and the immutable
//! id a custom endpoint carries wherever it is surfaced.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on, so a handler that computed nothing could not pass these assertions.
//!
//! The rules under test:
//!
//! 1. The report lists the CUSTOM endpoints created for a cluster — in the order of the
//!    identifiers they are stored under — ahead of the two endpoints the cluster has without
//!    a create, which follow writer first and reader second.
//! 2. Each entry publishes the address of its OWN endpoint kind, the same address the
//!    cluster's own report publishes for that kind: the connection endpoint's for the
//!    `WRITER` entry, the reader endpoint's for the `READER` entry, and the custom endpoint's
//!    own name for each `CUSTOM` entry — so the three names a cluster MINTS are distinct, and
//!    an entry's address is never read off this emulator's bind address instead of the
//!    cluster's endpoint for that kind. The one cluster class whose endpoints share an
//!    address is the one whose whole data plane is served at this emulator's own endpoint,
//!    where that address is where every one of its endpoints is reached.
//! 3. `DBClusterEndpointResourceIdentifier` is a server-minted id of the form
//!    `cluster-endpoint-<token>`, whose token is of the width every server-assigned id of a
//!    regional resource of this service carries, and it is the SAME value on the create, on
//!    every read, on a modification and on the delete that ends the endpoint's life — "it
//!    remains the same for the whole life of the endpoint".
//!
//! The tables span what the rules generalise over: how many custom endpoints a cluster
//! carries (none, one, several), the order they were created in next to the order they are
//! reported in, the requested endpoint types, and all three ways this emulator serves a
//! cluster's data plane (as the engine's own native wire, as an AWS API at its own endpoint,
//! and not at all).

use smithy_server_runtime::MultiServiceRouter;

use crate::db_clusters::describe_one;
use crate::db_instances::{
    call, call_addressed_to, emulator, expect_ok, optional_text, subtrees, text, texts,
    AWS_ENDPOINT, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The prefix `DBClusterEndpointResourceIdentifier` is written with.
const RESOURCE_ID_PREFIX: &str = "cluster-endpoint-";

/// How many characters a server-assigned id of a REGIONAL resource of this service carries
/// after its `<kind>-` prefix.
///
/// Source: the width AWS writes `DbClusterResourceId` and `DbiResourceId` in — which the
/// resource-id test below also reads off the cluster's own `DbClusterResourceId` rather than
/// trusting this constant alone.
const RESOURCE_ID_TOKEN_LEN: usize = 26;

/// The status a custom endpoint reports once the transition this service owes it has settled.
const AVAILABLE: &str = "available";

/// `EndpointType` as an endpoint a caller created is reported, and as the two a cluster has
/// without a create are.
const CUSTOM: &str = "CUSTOM";
const WRITER: &str = "WRITER";
const READER: &str = "READER";

/// `CreateDBCluster` addressed to `endpoint`, for a cluster of `engine`.
///
/// The endpoint the client is talking to is what the address a SERVED resource publishes is
/// read off, which is why each row names one.
async fn create_cluster(
    router: &MultiServiceRouter,
    endpoint: &str,
    identifier: &str,
    engine: &str,
) -> String {
    expect_ok(
        identifier,
        call_addressed_to(
            router,
            endpoint,
            REGION,
            "CreateDBCluster",
            &[("DBClusterIdentifier", identifier), ("Engine", engine)],
        )
        .await,
    )
}

/// `CreateDBClusterEndpoint` for `identifier` on `cluster`.
async fn create_endpoint(
    router: &MultiServiceRouter,
    cluster: &str,
    identifier: &str,
    endpoint_type: &str,
) -> String {
    expect_ok(
        identifier,
        call(
            router,
            "CreateDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", identifier),
                ("DBClusterIdentifier", cluster),
                ("EndpointType", endpoint_type),
            ],
        )
        .await,
    )
}

/// The `<DBClusterEndpointList>` entries the report carries for `cluster`, in the order it
/// serves them, once `expected` of them have settled.
///
/// The CONDITION is polled for rather than a span waited out, through the loop this test
/// target shares, and the read is a reporting call and nothing else.
async fn settled_endpoints(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    expected: usize,
) -> Vec<String> {
    let params = [("DBClusterIdentifier", cluster)];
    let (body, _polls) = poll_until(
        case,
        &format!("the {expected} endpoints of {cluster} settling"),
        || call(router, "DescribeDBClusterEndpoints", &params),
        |status, body| {
            let reported = subtrees(body, "DBClusterEndpointList");
            status == 200
                && reported.len() == expected
                && reported
                    .iter()
                    .all(|entry| text(entry, "Status") == AVAILABLE)
        },
    )
    .await;
    subtrees(&body, "DBClusterEndpointList")
}

/// One row of the report-order table.
struct OrderCase {
    case: &'static str,
    cluster: &'static str,
    engine: &'static str,
    /// The custom endpoints to create, in the order the requests are made, each with the
    /// `EndpointType` its create asks for.
    creates: &'static [(&'static str, &'static str)],
    /// The identifiers the report must carry, in the order it must serve them — the stored
    /// identifier order, which the creates above deliberately do not follow.
    reported_order: &'static [&'static str],
}

const ORDER_CASES: &[OrderCase] = &[
    OrderCase {
        case: "a cluster nobody created an endpoint on",
        cluster: "emu-cer-none",
        engine: "aurora-postgresql",
        creates: &[],
        reported_order: &[],
    },
    OrderCase {
        case: "a cluster carrying the one endpoint a caller created",
        cluster: "emu-cer-one",
        engine: "aurora-postgresql",
        creates: &[("emu-cer-one-reader", "READER")],
        reported_order: &["emu-cer-one-reader"],
    },
    OrderCase {
        case: "a graph cluster carrying three endpoints created out of order",
        cluster: "emu-cer-many",
        engine: "neptune",
        creates: &[
            ("emu-cer-many-z", "ANY"),
            ("emu-cer-many-a", "READER"),
            ("emu-cer-many-m", "WRITER"),
        ],
        reported_order: &["emu-cer-many-a", "emu-cer-many-m", "emu-cer-many-z"],
    },
];

#[tokio::test]
async fn the_report_lists_the_custom_endpoints_of_a_cluster_before_its_own_two() {
    let router = emulator();

    for row in ORDER_CASES {
        create_cluster(&router, AWS_ENDPOINT, row.cluster, row.engine).await;
        for (identifier, endpoint_type) in row.creates {
            create_endpoint(&router, row.cluster, identifier, endpoint_type).await;
        }

        let customs = row.reported_order.len();
        let reported = settled_endpoints(&router, row.case, row.cluster, customs + 2).await;

        // The types the report serves: every custom endpoint first, then the cluster's own
        // writer and reader.
        let mut expected_types: Vec<String> = vec![CUSTOM.to_string(); customs];
        expected_types.push(WRITER.to_string());
        expected_types.push(READER.to_string());
        let types: Vec<String> = reported
            .iter()
            .map(|entry| text(entry, "EndpointType"))
            .collect();
        assert_eq!(
            types, expected_types,
            "case {:?}: expected the report to serve {expected_types:?}, got {types:?}",
            row.case
        );

        // And the custom entries are the endpoints that were created, in stored-identifier
        // order rather than the order the creates were made in.
        let identifiers: Vec<String> = reported
            .iter()
            .filter_map(|entry| optional_text(entry, "DBClusterEndpointIdentifier"))
            .collect();
        let expected_identifiers: Vec<String> = row
            .reported_order
            .iter()
            .map(|identifier| (*identifier).to_string())
            .collect();
        assert_eq!(
            identifiers, expected_identifiers,
            "case {:?}: expected the custom endpoints ahead of the cluster's own, got \
             {identifiers:?}",
            row.case
        );

        // A caller that reads the first entry to pick up the endpoint it created finds a
        // full custom record there.
        if customs > 0 {
            let first = &reported[0];
            for member in [
                "DBClusterEndpointIdentifier",
                "DBClusterEndpointResourceIdentifier",
                "DBClusterEndpointArn",
                "CustomEndpointType",
            ] {
                assert!(
                    optional_text(first, member).is_some(),
                    "case {:?}: the first entry must be a custom record carrying {member}, got \
                     {first}",
                    row.case
                );
            }
        }
        // The two the cluster has without a create carry no identifier, no immutable id and
        // no ARN at all, because none of those exists for an endpoint no create made.
        for builtin in &reported[customs..] {
            for member in [
                "DBClusterEndpointIdentifier",
                "DBClusterEndpointResourceIdentifier",
                "DBClusterEndpointArn",
            ] {
                assert!(
                    optional_text(builtin, member).is_none(),
                    "case {:?}: an endpoint no create made carries no {member}, got {builtin}",
                    row.case
                );
            }
        }
    }
}

/// What a row expects the three entries of its cluster to publish — which follows from WHERE
/// that cluster's data plane is served, because the address an endpoint publishes is the
/// address a client reaches that endpoint at.
#[derive(Debug, PartialEq, Eq)]
enum Published {
    /// A cluster whose own engine LISTENER serves its data plane: the writer entry publishes
    /// the address that listener answers on, and the reader and the custom endpoint the names
    /// the cluster mints for them.
    ListenerThenNames,
    /// A cluster whose data plane is an AWS API this emulator serves at its OWN endpoint:
    /// every endpoint of such a cluster is reached at that one address, because its plane
    /// answers there and nowhere else.
    OneServedAddress,
    /// A cluster this emulator serves no data plane for: every entry publishes the name its
    /// own kind is minted under.
    NamePerKind,
}

/// One row of the published-address table: a cluster whose data plane this emulator serves
/// one of the three ways there are.
struct AddressCase {
    case: &'static str,
    cluster: &'static str,
    endpoint_identifier: &'static str,
    engine: &'static str,
    /// The endpoint the client that creates the cluster is talking to.
    addressed_to: &'static str,
    expected: Published,
}

const ADDRESS_CASES: &[AddressCase] = &[
    AddressCase {
        case: "a cluster whose native engine wire this emulator serves",
        cluster: "emu-cea-wire",
        endpoint_identifier: "emu-cea-wire-ep",
        engine: "aurora-postgresql",
        addressed_to: "https://127.0.0.1:9297/",
        expected: Published::ListenerThenNames,
    },
    AddressCase {
        case: "a graph cluster whose data plane this emulator serves as an API",
        cluster: "emu-cea-api",
        endpoint_identifier: "emu-cea-api-ep",
        engine: "neptune",
        addressed_to: "https://127.0.0.1:9297/",
        expected: Published::OneServedAddress,
    },
    AddressCase {
        case: "a graph cluster this emulator serves no data plane for",
        cluster: "emu-cea-none",
        endpoint_identifier: "emu-cea-none-ep",
        engine: "neptune",
        addressed_to: AWS_ENDPOINT,
        expected: Published::NamePerKind,
    },
];

#[tokio::test]
async fn every_endpoint_of_a_cluster_publishes_the_address_of_its_own_kind() {
    let router = emulator();

    for row in ADDRESS_CASES {
        create_cluster(&router, row.addressed_to, row.cluster, row.engine).await;
        create_endpoint(&router, row.cluster, row.endpoint_identifier, "READER").await;
        let reported = settled_endpoints(&router, row.case, row.cluster, 3).await;

        let address_of = |which: &str| -> String {
            let entry = reported
                .iter()
                .find(|entry| text(entry, "EndpointType") == which)
                .unwrap_or_else(|| {
                    panic!("case {:?}: the report must carry a {which} entry", row.case)
                });
            text(entry, "Endpoint")
        };
        let custom = address_of(CUSTOM);
        let writer = address_of(WRITER);
        let reader = address_of(READER);

        // Each entry publishes the address its own kind of endpoint is reached at, which is
        // the address the CLUSTER's own report publishes for that kind: one endpoint, one
        // address, whichever read a client makes.
        let cluster = describe_one(&router, row.case, row.cluster).await;
        let cluster_endpoint = text(&cluster, "Endpoint");
        assert_eq!(
            writer, cluster_endpoint,
            "case {:?}: the WRITER entry must publish the cluster's connection endpoint \
             {cluster_endpoint:?}, got {writer:?}",
            row.case
        );
        let cluster_reader = text(&cluster, "ReaderEndpoint");
        assert_eq!(
            reader, cluster_reader,
            "case {:?}: the READER entry must publish the cluster's reader endpoint \
             {cluster_reader:?}, got {reader:?}",
            row.case
        );
        let cluster_customs = subtrees(&cluster, "CustomEndpoints")
            .first()
            .map(|list| texts(list, "member"))
            .unwrap_or_default();
        assert_eq!(
            cluster_customs,
            vec![custom.clone()],
            "case {:?}: the cluster must report the custom endpoint's own address among its \
             CustomEndpoints, got {cluster_customs:?} beside {custom:?}",
            row.case
        );

        // No two of the three names a cluster MINTS collapse onto one address: a client that
        // resolves what it was given tells the write path, the read path and the custom path
        // apart. A cluster whose plane this emulator serves as an API is the one exception,
        // and it is one by the rule rather than beside it: every endpoint of such a cluster
        // is reached at the one address that plane answers on.
        match row.expected {
            Published::OneServedAddress => {
                for (which, address) in [(CUSTOM, &custom), (READER, &reader)] {
                    assert_eq!(
                        *address, writer,
                        "case {:?}: every endpoint of a cluster whose plane is served at this \
                         emulator's own endpoint is reached there, so the {which} entry must \
                         publish {writer:?}, got {address:?}",
                        row.case
                    );
                }
            }
            Published::ListenerThenNames | Published::NamePerKind => {
                for (left, left_name, right, right_name) in [
                    (&custom, CUSTOM, &writer, WRITER),
                    (&custom, CUSTOM, &reader, READER),
                    (&writer, WRITER, &reader, READER),
                ] {
                    assert_ne!(
                        left, right,
                        "case {:?}: the {left_name} and {right_name} endpoints must publish \
                         different addresses, got {left:?} for both",
                        row.case
                    );
                }
                // Each minted name carries the label its kind is published under, in front of
                // the resource it belongs to.
                assert!(
                    custom.starts_with(&format!("{}.cluster-custom-", row.endpoint_identifier)),
                    "case {:?}: expected a custom endpoint of the form \
                     <identifier>.cluster-custom-<label>…, got {custom:?}",
                    row.case
                );
                assert!(
                    reader.starts_with(&format!("{}.cluster-ro-", row.cluster)),
                    "case {:?}: expected a reader endpoint of the form \
                     <cluster>.cluster-ro-<label>…, got {reader:?}",
                    row.case
                );
                match row.expected {
                    // The writer entry of a cluster with a listener of its own publishes that
                    // listener's address, which is no DNS name at all.
                    Published::ListenerThenNames => assert!(
                        writer.parse::<std::net::IpAddr>().is_ok(),
                        "case {:?}: expected the address the cluster's own listener answers \
                         on, got the name {writer:?}",
                        row.case
                    ),
                    _ => assert!(
                        writer.starts_with(&format!("{}.cluster-", row.cluster)),
                        "case {:?}: expected a writer endpoint of the form \
                         <cluster>.cluster-<label>…, got {writer:?}",
                        row.case
                    ),
                }
            }
        }
        // Every entry publishes a bare HOST: the cluster has one `Port` member for all of
        // its endpoints, so no entry carries a port or a scheme of its own.
        for (which, address) in [(CUSTOM, &custom), (WRITER, &writer), (READER, &reader)] {
            assert!(
                !address.is_empty() && !address.contains(':') && !address.contains('/'),
                "case {:?}: the {which} entry publishes a bare host, got {address:?}",
                row.case
            );
        }
    }
}

/// One row of the resource-id table.
struct ResourceIdCase {
    case: &'static str,
    identifier: &'static str,
    endpoint_type: &'static str,
}

const RESOURCE_ID_CASES: &[ResourceIdCase] = &[
    ResourceIdCase {
        case: "an endpoint created for the read path",
        identifier: "emu-cerid-reader",
        endpoint_type: "READER",
    },
    ResourceIdCase {
        case: "an endpoint created for the write path",
        identifier: "emu-cerid-writer",
        endpoint_type: "WRITER",
    },
    ResourceIdCase {
        case: "an endpoint created for either path",
        identifier: "emu-cerid-any",
        endpoint_type: "ANY",
    },
];

/// The cluster the resource-id table creates its endpoints on.
const RESOURCE_ID_CLUSTER: &str = "emu-cerid-cluster";

/// The token a minted id carries after its prefix, asserting on the way that the id is of
/// this service's resource-id form: `prefix` plus [`RESOURCE_ID_TOKEN_LEN`] digits and
/// uppercase letters.
fn minted_token(case: &str, which: &str, prefix: &str, id: &str) -> String {
    let token = id.strip_prefix(prefix).unwrap_or_else(|| {
        panic!("case {case:?}: expected a {which} written {prefix}<token>, got {id:?}")
    });
    assert_eq!(
        token.len(),
        RESOURCE_ID_TOKEN_LEN,
        "case {case:?}: expected a {which} whose token is {RESOURCE_ID_TOKEN_LEN} characters, \
         got {id:?}"
    );
    assert!(
        token
            .chars()
            .all(|character| character.is_ascii_digit() || character.is_ascii_uppercase()),
        "case {case:?}: expected a {which} whose token is digits and uppercase letters, got \
         {id:?}"
    );
    token.to_string()
}

#[tokio::test]
async fn a_custom_endpoints_minted_id_keeps_this_services_resource_id_form_for_its_whole_life() {
    let router = emulator();
    let created_cluster = create_cluster(
        &router,
        AWS_ENDPOINT,
        RESOURCE_ID_CLUSTER,
        "aurora-postgresql",
    )
    .await;

    // The width is the one this service's OTHER server-assigned regional ids are written in,
    // read off the cluster the endpoints hang off rather than assumed.
    let cluster_token = minted_token(
        "the cluster the endpoints hang off",
        "DbClusterResourceId",
        "cluster-",
        &text(&created_cluster, "DbClusterResourceId"),
    );

    let mut minted: Vec<String> = Vec::new();
    for row in RESOURCE_ID_CASES {
        let created = create_endpoint(
            &router,
            RESOURCE_ID_CLUSTER,
            row.identifier,
            row.endpoint_type,
        )
        .await;
        let id = text(&created, "DBClusterEndpointResourceIdentifier");
        let token = minted_token(
            row.case,
            "DBClusterEndpointResourceIdentifier",
            RESOURCE_ID_PREFIX,
            &id,
        );
        assert_eq!(
            token.len(),
            cluster_token.len(),
            "case {:?}: a custom endpoint's id carries the width this service's other \
             resource ids do, got {id:?}",
            row.case
        );
        assert!(
            !minted.contains(&id),
            "case {:?}: every endpoint is minted an id of its own, got {id:?} twice",
            row.case
        );
        minted.push(id.clone());

        // The SAME id on every later surface of the endpoint: the read, and the
        // modification that changes what it fronts.
        let described = settled_endpoints(
            &router,
            row.case,
            RESOURCE_ID_CLUSTER,
            minted.len() + 2,
        )
        .await
        .into_iter()
        .find(|entry| {
            optional_text(entry, "DBClusterEndpointIdentifier").as_deref() == Some(row.identifier)
        })
        .unwrap_or_else(|| panic!("case {:?}: the report must carry the endpoint", row.case));
        assert_eq!(
            text(&described, "DBClusterEndpointResourceIdentifier"),
            id,
            "case {:?}: the report must carry the minted id, got {described}",
            row.case
        );

        let modified = expect_ok(
            row.case,
            call(
                &router,
                "ModifyDBClusterEndpoint",
                &[
                    ("DBClusterEndpointIdentifier", row.identifier),
                    ("EndpointType", "ANY"),
                ],
            )
            .await,
        );
        assert_eq!(
            text(&modified, "DBClusterEndpointResourceIdentifier"),
            id,
            "case {:?}: the modification must echo the minted id, got {modified}",
            row.case
        );
    }

    // And on the delete, which answers with the record it is ending — the terminal response
    // a client may persist for audit.
    for (deleted_so_far, (row, id)) in RESOURCE_ID_CASES.iter().zip(minted.iter()).enumerate() {
        let remaining = RESOURCE_ID_CASES.len() - deleted_so_far;
        settled_endpoints(&router, row.case, RESOURCE_ID_CLUSTER, remaining + 2).await;
        let deleted = expect_ok(
            row.case,
            call(
                &router,
                "DeleteDBClusterEndpoint",
                &[("DBClusterEndpointIdentifier", row.identifier)],
            )
            .await,
        );
        assert_eq!(
            text(&deleted, "DBClusterEndpointResourceIdentifier"),
            *id,
            "case {:?}: the delete must echo the minted id, got {deleted}",
            row.case
        );
    }
}
