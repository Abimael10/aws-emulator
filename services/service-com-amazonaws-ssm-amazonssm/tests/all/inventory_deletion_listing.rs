// SPDX-License-Identifier: CC0-1.0

//! The inventory DELETION listing, end to end: what `DescribeInventoryDeletions` projects out
//! of the deletions `DeleteInventory` recorded, how it pages over them, and how it judges the
//! token a page handed out.
//!
//! RULE under test: **`DescribeInventoryDeletions` is a `@paginated` READ over the inventory
//! deletions the Region a call is signed for has recorded (`smithy_query(query="operation",
//! operation="DescribeInventoryDeletions")`: `items: InventoryDeletions`, `inputToken`/
//! `outputToken` `NextToken`, `pageSize` `MaxResults` of range 1..50), so a walk of its pages
//! reports every recorded deletion of that Region exactly once in the listing's own order,
//! each page holding at most `MaxResults` items, carrying a `NextToken` only while a deletion
//! is still waiting and projecting each deletion under the id its delete minted; a
//! `DeletionId` narrows the read to the deletion it names, so one the Region recorded no
//! deletion under matches nothing and is served the empty list (RECORDED BEHAVIOUR: the
//! recorded call naming a well-formed id no delete handed out is served rather than refused);
//! and a `NextToken` naming no deletion the read holds names no page of it to resume, which is
//! `InvalidNextToken` (HTTP 400).**
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the router. The ONE thing seeded through the engine is a registered managed node,
//! because no call of this service registers one; every deletion under test is recorded by a
//! real `PutInventory` followed by a real `DeleteInventory`.
//!
//! The rows differ along what the rule generalises over: the page size relative to how many
//! deletions are recorded (one per page, a size that divides the set, one that does not, the
//! published upper bound, and no bound at all, against a set one deletion larger than that
//! bound), the Region a call is signed for, whether the read is narrowed by a `DeletionId`,
//! and the form of a resume token that names nothing (a well-formed id nothing was recorded
//! under, a value that is no id at all, another Region's id, and an id the narrowing
//! excludes).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    call_in_region, error_code, error_message, router_with_engine, REGION,
};

/// A second Region, to pin that a deletion is recorded and read through the one Region its
/// delete was signed for.
const OTHER_REGION: &str = "eu-west-1";

/// The node every item is written about: an EC2 node id of the 17-character form the modelled
/// `InstanceId` pattern admits (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`).
const NODE: &str = "i-0123456789abcdef0";

/// The instant the seeded node last reported, as a whole number of seconds.
const LAST_PING: Duration = Duration::from_secs(1_700_000_000);

/// A capture time in the form the modelled `InventoryItemCaptureTime` spells.
const CAPTURED: &str = "2024-03-04T05:06:07Z";

/// The schema version every item carries (`InventoryItemSchemaVersion`).
const VERSION: &str = "1.0";

/// The custom type names the deletions under test address, each well formed under the custom
/// name pattern `^(Custom:)([a-zA-Z0-9]{3,93})$`. One delete per name, so one deletion per
/// name: six is enough to page over with every row's page size and small enough to stay fast.
const TYPES: [&str; 6] = [
    "Custom:RackInfo",
    "Custom:Compliance",
    "Custom:Fleet",
    "Custom:Licences",
    "Custom:Owners",
    "Custom:Tenancy",
];

/// The published upper bound of `MaxResults` for this operation
/// (`smithy_query(query="operation", operation="DescribeInventoryDeletions")`: range 1..50),
/// which is also the most a read naming no `MaxResults` is served on one page: the bound the
/// member publishes is what an unbounded page is held to.
const MAX_PAGE_SIZE: i32 = 50;

/// The sentence a resume token naming no page of the read is refused with, written out here
/// independently of the service: fixed text, with nothing of the request in it. It is the
/// sentence this service answers for this error on every paginated read of the surface — and
/// it is NOT the model's prose for the error shape ("The specified token isn't valid."), which
/// states the condition rather than the wording on the wire; the suites of the other paginated
/// reads (`all/available_patches.rs`, `all/patch_baseline_listing.rs`,
/// `all/document_versions.rs`) pin the same sentence.
const INVALID_NEXT_TOKEN: &str = "The NextToken value is not valid";

