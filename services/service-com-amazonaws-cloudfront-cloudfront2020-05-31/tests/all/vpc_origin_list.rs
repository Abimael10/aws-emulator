// SPDX-License-Identifier: CC0-1.0

//! Listing the CloudFront VPC origins of the account, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rules under test:
//!
//! 1. the listing reports the VPC origins the STORE holds, in id order, each summary read from
//!    the same record a get of that VPC origin answers from — so the two can never disagree —
//!    with `Quantity` stating the length of `Items`, `Marker` echoing the value the request
//!    provided and `MaxItems` the bound it asked for;
//! 2. `Marker` names where the previous page ENDED, so walking the pages with `NextMarker`
//!    enumerates every stored VPC origin exactly once, and `IsTruncated` and `NextMarker`
//!    arrive exactly while VPC origins remain;
//! 3. a store holding no VPC origin at all is a page of none: `Quantity` 0 and no `Items`
//!    element;
//! 4. a request no page can be taken from is refused: a `MaxItems` that bounds nothing, a
//!    `Marker` outside the form CloudFront writes a VPC origin identifier in, and a `Marker`
//!    of that form the store holds nothing under;
//! 5. the listing is a REPORT of the store: a VPC origin a delete removed is listed by
//!    nothing, and asking twice answers the same page.
//!
//! The rows span what the rules generalise over: the number of VPC origins stored, the page
//! size asked for (one, two and more than the store holds), each shape a `Marker` can take,
//! and the kind of resource the endpoints front.
//!
//! The codes and sentences are LITERALS here rather than the crate's own constants, so a
//! reworded refusal fails this suite. The wire harness is [`crate::distribution_lifecycle`]'s
//! and the create is [`crate::vpc_origin_lifecycle`]'s.

use std::collections::BTreeMap;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::VPC_ORIGIN_ID_PREFIX;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, Node, ACCOUNT_ID, API_VERSION};
use crate::key_value_store_lifecycle::escape;
use crate::vpc_origin_lifecycle::{create_vpc_origin, load_balancer_arn};

/// The sentence a listing refuses a `Marker` that is no resume point it hands out with.
const INVALID_MARKER_MESSAGE: &str = "Marker must be a valid CF vpc origin Id";

/// The sentence a listing refuses a `MaxItems` that bounds no page with.
const INVALID_MAX_ITEMS_MESSAGE: &str = "MaxItems must be a non-zero positive integer.";

/// The sentence a `Marker` of a readable form the store holds nothing under is refused with.
const NOT_FOUND_MESSAGE: &str = "The specified vpc origin does not exist";

/// One VPC origin a case seeds, and what its summary has to report.
#[derive(Debug, Clone)]
struct Seed {
    name: String,
    /// The resource in the caller's VPC the endpoint fronts.
    endpoint_arn: String,
    /// One of `VpcIpAddressType`'s values (`ipv4`, `dualstack`, from
    /// `smithy_query(query="shape",shape="VpcIpAddressType")`), or `None` where the
    /// configuration names none.
    ip_address_type: Option<&'static str>,
}

/// The VPC origins the suite seeds: the kinds of resource an endpoint fronts, and each value
/// `IpAddressType` admits beside its absence.
fn seeds() -> Vec<Seed> {
    vec![
        Seed {
            name: "orders-alb".to_string(),
            endpoint_arn: load_balancer_arn("orders"),
            ip_address_type: None,
        },
        Seed {
            name: "checkout-nlb".to_string(),
            endpoint_arn: format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/net/edge/\
                 fedcba9876543210"
            ),
            ip_address_type: Some("ipv4"),
        },
        Seed {
            name: "reports-instance".to_string(),
            endpoint_arn: format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:instance/i-0123456789abcdef0"),
            ip_address_type: Some("dualstack"),
        },
    ]
}

