// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW `DescribeDBClusterEndpoints` serves the report in PAGES.
//!
//! RULE: the report of the endpoints of the Aurora DB clusters of the request's region is
//! served in one stable order — each cluster in identifier order, its CUSTOM endpoints in
//! identifier order, then the two endpoints it has without a create, writer and reader — cut
//! into pages of at most
//! `MaxRecords` entries; a page carries a `Marker` exactly while entries beyond it remain,
//! and a request carrying that `Marker` serves "only records beyond the marker"
//! (`DescribeDBClusterEndpointsMessage$Marker`), so looping over the pages visits every
//! endpoint of the scope exactly once, in the order a single whole-report read serves them.
//! A `Marker` no page of this report could have issued names no position in it and is
//! refused with the Query API's `InvalidParameterValue` and this report's own invalid-marker
//! sentence. The report is a READ: paging through it leaves what a later caller sees
//! unchanged.
//!
//! The tables below span what the rule generalises over:
//!
//! * the page size: each bound `MaxRecords` publishes ("Constraints: Minimum 20, maximum
//!   100"), a size between them that divides the scope into whole pages with nothing left
//!   over — so a FULL last page still carries no `Marker` — and the member omitted, which
//!   takes its documented default;
//! * the scope the page is cut on: every endpoint of the region, the endpoints of one named
//!   cluster, and the endpoints a published filter selects — so a page is cut on the report
//!   the request asked for rather than on the whole store;
//! * the shape of a token no page issued: punctuation, a word, one character, encoded bytes
//!   that are no key of this report, and characters the XML envelope has to escape.
//!
//! Every call goes over the wire through the harness `db_instances.rs` owns, so each
//! assertion reads the XML the emulator itself answered with.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    invalid_marker_message, CUSTOM_ENDPOINT_TYPES, DB_CLUSTER_ENDPOINT_QUOTA_PER_CLUSTER,
    DEFAULT_MAX_RECORDS, MAX_RECORDS_RANGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};

/// The engine the clusters below run: the report "only applies to Aurora DB clusters".
const AURORA: &str = "aurora-postgresql";

/// How many clusters the paged report is seeded with — enough, with the custom endpoints
/// each one carries, that the smallest page `MaxRecords` admits cuts the report into
/// several pages.
const CLUSTERS: usize = 6;

/// How many custom endpoints each of those clusters carries: the whole per-cluster
/// allowance, so the seeding needs no more clusters than it has to.
const CUSTOM_PER_CLUSTER: usize = DB_CLUSTER_ENDPOINT_QUOTA_PER_CLUSTER;

/// The two endpoints every cluster has without a create, in the order the report serves
/// them (`DBClusterEndpoint$EndpointType` publishes "One of: READER, WRITER, CUSTOM").
const BUILTIN_TYPES: [&str; 2] = ["WRITER", "READER"];

/// `EndpointType` as every endpoint a caller creates is reported.
const CUSTOM: &str = "CUSTOM";

/// The most pages one read below ever asks for, so a report that kept handing back a token
/// fails the test instead of looping forever. Far above the pages the seeded report cuts
/// into at the smallest size the member admits.
const MAX_PAGES: usize = 16;

/// The identifier of the `index`th seeded cluster. Zero-padded, so the identifier order the
/// report serves the clusters in is the order they are seeded in however many there are.
fn cluster_name(index: usize) -> String {
    format!("emu-cep-cluster-{index:02}")
}

/// The identifier of the `position`th custom endpoint of the `cluster`th cluster, padded for
/// the same reason.
fn endpoint_name(cluster: usize, position: usize) -> String {
    format!("emu-cep-{cluster:02}-endpoint-{position:02}")
}

/// One endpoint of the report, by the members that identify it: the cluster it fronts, its
/// type, and the identifier a create gave it — which the cluster's own two endpoints carry
/// none of.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reported {
    cluster: String,
    endpoint_type: String,
    identifier: Option<String>,
}

/// Every endpoint `body` reports, in the order it reports them.
fn reported(body: &str) -> Vec<Reported> {
    subtrees(body, "DBClusterEndpointList")
        .iter()
        .map(|endpoint| Reported {
            cluster: text(endpoint, "DBClusterIdentifier"),
            endpoint_type: text(endpoint, "EndpointType"),
            identifier: optional_text(endpoint, "DBClusterEndpointIdentifier"),
        })
        .collect()
}

