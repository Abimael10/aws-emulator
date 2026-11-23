// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the account's custom-domain listing answers when its page
//! holds no record at all.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns.
//!
//! The RULE under test, over a SET of inputs rather than one recorded call: a 200 listing
//! of a `position`/`limit` collection reports "the current page of elements from this
//! collection" (`smithy_query(query="doc", shape="DomainNames", member="items")`) as a
//! member that is always THERE — the empty list where the page holds no element — so a
//! client reads "nothing is listed" off a list it can count rather than off an absent
//! member, and the answer does not change shape with what the collection happens to hold.
//! An "is this record gone yet?" poll, which is how a caller confirms a delete, is written
//! against exactly that: it walks the pages and reads the page member on each one
//! unconditionally.
//!
//! The rows span the ways the page of this collection comes to hold nothing while the
//! listing still resolves — an account that never held a custom domain name, one whose only
//! record was deleted, one whose several records were all deleted, and an account holding
//! records whose listing is NARROWED to none by the `resourceOwner` filter — crossed with
//! the page sizes the `limit` member publishes and the request that asks for no size at
//! all. The records a row creates and deletes span the endpoint types a custom domain name
//! is served by, because nothing a record carried conditions the answer the emptied
//! collection makes.
//!
//! The sibling `collection_page_boundaries` suite reads this collection on an account that
//! never held a record as one row of its table over every listing of this service, and
//! `emptied_collection_page_member` drives the emptied answer of the collections that do
//! not hang off a custom domain name; this suite is the custom-domain listing's own, the
//! collection a client polls to watch a domain name disappear.
//!
//! [`Gateway`] and [`page_items`] are this crate's one wire-test harness, reused rather
//! than standing a second emulator up another way; `created_domain` is the domain-name
//! suite's one create of a custom domain name, `deleted` the one delete of a collection's
//! last element, and `empty_page_body` this crate's one spelling of the body an empty page
//! answers with.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    PRIVATE_ENDPOINT_TYPE, REGIONAL_ENDPOINT_TYPE,
};

use crate::domain_name_collection::created_domain;
use crate::emptied_collection_page_member::{deleted, empty_page_body};
use crate::rest_api_lifecycle::{page_items, string_member, Gateway};

/// The listing this suite reads, as [`empty_page_body`] names the collections of this
/// service.
const LISTING: &str = "GetDomainNames";

/// The page sizes a read asks this collection for: none at all, the smallest a page may be,
/// the documented default and the published maximum — "The maximum number of returned
/// results per page. The default value is 25 and the maximum value is 500"
/// (`smithy_query(query="doc", shape="GetDomainNamesRequest", member="limit")`). A page a
/// collection holds nothing for cannot be filled at any of them.
const PAGE_SIZES: [Option<i32>; 4] = [None, Some(1), Some(25), Some(500)];

/// One custom domain name a row stands up, as the row then deletes it: the name every
/// operation takes in its path label, and the `domainNameId` a PRIVATE record is addressed
/// by as well.
struct Created {
    domain_name: String,
    domain_name_id: Option<String>,
}

impl Created {
    /// The URI that deletes exactly this record.
    fn uri(&self) -> String {
        match &self.domain_name_id {
            Some(id) => format!("/domainnames/{}?domainNameId={id}", self.domain_name),
            None => format!("/domainnames/{}", self.domain_name),
        }
    }
}

/// `CreateDomainName` for `domain_name` carrying `body`, and the identifiers the stored
/// record is addressed by.
async fn create(gateway: &Gateway, domain_name: &str, body: Value, case: &str) -> Created {
    let record = created_domain(gateway, domain_name, body).await;
    assert_eq!(
        string_member(&record, "domainName", case),
        domain_name,
        "case {case}: expected the created record to carry the name it was created under, \
         got {record:?}"
    );
    Created {
        domain_name: domain_name.to_owned(),
        domain_name_id: record["domainNameId"].as_str().map(str::to_owned),
    }
}

/// `GetDomainNames` with `filter` and `limit` as a client asks for them, and the answered
/// body — asserting only the status the model declares for a success, so each test reads
/// the page member itself.
async fn listed(gateway: &Gateway, filter: Option<&str>, limit: Option<i32>, case: &str) -> Value {
    let mut query: Vec<String> = Vec::new();
    if let Some(filter) = filter {
        query.push(filter.to_owned());
    }
    if let Some(limit) = limit {
        query.push(format!("limit={limit}"));
    }
    let uri = if query.is_empty() {
        "/domainnames".to_string()
    } else {
        format!("/domainnames?{}", query.join("&"))
    };
    let answer = gateway.call("GET", &uri, None).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from GET {uri}, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The records a row creates: the case, the name, and the create body that gives the record
/// its endpoint type.
fn records() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "a regional record",
            "regional.empty-listing.example.com",
            json!({ "endpointConfiguration": { "types": [REGIONAL_ENDPOINT_TYPE] } }),
        ),
        (
            "an edge-optimized record naming no endpoint type",
            "edge.empty-listing.example.com",
            json!({}),
        ),
        (
            "a private record, addressed by its id as well",
            "private.empty-listing.example.org",
            json!({ "endpointConfiguration": { "types": [PRIVATE_ENDPOINT_TYPE] } }),
        ),
    ]
}