/// The `CreateVpcOriginRequest` document for one seed.
fn create_body(seed: &Seed) -> String {
    let ip_address_type = seed
        .ip_address_type
        .map(|kind| format!("<IpAddressType>{kind}</IpAddressType>"))
        .unwrap_or_default();
    format!(
        "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <VpcOriginEndpointConfig>\
         <Name>{}</Name><Arn>{}</Arn>\
         <HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         {ip_address_type}\
         </VpcOriginEndpointConfig></CreateVpcOriginRequest>",
        escape(&seed.name),
        escape(&seed.endpoint_arn)
    )
}

/// `GET /2020-05-31/vpc-origin` with the query string `query` (empty for none).
async fn list_vpc_origins(router: &MultiServiceRouter, query: &str) -> Answer {
    let uri = if query.is_empty() {
        format!("/{API_VERSION}/vpc-origin")
    } else {
        format!("/{API_VERSION}/vpc-origin?{query}")
    };
    call(router, "GET", &uri, None, None).await
}

/// `GET /2020-05-31/vpc-origin/{Id}`.
async fn get_vpc_origin(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/vpc-origin/{id}"),
        None,
        None,
    )
    .await
}

/// `DELETE /2020-05-31/vpc-origin/{Id}` under the version token `if_match`.
async fn delete_vpc_origin(router: &MultiServiceRouter, id: &str, if_match: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/{API_VERSION}/vpc-origin/{id}"),
        None,
        Some(if_match),
    )
    .await
}

/// Seed every VPC origin and report its id under its name, in creation order.
async fn seeded(router: &MultiServiceRouter, seeds: &[Seed]) -> BTreeMap<String, String> {
    let mut ids = BTreeMap::new();
    for seed in seeds {
        let created = create_vpc_origin(router, &create_body(seed)).await;
        assert_eq!(
            created.status, 202,
            "case seeding {}: expected HTTP 202, got {} with {}",
            seed.name, created.status, created.raw
        );
        let id = created
            .root()
            .find_text("Id")
            .unwrap_or_default()
            .to_string();
        assert!(
            id.starts_with(VPC_ORIGIN_ID_PREFIX),
            "case seeding {}: expected a minted VPC origin id, got {id:?}",
            seed.name
        );
        ids.insert(seed.name.clone(), id);
    }
    ids
}

/// The `VpcOriginSummary` elements of one page.
fn summaries<'a>(list: &'a Node, case: &str, raw: &str) -> Vec<&'a Node> {
    let Some(items) = list.at("Items") else {
        return Vec::new();
    };
    let found = items.children_named("VpcOriginSummary");
    assert!(
        !found.is_empty(),
        "case {case}: an Items element must carry summaries, got {raw}"
    );
    found
}

/// One member of a summary.
fn member(summary: &Node, name: &str, case: &str, raw: &str) -> String {
    summary
        .text_at(name)
        .unwrap_or_else(|| panic!("case {case}: no {name} in the summary: {raw}"))
        .to_string()
}