/// An emulator holding [`CLUSTERS`] Aurora clusters, each with [`CUSTOM_PER_CLUSTER`] custom
/// endpoints, and the endpoints the whole report is expected to carry, in order.
async fn seeded() -> (MultiServiceRouter, Vec<Reported>) {
    let router = emulator();
    let mut expected: Vec<Reported> = Vec::new();
    for index in 0..CLUSTERS {
        let cluster = cluster_name(index);
        expect_ok(
            &format!("creating {cluster}"),
            call(
                &router,
                "CreateDBCluster",
                &[
                    ("DBClusterIdentifier", cluster.clone()),
                    ("Engine", AURORA.to_string()),
                    ("MasterUsername", "emuadmin".to_string()),
                    ("MasterUserPassword", "emu-Secret-99".to_string()),
                ],
            )
            .await,
        );
        for position in 0..CUSTOM_PER_CLUSTER {
            let identifier = endpoint_name(index, position);
            // The requested type varies over every value the create publishes, so no page
            // below holds endpoints of one custom type only.
            let requested = CUSTOM_ENDPOINT_TYPES[position % CUSTOM_ENDPOINT_TYPES.len()];
            expect_ok(
                &format!("creating {identifier}"),
                call(
                    &router,
                    "CreateDBClusterEndpoint",
                    &[
                        ("DBClusterEndpointIdentifier", identifier.clone()),
                        ("DBClusterIdentifier", cluster.clone()),
                        ("EndpointType", requested.to_string()),
                    ],
                )
                .await,
            );
            expected.push(Reported {
                cluster: cluster.clone(),
                endpoint_type: CUSTOM.to_string(),
                identifier: Some(identifier),
            });
        }
        // The cluster's own two endpoints are reported BEHIND every custom endpoint created
        // for it, which is the order the report serves them in.
        for endpoint_type in BUILTIN_TYPES {
            expected.push(Reported {
                cluster: cluster.clone(),
                endpoint_type: endpoint_type.to_string(),
                identifier: None,
            });
        }
    }
    (router, expected)
}

