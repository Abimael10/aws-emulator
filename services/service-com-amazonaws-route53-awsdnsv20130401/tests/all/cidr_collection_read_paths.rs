// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of the two rules the CIDR-collection READ paths are held to.
//!
//! 1. Every ARN this service reports for a CIDR collection is a Route 53 resource ARN:
//!    Route 53 is a GLOBAL service, so neither the region field nor the ACCOUNT field of
//!    one of its resource ARNs is populated — the model spells that layout out itself
//!    where it names the ARN that identifies a hosted zone,
//!    "`aws:SourceArn: arn:aws:route53:::hostedzone/hosted zone ID`"
//!    (smithy_query(query="doc",shape="com.amazonaws.route53#CreateQueryLoggingConfig")).
//!    The create and the listing report the SAME ARN for one collection, so a client that
//!    enumerates collections and keys them by ARN finds the row for a collection it holds.
//!    The table spans the collection name's length and character classes and the version
//!    counter, because the layout holds for every collection — not merely for a renamed one.
//!    Each row is also read off the collection it names at the moment of the read, so a
//!    listing taken after the account's holdings have MOVED reconciles by ARN to exactly
//!    the collections the account holds then: a moved version counter under the same ARN,
//!    no row for a collection that was deleted, a row for one created afterwards.
//! 2. An existing collection ENUMERATES its blocks: `ListCidrBlocks` answers HTTP 200 with
//!    a `CidrBlocks` container carrying one row per block the request selects, and no row
//!    where it selects none — a location a change has emptied, a location the collection
//!    was never given, a collection that holds nothing at all. The 404s the operation
//!    models are for the collection id itself naming no collection of the account.
//! 3. The account's collections are read back in PAGES: `MaxResults` ("the maximum number
//!    of CIDR collections to return in the response") bounds the page and `NextToken`
//!    ("an opaque pagination token to indicate where the service is to begin enumerating
//!    results") resumes it, so the pages partition the whole listing in its own stable
//!    order, carrying each collection's stored summary once, and a token is answered
//!    exactly when collections remain. A `NextToken` this listing never minted names no
//!    page of it, so it is the modelled `InvalidInput` (HTTP 400) rather than a page.
//!    Being a read, repeating it reports the same collections and a refused request
//!    leaves the listing exactly as it was.
//!
//! Every test drives the service through the real router, using the SAME harness the
//! sibling CIDR-collection suites use, so what is asserted is the HTTP/restXml behaviour a
//! Route 53 client sees.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::DEFAULT_PAGE_SIZE;

use crate::cidr_collection_blocks::{change, change_collection, create_collection, DELETE_IF_EXISTS, PUT};
use crate::hosted_zone_records::{call, router, Reply};

/// The wire error code of the refusal of a collection id the account does not hold
/// (`NoSuchCidrCollectionException`, the shape name the model declares).
const NO_SUCH_COLLECTION: &str = "NoSuchCidrCollectionException";

/// The wire error code this service states every input-level refusal under
/// (`InvalidInput`, the one input error `ListCidrCollections` models).
const INVALID_INPUT: &str = "InvalidInput";

/// The partition, service and resource kind a CIDR collection's ARN names, with the
/// region and the account fields left EMPTY — the layout of a Route 53 resource ARN.
const COLLECTION_ARN_PREFIX: &str = "arn:aws:route53:::cidrcollection/";

/// One `CollectionSummary` as `ListCidrCollections` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListedCollection {
    arn: String,
    id: String,
    name: String,
    version: String,
}

/// `ListCidrCollections` with the query string `query` (`""` for none):
/// `GET /2013-04-01/cidrcollection`, as the model binds it.
async fn list_collections(router: &MultiServiceRouter, query: &str) -> Reply {
    call(router, "GET", &format!("/2013-04-01/cidrcollection{query}"), "").await
}

