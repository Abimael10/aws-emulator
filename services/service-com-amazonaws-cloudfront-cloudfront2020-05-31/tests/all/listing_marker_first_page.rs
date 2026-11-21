// SPDX-License-Identifier: CC0-1.0

//! What a listing's `Marker` names, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule under test: a `Marker` is the identifier the previous page ended at,
//! so a request carrying NO marker and a request carrying the EMPTY value both ask for the
//! FIRST page — the empty value is what every one of these listings echoes back as `Marker`
//! for a first page, so a client that pages by echoing it is paging with a resume point rather
//! than with a value this service never issued — and a marker the service itself minted
//! resumes the listing after the item it names, so walking the pages from the empty value
//! enumerates every item exactly once, in one order.
//!
//! The rows span what the rule generalises over: the KIND of identifier a listing's marker is
//! (an `E`-prefixed distribution id, a `P`-prefixed profile id, a `K`-prefixed public key id,
//! the UUID of a key group, a key value store's NAME, and the id of an invalidation, whose
//! page is cut inside an order the store does not hold the records in), whether the listing is
//! over the whole account or SCOPED to one resource (the distributions associated with one key
//! group), and the page size paged at (the listing's own default, a size the collection fits
//! in, and one item at a time so every page but the last is truncated).
//!
//! The harness (the router, the request helpers and the element scanner) is
//! [`crate::distribution_lifecycle`]'s, and every resource is created through the surface that
//! owns it with the sibling suites' request builders rather than through copies of them.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    batch_xml, call, config, create_distribution, create_invalidation, emulator, Answer, Node,
    API_VERSION,
};
use crate::field_level_encryption_profile_lifecycle::{create_profile, Profile};
use crate::key_group_lifecycle::{create_group, mint_public_keys};
use crate::key_value_store_lifecycle::{create_key_value_store, Create};

/// How many items of each kind the suite seeds: more than one, so a page can be cut inside the
/// collection, and a count every listing below can hold.
const SEEDED: usize = 3;

/// One listing under test.
struct Listing {
    case: &'static str,
    /// The path the listing is served at, without a query.
    path: String,
    /// The element one item of a page travels in.
    item: &'static str,
    /// How many items the seeded store holds for this listing.
    count: usize,
}

/// `GET path + query`, the way a client pages a CloudFront listing.
async fn list(router: &MultiServiceRouter, path: &str, query: &str) -> Answer {
    call(router, "GET", &format!("{path}{query}"), None, None).await
}

/// The items one page carries, each as the identity it reports: its `Id`, or — for a listing
/// whose item IS an identifier — the text of the element itself.
fn page_items(page: &Answer, item: &str) -> Vec<String> {
    page.root()
        .at("Items")
        .map(|items| {
            items
                .children_named(item)
                .into_iter()
                .map(identity)
                .collect()
        })
        .unwrap_or_default()
}

/// What one item of a page is identified by.
fn identity(item: &Node) -> String {
    item.find_text("Id")
        .unwrap_or_else(|| item.text())
        .to_string()
}

