// SPDX-License-Identifier: CC0-1.0

//! `ListDistributionsByVpcOriginId`, end to end over HTTP.
//!
//! The rule under test: a distribution is listed under a VPC origin exactly when one of the
//! origins of its STORED configuration names that VPC origin in
//! `VpcOriginConfig$VpcOriginId`, each matching distribution listed ONCE and in id order; the
//! page contract is the one every CloudFront distribution listing states (`MaxItems` is the
//! page size and is echoed, `Marker` resumes after the id the previous page ended at,
//! `NextMarker` and `IsTruncated` arrive exactly when matching distributions remain,
//! `Quantity` states the length of `Items`); a VPC origin no distribution fronts lists an
//! empty `Items` element rather than the whole store; and an id the store holds no VPC origin
//! for is `EntityNotFound` (HTTP 404).
//!
//! The rows below span what that rule generalises over: WHICH origin of a distribution names
//! the VPC origin (the only one, the second of two, the last of three), how many origins of
//! ONE distribution name the SAME VPC origin (so a distribution is still listed once), how
//! many distinct VPC origins one distribution fronts, how many distributions share a VPC
//! origin (so a page can cut inside a matching set), a distribution whose origins front no VPC
//! origin at all, and a VPC origin no distribution names.
//!
//! The wire harness (the router, the request helpers and the element scanner) is the one
//! [`crate::distribution_lifecycle`] already builds; the VPC origin create is
//! [`crate::vpc_origin_lifecycle`]'s.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    MAX_ITEMS, MAX_ITEMS_INVALID_MESSAGE, VPC_ORIGIN_ID_PREFIX, VPC_ORIGIN_NOT_FOUND_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, emulator, get_distribution, is_distribution_id, list_distributions,
    Answer, Node, API_VERSION,
};
use crate::vpc_origin_lifecycle::{create_vpc_origin, load_balancer_arn};

/// One distribution under test: one entry per origin it submits, in order. `Some(index)` is an
/// origin fronting the VPC origin created at that index; `None` is a custom origin, which
/// fronts no VPC origin at all.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    origins: &'static [Option<usize>],
}

impl Row {
    /// Whether this distribution fronts the VPC origin at `index` through any of its origins —
    /// the rule the listing implements, computed here from the row rather than from the
    /// handler.
    fn fronts(&self, index: usize) -> bool {
        self.origins.contains(&Some(index))
    }
}

/// How many VPC origins the suite creates. The last one is named by nothing, so an empty
/// listing is covered, and index 2 is only ever named beside another VPC origin.
const VPC_ORIGIN_COUNT: usize = 4;

/// The distributions created for every case below.
const ROWS: &[Row] = &[
    Row {
        case: "the only origin fronts VPC origin 0",
        origins: &[Some(0)],
    },
    Row {
        case: "a second distribution whose only origin fronts VPC origin 0",
        origins: &[Some(0)],
    },
    Row {
        case: "a third distribution whose only origin fronts VPC origin 0",
        origins: &[Some(0)],
    },
    Row {
        case: "two origins of which the second fronts VPC origin 1",
        origins: &[None, Some(1)],
    },
    Row {
        case: "three origins of which the last fronts VPC origin 1",
        origins: &[None, None, Some(1)],
    },
    Row {
        case: "two origins fronting the SAME VPC origin 2",
        origins: &[Some(2), Some(2)],
    },
    Row {
        case: "two origins fronting two different VPC origins (1 and 2)",
        origins: &[Some(1), Some(2)],
    },
    Row {
        case: "a custom origin only, fronting no VPC origin at all",
        origins: &[None],
    },
];

/// `GET /2020-05-31/distributionsByVpcOriginId/{VpcOriginId}` plus the query the caller pages
/// with — the route the model binds this operation to.
async fn list_by_vpc_origin(
    router: &MultiServiceRouter,
    vpc_origin_id: &str,
    query: &str,
) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/distributionsByVpcOriginId/{vpc_origin_id}{query}"),
        None,
        None,
    )
    .await
}