/// A value of the form the modelled `UUID` pattern admits that no delete of this endpoint
/// handed out, used here as a resume token that names no page of the read.
const UNISSUED: &str = "0ad1e69c-b8c1-4b4b-9e1b-0c5f5d0a5f20";

/// A router with just this service mounted, and the engine behind it.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    router_with_engine()
}

/// Register the node in `region` the way its own registration would, which is what makes a
/// put about it a put about a node this account holds.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str) {
    let at = SystemTime::UNIX_EPOCH + LAST_PING;
    let mut node = ManagedNodeRecord::registering(NODE, RESOURCE_TYPE_EC2_INSTANCE, at);
    node.properties.platform_type = Some("Linux".to_string());
    node.properties.instance_state = Some("Active".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, node),
        "a seeded node must carry the node id it is stored under"
    );
}

/// Record one deletion in `region`: write an item of `type_name` and delete the type, which
/// is what mints a `DeletionId`. Answers the id the delete handed out.
async fn record_deletion(router: &MultiServiceRouter, region: &str, type_name: &str) -> String {
    put_item(router, region, type_name).await;
    delete_inventory(router, region, type_name, true).await
}

/// `PutInventory` of one item of `type_name` about the seeded node, which must be served.
async fn put_item(router: &MultiServiceRouter, region: &str, type_name: &str) {
    let (status, written) = call_in_region(
        router,
        region,
        "PutInventory",
        &json!({
            "InstanceId": NODE,
            "Items": [{
                "TypeName": type_name,
                "SchemaVersion": VERSION,
                "CaptureTime": CAPTURED,
                "Content": [{ "RackLocation": "Bay B/Row 4" }],
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the put of {type_name} in {region} must be served, got {status} with {written}"
    );
}

/// `DeleteInventory` of `type_name` in `region`, taking the type's schema with it when
/// `with_schema`, which must be served. Answers the `DeletionId` it minted.
async fn delete_inventory(
    router: &MultiServiceRouter,
    region: &str,
    type_name: &str,
    with_schema: bool,
) -> String {
    let mut body = json!({ "TypeName": type_name });
    if with_schema {
        body["SchemaDeleteOption"] = json!("DeleteSchema");
    }
    let (status, deleted) = call_in_region(router, region, "DeleteInventory", &body).await;
    assert_eq!(
        status, 200,
        "the delete of {type_name} in {region} must be served, got {status} with {deleted}"
    );
    deleted["DeletionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a delete mints a DeletionId, got {deleted}"))
        .to_string()
}

/// `DescribeInventoryDeletions` signed for `region`, which must be served.
async fn listing(router: &MultiServiceRouter, region: &str, case: &str, body: &Value) -> Value {
    let (status, reported) =
        call_in_region(router, region, "DescribeInventoryDeletions", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a deletion listing is a successful read, got {status} with {reported}"
    );
    reported
}

/// The ids one reported page carries, in the order it reports them.
fn reported_ids(case: &str, reported: &Value) -> Vec<String> {
    reported["InventoryDeletions"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a listing reports InventoryDeletions, got {reported}")
        })
        .iter()
        .map(|item| {
            item["DeletionId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a reported deletion carries its id, got {item}")
                })
                .to_string()
        })
        .collect()
}

/// Record one deletion per name in [`TYPES`] in `region`, answering the ids in the order the
/// listing reports them (the store's own ascending deletion-id order).
async fn recorded_set(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for type_name in TYPES {
        ids.push(record_deletion(router, region, type_name).await);
    }
    ids.sort();
    ids
}

/// Record `count` deletions in `region` of ONE type, answering their ids in the order the
/// listing reports them.
///
/// A delete that leaves the SCHEMA alone leaves the type DEFINED and holding nothing, so the
/// next delete of that name is served and mints another `DeletionId` — which is how a Region
/// comes to hold more deletions than the type ceiling allows types, and so how a page bound
/// larger than a handful of deletions is reached at all.
async fn recorded_run(router: &MultiServiceRouter, region: &str, count: usize) -> Vec<String> {
    put_item(router, region, TYPES[0]).await;
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        ids.push(delete_inventory(router, region, TYPES[0], false).await);
    }
    assert_eq!(
        ids.len(),
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        "every delete mints an id of its own, got {ids:?}"
    );
    ids.sort();
    ids
}