/// The `CollectionSummary` rows a reply carries, in document order.
fn reported_collections(reply: &Reply, case: &str) -> Vec<ListedCollection> {
    let arns = reply.all("Arn");
    let ids = reply.all("Id");
    let names = reply.all("Name");
    let versions = reply.all("Version");
    assert!(
        arns.len() == ids.len() && ids.len() == names.len() && names.len() == versions.len(),
        "case {case:?}: every reported collection must carry its Arn, Id, Name and Version, got \
         {} Arn(s), {} Id(s), {} Name(s) and {} Version(s); body {}",
        arns.len(),
        ids.len(),
        names.len(),
        versions.len(),
        reply.body
    );
    arns.iter()
        .zip(&ids)
        .zip(&names)
        .zip(&versions)
        .map(|(((arn, id), name), version)| ListedCollection {
            arn: (*arn).to_string(),
            id: (*id).to_string(),
            name: (*name).to_string(),
            version: (*version).to_string(),
        })
        .collect()
}

/// Every row `ListCidrCollections` reports, in document order.
async fn listed_collections(router: &MultiServiceRouter) -> (Reply, Vec<ListedCollection>) {
    let reply = list_collections(router, "").await;
    assert_eq!(
        reply.status, 200,
        "expected 200 from ListCidrCollections, got {} with body {}",
        reply.status, reply.body
    );
    let rows = reported_collections(&reply, "the whole listing");
    (reply, rows)
}

/// The blocks a `ListCidrBlocks` reply reports, as `(location, block)` pairs in document
/// order.
fn reported_blocks(reply: &Reply) -> Vec<(String, String)> {
    let locations = reply.all("LocationName");
    let blocks = reply.all("CidrBlock");
    assert_eq!(
        locations.len(),
        blocks.len(),
        "every reported row must carry both its location and its block; body {}",
        reply.body
    );
    locations
        .iter()
        .zip(&blocks)
        .map(|(location, block)| ((*location).to_string(), (*block).to_string()))
        .collect()
}

/// One row of the enumeration table: the request sent, and the `(location, block)` pairs
/// `ListCidrBlocks` must report for it.
struct EnumerationRow {
    case: &'static str,
    uri: String,
    expected: Vec<(String, String)>,
}

/// Assert that `arn` is the Route 53 resource ARN of the collection `id`: six
/// colon-separated fields with the region AND the account field empty.
fn assert_collection_arn(case: &str, arn: &str, id: &str) {
    let fields: Vec<&str> = arn.split(':').collect();
    assert_eq!(
        fields.len(),
        6,
        "case {case:?}: an ARN has six colon-separated fields, got {arn:?}"
    );
    assert_eq!(
        (fields[0], fields[1], fields[2]),
        ("arn", "aws", "route53"),
        "case {case:?}: expected a Route 53 ARN, got {arn:?}"
    );
    assert_eq!(
        fields[3], "",
        "case {case:?}: Route 53 is a global service, so the region field is empty, got {arn:?}"
    );
    assert_eq!(
        fields[4], "",
        "case {case:?}: a Route 53 resource ARN names no account, got {arn:?}"
    );
    assert_eq!(
        fields[5],
        format!("cidrcollection/{id}"),
        "case {case:?}: expected the collection's own resource path, got {arn:?}"
    );
    assert_eq!(
        arn,
        format!("{COLLECTION_ARN_PREFIX}{id}"),
        "case {case:?}: expected the whole ARN in Route 53's layout, got {arn:?}"
    );
    assert!(
        !arn.contains(::engine::emulator_account_id()),
        "case {case:?}: the ARN must carry no account id at all, got {arn:?}"
    );
}

