// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of ONE rule, across every listing this service pages.
//!
//! The rule under test: a listing's page-size member (`maxitems`, `maxresults` — the
//! `@paginated` `pageSize` of the operation) says how many items the response may carry,
//! so a value that is not a positive integer asks for no page at all and the request is
//! REFUSED. The refusal is the one input error every listing in this service models:
//! `InvalidInput`, HTTP 400, with the restXml envelope's `Error/Type` `Sender`,
//! `Error/Code` `InvalidInput` and the fixed sentence Route 53 words that class with. A
//! non-positive page size is never quietly turned into a page size the caller never asked
//! for, because a client that relies on the refusal to validate its own input would
//! otherwise accept values the service rejects.
//!
//! One listing asked for one bad page size is ONE row of that class, so the table below
//! spans it: every listing this service serves that takes a page size, crossed with the
//! non-positive values an `Integer` admits — zero, minus one and the smallest `Integer`
//! there is — plus the positive control that must still be served.
//!
//! Every test drives the service through the real router, using the SAME harness the
//! sibling listing suites use, so what is asserted is the HTTP/restXml behaviour a
//! Route 53 client sees.

use smithy_server_runtime::MultiServiceRouter;

use crate::cidr_collection_blocks::create_collection;
use crate::hosted_zone_records::{call, create_zone_ok, router_holding_vpcs, Reply};
use crate::traffic_policies::{create_policy, document_with_start_endpoint};
use crate::vpc_associations::{create_private_zone, CREATION_VPC, HOME_REGION};

/// The sentence Route 53 refuses a page size that is not a positive integer with, written
/// out here rather than read from the crate under test, so the WORDING itself is what the
/// assertions below hold the service to.
const NON_POSITIVE_PAGE_SIZE_MESSAGE: &str = "Max items must be a positive integer.";

/// The wire error code every listing states a page size it cannot serve under
/// (`InvalidInput`, the shape name the model declares).
const INVALID_INPUT: &str = "InvalidInput";

/// The `Error/Type` a client-side refusal carries in the restXml envelope: the sender's
/// request is what is at fault.
const SENDER: &str = "Sender";

/// One listing of the table: where it is bound, and what it names its page size.
struct ListingRow {
    case: &'static str,
    /// The request path, including any query parameter the operation requires.
    uri: String,
    /// The query parameter the operation binds its page size to.
    parameter: &'static str,
}

/// `uri` with `parameter=value` appended, joined with `?` or `&` as the path needs.
fn with_page_size(uri: &str, parameter: &str, value: i32) -> String {
    let join = if uri.contains('?') { '&' } else { '?' };
    format!("{uri}{join}{parameter}={value}")
}

