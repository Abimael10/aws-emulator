// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListAuthorizers`: the paginated reader over the account's
//! custom-authorizer store, driven against the `CreateAuthorizer` / `DescribeAuthorizer`
//! / `UpdateAuthorizer` / `DeleteAuthorizer` helpers of the parent suite, so a listing is
//! compared with what the single-authorizer reader reports for the same name.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `GET /authorizers` request signed for the `iot` signing name — so what is asserted is
//! what an AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * the listing reports ONE PAGE of the records the store holds, each projected from the
//!   record a create persisted as the two members `AuthorizerSummary` declares
//!   (`authorizerName`, `authorizerArn` —
//!   `smithy_query(query="shape",shape="AuthorizerSummary")`) and nothing beside them; an
//!   account holding none is answered an empty list rather than an absent one;
//! * the order is the authorizer NAME, ascending when `ascendingOrder` asks for it and
//!   descending otherwise — "Return the list of authorizers in ascending alphabetical
//!   order", with the modelled `default false`
//!   (`smithy_query(query="operation",operation="ListAuthorizers")`);
//! * `status` selects exactly the authorizers holding that status, so the two values
//!   `AuthorizerStatus` admits PARTITION the unfiltered listing;
//! * `pageSize` bounds one page and `nextMarker` resumes the SAME listing exactly where
//!   the page stopped, so walking the marker chain visits every authorizer once and the
//!   last page carries no marker; a marker this listing did not issue — one of the other
//!   direction, of another filter, or of another resource's listing — is refused and lists
//!   nothing;
//! * the listing projects the store as it IS and changes nothing a later caller sees: a
//!   deleted authorizer is listed no more, an updated one keeps its place, and two
//!   identical reads answer identically.
//!
//! The tables span what the rules generalise over rather than one recorded request: the
//! forms `ascendingOrder` takes on the wire (absent, false, true), names whose order is
//! not their creation order and where one name is another's prefix, both statuses, every
//! page size from one to past the number of authorizers held plus the modelled maximum,
//! and the number of authorizers the store holds from none upward.

use super::*;

use service_com_amazonaws_iot_awsiotservice::state::MAX_LIST_PAGE_SIZE;

/// The authorizers each table below creates, in the order they are created — which is
/// NEITHER the ascending nor the descending name order, so an order that holds by
/// accident is caught. The names span what the ordering generalises over: a name that is
/// another name's prefix, the character classes `AuthorizerName` admits (modelled
/// `^[\w=,@-]+$`), upper and lower case, and a leading digit.
const NAMES: [&str; 6] = ["beta", "alpha-2", "Alpha", "gamma=9,x", "alpha", "0_zero"];

/// The status each created authorizer carries, by its position in [`NAMES`]: both values
/// `AuthorizerStatus` admits (`smithy_query(query="shape",shape="AuthorizerStatus")`), so
/// the `status` filter is checked over a store holding each of them.
fn status_of(name: &str) -> &'static str {
    let position = NAMES
        .iter()
        .position(|held| *held == name)
        .expect("every authorizer under test is one of NAMES");
    if position % 2 == 0 {
        "ACTIVE"
    } else {
        "INACTIVE"
    }
}

/// Every value of the modelled `AuthorizerStatus`, which the partition case spans.
const STATUSES: [&str; 2] = ["ACTIVE", "INACTIVE"];

/// The three forms `ascendingOrder` takes on the wire, and the order each one asks for.
#[derive(Debug, Clone, Copy)]
enum Order {
    /// The member is absent: the modelled `default false`, so descending.
    Absent,
    /// `isAscendingOrder=false`.
    Descending,
    /// `isAscendingOrder=true`.
    Ascending,
}

impl Order {
    fn wire_value(self) -> Option<bool> {
        match self {
            Self::Absent => None,
            Self::Descending => Some(false),
            Self::Ascending => Some(true),
        }
    }

    fn is_ascending(self) -> bool {
        matches!(self, Self::Ascending)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Absent => "no isAscendingOrder",
            Self::Descending => "isAscendingOrder=false",
            Self::Ascending => "isAscendingOrder=true",
        }
    }
}