// ---------------------------------------------------------------------------
// The ARN the create and the listing report
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_reported_collection_arn_is_matchable_in_route_53s_layout() {
    // The rows differ in the name's length and character classes (`CollectionName` is
    // `len 1..64, pattern ^[0-9A-Za-z_\-]+$`) and in how many changes have moved the
    // collection's version, because the ARN layout holds for every collection the
    // account holds.
    let rows = [
        ("the shortest name", "a".to_string(), 0usize),
        ("a hyphenated name", "office-ranges".to_string(), 1),
        ("an underscored name with digits", "ranges_2024_eu".to_string(), 2),
        // The longest name the shared create helper can reach: it derives the caller
        // reference from the name (`create-<name>`) and `CidrNonce` is `len 1..64`.
        ("a long name", "n".repeat(57), 0),
    ];
    let router = router();
    let mut expected: Vec<ListedCollection> = Vec::new();
    for (case, name, changes) in &rows {
        let id = create_collection(&router, name).await;
        for index in 0..*changes {
            let applied = change_collection(
                &router,
                &id,
                None,
                &change("east", PUT, &[&format!("198.51.100.{index}/32")]),
            )
            .await;
            assert_eq!(
                applied.status, 200,
                "case {case:?}: expected 200 from ChangeCidrCollection, got {} with body {}",
                applied.status, applied.body
            );
        }
        expected.push(ListedCollection {
            arn: format!("{COLLECTION_ARN_PREFIX}{id}"),
            id: id.clone(),
            name: name.clone(),
            // "A sequential counter that Route 53 sets to 1 when you create a collection
            // and increments it by 1 each time you update the collection."
            version: (1 + changes).to_string(),
        });
    }

    let (reply, listed) = listed_collections(&router).await;
    for row in &expected {
        let found = listed.iter().find(|candidate| candidate.arn == row.arn);
        assert_eq!(
            found,
            Some(row),
            "case {:?}: the listing must carry the collection keyed by its ARN {:?}, got {:?}; \
             body {}",
            row.name,
            row.arn,
            listed,
            reply.body
        );
        // The ARN the response actually carried, read out of the row the listing reported.
        let reported = found.map_or("", |row| row.arn.as_str());
        assert_collection_arn(&row.name, reported, &row.id);
    }
    assert_eq!(
        listed.len(),
        expected.len(),
        "the listing must report every collection the account holds and nothing else, got \
         {listed:?}; body {}",
        reply.body
    );
}

#[tokio::test]
async fn the_create_and_the_listing_report_one_collection_under_one_arn() {
    let router = router();
    let created = call(
        &router,
        "POST",
        "/2013-04-01/cidrcollection",
        "<CreateCidrCollectionRequest \
         xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
         <CallerReference>arn-agreement</CallerReference><Name>arn-agreement</Name>\
         </CreateCidrCollectionRequest>",
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"the create\": expected 201, got {} with body {}",
        created.status, created.body
    );
    let id = created
        .first("Collection/Id")
        .unwrap_or_else(|| panic!("case \"the create\": the reply must carry an id; body {}", created.body))
        .to_string();
    let created_arn = created
        .first("Collection/Arn")
        .unwrap_or_else(|| panic!("case \"the create\": the reply must carry an ARN; body {}", created.body))
        .to_string();
    assert_collection_arn("the create", &created_arn, &id);

    let (reply, listed) = listed_collections(&router).await;
    let listed_arn = listed
        .iter()
        .find(|row| row.id == id)
        .map(|row| row.arn.clone());
    assert_eq!(
        listed_arn.as_deref(),
        Some(created_arn.as_str()),
        "case \"the listing\": the two reads must report one collection under one ARN, got \
         {listed_arn:?}; body {}",
        reply.body
    );
}

// ---------------------------------------------------------------------------
// The rows a listing reports once the account's holdings have moved
// ---------------------------------------------------------------------------

/// `DeleteCidrCollection` for the collection `id`, as the model binds it:
/// `DELETE /2013-04-01/cidrcollection/{id}`.
async fn delete_collection(router: &MultiServiceRouter, id: &str) -> Reply {
    call(
        router,
        "DELETE",
        &format!("/2013-04-01/cidrcollection/{id}"),
        "",
    )
    .await
}