/// Seed one of every resource the listings below are addressed at, and return the table.
async fn seed(router: &MultiServiceRouter) -> Vec<ListingRow> {
    let zone = create_zone_ok(router, "page-size.example.com", "page-size-zone").await;
    let private_zone = create_private_zone(
        router,
        "page-size-private.example.org",
        "page-size-private",
        CREATION_VPC,
    )
    .await;
    let collection = create_collection(router, "page-size").await;
    let created = create_policy(
        router,
        "page-size-policy",
        &document_with_start_endpoint("A", "192.0.2.10"),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "seeding a traffic policy: expected 201, got {} with body {}",
        created.status, created.body
    );
    let policy = created
        .first("TrafficPolicy/Id")
        .unwrap_or_else(|| {
            panic!(
                "the created policy must carry TrafficPolicy/Id; body {}",
                created.body
            )
        })
        .to_string();

    vec![
        ListingRow {
            case: "ListHostedZones",
            uri: "/2013-04-01/hostedzone".to_string(),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListHostedZonesByName",
            uri: "/2013-04-01/hostedzonesbyname".to_string(),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListHostedZonesByVPC",
            uri: format!(
                "/2013-04-01/hostedzonesbyvpc?vpcid={CREATION_VPC}&vpcregion={HOME_REGION}"
            ),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListResourceRecordSets",
            uri: format!("/2013-04-01/hostedzone/{zone}/rrset"),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListVPCAssociationAuthorizations",
            uri: format!("/2013-04-01/hostedzone/{private_zone}/authorizevpcassociation"),
            parameter: "maxresults",
        },
        ListingRow {
            case: "ListHealthChecks",
            uri: "/2013-04-01/healthcheck".to_string(),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListReusableDelegationSets",
            uri: "/2013-04-01/delegationset".to_string(),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListTrafficPolicies",
            uri: "/2013-04-01/trafficpolicies".to_string(),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListTrafficPolicyInstances",
            uri: "/2013-04-01/trafficpolicyinstances".to_string(),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListTrafficPolicyVersions",
            uri: format!("/2013-04-01/trafficpolicies/{policy}/versions"),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListGeoLocations",
            uri: "/2013-04-01/geolocations".to_string(),
            parameter: "maxitems",
        },
        ListingRow {
            case: "ListQueryLoggingConfigs",
            uri: "/2013-04-01/queryloggingconfig".to_string(),
            parameter: "maxresults",
        },
        ListingRow {
            case: "ListCidrCollections",
            uri: "/2013-04-01/cidrcollection".to_string(),
            parameter: "maxresults",
        },
        ListingRow {
            case: "ListCidrLocations",
            uri: format!("/2013-04-01/cidrcollection/{collection}"),
            parameter: "maxresults",
        },
        ListingRow {
            case: "ListCidrBlocks",
            uri: format!("/2013-04-01/cidrcollection/{collection}/cidrblocks"),
            parameter: "maxresults",
        },
    ]
}

/// Assert that `reply` is the modelled refusal of a page size that is not a positive
/// integer.
fn assert_refused(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected 400, got {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        INVALID_INPUT,
        "case {case:?}: expected {INVALID_INPUT}, got {:?}; body {}",
        reply.error_code(),
        reply.body
    );
    assert_eq!(
        reply.first("Error/Type"),
        Some(SENDER),
        "case {case:?}: expected the refusal to blame the sender, got {:?}; body {}",
        reply.first("Error/Type"),
        reply.body
    );
    assert_eq!(
        reply.first("Error/Message"),
        Some(NON_POSITIVE_PAGE_SIZE_MESSAGE),
        "case {case:?}: expected the message {NON_POSITIVE_PAGE_SIZE_MESSAGE:?}, got {:?}; \
         body {}",
        reply.first("Error/Message"),
        reply.body
    );
}

#[tokio::test]
async fn a_page_size_that_is_not_a_positive_integer_is_refused() {
    let router = router_holding_vpcs(&[(CREATION_VPC, HOME_REGION)]);
    for row in seed(&router).await {
        for asked in [0, -1, i32::MIN] {
            let case = format!("{} {}={asked}", row.case, row.parameter);
            let uri = with_page_size(&row.uri, row.parameter, asked);
            let reply = call(&router, "GET", &uri, "").await;
            assert_refused(&case, &reply);
        }
    }
}

#[tokio::test]
async fn the_smallest_positive_page_size_is_still_served() {
    // The contrast that makes the refusal above a refusal of the VALUE and not of the
    // member: one is the smallest page a listing can serve, and every listing serves it.
    let router = router_holding_vpcs(&[(CREATION_VPC, HOME_REGION)]);
    for row in seed(&router).await {
        for asked in [1, 2] {
            let case = format!("{} {}={asked}", row.case, row.parameter);
            let uri = with_page_size(&row.uri, row.parameter, asked);
            let reply = call(&router, "GET", &uri, "").await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected 200, got {} with body {}",
                reply.status, reply.body
            );
            assert_ne!(
                reply.first("Error/Message"),
                Some(NON_POSITIVE_PAGE_SIZE_MESSAGE),
                "case {case:?}: a positive page size is no refusal; body {}",
                reply.body
            );
        }
    }
}
