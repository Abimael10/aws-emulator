// SPDX-License-Identifier: CC0-1.0

//! The `VpcLinkStatusMessage` a VPC link reports does not depend on HOW the collection it
//! is listed from was paged.
//!
//! RULE: **a VPC link's status message is the service's fixed sentence for the
//! `VpcLinkStatus` the link reports at the same instant** — one sentence per status, the
//! same for every link in that status, interpolating nothing from the request or from the
//! link's own record — so every reader of the link reports the same pair: the single-link
//! reader, and the collection reader on whatever page the link happens to fall, under
//! whatever page size the caller asked for, however many links the account holds and however
//! many times the collection is walked. A provisioned link's sentence is pinned verbatim, to
//! the one the service is known to serve; the status a link is still being provisioned in is
//! held to the invariants above instead, because no sentence of the service's has been
//! observed for it to pin.
//!
//! What this suite adds to the sibling suites that assert the same rule from the
//! single-link readers (`apigateway_vpc_link_status_message_surfaces`, and each API
//! Gateway crate's own) is the PAGED list path: `GetVpcLinks` is bound to `maxResults` and
//! `nextToken`, so the item a client reads can come off any page of a multi-page walk, and
//! a message derived from the stored status has to be the same on all of them.
//!
//! The rows differ along what the rule generalises over: the page SIZE the walk asks for
//! (one item per page, two, three, and none at all — the whole collection in one answer),
//! the position a link falls at within its page, how many subnets and security groups a
//! link carries, whether it is tagged, the length of its name, and how often the collection
//! is walked. The recorded case that filed the issue — a settled link read off a listing —
//! is one row of the `maxResults`-free walk.
//!
//! This suite lives beside the router because the subnets a link attaches to are Amazon
//! EC2's resources: they are minted by EC2's own operations on the SAME engine that serves
//! API Gateway v2, so every accepted create names an id EC2 actually handed out.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigatewayv2_vpc_link_subnets_resolve_in_ec2::{
    apigateway, ec2_vpc, emulator, VPC_LINKS,
};
use crate::query_wire::form_encode;

/// The sentence a provisioned VPC link reports.
///
/// Spelled out here rather than taken from the service crate's own constant: a test that
/// asserts a handler's constant back at it cannot catch the wording drifting away from the
/// service's. It is the one sentence the service is known to serve, so the provisioned
/// status is pinned to it; the status a link is still being provisioned in is held to the
/// rule's invariants instead, because this suite has no wording of the service's to pin it
/// to.
const AVAILABLE_SENTENCE: &str = "VPC link is ready to route traffic";

/// The `VpcLinkStatus` a link carries while it is being provisioned.
const PENDING: &str = "PENDING";

/// The `VpcLinkStatus` a link carries once it has been provisioned.
const AVAILABLE: &str = "AVAILABLE";

/// How long a poll waits for the provisioning the CLOCK owes before the run is called a
/// defect, and how long it waits between reads. Generous: the transition is polled for
/// rather than waited out, and the deadline is only what stops a test whose premise never
/// arrives.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// A cap on the pages one walk may fetch, so a walk whose token never clears fails with a
/// message instead of running forever. Far above the pages the rows below can produce.
const MAX_PAGES: usize = 64;

/// One string member of a projection.
fn member(case: &str, reader: &str, projection: &Value, name: &str) -> String {
    projection
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?} ({reader}): expected a string {name:?}, got {projection}")
        })
        .to_string()
}

/// Assert the status message `projection` carries summarises a status at all and carries
/// nothing of the request or of the record, and answer the status/message pair it reports.
fn pair(
    case: &str,
    reader: &str,
    projection: &Value,
    carries_nothing_of: &[String],
) -> (String, String) {
    let status = member(case, reader, projection, "vpcLinkStatus");
    let message = member(case, reader, projection, "vpcLinkStatusMessage");
    assert!(
        !message.is_empty(),
        "case {case:?} ({reader}): expected a VpcLinkStatusMessage summarising VpcLinkStatus \
         {status:?}, got {projection}"
    );
    for value in carries_nothing_of {
        assert!(
            !message.contains(value.as_str()),
            "case {case:?} ({reader}): expected the sentence for {status:?} to carry nothing of \
             the request or the record, but {message:?} carries {value:?}"
        );
    }
    (status, message)
}

/// One VPC link the rule is asserted over: the create request, and the values of that
/// request no status sentence may carry.
struct Row {
    case: &'static str,
    request: Value,
    carries_nothing_of: Vec<String>,
}