/// Seed the store and report every listing to run the rule over.
async fn seed(router: &MultiServiceRouter) -> Vec<Listing> {
    // Public keys, and one key group per key: the marker of one listing is a `K`-prefixed id
    // and of the other a UUID.
    let keys = mint_public_keys(router, "marker", SEEDED).await;
    let mut groups: Vec<String> = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let body = format!(
            "<KeyGroupConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>marker-group-{index}</Name>\
             <Items><PublicKey>{key}</PublicKey></Items></KeyGroupConfig>"
        );
        let answer = create_group(router, &body).await;
        assert_eq!(
            answer.status, 201,
            "case marker-group-{index}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
            answer.status, answer.raw
        );
        groups.push(
            answer
                .root()
                .find_text("Id")
                .unwrap_or_default()
                .to_string(),
        );
    }

    // Field-level encryption profiles, whose marker is a `P`-prefixed id.
    for index in 0..SEEDED {
        let profile = Profile::new(
            "a profile a listing pages over",
            &format!("marker-profile-{index}"),
            None,
            Vec::new(),
        );
        let answer = create_profile(router, &profile.xml()).await;
        assert_eq!(
            answer.status, 201,
            "case marker-profile-{index}: expected HTTP 201 from \
             CreateFieldLevelEncryptionProfile, got {} with {}",
            answer.status, answer.raw
        );
    }

    // Key value stores, whose marker is a NAME rather than an id.
    for index in 0..SEEDED {
        let create = Create {
            case: "a store a listing pages over",
            name: format!("marker-store-{index}"),
            comment: None,
            tags: Vec::new(),
        };
        let answer = create_key_value_store(router, &create.xml()).await;
        assert_eq!(
            answer.status, 201,
            "case marker-store-{index}: expected HTTP 201 from CreateKeyValueStore, got {} \
             with {}",
            answer.status, answer.raw
        );
    }

    // Distributions, every one of them trusting the first key group, so the listing SCOPED to
    // that group pages over the same set the unfiltered one does.
    let mut distributions: Vec<String> = Vec::new();
    for index in 0..SEEDED {
        let body = distribution_xml(&format!("marker-distribution-{index}"), &groups[0]);
        let answer = create_distribution(router, &body).await;
        assert_eq!(
            answer.status, 201,
            "case marker-distribution-{index}: expected HTTP 201 from CreateDistribution, got \
             {} with {}",
            answer.status, answer.raw
        );
        distributions.push(
            answer
                .root()
                .text_at("Id")
                .unwrap_or_default()
                .to_string(),
        );
    }

    // Invalidations of the first distribution, whose listing serves the most recent first —
    // an order the records are not held in, so its marker is a position in that order rather
    // than a range bound.
    let owner = distributions[0].clone();
    for index in 0..SEEDED {
        let batch = batch_xml(
            &format!("marker-batch-{index}"),
            &[&format!("/object-{index}.html")],
        );
        let answer = create_invalidation(router, &owner, &batch).await;
        assert_eq!(
            answer.status, 201,
            "case marker-batch-{index}: expected HTTP 201 from CreateInvalidation, got {} \
             with {}",
            answer.status, answer.raw
        );
    }

    vec![
        Listing {
            case: "the distributions of the account",
            path: format!("/{API_VERSION}/distribution"),
            item: "DistributionSummary",
            count: SEEDED,
        },
        Listing {
            case: "the distributions associated with one key group",
            path: format!("/{API_VERSION}/distributionsByKeyGroupId/{}", groups[0]),
            item: "DistributionId",
            count: SEEDED,
        },
        Listing {
            case: "the field-level encryption profiles of the account",
            path: format!("/{API_VERSION}/field-level-encryption-profile"),
            item: "FieldLevelEncryptionProfileSummary",
            count: SEEDED,
        },
        Listing {
            case: "the key groups of the account",
            path: format!("/{API_VERSION}/key-group"),
            item: "KeyGroupSummary",
            count: SEEDED,
        },
        Listing {
            case: "the public keys of the account",
            path: format!("/{API_VERSION}/public-key"),
            item: "PublicKeySummary",
            count: SEEDED,
        },
        Listing {
            case: "the key value stores of the account",
            path: format!("/{API_VERSION}/key-value-store"),
            item: "KeyValueStore",
            count: SEEDED,
        },
        Listing {
            case: "the invalidations of one distribution",
            path: format!("/{API_VERSION}/distribution/{owner}/invalidation"),
            item: "InvalidationSummary",
            count: SEEDED,
        },
    ]
}

/// A distribution configuration whose default cache behaviour trusts `key_group`.
fn distribution_xml(reference: &str, key_group: &str) -> String {
    let submitted = config("a distribution a listing pages over", reference);
    submitted.xml().replace(
        "<ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>",
        &format!(
            "<ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <TrustedKeyGroups><Enabled>true</Enabled><Quantity>1</Quantity>\
             <Items><KeyGroup>{key_group}</KeyGroup></Items></TrustedKeyGroups>"
        ),
    )
}