/// Assert the listing reports exactly `held` and nothing else, each row carried
/// under its own ARN in Route 53's layout — the reconciliation a client does when
/// it enumerates the account and matches the rows to the collections it holds.
async fn assert_listing_reconciles(
    router: &MultiServiceRouter,
    case: &str,
    held: &[ListedCollection],
) {
    let (reply, listed) = listed_collections(router).await;
    for row in held {
        let found = listed.iter().find(|candidate| candidate.arn == row.arn);
        assert_eq!(
            found,
            Some(row),
            "case {case:?}: the listing must carry the collection the account holds under its \
             own ARN {:?}, got {found:?} among {listed:?}; body {}",
            row.arn,
            reply.body
        );
        // The ARN the response actually carried, read out of the row it reported.
        assert_collection_arn(case, found.map_or("", |row| row.arn.as_str()), &row.id);
    }
    assert_eq!(
        listed.len(),
        held.len(),
        "case {case:?}: the listing must report the {} collection(s) the account holds and \
         nothing else, got {listed:?}; body {}",
        held.len(),
        reply.body
    );
}

#[tokio::test]
async fn the_listing_keys_each_row_to_the_collection_state_at_the_time_of_the_read() {
    // The same rule where a client meets it over TIME: every row is read off the
    // collection it names, so each listing reconciles by ARN to exactly the
    // collections the account holds at that moment. The steps below are the ways
    // the account's holdings move — a change that moves one collection's version
    // counter, a change that moves another's, a delete that takes a collection
    // away and a create that adds one afterwards — and the listing is reconciled
    // after each of them, so a row carrying a counter, a name or an ARN the store
    // no longer holds cannot pass.
    let router = router();
    // The collections differ in the length and character classes `CollectionName`
    // admits (`len 1..64, pattern ^[0-9A-Za-z_\-]+$`), so the rows the listing is
    // reconciled against are not one renamed collection.
    let mut held: Vec<ListedCollection> = Vec::new();
    for name in ["r", "moved-ranges", "spare_ranges_2"] {
        let id = create_collection(&router, name).await;
        held.push(ListedCollection {
            arn: format!("{COLLECTION_ARN_PREFIX}{id}"),
            id,
            name: name.to_string(),
            // "A sequential counter that Route 53 sets to 1 when you create a CIDR
            // collection and increments by 1 each time you update settings for the
            // CIDR collection."
            version: "1".to_string(),
        });
    }
    assert_listing_reconciles(&router, "the collections as created", &held).await;

    // A change moves ONE collection's version counter; the row for it must report
    // the moved counter under the same ARN, and no other row may move.
    for (case, index, block, version) in [
        ("one collection changed once", 1usize, "198.51.100.0/24", "2"),
        ("the same collection changed again", 1, "198.51.101.0/24", "3"),
        ("another collection changed", 2, "203.0.113.0/24", "2"),
    ] {
        let applied = change_collection(
            &router,
            &held[index].id,
            None,
            &change("east", PUT, &[block]),
        )
        .await;
        assert_eq!(
            applied.status, 200,
            "case {case:?}: expected 200 from ChangeCidrCollection, got {} with body {}",
            applied.status, applied.body
        );
        held[index].version = version.to_string();
        assert_listing_reconciles(&router, case, &held).await;
    }

    // A collection that holds nothing is taken away, so its ARN keys no row of the
    // listing any more while every collection beside it is still reported as it was.
    let removed = held.remove(0);
    let deleted = delete_collection(&router, &removed.id).await;
    assert_eq!(
        deleted.status, 200,
        "case \"a deleted collection\": expected 200 from DeleteCidrCollection, got {} with \
         body {}",
        deleted.status, deleted.body
    );
    assert_listing_reconciles(&router, "a deleted collection", &held).await;
    let (reply, listed) = listed_collections(&router).await;
    assert!(
        !listed.iter().any(|row| row.arn == removed.arn),
        "case \"a deleted collection\": the ARN {:?} must key no row once the collection is \
         gone, got {listed:?}; body {}",
        removed.arn,
        reply.body
    );

    // ...and a collection created afterwards keys its own row, so the listing is
    // the account's holdings rather than the set it was first read with.
    let id = create_collection(&router, "later-ranges").await;
    held.push(ListedCollection {
        arn: format!("{COLLECTION_ARN_PREFIX}{id}"),
        id,
        name: "later-ranges".to_string(),
        version: "1".to_string(),
    });
    assert_listing_reconciles(&router, "a collection created after the delete", &held).await;
}