/// Read the whole report of `scope` in one call, at the largest page the member admits.
async fn whole_report(
    router: &MultiServiceRouter,
    case: &str,
    scope: &[(&'static str, String)],
) -> Vec<Reported> {
    let mut params = scope.to_vec();
    params.push(("MaxRecords", MAX_RECORDS_RANGE.end().to_string()));
    let body = expect_ok(case, call(router, "DescribeDBClusterEndpoints", &params).await);
    assert!(
        optional_text(&body, "Marker").is_none(),
        "case {case:?}: a report served whole issues no Marker, got {body}"
    );
    reported(&body)
}

/// Page through the report of `scope`, resuming from each `Marker` it issues, and hand back
/// what each page carried.
async fn pages_of(
    router: &MultiServiceRouter,
    case: &str,
    scope: &[(&'static str, String)],
    page_size: Option<usize>,
) -> Vec<Vec<Reported>> {
    let mut pages: Vec<Vec<Reported>> = Vec::new();
    let mut marker: Option<String> = None;
    loop {
        let mut params = scope.to_vec();
        if let Some(size) = page_size {
            params.push(("MaxRecords", size.to_string()));
        }
        if let Some(token) = &marker {
            params.push(("Marker", token.clone()));
        }
        let body = expect_ok(case, call(router, "DescribeDBClusterEndpoints", &params).await);
        pages.push(reported(&body));
        marker = optional_text(&body, "Marker");
        if marker.is_none() {
            return pages;
        }
        assert!(
            pages.len() < MAX_PAGES,
            "case {case:?}: the report kept issuing a Marker after {} pages carrying {} \
             endpoints",
            pages.len(),
            pages.iter().map(Vec::len).sum::<usize>()
        );
    }
}

/// One row of the scope table: what the request asks for, and which endpoints of the whole
/// report that scope holds.
struct Scope {
    case: &'static str,
    params: Vec<(&'static str, String)>,
    holds: Box<dyn Fn(&Reported) -> bool>,
}

/// The scopes under test, and the page sizes each is paged at.
fn scopes() -> Vec<Scope> {
    let first_cluster = cluster_name(0);
    vec![
        Scope {
            case: "every endpoint of the region",
            params: Vec::new(),
            holds: Box::new(|_| true),
        },
        Scope {
            case: "the endpoints of one named cluster",
            params: vec![("DBClusterIdentifier", first_cluster.clone())],
            holds: Box::new(move |endpoint| endpoint.cluster == first_cluster),
        },
        Scope {
            // `db-cluster-endpoint-type` is one of the filter names the report publishes,
            // and `custom` one of that filter's published values.
            case: "the endpoints a published filter selects",
            params: vec![
                ("Filters.Filter.1.Name", "db-cluster-endpoint-type".to_string()),
                ("Filters.Filter.1.Values.Value.1", "custom".to_string()),
            ],
            holds: Box::new(|endpoint| endpoint.endpoint_type == CUSTOM),
        },
    ]
}

#[tokio::test]
async fn the_report_pages_every_endpoint_of_its_scope_exactly_once_in_one_order() {
    let (router, expected) = seeded().await;
    let whole = whole_report(&router, "every endpoint of the region", &[]).await;
    assert_eq!(
        whole, expected,
        "case {:?}: the report serves each cluster's custom endpoints, in identifier order, \
         and then its own two; expected {expected:#?}, got {whole:#?}",
        "every endpoint of the region"
    );

    // The page sizes each scope is paged at. The seeded report holds
    // CLUSTERS * (2 + CUSTOM_PER_CLUSTER) = 42 endpoints, so the size below the default
    // divides the whole scope into pages with nothing left over.
    let whole_scope_total = CLUSTERS * (BUILTIN_TYPES.len() + CUSTOM_PER_CLUSTER);
    let divides_evenly = whole_scope_total / 2;
    let sizes: [(&str, Option<usize>); 4] = [
        ("the smallest page the member admits", Some(*MAX_RECORDS_RANGE.start() as usize)),
        ("a page between the published bounds", Some(divides_evenly)),
        ("the largest page the member admits", Some(*MAX_RECORDS_RANGE.end() as usize)),
        ("no page size at all", None),
    ];

    for scope in scopes() {
        let in_scope: Vec<Reported> = expected
            .iter()
            .filter(|endpoint| (scope.holds)(endpoint))
            .cloned()
            .collect();
        assert!(
            !in_scope.is_empty(),
            "case {:?}: the seeding must leave this scope endpoints to page over",
            scope.case
        );
        let served_whole = whole_report(&router, scope.case, &scope.params).await;
        assert_eq!(
            served_whole, in_scope,
            "case {:?}: expected {in_scope:#?}, got {served_whole:#?}",
            scope.case
        );

        for (size_case, page_size) in sizes {
            let case = format!("{} at {size_case}", scope.case);
            let size = page_size.unwrap_or(DEFAULT_MAX_RECORDS);
            let pages = pages_of(&router, &case, &scope.params, page_size).await;

            // Every page but the last is FULL, the last carries what is left, and the count
            // of pages is the scope cut at that size.
            let expected_pages = in_scope.len().div_ceil(size);
            assert_eq!(
                pages.len(),
                expected_pages,
                "case {case:?}: {} endpoints at {size} per page is {expected_pages} page(s), \
                 got {} carrying {:?}",
                in_scope.len(),
                pages.len(),
                pages.iter().map(Vec::len).collect::<Vec<usize>>()
            );
            for (position, page) in pages.iter().enumerate() {
                let is_last = position + 1 == pages.len();
                let expected_len = if is_last {
                    in_scope.len() - position * size
                } else {
                    size
                };
                assert_eq!(
                    page.len(),
                    expected_len,
                    "case {case:?}: page {position} must carry {expected_len} endpoint(s), got \
                     {}",
                    page.len()
                );
            }

            // Concatenated, the pages are the scope's whole report: each endpoint once, in
            // the order one read serves them.
            let served: Vec<Reported> = pages.concat();
            assert_eq!(
                served, in_scope,
                "case {case:?}: expected {in_scope:#?}, got {served:#?}"
            );
        }
    }

    // Paging through a report is a READ: it moved nothing a later caller sees.
    let after = whole_report(&router, "the report after paging", &[]).await;
    assert_eq!(
        after, expected,
        "case {:?}: expected {expected:#?}, got {after:#?}",
        "the report after paging"
    );
}

#[tokio::test]
async fn a_marker_no_page_of_this_report_issued_is_refused() {
    let router = emulator();
    let cluster = cluster_name(0);
    expect_ok(
        "the cluster",
        call(
            &router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", cluster.clone()),
                ("Engine", AURORA.to_string()),
                ("MasterUsername", "emuadmin".to_string()),
                ("MasterUserPassword", "emu-Secret-99".to_string()),
            ],
        )
        .await,
    );

    for (case, token) in [
        ("punctuation only", "!!!!".to_string()),
        ("a word", "not-a-marker".to_string()),
        ("one character", "m".to_string()),
        ("encoded bytes that are no key", "/////w==".to_string()),
        (
            "characters the XML envelope has to escape",
            "a&b<c>\"d'".to_string(),
        ),
    ] {
        let body = expect_error(
            case,
            call(
                &router,
                "DescribeDBClusterEndpoints",
                &[("Marker", token.clone())],
            )
            .await,
            400,
            "InvalidParameterValue",
        );
        let message = text(&body, "Message");
        let expected = invalid_marker_message();
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // The sentence has no variable part, so it reflects nothing the request carried. A
        // token whose characters the sentence already holds (a single letter) cannot show
        // that, so it is read only on the tokens it can be read on.
        if !expected.contains(token.as_str()) {
            assert!(
                !message.contains(token.as_str()),
                "case {case:?}: this report's sentence reflects no part of the rejected token \
                 {token:?}, got {message:?}"
            );
        }
    }

    // The refusal is about the TOKEN, not about the member: the report still serves a page
    // when no marker is sent at all.
    let served = whole_report(&router, "the report with no marker", &[]).await;
    assert_eq!(
        served.len(),
        BUILTIN_TYPES.len(),
        "case {:?}: a cluster's own two endpoints are reported, got {served:#?}",
        "the report with no marker"
    );
}