/// An EMPTY `Marker` names no resume point, so the listing answers the first page — the same
/// answer, element for element, as the request that carries no `Marker` at all, at every page
/// size.
#[tokio::test]
async fn an_empty_marker_names_the_first_page() {
    let router = emulator();
    let listings = seed(&router).await;

    // (what the request asks for besides the marker, how many items the page holds)
    let sizes: [(&str, usize); 3] = [
        ("", SEEDED),
        ("MaxItems=2", 2),
        ("MaxItems=100", SEEDED),
    ];

    for listing in &listings {
        for (size, expected) in sizes {
            let (without, with) = match size {
                "" => (String::new(), "?Marker=".to_string()),
                size => (format!("?{size}"), format!("?{size}&Marker=")),
            };
            let absent = list(&router, &listing.path, &without).await;
            let empty = list(&router, &listing.path, &with).await;
            let label = format!("{} / {size:?}", listing.case);

            assert_eq!(
                empty.status, 200,
                "case {label}: expected an empty Marker to be served, got {} with {}",
                empty.status, empty.raw
            );
            assert_eq!(
                absent.status, 200,
                "case {label}: expected a listing with no Marker to be served, got {} with {}",
                absent.status, absent.raw
            );
            let items = page_items(&empty, listing.item);
            let served = page_items(&absent, listing.item);
            assert_eq!(
                items, served,
                "case {label}: expected an empty Marker to answer the page no Marker answers, \
                 got {items:?} instead of {served:?}"
            );
            // The page's own description of where it sits in the collection, which is what a
            // client pages by: the marker it was given back, whether more remain, and the
            // resume point for the next page.
            for member in ["Marker", "MaxItems", "Quantity", "IsTruncated", "NextMarker"] {
                let (left, right) = (
                    empty.root().find_text(member),
                    absent.root().find_text(member),
                );
                assert_eq!(
                    left, right,
                    "case {label}: expected the same {member} as the listing with no Marker, \
                     got {left:?} instead of {right:?}"
                );
            }
            assert_eq!(
                items.len(),
                expected.min(listing.count),
                "case {label}: expected {} items on the first page, got {items:?}",
                expected.min(listing.count)
            );
        }
    }
}

/// Paging from the empty value with the `NextMarker` each page hands back walks the whole
/// collection: every item exactly once, in the order the unpaged listing serves them.
#[tokio::test]
async fn paging_from_an_empty_marker_walks_every_item_exactly_once() {
    let router = emulator();
    let listings = seed(&router).await;

    for listing in &listings {
        let whole = list(&router, &listing.path, "").await;
        assert_eq!(
            whole.status, 200,
            "case {}: expected the unpaged listing to be served, got {} with {}",
            listing.case, whole.status, whole.raw
        );
        let expected = page_items(&whole, listing.item);
        assert_eq!(
            expected.len(),
            listing.count,
            "case {}: expected the unpaged listing to report {} items, got {expected:?}",
            listing.case,
            listing.count
        );

        // One item per page, starting from the empty value a client that has paged nothing
        // yet carries, so every page but the last is truncated and hands back a marker.
        let mut collected: Vec<String> = Vec::new();
        let mut marker = String::new();
        // At most one page per item plus one, so a listing that never stops truncating fails
        // here rather than looping for ever.
        for page in 0..=listing.count + 1 {
            let answer = list(&router, &listing.path, &format!("?MaxItems=1&Marker={marker}")).await;
            assert_eq!(
                answer.status, 200,
                "case {} page {page}: expected HTTP 200 for the marker {marker:?}, got {} with \
                 {}",
                listing.case, answer.status, answer.raw
            );
            let items = page_items(&answer, listing.item);
            assert!(
                items.len() <= 1,
                "case {} page {page}: a page of one holds at most one item, got {items:?}",
                listing.case
            );
            collected.extend(items);
            match answer.root().find_text("NextMarker") {
                Some(next) => {
                    assert!(
                        !next.is_empty(),
                        "case {} page {page}: a NextMarker names the item the page ended at, \
                         got {next:?}",
                        listing.case
                    );
                    marker = next.to_string();
                }
                None => break,
            }
        }

        assert_eq!(
            collected, expected,
            "case {}: expected the walk to enumerate every item once in the listing's own \
             order, got {collected:?} instead of {expected:?}",
            listing.case
        );
    }
}
