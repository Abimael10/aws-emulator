// SPDX-License-Identifier: CC0-1.0

//! How `GetTransitGatewayRouteTableAssociations` PAGES the associations one transit gateway
//! route table holds, and what it refuses, driven over `ec2Query` (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a paged report answers, for EVERY page size the model publishes,
//! each association of the route table exactly once in attachment-id order, mints a
//! `NextToken` only while an association still follows the page it answered, refuses a page
//! size outside the published range and a token it did not mint, and — being a report —
//! leaves the associations a later caller sees exactly as they were.
//!
//! Each row of each table below is a distinct input along what the rule generalises over:
//! which page size walks the set (none, each published bound, and the sizes around the count
//! the store holds), how a token fails to be one this service minted, and whether the request
//! is a dry run. The suite beside this one
//! (`transit_gateway_route_table_associations.rs`) covers what an association REPORTS; this
//! one covers how the report is paged and refused.

use ec2::ops::types::TransitGatewayVpcAttachment;
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE,
    DRY_RUN_OPERATION_STATUS, INVALID_MAX_RESULTS, INVALID_PAGINATION_TOKEN,
    INVALID_PAGINATION_TOKEN_MESSAGE, TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
    TransitGatewayAttachmentView,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message};
use super::wire::param;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The `(minimum, maximum)` page size this operation publishes — the `@range` of the shape its
/// `MaxResults` targets:
///
/// ```text
/// smithy_query(query="shape",shape="TransitGatewayMaxResults",model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")
/// ```
const PUBLISHED_PAGE_SIZE_RANGE: (i64, i64) = (5, 1000);

/// How many associations the route table under test holds: more than the smallest published
/// page size, so a page of that size leaves a shorter page behind it.
const SEEDED: usize = 7;

/// One call of `action` signed for the Region above.
async fn invoke(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One `TransitGatewayRouteTableId=` request member.
fn table_param(route_table_id: &str) -> Vec<(String, String)> {
    vec![param("TransitGatewayRouteTableId", route_table_id)]
}

/// A transit gateway created through the wire, as the id of the default ASSOCIATION route
/// table the create owes it ("we create a default transit gateway route table and use it as
/// the default association route table" — `CreateTransitGateway`), with [`SEEDED`] VPC
/// attachments associated with it.
///
/// Answers `(router, engine, route table id, the associated attachment ids in order)`.
async fn seeded() -> (
    MultiServiceRouter,
    std::sync::Arc<engine::InMemoryEngine>,
    String,
    Vec<String>,
) {
    let (router, engine) = super::ec2_query::emulator();
    let (status, body) = invoke(&router, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "CreateTransitGateway must succeed, got {status} with body {body}"
    );
    let route_table_id = element(&body, "associationDefaultRouteTableId")
        .expect("a gateway created with the documented defaults has an association route table");
    let transit_gateway_id =
        element(&body, "transitGatewayId").expect("a created gateway reports its id");

    let attachment_ids: Vec<String> = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        (0..SEEDED)
            .map(|index| {
                guard
                    .insert_transit_gateway_attachment(TransitGatewayAttachmentView::Vpc(
                        TransitGatewayVpcAttachment {
                            transit_gateway_id: Some(transit_gateway_id.clone()),
                            vpc_id: Some(format!("vpc-0123456789abcdef{index}")),
                            vpc_owner_id: Some("210987654321".to_string()),
                            state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
                            ..Default::default()
                        },
                    ))
                    .expect("the store holds the seeded attachment")
            })
            .collect()
    };
    for attachment_id in &attachment_ids {
        let (status, body) = invoke(
            &router,
            "AssociateTransitGatewayRouteTable",
            &[
                param("TransitGatewayRouteTableId", &route_table_id),
                param("TransitGatewayAttachmentId", attachment_id),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "seeding the association of {attachment_id}: expected HTTP 200, got {status} with \
             body {body}"
        );
    }

    let mut ordered = attachment_ids;
    ordered.sort();
    (router, engine, route_table_id, ordered)
}

/// The attachment ids one page of the report carries, in the order the body lists them.
fn reported_ids(body: &str) -> Vec<String> {
    elements(body, "transitGatewayAttachmentId")
}

/// Walk the whole report with `page_size` as its page size (`None` sends no `MaxResults`),
/// answering every attachment id the walk yielded, in order, and how many calls it took.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    route_table_id: &str,
    page_size: Option<i64>,
) -> (Vec<String>, usize) {
    let mut walked = Vec::new();
    let mut token: Option<String> = None;
    let mut calls = 0;
    loop {
        let mut params = table_param(route_table_id);
        if let Some(limit) = page_size {
            params.push(param("MaxResults", &limit.to_string()));
        }
        if let Some(cursor) = &token {
            params.push(param("NextToken", cursor));
        }
        let (status, body) = invoke(
            router,
            "GetTransitGatewayRouteTableAssociations",
            &params,
        )
        .await;
        calls += 1;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let page = reported_ids(&body);
        if let Some(limit) = page_size {
            assert!(
                i64::try_from(page.len()).unwrap_or(i64::MAX) <= limit,
                "case {case:?}: a page carries at most {limit} associations, got {page:?}"
            );
        }
        walked.extend(page.clone());
        token = element(&body, "nextToken");
        match token.as_deref() {
            None => break,
            // A request that names no page size is served the largest page the model publishes,
            // so the page a token may follow is a full one either way.
            Some(cursor) => assert!(
                !cursor.is_empty()
                    && i64::try_from(page.len()).unwrap_or(i64::MAX)
                        == page_size.unwrap_or(PUBLISHED_PAGE_SIZE_RANGE.1),
                "case {case:?}: a token is minted only after a full page, got {body}"
            ),
        }
        assert!(
            calls <= SEEDED + 1,
            "case {case:?}: the walk must advance; it has made {calls} calls over {SEEDED} \
             associations"
        );
    }
    (walked, calls)
}