// ---------------------------------------------------------------------------
// The block enumeration of a collection that holds nothing matching
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_existing_collection_enumerates_to_an_empty_page_when_no_block_matches() {
    let router = router();
    let id = create_collection(&router, "enumerated").await;
    let held = "198.51.100.0/24";
    let applied = change_collection(
        &router,
        &id,
        None,
        &format!(
            "{}{}",
            change("east", PUT, &[held]),
            change("west", PUT, &["203.0.113.0/24"])
        ),
    )
    .await;
    assert_eq!(
        applied.status, 200,
        "seeding the blocks: expected 200, got {} with body {}",
        applied.status, applied.body
    );
    // The location `west` is emptied by a change, so nothing is left to report for it,
    // while `east` still holds its block.
    let emptied = change_collection(
        &router,
        &id,
        None,
        &change("west", DELETE_IF_EXISTS, &["203.0.113.0/24"]),
    )
    .await;
    assert_eq!(
        emptied.status, 200,
        "emptying a location: expected 200, got {} with body {}",
        emptied.status, emptied.body
    );

    // A collection of the account that holds no block at all, so the unnarrowed
    // enumeration of it selects nothing either.
    let barren = create_collection(&router, "enumerated-empty").await;

    // The rows differ in WHY the request selects no row: a location a change emptied, a
    // location the collection was never given, one whose name is at each end of
    // `CidrLocationNameDefaultNotAllowed`'s `len 1..16`, and a collection holding nothing.
    let rows = [
        EnumerationRow {
            case: "a location a change emptied",
            uri: format!("/2013-04-01/cidrcollection/{id}/cidrblocks?location=west"),
            expected: Vec::new(),
        },
        EnumerationRow {
            case: "a location the collection was never given",
            uri: format!("/2013-04-01/cidrcollection/{id}/cidrblocks?location=north"),
            expected: Vec::new(),
        },
        EnumerationRow {
            case: "the shortest location name",
            uri: format!("/2013-04-01/cidrcollection/{id}/cidrblocks?location=z"),
            expected: Vec::new(),
        },
        EnumerationRow {
            case: "the longest location name",
            uri: format!(
                "/2013-04-01/cidrcollection/{id}/cidrblocks?location={}",
                "x".repeat(16)
            ),
            expected: Vec::new(),
        },
        EnumerationRow {
            case: "a collection that holds nothing",
            uri: format!("/2013-04-01/cidrcollection/{barren}/cidrblocks"),
            expected: Vec::new(),
        },
        EnumerationRow {
            case: "a location that still holds its block",
            uri: format!("/2013-04-01/cidrcollection/{id}/cidrblocks?location=east"),
            expected: vec![("east".to_string(), held.to_string())],
        },
    ];
    for EnumerationRow {
        case,
        uri,
        expected,
    } in rows
    {
        let reply = call(&router, "GET", &uri, "").await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200 from ListCidrBlocks, got {} with body {}",
            reply.status, reply.body
        );
        assert!(
            reply.body.contains("ListCidrBlocksResponse"),
            "case {case:?}: the enumeration must answer a ListCidrBlocksResponse, got body {}",
            reply.body
        );
        let reported = reported_blocks(&reply);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the rows {expected:?}, got {reported:?}; body {}",
            reply.body
        );
        assert!(
            reply.first("NextToken").is_none(),
            "case {case:?}: a page that carries the whole selection names no NextToken; body {}",
            reply.body
        );
    }

    // The 404 this operation models is for the collection id: one the account does not
    // hold is still refused, whichever location the request narrows to.
    for (case, uri) in [
        (
            "an unheld collection",
            "/2013-04-01/cidrcollection/00000000-0000-4000-8000-000000000000/cidrblocks"
                .to_string(),
        ),
        (
            "an unheld collection narrowed to a location",
            "/2013-04-01/cidrcollection/00000000-0000-4000-8000-000000000000/cidrblocks\
             ?location=east"
                .to_string(),
        ),
    ] {
        let refused = call(&router, "GET", &uri, "").await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected 404, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            NO_SUCH_COLLECTION,
            "case {case:?}: expected {NO_SUCH_COLLECTION}, got {:?}; body {}",
            refused.error_code(),
            refused.body
        );
    }
}

