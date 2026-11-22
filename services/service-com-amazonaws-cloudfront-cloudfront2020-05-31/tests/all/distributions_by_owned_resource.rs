// SPDX-License-Identifier: CC0-1.0

//! `ListDistributionsByOwnedResource`, end to end over HTTP.
//!
//! The rule under test: the listing of the distributions associated with a resource this
//! account owns answers from the SAME stored association the resource's own by-resource
//! listing serves, addressed by the resource's ARN instead of by its id — a distribution is
//! listed exactly when one of the origins of its STORED configuration names that resource,
//! each matching distribution listed ONCE and in id order, paired with the account that owns
//! the distribution; the page contract is the one every CloudFront distribution listing states
//! (`MaxItems` bounds the page and is echoed, `Marker` resumes after the id the previous page
//! ended at, `NextMarker` and `IsTruncated` arrive exactly when matching distributions remain,
//! `Quantity` states the length of `Items`); and the `ResourceArn` is read the way the whole
//! cross-account sharing surface reads that member — a value that is no ARN at all is refused
//! on its FORM, an ARN naming another resource type on the TYPE it names, and an ARN of the
//! shareable type naming no resource this store holds is `EntityNotFound` (HTTP 404) rather
//! than a page of nothing.
//!
//! The rows below span what that rule generalises over: WHICH origin of a distribution names
//! the shared resource (the only one, the second of two, the last of three), how many origins
//! of ONE distribution name the SAME resource (so a distribution is still listed once), how
//! many distinct shared resources one distribution uses, how many distributions share one
//! resource (so a page can cut inside a matching set), a distribution using no shared resource
//! at all, a shared resource no distribution uses, and whether a resource policy has been
//! attached to the resource (the index is the association, not the share). The refusal table's
//! rows differ along the FORM of the `ResourceArn` (no ARN at all, an ARN of another resource
//! type — held and unheld — and an ARN of the shareable type naming nothing, in this account
//! and in another).
//!
//! The wire harness (the router, the request helpers and the element scanner) is the one
//! [`crate::distribution_lifecycle`] already builds; the VPC origin create and the
//! resource-policy writer are the sibling suites' own.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    INVALID_RESOURCE_POLICY_ARN_MESSAGE, MALFORMED_ARN_MESSAGE, MAX_ITEMS,
    MAX_ITEMS_INVALID_MESSAGE, NO_SUCH_RESOURCE_MESSAGE, RESOURCE_POLICY_RESOURCE_TYPE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, emulator, get_distribution, is_distribution_id, list_distributions,
    Answer, Node, ACCOUNT_ID, API_VERSION,
};
use crate::distributions_by_web_acl_id::encode_label;
use crate::resource_policy_lifecycle::{
    created_vpc_origin_arn, policy, put_resource_policy, OTHER_RESOURCE_TYPES,
};

/// The sentence this service refuses a distribution listing's unusable `Marker` with, written
/// out as a literal so a change to the service's own constant fails this suite.
const INVALID_MARKER_MESSAGE: &str = "Marker must be a valid Distribution Id";

/// One distribution under test: one entry per origin it submits, in order. `Some(index)` is an
/// origin fronting the shared resource created at that index; `None` is a custom origin, which
/// uses no shared resource at all.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    origins: &'static [Option<usize>],
}

impl Row {
    /// Whether this distribution uses the shared resource at `index` through any of its
    /// origins — the rule the listing implements, computed here from the row rather than from
    /// the handler.
    fn uses(&self, index: usize) -> bool {
        self.origins.contains(&Some(index))
    }
}

/// How many shared resources the suite creates. The last one is used by nothing, so an empty
/// listing is covered, and index 2 is only ever named beside another resource.
const RESOURCE_COUNT: usize = 4;

/// The distributions created for every case below.
const ROWS: &[Row] = &[
    Row {
        case: "the only origin uses shared resource 0",
        origins: &[Some(0)],
    },
    Row {
        case: "a second distribution whose only origin uses shared resource 0",
        origins: &[Some(0)],
    },
    Row {
        case: "a third distribution whose only origin uses shared resource 0",
        origins: &[Some(0)],
    },
    Row {
        case: "two origins of which the second uses shared resource 1",
        origins: &[None, Some(1)],
    },
    Row {
        case: "three origins of which the last uses shared resource 1",
        origins: &[None, None, Some(1)],
    },
    Row {
        case: "two origins using the SAME shared resource 2",
        origins: &[Some(2), Some(2)],
    },
    Row {
        case: "two origins using two different shared resources (1 and 2)",
        origins: &[Some(1), Some(2)],
    },
    Row {
        case: "a custom origin only, using no shared resource at all",
        origins: &[None],
    },
];