/// The distribution ids of a `DistributionIdList` answer, in document order. Each item travels
/// in the `DistributionId` element the model's `xmlName` gives `DistributionIdListSummary`'s
/// member.
fn listed_ids(list: &Node) -> Vec<String> {
    list.child("Items")
        .map(|items| {
            items
                .children_named("DistributionId")
                .into_iter()
                .map(|item| item.text().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Create [`VPC_ORIGIN_COUNT`] VPC origins and report the id of each in creation order.
async fn create_vpc_origins(router: &MultiServiceRouter) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..VPC_ORIGIN_COUNT {
        let body = format!(
            "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <VpcOriginEndpointConfig>\
             <Name>by-vpc-origin-{index}</Name>\
             <Arn>{}</Arn>\
             <HTTPPort>80</HTTPPort>\
             <HTTPSPort>443</HTTPSPort>\
             <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
             </VpcOriginEndpointConfig>\
             </CreateVpcOriginRequest>",
            load_balancer_arn(&format!("by-vpc-origin-{index}"))
        );
        let answer = create_vpc_origin(router, &body).await;
        assert_eq!(
            answer.status, 202,
            "case VPC origin {index}: expected HTTP 202 from CreateVpcOrigin, got {} with {}",
            answer.status, answer.raw
        );
        let id = answer.root().find_text("Id").unwrap_or_default().to_string();
        assert!(
            id.starts_with(VPC_ORIGIN_ID_PREFIX) && id.len() > VPC_ORIGIN_ID_PREFIX.len(),
            "case VPC origin {index}: expected a minted VPC origin id, got {id:?} in {}",
            answer.raw
        );
        ids.push(id);
    }
    ids
}

/// The `DistributionConfig` a row submits: one origin per entry of the row — a VPC origin
/// where the entry names one, a custom origin otherwise — and a default cache behaviour
/// targeting the first of them.
fn config_xml(caller_reference: &str, row: &Row, vpc_origins: &[String]) -> String {
    let mut origins = String::new();
    for (position, slot) in row.origins.iter().enumerate() {
        // `Origin`'s members in the order the model declares them: the identity first, then
        // the one block that says WHAT kind of origin this is.
        let kind = match slot {
            Some(index) => format!(
                "<VpcOriginConfig><VpcOriginId>{}</VpcOriginId></VpcOriginConfig>",
                vpc_origins[*index]
            ),
            None => "<CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
                     <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
                     </CustomOriginConfig>"
                .to_string(),
        };
        origins.push_str(&format!(
            "<Origin><Id>origin-{position}</Id>\
             <DomainName>origin-{position}.{caller_reference}.example.com</DomainName>\
             {kind}</Origin>"
        ));
    }
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>{}</Quantity><Items>{origins}</Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-0</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         </DefaultCacheBehavior>\
         <Comment>{caller_reference}</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        row.origins.len()
    )
}

/// Create every VPC origin and every row, returning the VPC origin ids and the `(row, id)`
/// pairs.
async fn seed(router: &MultiServiceRouter) -> (Vec<String>, Vec<(&'static Row, String)>) {
    let vpc_origins = create_vpc_origins(router).await;
    let mut created = Vec::new();
    for (position, row) in ROWS.iter().enumerate() {
        let body = config_xml(&format!("ref-{position}"), row, &vpc_origins);
        let answer = create_distribution(router, &body).await;
        assert_eq!(
            answer.status, 201,
            "case {}: expected HTTP 201 from CreateDistribution, got {} with {}",
            row.case, answer.status, answer.raw
        );
        let id = answer.root().text_at("Id").unwrap_or_default().to_string();
        assert!(
            is_distribution_id(&id),
            "case {}: expected a minted distribution id, got {id:?}",
            row.case
        );
        created.push((row, id));
    }
    (vpc_origins, created)
}

/// The ids this suite expects under the VPC origin at `index`, in id order.
fn expected_ids(created: &[(&'static Row, String)], index: usize) -> Vec<String> {
    let mut ids: Vec<String> = created
        .iter()
        .filter(|(row, _)| row.fronts(index))
        .map(|(_, id)| id.clone())
        .collect();
    ids.sort();
    ids
}

/// Every distribution id in `ListDistributions`, in document order.
async fn listed_ids_of_distributions(router: &MultiServiceRouter) -> Vec<String> {
    let answer = list_distributions(router, "").await;
    assert_eq!(
        answer.status, 200,
        "case the unfiltered listing: expected HTTP 200, got {} with {}",
        answer.status, answer.raw
    );
    answer
        .root()
        .child("Items")
        .map(|items| {
            items
                .children_named("DistributionSummary")
                .into_iter()
                .map(|summary| summary.text_at("Id").unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Walk every page of one VPC origin, returning the ids in the order they were served.
async fn walk(
    router: &MultiServiceRouter,
    vpc_origin_id: &str,
    page_size: Option<i32>,
) -> Vec<String> {
    let label = match page_size {
        None => format!("{vpc_origin_id}/default"),
        Some(size) => format!("{vpc_origin_id}/MaxItems={size}"),
    };
    let mut collected: Vec<String> = Vec::new();
    let mut marker: Option<String> = None;
    // At most one page per distribution plus one, so a listing that never stops truncating
    // fails here rather than looping forever.
    for page in 0..=ROWS.len() + 1 {
        let query = match (page_size, marker.as_deref()) {
            (None, None) => String::new(),
            (None, Some(value)) => format!("?Marker={value}"),
            (Some(size), None) => format!("?MaxItems={size}"),
            (Some(size), Some(value)) => format!("?MaxItems={size}&Marker={value}"),
        };
        let answer = list_by_vpc_origin(router, vpc_origin_id, &query).await;
        assert_eq!(
            answer.status, 200,
            "case {label} page {page}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        let list = answer.root();
        // "Marker: the value provided in the Marker request field."
        assert_eq!(
            list.text_at("Marker"),
            Some(marker.as_deref().unwrap_or_default()),
            "case {label} page {page}: expected the Marker the request carried, got {:?}",
            list.text_at("Marker")
        );
        // "MaxItems: the maximum number of distribution IDs requested."
        assert_eq!(
            list.text_at("MaxItems"),
            Some(page_size.unwrap_or(MAX_ITEMS).to_string().as_str()),
            "case {label} page {page}: expected MaxItems to echo the page size, got {:?}",
            list.text_at("MaxItems")
        );
        let ids = listed_ids(list);
        assert_eq!(
            list.text_at("Quantity"),
            Some(ids.len().to_string().as_str()),
            "case {label} page {page}: expected Quantity to state the ids served ({}), got {:?}",
            ids.len(),
            list.text_at("Quantity")
        );
        if let Some(size) = page_size {
            assert!(
                i32::try_from(ids.len()).unwrap_or(i32::MAX) <= size.min(MAX_ITEMS),
                "case {label} page {page}: expected at most {} ids, got {}",
                size.min(MAX_ITEMS),
                ids.len()
            );
        }
        collected.extend(ids.iter().cloned());

        let next_marker = list.text_at("NextMarker").map(str::to_string);
        let is_truncated = list.text_at("IsTruncated").unwrap_or_default().to_string();
        match next_marker {
            Some(next) => {
                assert_eq!(
                    is_truncated, "true",
                    "case {label} page {page}: a NextMarker means IsTruncated true, got \
                     {is_truncated:?}"
                );
                assert_eq!(
                    ids.last().map(String::as_str),
                    Some(next.as_str()),
                    "case {label} page {page}: expected NextMarker to be the id this page ended \
                     at, got {next:?}"
                );
                marker = Some(next);
            }
            None => {
                assert_eq!(
                    is_truncated, "false",
                    "case {label} page {page}: no NextMarker means IsTruncated false, got \
                     {is_truncated:?}"
                );
                return collected;
            }
        }
    }
    panic!("case {label}: the listing never stopped truncating");
}

/// Each VPC origin lists exactly the distributions whose stored origins front it, once each and
/// in id order — and every id it lists is a distribution the store holds whose configuration
/// really names that VPC origin.
#[tokio::test]
async fn each_vpc_origin_lists_the_distributions_that_front_it() {
    let router = emulator();
    let (vpc_origins, created) = seed(&router).await;

    let all_ids = listed_ids_of_distributions(&router).await;

    for (index, vpc_origin_id) in vpc_origins.iter().enumerate() {
        let expected = expected_ids(&created, index);
        let answer = list_by_vpc_origin(&router, vpc_origin_id, "").await;
        assert_eq!(
            answer.status, 200,
            "case VPC origin {index}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        let ids = listed_ids(answer.root());
        assert_eq!(
            ids, expected,
            "case VPC origin {index}: expected the distributions that front it, in id order, got \
             {ids:?}"
        );

        for id in &ids {
            assert!(
                all_ids.contains(id),
                "case VPC origin {index}: expected {id} to be a distribution ListDistributions \
                 reports, got {all_ids:?}"
            );
            // The listing answers from the stored configuration: read the distribution back and
            // find the VPC origin among its origins.
            let fetched = get_distribution(&router, id).await;
            assert_eq!(
                fetched.status, 200,
                "case VPC origin {index}: expected HTTP 200 from GetDistribution for {id}, got \
                 {} with {}",
                fetched.status, fetched.raw
            );
            assert!(
                fetched
                    .raw
                    .contains(&format!("<VpcOriginId>{vpc_origin_id}</VpcOriginId>")),
                "case VPC origin {index}: expected the stored configuration of {id} to name the \
                 VPC origin, got {}",
                fetched.raw
            );
        }

        // The listing REPORTS: asking twice answers the same page, so nothing about the store
        // moved because it was read.
        let again = list_by_vpc_origin(&router, vpc_origin_id, "").await;
        assert_eq!(
            listed_ids(again.root()),
            ids,
            "case VPC origin {index}: expected the same page on a second read, got {}",
            again.raw
        );
    }
}

/// Walking the pages at ANY admissible page size enumerates that VPC origin's distributions
/// exactly once, in id order.
#[tokio::test]
async fn walking_the_pages_enumerates_each_vpc_origin_exactly_once() {
    let router = emulator();
    let (vpc_origins, created) = seed(&router).await;

    for (index, vpc_origin_id) in vpc_origins.iter().enumerate() {
        let expected = expected_ids(&created, index);
        for page_size in [None, Some(1), Some(2), Some(3), Some(MAX_ITEMS), Some(i32::MAX)] {
            let collected = walk(&router, vpc_origin_id, page_size).await;
            assert_eq!(
                collected, expected,
                "case VPC origin {index} page size {page_size:?}: expected every distribution \
                 that fronts it exactly once in id order, got {collected:?}"
            );
        }
    }
}

/// A VPC origin no distribution fronts lists nothing — an `Items` element carrying no id,
/// `Quantity` zero and no `NextMarker` — rather than falling back to the whole store.
#[tokio::test]
async fn a_vpc_origin_nothing_fronts_lists_nothing() {
    let router = emulator();
    let (vpc_origins, created) = seed(&router).await;

    let unused = vpc_origins
        .iter()
        .enumerate()
        .find(|(index, _)| expected_ids(&created, *index).is_empty())
        .map(|(_, id)| id.clone())
        .expect("the table must cover a VPC origin no distribution fronts");

    let answer = list_by_vpc_origin(&router, &unused, "").await;
    assert_eq!(
        answer.status, 200,
        "case the unused VPC origin: expected HTTP 200, got {} with {}",
        answer.status, answer.raw
    );
    let list = answer.root();
    assert_eq!(
        list.text_at("Quantity"),
        Some("0"),
        "case the unused VPC origin: expected Quantity 0, got {:?}",
        list.text_at("Quantity")
    );
    assert!(
        listed_ids(list).is_empty(),
        "case the unused VPC origin: expected no distribution id, got {:?}",
        listed_ids(list)
    );
    // The element is present and empty rather than absent, so every page's ids are read the
    // same way.
    assert!(
        list.child("Items").is_some(),
        "case the unused VPC origin: expected an empty Items element, got {}",
        answer.raw
    );
    assert_eq!(
        list.text_at("IsTruncated"),
        Some("false"),
        "case the unused VPC origin: expected IsTruncated false, got {:?}",
        list.text_at("IsTruncated")
    );
    assert!(
        list.text_at("NextMarker").is_none(),
        "case the unused VPC origin: expected no NextMarker, got {:?}",
        list.text_at("NextMarker")
    );
}

/// An id the store holds no VPC origin for is `EntityNotFound` — never an empty page, which
/// would claim the VPC origin exists. The rows differ along the BODY of an identifier of the
/// VPC origin namespace, since a value of any OTHER form never named a VPC origin at all and
/// is the argument refusal instead ([`crate::vpc_origin_argument_validation`]).
#[tokio::test]
async fn an_id_no_vpc_origin_holds_is_refused() {
    let router = emulator();
    let (vpc_origins, _) = seed(&router).await;
    let known = vpc_origins.first().cloned().expect("a created VPC origin");

    let mut flipped: Vec<char> = known.chars().collect();
    let last = flipped.len() - 1;
    flipped[last] = if flipped[last] == 'A' { 'B' } else { 'A' };
    let absent: [(&str, String); 3] = [
        (
            "the minted namespace with a body of digits nothing holds",
            format!("{VPC_ORIGIN_ID_PREFIX}0000000000000"),
        ),
        (
            "a known id with one character changed",
            flipped.into_iter().collect(),
        ),
        (
            "the minted namespace with a body of letters nothing holds",
            format!("{VPC_ORIGIN_ID_PREFIX}ABSENTABSENTX"),
        ),
    ];

    for (case, candidate) in absent {
        let answer = list_by_vpc_origin(&router, &candidate, "").await;
        assert_eq!(
            answer.status, 404,
            "case {case}: expected HTTP 404 for {candidate:?}, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "EntityNotFound",
            "case {case}: expected EntityNotFound for {candidate:?}, got {}",
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message").unwrap_or_default(),
            VPC_ORIGIN_NOT_FOUND_MESSAGE,
            "case {case}: expected the VPC origin absent sentence for {candidate:?}, got {}",
            answer.raw
        );
    }
}

/// A `MaxItems` no page size can be taken from is `InvalidArgument` with the one sentence this
/// service states for that constraint, and the refusal stands in front of the VPC origin
/// lookup's own answer.
#[tokio::test]
async fn a_max_items_no_page_can_be_taken_from_is_refused() {
    let router = emulator();
    let (vpc_origins, _) = seed(&router).await;
    let known = vpc_origins.first().cloned().expect("a created VPC origin");

    for (case, value) in [
        ("one below zero", "-1".to_string()),
        ("far below zero", i32::MIN.to_string()),
        ("no integer at all", "many".to_string()),
    ] {
        let answer = list_by_vpc_origin(&router, &known, &format!("?MaxItems={value}")).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400 for MaxItems={value}, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message").unwrap_or_default(),
            MAX_ITEMS_INVALID_MESSAGE,
            "case {case}: expected the MaxItems constraint sentence, got {}",
            answer.raw
        );
    }
}
