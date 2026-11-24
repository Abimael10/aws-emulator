// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `GetVpcLinks`, the account's VPC link collection.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns.
//!
//! The RULES under test, each over a SET of inputs rather than one recorded call:
//!
//! * the collection reports the account's STORED links in creation order, each rendered
//!   exactly as the link's own `GetVpcLink` read reports it, over a table of links
//!   spanning the optional members a link may and may not carry and the number of targets
//!   it names;
//! * it reports one `position`/`limit` page of that sequence at a time, over page sizes
//!   across the documented range (default 25, maximum 500), each published bound and one
//!   past each, crossed with every cursor the sequence holds, and a cursor naming nothing
//!   in the sequence is a bad request rather than an empty page;
//! * an account with no link has an EMPTY collection, and a refused create adds nothing
//!   to it;
//! * a deleted link leaves the collection and the links around it keep their place;
//! * a read changes nothing a later caller sees.
//!
//! [`Gateway`], [`Answer`], [`assert_failure`], [`string_member`] and [`page_items`] are
//! this crate's one wire-test harness, reused rather than standing a
//! second emulator up another way; `create_vpc_link`, `get_vpc_link` and `target_arn` are
//! the `vpc_links` suite's one spelling of those calls.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    VPC_LINK_AVAILABLE, VPC_LINK_PENDING, VPC_LINK_PROVISION_SPAN,
};

use crate::rest_api_lifecycle::{assert_failure, page_items, string_member, Answer, Gateway};
use crate::vpc_links::{create_vpc_link, get_vpc_link, settled_link, target_arn};

/// The default page size API Gateway applies when a request names no `limit`, and the
/// largest it accepts — from `GetVpcLinksRequest$limit`'s own documentation, "The maximum
/// number of returned results per page. The default value is 25 and the maximum value is
/// 500" (`smithy_query(query="doc", shape="GetVpcLinksRequest", member="limit")`).
const DEFAULT_PAGE_LIMIT: usize = 25;
const MAX_PAGE_LIMIT: i64 = 500;

/// Slack added to the provisioning span before a link's settled record is compared, so a
/// loaded machine does not read it one tick early.
const MARGIN: Duration = Duration::from_millis(100);

/// `GetVpcLinks` with `query` appended when it is not empty.
async fn list_links(gateway: &Gateway, query: &str) -> Answer {
    let uri = if query.is_empty() {
        "/vpclinks".to_string()
    } else {
        format!("/vpclinks?{query}")
    };
    gateway.call("GET", &uri, None).await
}