/// RULE: a walk of the pages reports every deletion the Region recorded exactly once, in the
/// listing's own order, with at most `MaxResults` items per page and a `NextToken` only while
/// a deletion is still waiting — whatever page size the caller chose, and whether or not it
/// chose one at all, a request naming none being served the published upper bound.
#[tokio::test]
async fn a_page_walk_reports_every_recorded_deletion_exactly_once() {
    // One deletion more than an unbounded read serves on a page, so the row naming no
    // `MaxResults` has a second page to fetch: that is what pins the implicit page size
    // instead of assuming it.
    let recorded = MAX_PAGE_SIZE as usize + 1;

    let (router, engine) = service();
    register(&engine, REGION);
    let all = recorded_run(&router, REGION, recorded).await;
    assert_eq!(
        all.len(),
        recorded,
        "the Region must hold {recorded} deletions to page over, got {}",
        all.len()
    );

    // One row per page size relative to the recorded set: one item a page, a size that
    // divides the set, one that does not, the published upper bound (one short of the set),
    // and no bound at all.
    let rows: [(&str, Option<i32>); 5] = [
        ("one deletion a page", Some(1)),
        ("a page size that divides the set", Some(3)),
        ("a page size that does not divide the set", Some(4)),
        ("the published upper bound", Some(MAX_PAGE_SIZE)),
        ("no page size at all", None),
    ];

    for (case, max_results) in rows {
        let page_size = max_results.unwrap_or(MAX_PAGE_SIZE) as usize;
        let expected_pages = all.len().div_ceil(page_size);
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let mut body = json!({});
            if let Some(max_results) = max_results {
                body["MaxResults"] = json!(max_results);
            }
            if let Some(token) = &token {
                body["NextToken"] = json!(token);
            }
            let reported = listing(&router, REGION, case, &body).await;
            let ids = reported_ids(case, &reported);
            let expected_page = page_size.min(all.len() - seen.len());
            // A page is FULL while deletions are still waiting — the bound the request named,
            // or the one an unbounded read serves — and holds what is left on the last page.
            assert_eq!(
                ids.len(),
                expected_page,
                "case {case:?}: expected a page of {expected_page} after {} of {} deletions, \
                 got {ids:?}",
                seen.len(),
                all.len()
            );
            let next = reported["NextToken"].as_str().map(str::to_string);
            seen.extend(ids);
            pages += 1;
            assert!(
                pages <= all.len() + 1,
                "case {case:?}: the walk must terminate, got {pages} pages and {seen:?}"
            );
            assert_eq!(
                next.is_some(),
                seen.len() < all.len(),
                "case {case:?}: a page carries a resume token only while a deletion is still \
                 waiting, got {next:?} after {} of {} deletions",
                seen.len(),
                all.len()
            );
            match next {
                None => break,
                Some(next) => token = Some(next),
            }
        }
        assert_eq!(
            seen, all,
            "case {case:?}: the walk reports every recorded deletion once, in the listing's \
             order, got {seen:?}"
        );
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} pages of at most {page_size}, got {pages}"
        );
    }
}