/// How one row's page comes to hold no record.
struct Row {
    case: &'static str,
    /// How many of [`records`] the row creates before it reads the listing.
    created: usize,
    /// Whether the row deletes every record it created again.
    emptied: bool,
    /// The query that narrows the listing to no record, for the row that empties its page
    /// with a filter rather than with a delete.
    filter: Option<&'static str>,
}

/// Every way the page of this collection holds nothing while the listing still resolves.
///
/// The `resourceOwner` row is the filter half: the modelled values are `SELF` and
/// `OTHER_ACCOUNTS` (`smithy_query(query="shape", shape="ResourceOwner")`), and a custom
/// domain name of ANOTHER account is visible to this one only through a domain name access
/// association, so a listing narrowed to `OTHER_ACCOUNTS` selects none of the records this
/// account owns.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an account that never held a custom domain name",
            created: 0,
            emptied: false,
            filter: None,
        },
        Row {
            case: "an account whose only custom domain name was deleted",
            created: 1,
            emptied: true,
            filter: None,
        },
        Row {
            case: "an account whose every custom domain name was deleted",
            created: 3,
            emptied: true,
            filter: None,
        },
        Row {
            case: "an account holding records, listed for another account's",
            created: 3,
            emptied: false,
            filter: Some("resourceOwner=OTHER_ACCOUNTS"),
        },
        Row {
            case: "an emptied account, listed for the records it owns itself",
            created: 2,
            emptied: true,
            filter: Some("resourceOwner=SELF"),
        },
    ]
}

/// Assert `body` is the whole answer an empty page of this collection makes: the page member
/// PRESENT and empty, and nothing beside it — no cursor to carry on at and no page-size
/// echo, because a page of nothing pages nowhere.
fn assert_empty_page(body: &Value, case: &str) {
    let member = body.get("item");
    assert!(
        member.is_some_and(|item| item.as_array().is_some_and(Vec::is_empty)),
        "case {case}: expected the page member present and empty, got {body:?}"
    );
    let expected = empty_page_body(LISTING);
    assert_eq!(
        body, &expected,
        "case {case}: expected {expected:?}, got {body:?}"
    );
}

#[tokio::test]
async fn a_custom_domain_listing_whose_page_holds_nothing_reports_the_empty_page_member() {
    for row in rows() {
        // One emulator per row, so what the listing reports is this row's own doing.
        let gateway = Gateway::new();
        let mut created: Vec<Created> = Vec::new();
        for (case, domain_name, body) in records().into_iter().take(row.created) {
            let case = format!("{}: {case}", row.case);
            created.push(create(&gateway, domain_name, body, &case).await);
        }
        if row.emptied {
            for record in &created {
                deleted(&gateway, &record.uri(), row.case).await;
            }
        }

        for limit in PAGE_SIZES {
            let case = format!("{} (limit={limit:?}, filter={:?})", row.case, row.filter);
            let body = listed(&gateway, row.filter, limit, &case).await;
            assert_empty_page(&body, &case);
        }
    }
}

#[tokio::test]
async fn polling_the_custom_domain_listing_through_a_delete_reads_a_page_member_every_time() {
    // The poll a client writes to confirm a delete: read the listing, read its page member,
    // and look again until the record it deleted is not among the names — including the
    // read taken once the last record is gone, where the page member has to be there to be
    // read at all.
    let gateway = Gateway::new();
    let mut created: Vec<Created> = Vec::new();
    for (case, domain_name, body) in records() {
        created.push(create(&gateway, domain_name, body, case).await);
    }

    let mut remaining: Vec<String> = created
        .iter()
        .map(|record| record.domain_name.clone())
        .collect();
    for record in &created {
        let case = format!("after deleting {}", record.domain_name);
        deleted(&gateway, &record.uri(), &case).await;
        remaining.retain(|name| name != &record.domain_name);

        let body = listed(&gateway, None, Some(500), &case).await;
        let listed_names: Vec<String> = page_items(&body, &case)
            .iter()
            .map(|item| string_member(item, "domainName", &case))
            .collect();
        assert!(
            body.get("item").is_some_and(Value::is_array),
            "case {case}: expected the page member present, got {body:?}"
        );
        assert_eq!(
            listed_names, remaining,
            "case {case}: expected the records that are left {remaining:?}, got {listed_names:?}"
        );
    }

    // The collection is empty again, and the listing says so with a list that counts zero
    // rather than with an answer that leaves the member out.
    let body = listed(&gateway, None, Some(500), "the last record deleted").await;
    assert_empty_page(&body, "the last record deleted");
}