/// `GetVpcLinks` with `query`, asserting the modelled 200 and returning the body.
async fn listed(gateway: &Gateway, query: &str) -> Value {
    let answer = list_links(gateway, query).await;
    assert_eq!(
        answer.status, 200,
        "case GetVpcLinks({query:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The `id` of every link on a page, in page order.
fn listed_ids(body: &Value, case: &str) -> Vec<String> {
    page_items(body, case)
        .iter()
        .map(|item| string_member(item, "id", case))
        .collect()
}

/// A table of VPC link requests spanning what the collection's rule generalises over: how
/// many targets a link names, and which optional members it carries — none of them, each
/// of them, and all at once.
fn link_table() -> Vec<(&'static str, Value)> {
    vec![
        (
            "one target and nothing else",
            json!({ "name": "collection-link-a", "targetArns": [target_arn(0)] }),
        ),
        (
            "described",
            json!({
                "name": "collection-link-b",
                "description": "the second link",
                "targetArns": [target_arn(1)],
            }),
        ),
        (
            "three targets",
            json!({
                "name": "collection-link-c",
                "targetArns": [target_arn(0), target_arn(1), target_arn(2)],
            }),
        ),
        (
            "tagged",
            json!({
                "name": "collection-link-d",
                "targetArns": [target_arn(2)],
                "tags": { "team": "api" },
            }),
        ),
        (
            "every member",
            json!({
                "name": "collection-link-e",
                "description": "the last link",
                "targetArns": [target_arn(3), target_arn(4)],
                "tags": { "team": "api", "env": "test" },
            }),
        ),
    ]
}

/// Create `requests` as VPC links, in order, answering the ids in creation order.
async fn created_links(gateway: &Gateway, requests: &[(&str, Value)]) -> Vec<String> {
    let mut ids = Vec::new();
    for (case, request) in requests {
        let created = create_vpc_link(gateway, request.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {case}: expected 202 from CreateVpcLink, got {} with {:?}",
            created.status, created.body
        );
        ids.push(string_member(&created.body, "id", case));
    }
    ids
}

/// `DeleteVpcLink` for `vpc_link_id`, asserting the modelled 202.
async fn delete_link(gateway: &Gateway, vpc_link_id: &str) {
    let answer = gateway
        .call("DELETE", &format!("/vpclinks/{vpc_link_id}"), None)
        .await;
    assert_eq!(
        answer.status, 202,
        "case DeleteVpcLink({vpc_link_id}): expected 202, got {} with {:?}",
        answer.status, answer.body
    );
}

#[tokio::test]
async fn get_vpc_links_reports_every_stored_link_as_its_own_read_does() {
    let gateway = Gateway::new();

    // An account with no VPC link has an EMPTY collection, not a missing one.
    let empty = listed(&gateway, "").await;
    assert!(
        listed_ids(&empty, "GetVpcLinks(no links)").is_empty(),
        "case GetVpcLinks(no links): expected no items, got {empty:?}"
    );
    assert!(
        empty["position"].is_null(),
        "case GetVpcLinks(no links): the last page omits position, got {:?}",
        empty["position"]
    );

    let table = link_table();
    let created = created_links(&gateway, &table).await;

    // A link listed while it is still being provisioned reports a modelled provisioning
    // status; WHICH of the two the clock owes is the `vpc_link_provisioning` suite's rule.
    let fresh = listed(&gateway, "").await;
    for (index, item) in page_items(&fresh, "GetVpcLinks(fresh)").iter().enumerate() {
        let status = item["status"].as_str();
        assert!(
            status == Some(VPC_LINK_PENDING) || status == Some(VPC_LINK_AVAILABLE),
            "case GetVpcLinks(fresh) item {index}: expected a modelled provisioning \
             status, got {status:?}"
        );
    }

    // Past the provisioning span every link has settled, so the listed record and the
    // link's own read are compared whole rather than minus the member the clock moves.
    tokio::time::sleep(VPC_LINK_PROVISION_SPAN + MARGIN).await;

    let all = listed(&gateway, "").await;
    let ids = listed_ids(&all, "GetVpcLinks(all)");
    assert_eq!(
        ids, created,
        "case GetVpcLinks(all): expected the links in creation order {created:?}, got {ids:?}"
    );

    // Each listed link is the SAME record the link's own read reports — the collection
    // renders the stored record, it does not build a second rendering of it.
    let items = page_items(&all, "GetVpcLinks(all)");
    for ((case, request), (id, item)) in table.iter().zip(created.iter().zip(items.iter())) {
        let read = get_vpc_link(&gateway, id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetVpcLink 200, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            item, &read.body,
            "case {case}: the listed link must equal its own read {:?}, got {item:?}",
            read.body
        );
        // The members the create set are reported by the LISTING as the request set them.
        for member in ["name", "description", "targetArns", "tags"] {
            assert_eq!(
                item[member], request[member],
                "case {case}: expected the listed {member} {:?}, got {:?}",
                request[member], item[member]
            );
        }
    }

    // A listing with no `limit` reports the default page size at most.
    assert!(
        items.len() <= DEFAULT_PAGE_LIMIT,
        "case GetVpcLinks(all): a default page holds at most {DEFAULT_PAGE_LIMIT} items, got {}",
        items.len()
    );
}

#[tokio::test]
async fn get_vpc_links_pages_through_creation_order_on_position_and_limit() {
    let gateway = Gateway::new();
    let created = created_links(&gateway, &link_table()).await;
    let total = created.len();

    // Rows span the documented page-size range, its bounds and one past each bound: a
    // size below 1 and a size above the maximum are clamped into the range rather than
    // refused.
    for limit in [
        0,
        1,
        2,
        3,
        total as i64,
        total as i64 + 1,
        DEFAULT_PAGE_LIMIT as i64,
        MAX_PAGE_LIMIT,
        MAX_PAGE_LIMIT + 1,
        10_000,
    ] {
        let case = format!("GetVpcLinks(limit={limit})");
        let effective = usize::try_from(limit.clamp(1, MAX_PAGE_LIMIT)).unwrap_or(1);
        let mut seen: Vec<String> = Vec::new();
        let mut position: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &position {
                None => format!("limit={limit}"),
                Some(cursor) => format!("limit={limit}&position={cursor}"),
            };
            let body = listed(&gateway, &query).await;
            let page = listed_ids(&body, &case);
            assert!(
                page.len() <= effective,
                "case {case}: a page holds at most {effective} items, got {}",
                page.len()
            );
            seen.extend(page);
            pages += 1;
            assert!(
                pages <= total + 1,
                "case {case}: paging did not terminate after {pages} pages"
            );
            match body["position"].as_str() {
                None => break,
                Some(cursor) => position = Some(cursor.to_owned()),
            }
        }
        assert_eq!(
            seen, created,
            "case {case}: paging must reassemble creation order {created:?}, got {seen:?}"
        );
    }

    // Crossed with every cursor the collection hands out: a page resumes AT the position
    // its cursor names — the cursor a page hands back names the first link the page did NOT
    // report — so the page is the sequence from there on, and a cursor is handed back only
    // while the sequence holds more. The cursors are read off a one-at-a-time walk, because
    // a `position` is a continuation token this service mints rather than a link id a caller
    // reads off the page.
    let mut cursors: Vec<(usize, String)> = Vec::new();
    let mut reported = 0usize;
    let mut position: Option<String> = None;
    loop {
        let case = "GetVpcLinks(the cursors of the collection)".to_string();
        let query = match &position {
            None => "limit=1".to_string(),
            Some(cursor) => format!("limit=1&position={cursor}"),
        };
        let body = listed(&gateway, &query).await;
        reported += listed_ids(&body, &case).len();
        match body["position"].as_str() {
            None => break,
            Some(cursor) => {
                cursors.push((reported, cursor.to_owned()));
                position = Some(cursor.to_owned());
            }
        }
    }
    assert_eq!(
        cursors.len(),
        total - 1,
        "case GetVpcLinks(the cursors of the collection): expected a cursor for every index \
         past the first of {total}, got {cursors:?}"
    );
    for (start, cursor) in &cursors {
        let case = format!("GetVpcLinks(position=<the cursor of index {start}>, limit=2)");
        assert!(
            created.iter().all(|id| !cursor.contains(id.as_str())),
            "case {case}: a cursor spells no link's id, got {cursor:?}"
        );
        let body = listed(&gateway, &format!("position={cursor}&limit=2")).await;
        let page = listed_ids(&body, &case);
        let end = (start + 2).min(total);
        assert_eq!(
            page,
            created[*start..end].to_vec(),
            "case {case}: expected {:?}, got {page:?}",
            &created[*start..end]
        );
        let next = cursors
            .iter()
            .find(|(index, _)| *index == end)
            .map(|(_, cursor)| cursor.clone());
        assert_eq!(
            body["position"].as_str().map(str::to_owned),
            next,
            "case {case}: expected the cursor of index {end} ({next:?}), got {:?}",
            body["position"]
        );
    }

    // A value this service did not issue as a cursor names no position: a bad request
    // rather than an empty page — an unminted id, an id of another form, a link's NAME,
    // and a link's own id, since the cursor is the service's token rather than an
    // identifier a caller holds.
    let own_id = created[1].clone();
    for cursor in [
        "nosuchcursor",
        "0000000000",
        "collection-link-a",
        own_id.as_str(),
    ] {
        let answer = list_links(&gateway, &format!("position={cursor}")).await;
        assert_failure(
            &answer,
            400,
            "BadRequestException",
            &format!("GetVpcLinks(position={cursor:?})"),
        );
    }
}

#[tokio::test]
async fn the_collection_holds_exactly_the_links_the_account_still_has() {
    let gateway = Gateway::new();
    let created = created_links(&gateway, &link_table()).await;

    // Rows differ along the way a request falls outside what the service stores: no
    // target, a target that is not an ARN, and an empty name. None of them joins the
    // collection.
    let refused: Vec<(&str, Value)> = vec![
        ("no target at all", json!({ "name": "unstored-a", "targetArns": [] })),
        (
            "a target that is not an ARN",
            json!({ "name": "unstored-b", "targetArns": ["not-an-arn"] }),
        ),
        (
            "an empty name",
            json!({ "name": "", "targetArns": [target_arn(0)] }),
        ),
    ];
    for (case, request) in &refused {
        assert_failure(
            &create_vpc_link(&gateway, request.clone()).await,
            400,
            "BadRequestException",
            case,
        );
        let ids = listed_ids(&listed(&gateway, "").await, case);
        assert_eq!(
            ids, created,
            "case {case}: a refused create adds nothing to the collection, got {ids:?}"
        );
    }

    // A deleted link LEAVES the collection and the links around it keep their place: the
    // first, one in the middle and the last are deleted in turn, each time against the
    // sequence that is left.
    let mut remaining = created.clone();
    for which in ["the first", "one in the middle", "the last"] {
        let index = match which {
            "the first" => 0,
            "the last" => remaining.len() - 1,
            _ => remaining.len() / 2,
        };
        let gone = remaining.remove(index);
        delete_link(&gateway, &gone).await;
        let case = format!("GetVpcLinks after deleting {which} ({gone})");
        let ids = listed_ids(&listed(&gateway, "").await, &case);
        assert_eq!(
            ids, remaining,
            "case {case}: expected {remaining:?}, got {ids:?}"
        );
    }
}

#[tokio::test]
async fn listing_vpc_links_does_not_change_what_a_later_caller_sees() {
    let gateway = Gateway::new();
    let created = created_links(&gateway, &link_table()).await;
    // The provisioning is a transition the CLOCK owes, so this LOOKS AGAIN until every link
    // reports itself settled (the suite family's one poll, `settled_link`) rather than
    // waiting a span out: once they have settled, the only member the clock could move is
    // already where it stays and two listings must be identical.
    for id in &created {
        let _ = settled_link(&gateway, id, "GetVpcLinks(the links settling)").await;
    }

    let before = listed(&gateway, "").await;
    // The cursors a walk of one at a time hands out, which are the only `position` values a
    // caller can send: the first page's and the one that resumes at the last link.
    let first_page = listed(&gateway, "limit=1").await;
    let after_first = first_page["position"]
        .as_str()
        .unwrap_or_else(|| panic!("case GetVpcLinks(limit=1): expected a cursor, got {first_page:?}"))
        .to_string();
    let before_last = listed(&gateway, &format!("limit={}", created.len() - 1)).await;
    let at_last = before_last["position"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case GetVpcLinks(all but the last): expected a cursor, got {before_last:?}")
        })
        .to_string();
    // Every shape of read this collection serves: the whole sequence, a page of it, a page
    // from a cursor, the last page, and a page past the published maximum size.
    let queries = [
        String::new(),
        "limit=1".to_string(),
        format!("limit=2&position={after_first}"),
        format!("position={at_last}"),
        format!("limit={}", MAX_PAGE_LIMIT + 1),
    ];
    for query in &queries {
        let _ = list_links(&gateway, query).await;
    }
    for id in &created {
        let _ = get_vpc_link(&gateway, id).await;
    }
    let after = listed(&gateway, "").await;
    assert_eq!(
        after, before,
        "case GetVpcLinks(after reads): a read must change nothing, got {after:?}"
    );

    // And the per-link read still reports the same record the listing does, so no read
    // above consumed or moved the link it reported.
    for (id, item) in created.iter().zip(page_items(&after, "GetVpcLinks(after)")) {
        let read = get_vpc_link(&gateway, id).await;
        assert_eq!(
            read.body, item,
            "case GetVpcLink({id}) after the listings: expected the listed record {item:?}, \
             got {:?}",
            read.body
        );
    }
}