/// Whatever page size the request names — including naming none — a walk of the report yields
/// every association of the route table exactly once, in attachment-id order, and ends on the
/// page no association follows.
#[tokio::test]
async fn every_published_page_size_walks_each_association_exactly_once() {
    let (router, _engine, route_table_id, expected) = seeded().await;

    // No `MaxResults`, each published bound, and the sizes around the count the store holds —
    // so a page shorter than, equal to and longer than the whole set are each walked.
    for page_size in [
        None,
        Some(PUBLISHED_PAGE_SIZE_RANGE.0),
        Some(6),
        Some(7),
        Some(8),
        Some(PUBLISHED_PAGE_SIZE_RANGE.1),
    ] {
        let case = format!("MaxResults={page_size:?}");
        let (walked, calls) = walk(&router, &case, &route_table_id, page_size).await;
        assert_eq!(
            walked, expected,
            "case {case:?}: the pages must cover every association once, in attachment-id \
             order, got {walked:?}"
        );
        let expected_calls = page_size.map_or(1, |limit| {
            let limit = usize::try_from(limit).unwrap_or(SEEDED);
            SEEDED.div_ceil(limit)
        });
        assert_eq!(
            calls, expected_calls,
            "case {case:?}: expected {expected_calls} call(s) to walk {SEEDED} associations, \
             took {calls}"
        );
    }
}