/// Every form of the order member, which each table below spans.
const ORDERS: [Order; 3] = [Order::Absent, Order::Descending, Order::Ascending];

/// The names `held` must be reported in, in `order`: the name decides the order and
/// `ascendingOrder` its direction.
fn expected_names(held: &[&str], order: Order) -> Vec<String> {
    let mut sorted: Vec<String> = held.iter().map(|name| (*name).to_string()).collect();
    sorted.sort();
    if !order.is_ascending() {
        sorted.reverse();
    }
    sorted
}

impl Emulator {
    /// `ListAuthorizers`: `GET /authorizers`, with every input member bound as an
    /// `@httpQuery` parameter (`ascendingOrder` under the wire name `isAscendingOrder`).
    async fn list_authorizers(
        &self,
        order: Order,
        page_size: Option<i32>,
        marker: Option<&str>,
        status: Option<&str>,
    ) -> Reply {
        let mut query: Vec<String> = Vec::new();
        if let Some(ascending) = order.wire_value() {
            query.push(format!("isAscendingOrder={ascending}"));
        }
        if let Some(page_size) = page_size {
            query.push(format!("pageSize={page_size}"));
        }
        if let Some(marker) = marker {
            query.push(format!("marker={marker}"));
        }
        if let Some(status) = status {
            query.push(format!("status={status}"));
        }
        let path = if query.is_empty() {
            "/authorizers".to_string()
        } else {
            format!("/authorizers?{}", query.join("&"))
        };
        self.call("GET", &path, None, REGION).await
    }

    /// The authorizers one listing reports, walked to exhaustion: the names in the order
    /// they were reported, and the length of each page. The deadline is the number of
    /// pages, not a clock: a listing of `held` authorizers cannot take more than `held + 1`
    /// pages, so a pager that fails to advance is caught instead of looping.
    async fn walk_authorizers(
        &self,
        order: Order,
        page_size: Option<i32>,
        status: Option<&str>,
        held: usize,
    ) -> (Vec<String>, Vec<usize>) {
        let label = order.label();
        let mut names: Vec<String> = Vec::new();
        let mut page_lengths: Vec<usize> = Vec::new();
        let mut marker: Option<String> = None;
        for page_number in 0..=held.saturating_add(1) {
            let page = self
                .list_authorizers(order, page_size, marker.as_deref(), status)
                .await;
            assert_eq!(
                page.status, 200,
                "case {label:?} pageSize={page_size:?} status={status:?} page {page_number}: \
                 the listing must succeed, got {page:?}"
            );
            let reported = page.listed_names();
            page_lengths.push(reported.len());
            names.extend(reported);
            marker = page.next_marker();
            if marker.is_none() {
                return (names, page_lengths);
            }
        }
        panic!(
            "case {label:?} pageSize={page_size:?} status={status:?}: the listing never ran out \
             of pages over {held} authorizers, reported {names:?}"
        );
    }
}

impl Reply {
    /// The reported `authorizers` list, or `None` when the body carries no list.
    fn listed(&self) -> Option<&Vec<Value>> {
        self.body.get("authorizers").and_then(Value::as_array)
    }