/// RULE 1 and RULE 5: the listing reports the store, each summary read from the record a get
/// answers from, with the wrapper's own members stating the page.
#[tokio::test]
async fn the_listing_reports_every_stored_vpc_origin_from_its_own_record() {
    let router = emulator();
    let rows = seeds();
    let ids = seeded(&router, &rows).await;

    let listed = list_vpc_origins(&router, "").await;
    assert_eq!(
        listed.status, 200,
        "case the whole store: expected HTTP 200, got {} with {}",
        listed.status, listed.raw
    );
    let list = listed.root();
    let raw = listed.raw.as_str();
    assert_eq!(
        list.text_at("Marker").unwrap_or_default(),
        "",
        "case the whole store: a first page echoes the empty marker, got {raw}"
    );
    assert_eq!(
        list.text_at("IsTruncated").unwrap_or_default(),
        "false",
        "case the whole store: nothing remains after it, got {raw}"
    );
    assert_eq!(
        list.text_at("Quantity").unwrap_or_default(),
        rows.len().to_string(),
        "case the whole store: Quantity states the length of Items, got {raw}"
    );
    assert_eq!(
        list.text_at("MaxItems").unwrap_or_default(),
        "100",
        "case the whole store: expected this listing's own page size, got {raw}"
    );
    assert!(
        list.at("NextMarker").is_none(),
        "case the whole store: an untruncated page names no next marker, got {raw}"
    );

    let page = summaries(list, "the whole store", raw);
    assert_eq!(
        page.len(),
        rows.len(),
        "case the whole store: expected one summary per stored VPC origin, got {raw}"
    );
    // In ID order, which is the order the pages resume through.
    let listed_ids: Vec<String> = page
        .iter()
        .map(|summary| member(summary, "Id", "the whole store", raw))
        .collect();
    let mut sorted = listed_ids.clone();
    sorted.sort();
    assert_eq!(
        listed_ids, sorted,
        "case the whole store: expected the summaries in id order, got {listed_ids:?}"
    );

    for seed in &rows {
        let id = ids.get(&seed.name).expect("every seed was created");
        let summary = page
            .iter()
            .find(|summary| member(summary, "Id", &seed.name, raw) == *id)
            .unwrap_or_else(|| panic!("case {}: {id} is not listed: {raw}", seed.name));
        assert_eq!(
            member(summary, "Name", &seed.name, raw),
            seed.name,
            "case {}: expected the stored endpoint name",
            seed.name
        );
        assert_eq!(
            member(summary, "OriginEndpointArn", &seed.name, raw),
            seed.endpoint_arn,
            "case {}: expected the stored endpoint Arn",
            seed.name
        );
        assert_eq!(
            member(summary, "AccountId", &seed.name, raw),
            ACCOUNT_ID,
            "case {}: expected the owning account",
            seed.name
        );
        match seed.ip_address_type {
            Some(kind) => assert_eq!(
                member(summary, "IpAddressType", &seed.name, raw),
                kind,
                "case {}: expected the stored IpAddressType",
                seed.name
            ),
            None => assert!(
                summary.at("IpAddressType").is_none(),
                "case {}: a configuration naming no IpAddressType reports none, got {raw}",
                seed.name
            ),
        }

        // The same record the get answers from: the summary cannot drift from the read.
        let read = get_vpc_origin(&router, id).await;
        assert_eq!(
            member(summary, "Arn", &seed.name, raw),
            read.root().text_at("Arn").unwrap_or_default(),
            "case {}: expected the ARN the get reports",
            seed.name
        );
        assert_eq!(
            member(summary, "Status", &seed.name, raw),
            read.root().text_at("Status").unwrap_or_default(),
            "case {}: expected the status the get reports",
            seed.name
        );
        for instant in ["CreatedTime", "LastModifiedTime"] {
            assert_eq!(
                member(summary, instant, &seed.name, raw),
                read.root().text_at(instant).unwrap_or_default(),
                "case {}: expected the {instant} the get reports",
                seed.name
            );
        }
    }

    // Asking twice answers the same page: a listing is a report, not a consumption.
    let again = list_vpc_origins(&router, "").await;
    assert_eq!(
        again.raw, listed.raw,
        "case asking twice: expected the same page, got {}",
        again.raw
    );
}