// ---------------------------------------------------------------------------
// The pages the collection listing is read back in
// ---------------------------------------------------------------------------

/// The collections the paging test seeds the account with, differing in length and in the
/// character classes `CollectionName` admits (`len 1..64, pattern ^[0-9A-Za-z_\-]+$`), so
/// the pages are asserted over a listing whose rows are not one renamed collection. There
/// are enough of them that every page size below takes several pages.
const PAGED_NAMES: &[&str] = &[
    "alpha",
    "Bravo_2",
    "charlie-3",
    "d",
    "ECHO",
    "foxtrot_golf",
    "h-9",
    "india_JULIET-10",
];

/// The page sizes under test: one row at a time, sizes that leave a remainder, the size
/// that is exactly the whole listing, one past it, and [`DEFAULT_PAGE_SIZE`] — the page a
/// request naming no `MaxResults` is served.
const PAGE_SIZES: &[i32] = &[1, 3, 5, 7, 8, 9, DEFAULT_PAGE_SIZE];

#[tokio::test]
async fn the_pages_partition_the_collection_listing_in_its_own_order() {
    let router = router();
    let mut created: Vec<ListedCollection> = Vec::new();
    for name in PAGED_NAMES {
        let id = create_collection(&router, name).await;
        created.push(ListedCollection {
            arn: format!("{COLLECTION_ARN_PREFIX}{id}"),
            id,
            name: (*name).to_string(),
            // "A sequential counter that Route 53 sets to 1 when you create a CIDR
            // collection": no change below moves it.
            version: "1".to_string(),
        });
    }

    // The unpaged listing is the whole of what the pages must partition, and it carries
    // every collection the account holds, each with the summary the store holds for it.
    let (reply, whole) = listed_collections(&router).await;
    assert_eq!(
        whole.len(),
        created.len(),
        "case \"the whole listing\": expected the {} seeded collections, got {whole:?}; body {}",
        created.len(),
        reply.body
    );
    for row in &created {
        assert!(
            whole.contains(row),
            "case {:?}: the listing must report the stored collection {row:?}, got {whole:?}; \
             body {}",
            row.name,
            reply.body
        );
    }

    // It is a read: asking again answers the same rows in the same order, so the order
    // the pages are asserted against is the listing's own and nothing the first read did
    // moved it.
    let (repeated, again) = listed_collections(&router).await;
    assert_eq!(
        again, whole,
        "case \"the listing repeated\": a read must answer the same rows in the same order, \
         got {again:?} against {whole:?}; body {}",
        repeated.body
    );

    for &page_size in PAGE_SIZES {
        let case = format!("maxresults={page_size}");
        let mut walked: Vec<ListedCollection> = Vec::new();
        let mut token: Option<String> = None;
        // Every page carries at least one collection while any remain, so the walk cannot
        // take more pages than there are collections; one more iteration than that is a
        // paging defect rather than a slow test.
        for _ in 0..=PAGED_NAMES.len() {
            let query = match &token {
                Some(token) => format!("?maxresults={page_size}&nexttoken={token}"),
                None => format!("?maxresults={page_size}"),
            };
            let page = list_collections(&router, &query).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: expected 200 from ListCidrCollections, got {} with body {}",
                page.status, page.body
            );
            let rows = reported_collections(&page, &case);
            let expected_len = (whole.len() - walked.len()).min(page_size as usize);
            assert_eq!(
                rows.len(),
                expected_len,
                "case {case:?}: expected a page of {expected_len} after {} walked, got {rows:?}; \
                 body {}",
                walked.len(),
                page.body
            );
            walked.extend(rows);
            token = page.first("NextToken").map(str::to_string);
            assert_eq!(
                token.is_some(),
                walked.len() < whole.len(),
                "case {case:?}: a token is answered exactly when collections remain, got \
                 {token:?} after {} of {}",
                walked.len(),
                whole.len()
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, whole,
            "case {case:?}: expected the pages to partition the listing {whole:?}, got {walked:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A pagination token this listing never minted
// ---------------------------------------------------------------------------

/// One row of the token table: the query string sent, and what makes the token name no
/// page of this listing.
struct TokenRow {
    case: &'static str,
    query: &'static str,
}

/// The rows differ in HOW the token departs from the ids this listing resumes at (`UUID`,
/// `pattern ^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$`): not an id at all, a
/// character no id carries, a field count or field width an id does not have, and the
/// upper-case spelling of hexadecimal no minted id uses.
const UNMINTED_TOKEN_ROWS: &[TokenRow] = &[
    TokenRow {
        case: "a token that is no id at all",
        query: "?nexttoken=not-a-token",
    },
    TokenRow {
        case: "a token carrying a character no id does",
        query: "?nexttoken=%2F11111111-2222-3333-4444-555555555555",
    },
    TokenRow {
        case: "a token one field short of an id",
        query: "?nexttoken=11111111-2222-3333-555555555555",
    },
    TokenRow {
        case: "a token whose last field is one character short",
        query: "?nexttoken=11111111-2222-3333-4444-55555555555",
    },
    TokenRow {
        case: "a token spelt in upper-case hexadecimal",
        query: "?nexttoken=11111111-2222-3333-4444-55555555555A",
    },
    TokenRow {
        case: "an unminted token on a bounded page",
        query: "?maxresults=1&nexttoken=not-a-token",
    },
];

#[tokio::test]
async fn a_next_token_this_listing_never_minted_names_no_page_of_it() {
    let router = router();
    let mut held: Vec<ListedCollection> = Vec::new();
    for name in ["held-ranges", "other-ranges"] {
        let id = create_collection(&router, name).await;
        held.push(ListedCollection {
            arn: format!("{COLLECTION_ARN_PREFIX}{id}"),
            id,
            name: name.to_string(),
            version: "1".to_string(),
        });
    }
    let (_, before) = listed_collections(&router).await;

    for row in UNMINTED_TOKEN_ROWS {
        let refused = list_collections(&router, row.query).await;
        assert_eq!(
            refused.status, 400,
            "case {:?}: expected 400, got {} with body {}",
            row.case, refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            INVALID_INPUT,
            "case {:?}: expected {INVALID_INPUT}, got {:?}; body {}",
            row.case,
            refused.error_code(),
            refused.body
        );
        assert!(
            refused.first("CollectionSummary").is_none() && refused.first("Arn").is_none(),
            "case {:?}: a refused listing must report no collection; body {}",
            row.case,
            refused.body
        );
    }

    // The refusals changed nothing: the listing still reports exactly the collections the
    // account holds.
    let (reply, after) = listed_collections(&router).await;
    assert_eq!(
        after, before,
        "case \"after the refusals\": expected the listing untouched, got {after:?} against \
         {before:?}; body {}",
        reply.body
    );
    for row in &held {
        assert!(
            after.contains(row),
            "case {:?}: the collection must still be reported as {row:?}, got {after:?}; body {}",
            row.name,
            reply.body
        );
    }
}