/// Each published bound of the page size is SERVED and one past either bound is refused, with
/// EC2's own code for a page size the operation will not page with and a sentence naming the
/// member and the bound the value missed.
#[tokio::test]
async fn a_page_size_outside_the_published_range_is_refused() {
    let (router, _engine, route_table_id, expected) = seeded().await;
    let (minimum, maximum) = PUBLISHED_PAGE_SIZE_RANGE;

    for (case, page_size, expected_status) in [
        ("one below the published minimum", minimum - 1, 400),
        ("the published minimum itself", minimum, 200),
        ("the published maximum itself", maximum, 200),
        ("one above the published maximum", maximum + 1, 400),
    ] {
        let mut params = table_param(&route_table_id);
        params.push(param("MaxResults", &page_size.to_string()));
        let (status, body) = invoke(
            &router,
            "GetTransitGatewayRouteTableAssociations",
            &params,
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        if expected_status == 400 {
            assert_eq!(
                error_code(&body),
                INVALID_MAX_RESULTS,
                "case {case:?}: expected {INVALID_MAX_RESULTS}, got {body}"
            );
            let message = error_message(&body);
            assert!(
                message.contains("maxResults") && message.contains(&page_size.to_string()),
                "case {case:?}: the refusal must name the member and the value it rejected, \
                 got {message:?}"
            );
            assert!(
                reported_ids(&body).is_empty(),
                "case {case:?}: a refused request reports no association, got {body}"
            );
        } else {
            // A served page carries as many associations as the page size admits, which at the
            // published minimum is fewer than the route table holds.
            let served = usize::try_from(page_size).unwrap_or(SEEDED).min(SEEDED);
            assert_eq!(
                reported_ids(&body),
                expected[..served].to_vec(),
                "case {case:?}: a page size inside the range serves the first {served} \
                 associations, got {body}"
            );
        }
    }
}

/// A `NextToken` this service did not mint is refused rather than read as a position, however
/// it fails to be one — and refused before the store is read, so a bogus token is caught even
/// where the route table the request names is one no call ever created.
#[tokio::test]
async fn a_next_token_this_service_did_not_mint_is_refused() {
    let (router, _engine, route_table_id, expected) = seeded().await;
    let an_attachment = expected[0].clone();
    // A token this service DID mint, with the position it carries altered — so the checksum no
    // longer matches what the payload says.
    let tampered = {
        let minted = ec2::state::encode_page_token(&an_attachment);
        format!("{minted}00")
    };
    // A well-formed route table id of this kind that names no route table the account holds:
    // the token is refused ahead of it.
    let absent_route_table = "tgw-rtb-0fedcba9876543210";

    for (case, table, token) in [
        ("plain text", route_table_id.as_str(), "not-a-token"),
        (
            "an id of the kind the cursor names",
            route_table_id.as_str(),
            an_attachment.as_str(),
        ),
        ("odd-length hex", route_table_id.as_str(), "abc"),
        ("hex of a foreign payload", route_table_id.as_str(), "6162"),
        (
            "a minted token whose payload was altered",
            route_table_id.as_str(),
            tampered.as_str(),
        ),
        (
            "a bogus token on a route table the account does not hold",
            absent_route_table,
            "not-a-token",
        ),
    ] {
        let mut params = table_param(table);
        params.push(param("NextToken", token));
        let (status, body) = invoke(
            &router,
            "GetTransitGatewayRouteTableAssociations",
            &params,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PAGINATION_TOKEN,
            "case {case:?}: expected {INVALID_PAGINATION_TOKEN}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            INVALID_PAGINATION_TOKEN_MESSAGE,
            "case {case:?}: expected {INVALID_PAGINATION_TOKEN_MESSAGE:?}, got {body}"
        );
        assert!(
            reported_ids(&body).is_empty(),
            "case {case:?}: a refused request reports no association, got {body}"
        );
    }

    // The token the report itself mints is still read, so the refusals above are about the
    // token and not about paging at all.
    let mut first = table_param(&route_table_id);
    first.push(param("MaxResults", "5"));
    let (status, body) = invoke(&router, "GetTransitGatewayRouteTableAssociations", &first).await;
    assert_eq!(
        status, 200,
        "a first page: expected HTTP 200, got {status} with body {body}"
    );
    let minted = element(&body, "nextToken").expect("a page the rest of the set follows");
    let mut resumed = table_param(&route_table_id);
    resumed.push(param("MaxResults", "5"));
    resumed.push(param("NextToken", &minted));
    let (status, body) = invoke(
        &router,
        "GetTransitGatewayRouteTableAssociations",
        &resumed,
    )
    .await;
    assert_eq!(
        status, 200,
        "the resumed page: expected HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        reported_ids(&body),
        expected[5..].to_vec(),
        "the resumed page: expected the associations after the first page, got {body}"
    );
}

/// A dry run of the report answers the probe and reports nothing, and the report itself leaves
/// the associations a later caller sees exactly as they were — a read changes nothing.
#[tokio::test]
async fn a_dry_run_probes_and_the_report_leaves_the_associations_unchanged() {
    let (router, _engine, route_table_id, expected) = seeded().await;

    let (status, body) = invoke(
        &router,
        "GetTransitGatewayRouteTableAssociations",
        &table_param(&route_table_id),
    )
    .await;
    assert_eq!(
        status, 200,
        "the first report: expected HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        reported_ids(&body),
        expected,
        "the first report: expected every association, got {body}"
    );

    let mut probe = table_param(&route_table_id);
    probe.push(param("DryRun", "true"));
    let (status, body) = invoke(&router, "GetTransitGatewayRouteTableAssociations", &probe).await;
    assert_eq!(
        status, DRY_RUN_OPERATION_STATUS,
        "a dry run: expected HTTP {DRY_RUN_OPERATION_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "a dry run: expected {DRY_RUN_OPERATION}, got {body}"
    );
    assert_eq!(
        error_message(&body),
        DRY_RUN_OPERATION_MESSAGE,
        "a dry run: expected {DRY_RUN_OPERATION_MESSAGE:?}, got {body}"
    );
    assert!(
        reported_ids(&body).is_empty(),
        "a dry run: the probe reports no association, got {body}"
    );

    // Everything above was a READ, so the set a later caller sees is the one the associations
    // left behind.
    let (status, body) = invoke(
        &router,
        "GetTransitGatewayRouteTableAssociations",
        &table_param(&route_table_id),
    )
    .await;
    assert_eq!(
        status, 200,
        "the report after the reads: expected HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        reported_ids(&body),
        expected,
        "the report after the reads: expected the same associations, got {body}"
    );
}