/// The rows, written against the subnets EC2 holds.
fn rows(subnets: &[String]) -> Vec<Row> {
    // `VpcLink$Name` targets `StringWithLengthBetween1And128`, so this is a link named at
    // the modelled upper bound: the size of a link's own members changes nothing about the
    // sentence for its status.
    let longest_name = "n".repeat(128);
    vec![
        Row {
            case: "the required members only: one subnet",
            request: json!({ "name": "paged-minimal", "subnetIds": [subnets[0]] }),
            carries_nothing_of: vec!["paged-minimal".to_string(), subnets[0].clone()],
        },
        Row {
            case: "two subnets and one security group",
            request: json!({
                "name": "paged-two-subnets",
                "subnetIds": [subnets[0], subnets[1]],
                "securityGroupIds": ["sg-0123456789abcdef0"],
            }),
            carries_nothing_of: vec![
                "paged-two-subnets".to_string(),
                subnets[1].clone(),
                "sg-0123456789abcdef0".to_string(),
            ],
        },
        Row {
            case: "three subnets, two security groups, and tags",
            request: json!({
                "name": "paged-multi-az",
                "subnetIds": subnets,
                "securityGroupIds": ["sg-0a1b2c3d", "sg-00112233445566778"],
                "tags": { "team": "platform", "env": "canary" },
            }),
            carries_nothing_of: vec![
                "paged-multi-az".to_string(),
                subnets[2].clone(),
                "sg-00112233445566778".to_string(),
                "platform".to_string(),
                "canary".to_string(),
            ],
        },
        Row {
            case: "a name at the modelled 128-character bound",
            request: json!({ "name": longest_name, "subnetIds": [subnets[0]] }),
            carries_nothing_of: vec![longest_name.clone()],
        },
        Row {
            case: "a fifth link, so a page size of two leaves a part-full last page",
            request: json!({
                "name": "paged-odd-one-out",
                "subnetIds": [subnets[1], subnets[2]],
                "tags": { "owner": "listing" },
            }),
            carries_nothing_of: vec![
                "paged-odd-one-out".to_string(),
                "listing".to_string(),
            ],
        },
    ]
}

/// One created link: the row that created it, its minted id, and every value no sentence of
/// its projections may carry.
struct Link {
    case: &'static str,
    id: String,
    carries_nothing_of: Vec<String>,
}

/// Read `GetVpcLink` until it reports the link has been provisioned, and answer that
/// reading.
///
/// The provisioning is a transition the CLOCK owes, so this LOOKS AGAIN until the record
/// reports it rather than waiting the span out: nothing is nudged between reads.
async fn settled(router: &MultiServiceRouter, link: &Link) -> Value {
    let uri = format!("{VPC_LINKS}/{}", form_encode(&link.id));
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = apigateway(router, "GET", &uri, None).await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: expected 200 from GetVpcLink({}), got {} with {}",
            link.case,
            link.id,
            read.status,
            read.body
        );
        let (status, _) = pair(link.case, "GetVpcLink", &read.body, &link.carries_nothing_of);
        if status == AVAILABLE {
            return read.body;
        }
        assert_eq!(
            status, PENDING,
            "case {:?}: a link over subnets the account holds reaches {AVAILABLE} through \
             {PENDING} only, got {}",
            link.case, read.body
        );
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the clock to settle {} out of {PENDING}, last read {}",
            link.case,
            link.id,
            read.body
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// Walk the whole VPC-link collection with `GetVpcLinks`, asking for pages of
/// `max_results` items (`None` asks for no page size at all), and answer every item it
/// reported together with how many pages the walk took.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    max_results: Option<usize>,
) -> (Vec<Value>, usize) {
    let mut items: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = max_results {
            query.push(format!("maxResults={size}"));
        }
        if let Some(cursor) = &token {
            query.push(format!("nextToken={}", form_encode(cursor)));
        }
        let uri = if query.is_empty() {
            VPC_LINKS.to_string()
        } else {
            format!("{VPC_LINKS}?{}", query.join("&"))
        };
        let page = apigateway(router, "GET", &uri, None).await;
        assert_eq!(
            page.status,
            200,
            "case {case:?}: expected 200 from GetVpcLinks({uri}), got {} with {}",
            page.status,
            page.body
        );
        let listed = page
            .body
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected GetVpcLinks({uri}) to report a collection, got {}",
                    page.body
                )
            });
        if let Some(size) = max_results {
            assert!(
                listed.len() <= size,
                "case {case:?}: expected at most {size} item(s) on a page, got {} from {uri}",
                listed.len()
            );
        }
        pages += 1;
        items.extend(listed);
        token = page
            .body
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return (items, pages);
        }
        assert!(
            pages < MAX_PAGES,
            "case {case:?}: a paged walk of the collection must terminate, still paging after \
             {pages} pages"
        );
    }
}