/// One shared resource under test: the ARN its create minted and the id the store holds it
/// under, which is that ARN's own last segment.
#[derive(Debug, Clone)]
struct Shared {
    arn: String,
    id: String,
}

/// `GET /2020-05-31/distributionsByOwnedResource/{ResourceArn}` plus the query the caller
/// pages with — the route the model binds this operation to. The ARN travels as ONE path
/// label, so its separators are percent-encoded the way an SDK client encodes them.
async fn list_by_owned_resource(
    router: &MultiServiceRouter,
    resource_arn: &str,
    query: &str,
) -> Answer {
    call(
        router,
        "GET",
        &format!(
            "/{API_VERSION}/distributionsByOwnedResource/{}{query}",
            encode_label(resource_arn)
        ),
        None,
        None,
    )
    .await
}

/// `GET /2020-05-31/distributionsByVpcOriginId/{VpcOriginId}` — the resource's OWN by-id
/// listing, which this suite holds the owned-resource listing against.
async fn list_by_vpc_origin_id(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/distributionsByVpcOriginId/{id}"),
        None,
        None,
    )
    .await
}

/// The `(DistributionId, OwnerAccountId)` pairs a `DistributionIdOwnerList` answer carries, in
/// document order. Each entry travels in the `DistributionIdOwner` element the model's
/// `xmlName` gives `DistributionIdOwnerItemList`'s member.
fn listed_pairs(list: &Node) -> Vec<(String, String)> {
    list.child("Items")
        .map(|items| {
            items
                .children_named("DistributionIdOwner")
                .into_iter()
                .map(|item| {
                    (
                        item.text_at("DistributionId").unwrap_or_default().to_string(),
                        item.text_at("OwnerAccountId")
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Just the distribution ids of a `DistributionIdOwnerList` answer, in document order.
fn listed_ids(list: &Node) -> Vec<String> {
    listed_pairs(list)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// The distribution ids a `DistributionIdList` answer carries, in document order — the shape
/// the resource's own by-id listing serves.
fn listed_ids_by_id_listing(list: &Node) -> Vec<String> {
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

/// Create [`RESOURCE_COUNT`] shared resources and report each in creation order.
async fn create_shared_resources(router: &MultiServiceRouter) -> Vec<Shared> {
    let mut created = Vec::new();
    for index in 0..RESOURCE_COUNT {
        let arn = created_vpc_origin_arn(router, &format!("by-owned-resource-{index}")).await;
        let id = arn
            .rsplit_once('/')
            .map(|(_, id)| id.to_string())
            .unwrap_or_default();
        assert!(
            !id.is_empty() && arn.ends_with(&format!("{RESOURCE_POLICY_RESOURCE_TYPE}/{id}")),
            "case shared resource {index}: expected the minted ARN to end in the resource id, \
             got {arn:?}"
        );
        created.push(Shared { arn, id });
    }
    created
}

/// The `DistributionConfig` a row submits: one origin per entry of the row — a VPC origin
/// where the entry names one, a custom origin otherwise — and a default cache behaviour
/// targeting the first of them.
fn config_xml(caller_reference: &str, row: &Row, shared: &[Shared]) -> String {
    let mut origins = String::new();
    for (position, slot) in row.origins.iter().enumerate() {
        // `Origin`'s members in the order the model declares them: the identity first, then
        // the one block that says WHAT kind of origin this is.
        let kind = match slot {
            Some(index) => format!(
                "<VpcOriginConfig><VpcOriginId>{}</VpcOriginId></VpcOriginConfig>",
                shared[*index].id
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

/// Create every shared resource and every row, returning the resources and the `(row, id)`
/// pairs.
async fn seed(router: &MultiServiceRouter) -> (Vec<Shared>, Vec<(&'static Row, String)>) {
    let shared = create_shared_resources(router).await;
    let mut created = Vec::new();
    for (position, row) in ROWS.iter().enumerate() {
        let body = config_xml(&format!("owned-ref-{position}"), row, &shared);
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
    (shared, created)
}

/// The pairs this suite expects under the shared resource at `index`, in id order: every
/// distribution that uses it, owned by the one account this emulator serves.
fn expected_pairs(created: &[(&'static Row, String)], index: usize) -> Vec<(String, String)> {
    let mut ids: Vec<String> = created
        .iter()
        .filter(|(row, _)| row.uses(index))
        .map(|(_, id)| id.clone())
        .collect();
    ids.sort();
    ids.into_iter()
        .map(|id| (id, ACCOUNT_ID.to_string()))
        .collect()
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

/// Walk every page of one shared resource, returning the ids in the order they were served.
async fn walk(
    router: &MultiServiceRouter,
    resource_arn: &str,
    page_size: Option<i32>,
) -> Vec<String> {
    let label = match page_size {
        None => format!("{resource_arn}/default"),
        Some(size) => format!("{resource_arn}/MaxItems={size}"),
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
        let answer = list_by_owned_resource(router, resource_arn, &query).await;
        assert_eq!(
            answer.status, 200,
            "case {label} page {page}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        let list = answer.root();
        // "Marker: use this field when paginating results to indicate where to begin" — the
        // value the request carried is echoed.
        assert_eq!(
            list.text_at("Marker"),
            Some(marker.as_deref().unwrap_or_default()),
            "case {label} page {page}: expected the Marker the request carried, got {:?}",
            list.text_at("Marker")
        );
        // "MaxItems: the maximum number of DistributionIdOwner objects to return."
        assert_eq!(
            list.text_at("MaxItems"),
            Some(page_size.unwrap_or(MAX_ITEMS).to_string().as_str()),
            "case {label} page {page}: expected MaxItems to echo the page size, got {:?}",
            list.text_at("MaxItems")
        );
        let pairs = listed_pairs(list);
        assert_eq!(
            list.text_at("Quantity"),
            Some(pairs.len().to_string().as_str()),
            "case {label} page {page}: expected Quantity to state the entries served ({}), got \
             {:?}",
            pairs.len(),
            list.text_at("Quantity")
        );
        if let Some(size) = page_size {
            assert!(
                i32::try_from(pairs.len()).unwrap_or(i32::MAX) <= size.min(MAX_ITEMS),
                "case {label} page {page}: expected at most {} entries, got {}",
                size.min(MAX_ITEMS),
                pairs.len()
            );
        }
        collected.extend(pairs.iter().map(|(id, _)| id.clone()));

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
                    collected.last().map(String::as_str),
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

/// Each shared resource lists exactly the distributions whose stored origins use it, once each
/// and in id order, each paired with the account that owns the distribution — and every id it
/// lists is a distribution the store holds whose configuration really names that resource.
#[tokio::test]
async fn each_owned_resource_lists_the_distributions_that_use_it() {
    let router = emulator();
    let (shared, created) = seed(&router).await;

    let all_ids = listed_ids_of_distributions(&router).await;

    for (index, resource) in shared.iter().enumerate() {
        let expected = expected_pairs(&created, index);
        let answer = list_by_owned_resource(&router, &resource.arn, "").await;
        assert_eq!(
            answer.status, 200,
            "case shared resource {index}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        let pairs = listed_pairs(answer.root());
        assert_eq!(
            pairs, expected,
            "case shared resource {index}: expected the distributions that use it with their \
             owner, in id order, got {pairs:?}"
        );

        for (id, _) in &pairs {
            assert!(
                all_ids.contains(id),
                "case shared resource {index}: expected {id} to be a distribution \
                 ListDistributions reports, got {all_ids:?}"
            );
            // The listing answers from the stored configuration: read the distribution back
            // and find the shared resource among its origins.
            let fetched = get_distribution(&router, id).await;
            assert_eq!(
                fetched.status, 200,
                "case shared resource {index}: expected HTTP 200 from GetDistribution for {id}, \
                 got {} with {}",
                fetched.status, fetched.raw
            );
            assert!(
                fetched
                    .raw
                    .contains(&format!("<VpcOriginId>{}</VpcOriginId>", resource.id)),
                "case shared resource {index}: expected the stored configuration of {id} to name \
                 the resource, got {}",
                fetched.raw
            );
        }

        // The listing REPORTS: asking twice answers the same page, so nothing about the store
        // moved because it was read.
        let again = list_by_owned_resource(&router, &resource.arn, "").await;
        assert_eq!(
            listed_pairs(again.root()),
            pairs,
            "case shared resource {index}: expected the same page on a second read, got {}",
            again.raw
        );
    }
}

/// The owned-resource listing and the resource's OWN by-id listing serve the same index: the
/// ids each reports for a resource are the same ids in the same order, whichever identifier
/// the caller addressed the resource by.
#[tokio::test]
async fn the_listing_agrees_with_the_resources_own_by_id_listing() {
    let router = emulator();
    let (shared, _) = seed(&router).await;

    for (index, resource) in shared.iter().enumerate() {
        let by_arn = list_by_owned_resource(&router, &resource.arn, "").await;
        assert_eq!(
            by_arn.status, 200,
            "case shared resource {index}: expected HTTP 200 from the owned-resource listing, \
             got {} with {}",
            by_arn.status, by_arn.raw
        );
        let by_id = list_by_vpc_origin_id(&router, &resource.id).await;
        assert_eq!(
            by_id.status, 200,
            "case shared resource {index}: expected HTTP 200 from the by-id listing, got {} \
             with {}",
            by_id.status, by_id.raw
        );
        assert_eq!(
            listed_ids(by_arn.root()),
            listed_ids_by_id_listing(by_id.root()),
            "case shared resource {index}: expected the two listings of one resource to agree, \
             got {} and {}",
            by_arn.raw,
            by_id.raw
        );
    }
}

/// Walking the pages at ANY admissible page size enumerates that resource's distributions
/// exactly once, in id order.
#[tokio::test]
async fn walking_the_pages_enumerates_each_owned_resource_exactly_once() {
    let router = emulator();
    let (shared, created) = seed(&router).await;

    for (index, resource) in shared.iter().enumerate() {
        let expected: Vec<String> = expected_pairs(&created, index)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        for page_size in [None, Some(1), Some(2), Some(3), Some(MAX_ITEMS), Some(i32::MAX)] {
            let collected = walk(&router, &resource.arn, page_size).await;
            assert_eq!(
                collected, expected,
                "case shared resource {index} page size {page_size:?}: expected every \
                 distribution that uses it exactly once in id order, got {collected:?}"
            );
        }
    }
}

/// A shared resource no distribution uses lists nothing — an `Items` element carrying no
/// entry, `Quantity` zero and no `NextMarker` — rather than falling back to the whole store.
#[tokio::test]
async fn an_owned_resource_nothing_uses_lists_nothing() {
    let router = emulator();
    let (shared, created) = seed(&router).await;

    let unused = shared
        .iter()
        .enumerate()
        .find(|(index, _)| expected_pairs(&created, *index).is_empty())
        .map(|(_, resource)| resource.clone())
        .expect("the table must cover a shared resource no distribution uses");

    let answer = list_by_owned_resource(&router, &unused.arn, "").await;
    assert_eq!(
        answer.status, 200,
        "case the unused resource: expected HTTP 200, got {} with {}",
        answer.status, answer.raw
    );
    let list = answer.root();
    assert_eq!(
        list.text_at("Quantity"),
        Some("0"),
        "case the unused resource: expected Quantity 0, got {:?}",
        list.text_at("Quantity")
    );
    assert!(
        listed_pairs(list).is_empty(),
        "case the unused resource: expected no entry, got {:?}",
        listed_pairs(list)
    );
    // The element is present and empty rather than absent, so every page's entries are read
    // the same way.
    assert!(
        list.child("Items").is_some(),
        "case the unused resource: expected an empty Items element, got {}",
        answer.raw
    );
    assert_eq!(
        list.text_at("IsTruncated"),
        Some("false"),
        "case the unused resource: expected IsTruncated false, got {:?}",
        list.text_at("IsTruncated")
    );
    assert!(
        list.text_at("NextMarker").is_none(),
        "case the unused resource: expected no NextMarker, got {:?}",
        list.text_at("NextMarker")
    );
}

/// The index is the ASSOCIATION, not the share: attaching a resource policy to the resource
/// leaves the page it lists unchanged, and so does attaching one to a resource nothing uses.
#[tokio::test]
async fn attaching_a_resource_policy_does_not_change_the_page() {
    let router = emulator();
    let (shared, created) = seed(&router).await;

    for (index, resource) in shared.iter().enumerate() {
        let before = list_by_owned_resource(&router, &resource.arn, "").await;
        let expected = expected_pairs(&created, index);
        assert_eq!(
            listed_pairs(before.root()),
            expected,
            "case shared resource {index}: expected the association page before the share, got {}",
            before.raw
        );

        let attached = put_resource_policy(
            &router,
            &resource.arn,
            &policy(&resource.arn, &format!("owned-resource-{index}")),
        )
        .await;
        assert_eq!(
            attached.status, 200,
            "case shared resource {index}: expected the policy to be attached, got {} with {}",
            attached.status, attached.raw
        );

        let after = list_by_owned_resource(&router, &resource.arn, "").await;
        assert_eq!(
            listed_pairs(after.root()),
            expected,
            "case shared resource {index}: expected the same page after the share, got {}",
            after.raw
        );
    }
}

/// A `ResourceArn` the sharing surface cannot act on is refused as that surface refuses one: a
/// value that is no ARN at all on its FORM, an ARN naming another resource type on the TYPE it
/// names, and an ARN of the shareable type naming no resource this store holds as absent.
#[tokio::test]
async fn a_resource_arn_the_surface_cannot_act_on_is_refused() {
    let router = emulator();
    let (shared, created) = seed(&router).await;
    let held = shared.first().cloned().expect("a created shared resource");
    let held_distribution = created
        .first()
        .map(|(_, id)| id.clone())
        .expect("a created distribution");
    let distribution_arn = format!("arn:aws:cloudfront::{ACCOUNT_ID}:distribution/{held_distribution}");

    // (case, the ResourceArn submitted, the status expected, the sentence expected)
    let mut rows: Vec<(String, String, u16, &str)> = vec![
        (
            "no ARN at all: the resource's bare id".to_string(),
            held.id.clone(),
            400,
            MALFORMED_ARN_MESSAGE,
        ),
        (
            "no ARN at all: another word".to_string(),
            "not-an-arn".to_string(),
            400,
            MALFORMED_ARN_MESSAGE,
        ),
        (
            "the ARN of a distribution that exists".to_string(),
            distribution_arn,
            400,
            INVALID_RESOURCE_POLICY_ARN_MESSAGE,
        ),
        (
            "an ARN of another service".to_string(),
            format!("arn:aws:s3:::bucket-{ACCOUNT_ID}"),
            400,
            INVALID_RESOURCE_POLICY_ARN_MESSAGE,
        ),
        (
            "the shareable type with a body nothing holds".to_string(),
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo_ABSENT"),
            404,
            NO_SUCH_RESOURCE_MESSAGE,
        ),
        (
            "a held resource's id under ANOTHER account".to_string(),
            format!(
                "arn:aws:cloudfront::111122223333:{RESOURCE_POLICY_RESOURCE_TYPE}/{}",
                held.id
            ),
            404,
            NO_SUCH_RESOURCE_MESSAGE,
        ),
    ];
    // Every other CloudFront resource type this service addresses by a route of its own: an
    // ARN naming one is refused on the type it names, whether or not such a resource exists.
    rows.extend(OTHER_RESOURCE_TYPES.iter().map(|kind| {
        (
            format!("an ARN naming a {kind}"),
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:{kind}/ABCDEF1234567"),
            400,
            INVALID_RESOURCE_POLICY_ARN_MESSAGE,
        )
    }));

    for (case, resource_arn, status, sentence) in rows {
        let answer = list_by_owned_resource(&router, &resource_arn, "").await;
        assert_eq!(
            answer.status, status,
            "case {case}: expected HTTP {status} for {resource_arn:?}, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            if status == 404 {
                "EntityNotFound"
            } else {
                "InvalidArgument"
            },
            "case {case}: expected the modelled code for {resource_arn:?}, got {}",
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message").unwrap_or_default(),
            sentence,
            "case {case}: expected the sentence for {resource_arn:?}, got {}",
            answer.raw
        );
    }
}

/// A page parameter no page can be taken from is `InvalidArgument` with the one sentence this
/// service states for that constraint: a negative `MaxItems`, and a `Marker` that is no
/// distribution id this listing ever handed out.
#[tokio::test]
async fn a_page_parameter_no_page_can_be_taken_from_is_refused() {
    let router = emulator();
    let (shared, _) = seed(&router).await;
    let held = shared.first().cloned().expect("a created shared resource");

    for (case, value) in [
        ("one below zero", "-1".to_string()),
        ("far below zero", i32::MIN.to_string()),
        ("no integer at all", "many".to_string()),
    ] {
        let answer = list_by_owned_resource(&router, &held.arn, &format!("?MaxItems={value}")).await;
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

    for (case, value) in [
        ("another word", "not-an-id"),
        ("a bare number", "12345"),
        ("a resource id of another kind", "E1234567890ABC-no"),
    ] {
        let answer = list_by_owned_resource(&router, &held.arn, &format!("?Marker={value}")).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400 for Marker={value}, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message").unwrap_or_default(),
            INVALID_MARKER_MESSAGE,
            "case {case}: expected the Marker constraint sentence, got {}",
            answer.raw
        );
    }
}