/// RULE 2: walking the pages with `NextMarker` enumerates every stored VPC origin exactly
/// once, whatever page size is asked for.
#[tokio::test]
async fn walking_the_pages_enumerates_each_vpc_origin_exactly_once() {
    let router = emulator();
    let rows = seeds();
    let ids = seeded(&router, &rows).await;
    let mut expected: Vec<String> = ids.values().cloned().collect();
    expected.sort();

    // Page sizes below, at and above the number stored.
    for page_size in [1_usize, 2, rows.len(), rows.len() + 1] {
        let case = format!("pages of {page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let query = match &marker {
                Some(value) => format!("MaxItems={page_size}&Marker={value}"),
                None => format!("MaxItems={page_size}"),
            };
            let page = list_vpc_origins(&router, &query).await;
            assert_eq!(
                page.status, 200,
                "case {case}: expected HTTP 200, got {} with {}",
                page.status, page.raw
            );
            let list = page.root();
            let raw = page.raw.as_str();
            assert_eq!(
                list.text_at("Marker").unwrap_or_default(),
                marker.clone().unwrap_or_default(),
                "case {case}: expected the marker the request provided, got {raw}"
            );
            assert_eq!(
                list.text_at("MaxItems").unwrap_or_default(),
                page_size.to_string(),
                "case {case}: expected the bound the request asked for, got {raw}"
            );
            let this_page: Vec<String> = summaries(list, &case, raw)
                .iter()
                .map(|summary| member(summary, "Id", &case, raw))
                .collect();
            assert!(
                this_page.len() <= page_size,
                "case {case}: a page must not exceed the bound, got {raw}"
            );
            assert_eq!(
                list.text_at("Quantity").unwrap_or_default(),
                this_page.len().to_string(),
                "case {case}: Quantity states the length of Items, got {raw}"
            );
            walked.extend(this_page.iter().cloned());

            let truncated = list.text_at("IsTruncated").unwrap_or_default() == "true";
            match list.text_at("NextMarker") {
                Some(next) => {
                    assert!(
                        truncated,
                        "case {case}: a next marker arrives only while VPC origins remain, got \
                         {raw}"
                    );
                    assert_eq!(
                        Some(next.to_string()),
                        this_page.last().cloned(),
                        "case {case}: the next marker is the id the page ended at, got {raw}"
                    );
                    marker = Some(next.to_string());
                }
                None => {
                    assert!(
                        !truncated,
                        "case {case}: a truncated page must name its next marker, got {raw}"
                    );
                    break;
                }
            }
        }
        assert_eq!(
            walked, expected,
            "case {case}: expected every stored VPC origin exactly once, in id order"
        );
    }
}

/// RULE 3: a store holding no VPC origin answers a page of none — and the element a page of
/// none carries no summaries at all.
#[tokio::test]
async fn a_store_holding_no_vpc_origin_lists_nothing() {
    let router = emulator();

    let listed = list_vpc_origins(&router, "").await;
    assert_eq!(
        listed.status, 200,
        "case an empty store: expected HTTP 200, got {} with {}",
        listed.status, listed.raw
    );
    assert_eq!(
        listed.root().text_at("Quantity").unwrap_or_default(),
        "0",
        "case an empty store: expected Quantity 0, got {}",
        listed.raw
    );
    assert_eq!(
        listed.root().text_at("IsTruncated").unwrap_or_default(),
        "false",
        "case an empty store: nothing remains, got {}",
        listed.raw
    );
    assert!(
        listed.root().at("Items").is_none(),
        "case an empty store: expected no Items element, got {}",
        listed.raw
    );
}