/// RULE: the sentence a provisioned link reports is the service's one for that status, from
/// the single-link reader and from every page of every paged listing alike.
#[tokio::test]
async fn a_listed_vpc_link_reports_one_sentence_whatever_page_it_falls_on() {
    let (router, _engine) = emulator();
    let vpc = ec2_vpc(&router, 85, &["us-west-2a", "us-west-2b", "us-west-2c"]).await;

    // Create every row. A link is still being provisioned when its create answers, so the
    // creates also give the sentence for THAT status: one for every link, whatever each
    // link carries.
    let mut links: Vec<Link> = Vec::new();
    let mut pending_sentence: Option<String> = None;
    for row in rows(&vpc.subnets) {
        let case = row.case;
        let created = apigateway(&router, "POST", VPC_LINKS, Some(row.request.clone())).await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: expected 201 from CreateVpcLink, got {} with {}",
            created.status,
            created.body
        );
        let id = member(case, "CreateVpcLink", &created.body, "vpcLinkId");
        // Nothing of the request, and nothing of the record, may be interpolated into a
        // sentence — the minted id included.
        let mut carries_nothing_of = row.carries_nothing_of.clone();
        carries_nothing_of.push(id.clone());

        let (status, message) = pair(case, "CreateVpcLink", &created.body, &carries_nothing_of);
        assert_eq!(
            status, PENDING,
            "case {case:?}: expected CreateVpcLink to report VpcLinkStatus {PENDING}, got {}",
            created.body
        );
        match &pending_sentence {
            Some(first) => assert_eq!(
                &message, first,
                "case {case:?}: expected every link in {PENDING} to report the one sentence \
                 {first:?}, got {message:?}"
            ),
            None => pending_sentence = Some(message),
        }
        links.push(Link {
            case,
            id,
            carries_nothing_of,
        });
    }

    // Read each link to the status it settles in, and keep the sentence its own reader
    // reported: that is what every page of every listing below is held to.
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    for link in &links {
        let read = settled(&router, link).await;
        let (status, message) = pair(link.case, "GetVpcLink", &read, &link.carries_nothing_of);
        assert_eq!(
            (status.as_str(), message.as_str()),
            (AVAILABLE, AVAILABLE_SENTENCE),
            "case {:?}: expected a provisioned link to report {AVAILABLE_SENTENCE:?} for \
             {AVAILABLE}, got {read}",
            link.case
        );
        by_id.insert(link.id.clone(), message);
    }

    // No sentence on any page may carry a value of ANY row's request or record, whichever
    // link the page reports.
    let carries_nothing_of: Vec<String> = links
        .iter()
        .flat_map(|link| link.carries_nothing_of.clone())
        .collect();

    // Every page size, walked twice: a sentence derived from the stored status is the same
    // on the second walk as on the first, so no reading of the collection reworded or
    // consumed it.
    for round in 0..2 {
        for max_results in [Some(1), Some(2), Some(3), None] {
            let case = match max_results {
                Some(size) => format!("walk {round} of the collection in pages of {size}"),
                None => format!("walk {round} of the collection with no page size asked for"),
            };
            let (items, pages) = walk(&router, &case, max_results).await;

            // The walk reported the whole account, each link exactly once.
            let mut seen: BTreeMap<String, String> = BTreeMap::new();
            for item in &items {
                let id = member(&case, "GetVpcLinks", item, "vpcLinkId");
                let (status, message) = pair(&case, "GetVpcLinks", item, &carries_nothing_of);
                assert_eq!(
                    (status.as_str(), message.as_str()),
                    (AVAILABLE, AVAILABLE_SENTENCE),
                    "case {case:?}: expected the listed link {id} to report \
                     {AVAILABLE_SENTENCE:?} for {AVAILABLE}, got {item}"
                );
                let expected = by_id.get(&id).unwrap_or_else(|| {
                    panic!("case {case:?}: GetVpcLinks listed {id}, which no row created")
                });
                assert_eq!(
                    &message, expected,
                    "case {case:?}: expected the listed link {id} to summarise its status as \
                     its own reader does ({expected:?}), got {message:?}"
                );
                assert!(
                    seen.insert(id.clone(), message).is_none(),
                    "case {case:?}: expected a paged walk to report {id} once, got it twice"
                );
            }
            assert_eq!(
                seen.keys().cloned().collect::<Vec<_>>(),
                by_id.keys().cloned().collect::<Vec<_>>(),
                "case {case:?}: expected the walk to report every link the account holds"
            );

            // The asked-for page size really did split the collection, so the items above
            // were read off several pages rather than all off one.
            if let Some(size) = max_results {
                let expected_pages = by_id.len().div_ceil(size);
                assert_eq!(
                    pages, expected_pages,
                    "case {case:?}: expected {} link(s) to take {expected_pages} page(s) of \
                     {size}, got {pages}",
                    by_id.len()
                );
            } else {
                assert_eq!(
                    pages, 1,
                    "case {case:?}: expected the collection to fit one answer, got {pages} pages"
                );
            }
        }
    }

    // A link being provisioned and a provisioned one are summarised by sentences of their
    // own, so the message MOVED with the status rather than standing still.
    let pending_sentence = pending_sentence.expect("the rows created at least one link");
    assert_ne!(
        pending_sentence, AVAILABLE_SENTENCE,
        "expected a link being provisioned to be summarised by a sentence of its own, got \
         {pending_sentence:?}"
    );
}