/// RULE: the deletions a read projects are the ones the Region it was signed for recorded, and
/// a `DeletionId` narrows that read to the deletion it names — so an id another Region's
/// delete minted matches nothing there.
///
/// That a well-formed id the read matches nothing under is SERVED the empty list rather than
/// refused is the recorded behaviour of this operation: the recorded call naming an id no
/// delete handed out is answered, and refusing it instead makes that recorded call fail. The
/// modelled `InvalidDeletionIdException` is what a `DeletionId` that is no id at all is judged
/// by, which `all/constraint_set_refusal_wording.rs` covers; the unnarrowed and unissued-id
/// rows of the same rule are covered by `all/inventory_type_catalog.rs`
/// (`a_deletion_listing_projects_the_deletions_it_matches`), so they are not repeated here.
#[tokio::test]
async fn a_deletion_listing_projects_the_region_it_is_signed_for() {
    let (router, engine) = service();
    register(&engine, REGION);
    register(&engine, OTHER_REGION);
    let here = recorded_set(&router, REGION).await;
    let there = record_deletion(&router, OTHER_REGION, TYPES[0]).await;

    let case = "the other Region's own listing";
    let reported = listing(&router, OTHER_REGION, case, &json!({})).await;
    assert_eq!(
        reported_ids(case, &reported),
        vec![there.clone()],
        "case {case:?}: it reports the one deletion that Region recorded, got {reported}"
    );

    let case = "this Region's listing, unnarrowed";
    let reported = listing(&router, REGION, case, &json!({})).await;
    assert_eq!(
        reported_ids(case, &reported),
        here,
        "case {case:?}: it reports this Region's deletions and no other's, got {reported}"
    );

    // (case, the Region the read is signed for, the id it names, what it must match)
    let rows: [(&str, &str, &str, Vec<String>); 3] = [
        (
            "an id this Region recorded",
            REGION,
            here[0].as_str(),
            vec![here[0].clone()],
        ),
        (
            "an id the other Region recorded, read here",
            REGION,
            there.as_str(),
            Vec::new(),
        ),
        (
            "an id this Region recorded, read there",
            OTHER_REGION,
            here[0].as_str(),
            Vec::new(),
        ),
    ];
    for (case, region, deletion_id, expected) in rows {
        let reported = listing(
            &router,
            region,
            case,
            &json!({ "DeletionId": deletion_id, "MaxResults": 1 }),
        )
        .await;
        assert_eq!(
            reported_ids(case, &reported),
            expected,
            "case {case:?}: expected {expected:?}, got {reported}"
        );
        assert_eq!(
            reported["NextToken"],
            Value::Null,
            "case {case:?}: a narrowed read matching at most one deletion has no next page, \
             got {reported}"
        );
    }
}

/// RULE: the resume token a page hands out names the deletion the next page starts at, so a
/// token naming no deletion THIS read holds names no page of it to resume and is refused
/// `InvalidNextToken` (HTTP 400) — whatever form the value takes.
#[tokio::test]
async fn a_resume_token_naming_no_held_deletion_is_refused() {
    let (router, engine) = service();
    register(&engine, REGION);
    register(&engine, OTHER_REGION);
    let here = recorded_set(&router, REGION).await;
    let there = record_deletion(&router, OTHER_REGION, TYPES[0]).await;

    // (case, the Region the read is signed for, the request)
    let rows: [(&str, &str, Value); 5] = [
        (
            "a well-formed id nothing was recorded under",
            REGION,
            json!({ "NextToken": UNISSUED }),
        ),
        (
            "a value that is no deletion id at all",
            REGION,
            json!({ "NextToken": "not-a-page-token" }),
        ),
        (
            "an id another Region recorded",
            REGION,
            json!({ "NextToken": there }),
        ),
        (
            "this Region's id, on the other Region's read",
            OTHER_REGION,
            json!({ "NextToken": here[0].clone() }),
        ),
        (
            // The token is a position in what the read HOLDS, and a narrowed read holds only
            // the deletion its `DeletionId` names.
            "an id the narrowing excludes",
            REGION,
            json!({ "DeletionId": here[0].clone(), "NextToken": here[1].clone() }),
        ),
    ];

    for (case, region, body) in rows {
        let (status, refused) =
            call_in_region(&router, region, "DescribeInventoryDeletions", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a token naming no page is refused, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidNextToken",
            "case {case:?}: expected InvalidNextToken, got {refused}"
        );
        assert_eq!(
            error_message(&refused),
            INVALID_NEXT_TOKEN,
            "case {case:?}: expected {INVALID_NEXT_TOKEN:?}, got {:?}",
            error_message(&refused)
        );
    }

    // The token a page DID hand out resumes that read, so the walk it belongs to still works
    // beside every refusal above.
    let case = "the token the first page handed out";
    let first = listing(&router, REGION, case, &json!({ "MaxResults": 1 })).await;
    let token = first["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a bounded page hands out a token, got {first}"))
        .to_string();
    let second = listing(
        &router,
        REGION,
        case,
        &json!({ "MaxResults": 1, "NextToken": token.clone() }),
    )
    .await;
    assert_eq!(
        reported_ids(case, &second),
        vec![token],
        "case {case:?}: the next page starts at the deletion the token names, got {second}"
    );
    assert_eq!(
        reported_ids(case, &first),
        vec![here[0].clone()],
        "case {case:?}: the first page starts at the listing's first deletion, got {first}"
    );
}