/// RULE 4: a request no page can be taken from is refused — along the `MaxItems` that bounds
/// nothing and each shape of `Marker` that is no resume point of this listing.
#[tokio::test]
async fn a_request_no_page_can_be_taken_from_is_refused() {
    let router = emulator();
    let rows = seeds();
    let ids = seeded(&router, &rows).await;
    let held = ids
        .values()
        .next()
        .expect("the seeds are not empty")
        .clone();

    // A page size that bounds nothing.
    for (case, max_items) in [("a negative page size", "-1"), ("a very negative one", "-100")] {
        let refused = list_vpc_origins(&router, &format!("MaxItems={max_items}")).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected HTTP 400, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "InvalidArgument",
            "case {case}: expected InvalidArgument, got {:?} ({})",
            refused.error_code(),
            refused.raw
        );
        assert_eq!(
            refused.root().find_text("Message").unwrap_or_default(),
            INVALID_MAX_ITEMS_MESSAGE,
            "case {case}: expected message {INVALID_MAX_ITEMS_MESSAGE:?}, got {}",
            refused.raw
        );
    }

    // Rows along the shape of a `Marker` this listing never handed out.
    let unreadable: [(&str, String); 4] = [
        ("a bare word", "vpc-origin".to_string()),
        ("a distribution id", "EZZZZZZZZZZZZ9".to_string()),
        (
            "a lower-cased body",
            format!("{VPC_ORIGIN_ID_PREFIX}absent0000000"),
        ),
        (
            "a body of the wrong width",
            format!("{VPC_ORIGIN_ID_PREFIX}SHORT"),
        ),
    ];
    for (case, marker) in &unreadable {
        let refused = list_vpc_origins(&router, &format!("Marker={marker}")).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected HTTP 400 for {marker:?}, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "InvalidArgument",
            "case {case}: expected InvalidArgument for {marker:?}, got {:?} ({})",
            refused.error_code(),
            refused.raw
        );
        assert_eq!(
            refused.root().find_text("Message").unwrap_or_default(),
            INVALID_MARKER_MESSAGE,
            "case {case}: expected message {INVALID_MARKER_MESSAGE:?}, got {}",
            refused.raw
        );
    }

    // A marker of the form this listing hands out that the store holds nothing under resumes
    // nothing: the entity it names is absent.
    let mut flipped: Vec<char> = held.chars().collect();
    let last = flipped.len() - 1;
    flipped[last] = if flipped[last] == 'A' { 'B' } else { 'A' };
    let absent: [(&str, String); 2] = [
        (
            "a body no VPC origin holds",
            format!("{VPC_ORIGIN_ID_PREFIX}ABSENT0000000"),
        ),
        (
            "a held id with its last character changed",
            flipped.into_iter().collect(),
        ),
    ];
    for (case, marker) in &absent {
        let refused = list_vpc_origins(&router, &format!("Marker={marker}")).await;
        assert_eq!(
            refused.status, 404,
            "case {case}: expected HTTP 404 for {marker:?}, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "EntityNotFound",
            "case {case}: expected EntityNotFound for {marker:?}, got {:?} ({})",
            refused.error_code(),
            refused.raw
        );
        assert_eq!(
            refused.root().find_text("Message").unwrap_or_default(),
            NOT_FOUND_MESSAGE,
            "case {case}: expected message {NOT_FOUND_MESSAGE:?}, got {}",
            refused.raw
        );
    }

    // Every refusal left the store as it was.
    let listed = list_vpc_origins(&router, "").await;
    assert_eq!(
        listed.root().text_at("Quantity").unwrap_or_default(),
        rows.len().to_string(),
        "case after the refusals: expected the store unchanged, got {}",
        listed.raw
    );
}

/// RULE 5: a VPC origin a delete removed is listed by nothing — the listing reads the store
/// rather than a second copy of it.
#[tokio::test]
async fn a_deleted_vpc_origin_is_no_longer_listed() {
    let router = emulator();
    let rows = seeds();
    let ids = seeded(&router, &rows).await;
    let (name, removed) = ids
        .iter()
        .next()
        .map(|(name, id)| (name.clone(), id.clone()))
        .expect("the seeds are not empty");

    // The delete runs once the create has propagated, which is the one state a delete is
    // refused in; the token is the one the record reports right now. The CONDITION is what is
    // waited for, never a span of the host's clock.
    let deadline = std::time::Instant::now() + crate::distribution_lifecycle::READY_DEADLINE;
    let etag = loop {
        let read = get_vpc_origin(&router, &removed).await;
        if read.root().text_at("Status").unwrap_or_default() == "Deployed" {
            break read.header("etag").to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case deleting {name}: the VPC origin never reached Deployed, last answer {}",
            read.raw
        );
        ::tokio::time::sleep(crate::distribution_lifecycle::POLL_INTERVAL).await;
    };
    let deleted = delete_vpc_origin(&router, &removed, &etag).await;
    assert_eq!(
        deleted.status, 202,
        "case deleting {name}: expected HTTP 202, got {} with {}",
        deleted.status, deleted.raw
    );

    let listed = list_vpc_origins(&router, "").await;
    let raw = listed.raw.clone();
    let remaining: Vec<String> = summaries(listed.root(), "after the delete", &raw)
        .iter()
        .map(|summary| member(summary, "Id", "after the delete", &raw))
        .collect();
    assert!(
        !remaining.contains(&removed),
        "case after the delete: {removed} must not be listed, got {remaining:?}"
    );
    assert_eq!(
        remaining.len(),
        rows.len() - 1,
        "case after the delete: expected the rest of the store, got {raw}"
    );
    assert_eq!(
        listed.root().text_at("Quantity").unwrap_or_default(),
        (rows.len() - 1).to_string(),
        "case after the delete: Quantity states the length of Items, got {raw}"
    );
}