    /// The `authorizerName`s the page reports, in the order it reports them.
    fn listed_names(&self) -> Vec<String> {
        self.listed()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        entry
                            .get("authorizerName")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `nextMarker` the page carries, when it carries one.
    fn next_marker(&self) -> Option<String> {
        self.text("nextMarker")
    }
}

/// Create every authorizer of `names`, each with the status [`status_of`] gives it, and
/// prove each one is readable before any listing is asserted on.
async fn create_authorizers(emulator: &Emulator, names: &[&str]) {
    for name in names {
        let created = emulator
            .create(
                name,
                &json!({
                    "authorizerFunctionArn":
                        format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:authorize"),
                    "status": status_of(name),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: CreateAuthorizer must succeed, got {created:?}"
        );
    }
}

#[tokio::test]
async fn list_authorizers_reports_every_authorizer_in_name_order() {
    let emulator = Emulator::new();
    create_authorizers(&emulator, &NAMES).await;

    for order in ORDERS {
        let label = order.label();
        let page = emulator.list_authorizers(order, None, None, None).await;
        assert_eq!(
            page.status, 200,
            "case {label:?}: the listing must succeed, got {page:?}"
        );
        assert_eq!(
            page.listed_names(),
            expected_names(&NAMES, order),
            "case {label:?}: expected the authorizers in alphabetical name order, got {page:?}"
        );
        // The store holds fewer authorizers than one page admits, so nothing remains.
        assert!(
            page.next_marker().is_none(),
            "case {label:?}: a listing that returned every authorizer carries no nextMarker, \
             got {page:?}"
        );
    }
}

#[tokio::test]
async fn an_account_holding_no_authorizer_lists_none_and_no_marker() {
    let emulator = Emulator::new();

    for order in ORDERS {
        for status in [None, Some("ACTIVE"), Some("INACTIVE")] {
            let label = order.label();
            let page = emulator.list_authorizers(order, None, None, status).await;
            assert_eq!(
                page.status, 200,
                "case {label:?} status={status:?}: listing an empty store must succeed, \
                 got {page:?}"
            );
            assert_eq!(
                page.listed_names(),
                Vec::<String>::new(),
                "case {label:?} status={status:?}: an account holding no authorizer reports \
                 none, got {page:?}"
            );
            assert!(
                page.listed().is_some(),
                "case {label:?} status={status:?}: the empty listing carries an authorizers \
                 list, got {page:?}"
            );
            assert!(
                page.next_marker().is_none(),
                "case {label:?} status={status:?}: nothing remains after an empty page, \
                 got {page:?}"
            );
        }
    }
}

#[tokio::test]
async fn each_entry_is_the_stored_summary_and_carries_nothing_else() {
    let emulator = Emulator::new();
    create_authorizers(&emulator, &NAMES).await;

    let page = emulator
        .list_authorizers(Order::Ascending, None, None, None)
        .await;
    let entries = page
        .listed()
        .unwrap_or_else(|| panic!("the listing must report an authorizers list, got {page:?}"))
        .clone();
    assert_eq!(
        entries.len(),
        NAMES.len(),
        "expected {} entries, got {page:?}",
        NAMES.len()
    );

    for name in NAMES {
        let entry = entries
            .iter()
            .find(|entry| entry.get("authorizerName").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("case {name:?}: the listing must report it, got {page:?}"));
        let described = emulator.describe(name).await;
        assert_eq!(
            entry.get("authorizerArn").and_then(Value::as_str),
            described.member_str("authorizerArn"),
            "case {name:?}: expected the ARN DescribeAuthorizer reports, got {entry:?} against \
             {described:?}"
        );
        assert_eq!(
            entry.get("authorizerArn").and_then(Value::as_str),
            Some(expected_arn(REGION, name).as_str()),
            "case {name:?}: expected the authorizer's ARN, got {entry:?}"
        );
        // `AuthorizerSummary` declares the name and the ARN and nothing else
        // (`smithy_query(query="shape",shape="AuthorizerSummary")`), so a listing entry
        // carries neither the function it calls nor its status, keys or timestamps.
        for absent in [
            "authorizerFunctionArn",
            "status",
            "tokenKeyName",
            "tokenSigningPublicKeys",
            "creationDate",
            "lastModifiedDate",
            "signingDisabled",
            "enableCachingForHttp",
        ] {
            assert!(
                entry.get(absent).is_none(),
                "case {name:?}: a listing entry carries no {absent}, got {entry:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_status_filter_partitions_the_listing() {
    let emulator = Emulator::new();
    create_authorizers(&emulator, &NAMES).await;

    for order in ORDERS {
        let label = order.label();
        let mut partitioned: Vec<String> = Vec::new();
        for status in STATUSES {
            let expected: Vec<&str> = NAMES
                .iter()
                .copied()
                .filter(|name| status_of(name) == status)
                .collect();
            let page = emulator
                .list_authorizers(order, None, None, Some(status))
                .await;
            assert_eq!(
                page.status, 200,
                "case {label:?} status={status:?}: the filtered listing must succeed, got {page:?}"
            );
            assert_eq!(
                page.listed_names(),
                expected_names(&expected, order),
                "case {label:?} status={status:?}: expected exactly the authorizers in that \
                 status, got {page:?}"
            );
            partitioned.extend(page.listed_names());
        }
        partitioned.sort();
        let mut unfiltered = expected_names(&NAMES, order);
        unfiltered.sort();
        assert_eq!(
            partitioned, unfiltered,
            "case {label:?}: the statuses must partition the unfiltered listing, got \
             {partitioned:?}"
        );
    }

    // An update that moves an authorizer's status moves it between the two filtered
    // listings, because the filter reads the record rather than the create request.
    let moved = NAMES[0];
    let updated = emulator.deactivate(moved).await;
    assert_eq!(
        updated.status, 200,
        "case {moved:?}: UpdateAuthorizer must succeed, got {updated:?}"
    );
    let active = emulator
        .list_authorizers(Order::Ascending, None, None, Some("ACTIVE"))
        .await;
    assert!(
        !active.listed_names().contains(&moved.to_string()),
        "case {moved:?}: a deactivated authorizer is no longer in the ACTIVE listing, \
         got {active:?}"
    );
    let inactive = emulator
        .list_authorizers(Order::Ascending, None, None, Some("INACTIVE"))
        .await;
    assert!(
        inactive.listed_names().contains(&moved.to_string()),
        "case {moved:?}: a deactivated authorizer is in the INACTIVE listing, got {inactive:?}"
    );
}

#[tokio::test]
async fn every_page_size_walks_every_authorizer_exactly_once() {
    let emulator = Emulator::new();
    create_authorizers(&emulator, &NAMES).await;

    // Every page size from one to past the number of authorizers held, plus the modelled
    // maximum `PageSize` (`range 1..250`) and the absent member.
    let page_sizes: Vec<Option<i32>> = vec![
        Some(1),
        Some(2),
        Some(3),
        Some(NAMES.len() as i32),
        Some(NAMES.len() as i32 + 1),
        Some(MAX_LIST_PAGE_SIZE),
        None,
    ];
    for order in ORDERS {
        for page_size in &page_sizes {
            let label = order.label();
            let (names, page_lengths) = emulator
                .walk_authorizers(order, *page_size, None, NAMES.len())
                .await;
            assert_eq!(
                names,
                expected_names(&NAMES, order),
                "case {label:?} pageSize={page_size:?}: paging must walk the listing's order \
                 exactly once, got pages of {page_lengths:?}"
            );
            // An absent `pageSize` bounds the page at the listing's own maximum, which is
            // above the number of authorizers an account can hold.
            let bound = page_size.unwrap_or(MAX_LIST_PAGE_SIZE) as usize;
            for (page_number, length) in page_lengths.iter().enumerate() {
                assert!(
                    *length <= bound,
                    "case {label:?} pageSize={page_size:?}: page {page_number} holds {length} \
                     entries, more than the page bound {bound}"
                );
            }
            // Only the last page of a listing may be short: a `nextMarker` is returned
            // while authorizers remain, not instead of filling a page.
            for (page_number, length) in page_lengths
                .iter()
                .enumerate()
                .take(page_lengths.len().saturating_sub(1))
            {
                assert_eq!(
                    *length, bound,
                    "case {label:?} pageSize={page_size:?}: page {page_number} is short at \
                     {length} entries while the listing continues"
                );
            }
        }
    }

    // The same walk over one filtered listing: the filter is part of the listing, so its
    // pages are the filtered order and nothing else.
    for status in STATUSES {
        let expected: Vec<&str> = NAMES
            .iter()
            .copied()
            .filter(|name| status_of(name) == status)
            .collect();
        let (names, page_lengths) = emulator
            .walk_authorizers(Order::Ascending, Some(1), Some(status), NAMES.len())
            .await;
        assert_eq!(
            names,
            expected_names(&expected, Order::Ascending),
            "case status={status:?} pageSize=1: paging a filtered listing must walk its order \
             exactly once, got pages of {page_lengths:?}"
        );
    }
}

#[tokio::test]
async fn a_marker_this_listing_did_not_issue_is_refused() {
    let emulator = Emulator::new();
    create_authorizers(&emulator, &NAMES).await;

    // A marker issued by the ASCENDING listing names a position in that listing's order,
    // which means something else in the descending one, so each direction refuses the
    // other's marker; the same holds for a marker of a differently filtered listing.
    let ascending = emulator
        .list_authorizers(Order::Ascending, Some(1), None, None)
        .await;
    let ascending_marker = ascending.next_marker().unwrap_or_else(|| {
        panic!("the first page of one leaves a nextMarker, got {ascending:?}")
    });
    let descending = emulator
        .list_authorizers(Order::Descending, Some(1), None, None)
        .await;
    let descending_marker = descending.next_marker().unwrap_or_else(|| {
        panic!("the first page of one leaves a nextMarker, got {descending:?}")
    });
    let filtered = emulator
        .list_authorizers(Order::Ascending, Some(1), None, Some("ACTIVE"))
        .await;
    let filtered_marker = filtered
        .next_marker()
        .unwrap_or_else(|| panic!("the first ACTIVE page of one leaves a marker, got {filtered:?}"));

    // Every marker below satisfies the modelled `Marker` (`length 0..1024, pattern
    // ^[A-Za-z0-9+/]+={0,2}$`), so each one reaches the handler and is refused for what it
    // NAMES rather than for its shape.
    let cases: Vec<(&str, Order, Option<&str>, String)> = vec![
        (
            "an ascending marker on the descending listing",
            Order::Descending,
            None,
            ascending_marker.clone(),
        ),
        (
            "an ascending marker on the defaulted listing",
            Order::Absent,
            None,
            ascending_marker.clone(),
        ),
        (
            "a descending marker on the ascending listing",
            Order::Ascending,
            None,
            descending_marker,
        ),
        (
            "an unfiltered marker on a filtered listing",
            Order::Ascending,
            Some("ACTIVE"),
            ascending_marker.clone(),
        ),
        (
            "a filtered marker on the unfiltered listing",
            Order::Ascending,
            None,
            filtered_marker.clone(),
        ),
        (
            "a marker of the other status",
            Order::Ascending,
            Some("INACTIVE"),
            filtered_marker,
        ),
        ("not hex at all", Order::Ascending, None, "zzzz".to_string()),
        (
            "hex of nothing this listing issued",
            Order::Ascending,
            None,
            "deadbeef".to_string(),
        ),
        (
            "a marker of another resource's listing",
            Order::Ascending,
            None,
            hex::encode("certificates/a\u{1f}0000"),
        ),
    ];
    for (label, order, status, marker) in cases {
        let refused = emulator
            .list_authorizers(order, Some(1), Some(&marker), status)
            .await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: expected the marker to be refused 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            "case {label:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert!(
            refused.listed().is_none(),
            "case {label:?}: a refusal carries no authorizers, got {refused:?}"
        );
    }

    // The refusals wrote nothing: every authorizer is still listed, in order.
    let page = emulator
        .list_authorizers(Order::Ascending, None, None, None)
        .await;
    assert_eq!(
        page.listed_names(),
        expected_names(&NAMES, Order::Ascending),
        "the refused markers must leave the store untouched, got {page:?}"
    );
}

#[tokio::test]
async fn an_authorizer_created_without_a_status_is_out_of_service_until_an_update_puts_it_in() {
    let emulator = Emulator::new();

    // Three creates that name NO status: `AuthorizerStatus` carries no modelled default,
    // and an authorizer AWS has not put into use is INACTIVE, so none of them is in the
    // ACTIVE listing and all of them are in the INACTIVE one.
    let names = ["unstated-1", "unstated-2", "unstated-3"];
    for name in names {
        let created = emulator
            .create(
                name,
                &json!({
                    "authorizerFunctionArn":
                        format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:authorize"),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: CreateAuthorizer must succeed, got {created:?}"
        );
        let read = emulator.describe(name).await;
        assert_eq!(
            read.member_str("status"),
            Some("INACTIVE"),
            "case {name:?}: an authorizer created without a status is out of service, \
             got {read:?}"
        );
    }

    for order in ORDERS {
        let label = order.label();
        let active = emulator
            .list_authorizers(order, None, None, Some("ACTIVE"))
            .await;
        assert_eq!(
            active.listed_names(),
            Vec::<String>::new(),
            "case {label:?}: no authorizer was put into service, so the ACTIVE listing is \
             empty, got {active:?}"
        );
        let inactive = emulator
            .list_authorizers(order, None, None, Some("INACTIVE"))
            .await;
        assert_eq!(
            inactive.listed_names(),
            expected_names(&names, order),
            "case {label:?}: every authorizer created without a status is out of service, \
             got {inactive:?}"
        );
    }

    // `UpdateAuthorizer`'s `status` is what puts one into service, and only that one.
    let promoted = names[1];
    let updated = emulator
        .update(promoted, &json!({ "status": "ACTIVE" }))
        .await;
    assert_eq!(
        updated.status, 200,
        "case {promoted:?}: UpdateAuthorizer must succeed, got {updated:?}"
    );
    let active = emulator
        .list_authorizers(Order::Ascending, None, None, Some("ACTIVE"))
        .await;
    assert_eq!(
        active.listed_names(),
        vec![promoted.to_string()],
        "case {promoted:?}: the ACTIVE listing is exactly the authorizer put into service, \
         got {active:?}"
    );
    let still_held: Vec<&str> = names.iter().copied().filter(|n| *n != promoted).collect();
    let inactive = emulator
        .list_authorizers(Order::Ascending, None, None, Some("INACTIVE"))
        .await;
    assert_eq!(
        inactive.listed_names(),
        expected_names(&still_held, Order::Ascending),
        "case {promoted:?}: the rest are still out of service, got {inactive:?}"
    );
}

#[tokio::test]
async fn listing_authorizers_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    create_authorizers(&emulator, &NAMES).await;

    // `ListAuthorizers` reports the store; it must not re-time, re-mint or re-order it,
    // so two identical listings are byte-identical.
    for order in ORDERS {
        for page_size in [None, Some(1), Some(2)] {
            let label = order.label();
            let first = emulator
                .list_authorizers(order, page_size, None, None)
                .await;
            let second = emulator
                .list_authorizers(order, page_size, None, None)
                .await;
            assert_eq!(
                first.body, second.body,
                "case {label:?} pageSize={page_size:?}: a listing must not change what the next \
                 one sees, got {first:?} then {second:?}"
            );
        }
    }

    for name in NAMES {
        let read = emulator.describe(name).await;
        assert_eq!(
            read.status, 200,
            "case {name:?}: every listed authorizer still reads back, got {read:?}"
        );
        assert_eq!(
            read.member_str("status"),
            Some(status_of(name)),
            "case {name:?}: a listing must not move the authorizer's status, got {read:?}"
        );
        assert_eq!(
            read.member_epoch("creationDate"),
            read.member_epoch("lastModifiedDate"),
            "case {name:?}: a listing must not re-time the authorizer, got {read:?}"
        );
    }

    // A record the store no longer holds is listed no more, and the rest keep their order.
    let removed = NAMES[1];
    let deactivated = emulator.deactivate(removed).await;
    assert_eq!(
        deactivated.status, 200,
        "case {removed:?}: UpdateAuthorizer must succeed, got {deactivated:?}"
    );
    let deleted = emulator.delete(removed).await;
    assert_eq!(
        deleted.status, 200,
        "case {removed:?}: DeleteAuthorizer must succeed, got {deleted:?}"
    );
    let remaining: Vec<&str> = NAMES.iter().copied().filter(|n| *n != removed).collect();
    for order in ORDERS {
        let label = order.label();
        let page = emulator.list_authorizers(order, None, None, None).await;
        assert_eq!(
            page.listed_names(),
            expected_names(&remaining, order),
            "case {label:?}: a deleted authorizer is listed no more, got {page:?}"
        );
    }
}
