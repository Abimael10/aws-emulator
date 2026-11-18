// SPDX-License-Identifier: CC0-1.0

//! The saved queries of one workgroup are LISTED a page at a time, and a deleted one is gone
//! from every read of them.
//!
//! The rule under test: **a listing of saved queries reports exactly the ids of the queries the
//! workgroup the request names holds — the account's default workgroup where the request names
//! none — a page at a time, and a delete takes a query out of that set and out of the read that
//! resolves its id.** So the set a client walks is the set the creates left behind and the
//! deletes removed, never a page computed from the request.
//!
//! The rows differ along what the rule generalises over: how many queries the workgroup holds,
//! the page size the request asks for (none, the modelled minimum of 0, one, several, the
//! modelled maximum of 50, and one past that maximum, which is refused), which workgroup is
//! listed (one a create registered, and the default workgroup a request naming none addresses),
//! and whether the id a read or a delete names is one this account ever minted.
//!
//! Sources: `MaxNamedQueriesCount` is modelled `range 0..50` and `NamedQueryIdList` `@length
//! 1..50`, and "if a workgroup is not specified, lists the saved queries for the primary
//! workgroup" is `ListNamedQueries`'s own documentation
//! (`smithy_query(query="operation",operation="ListNamedQueries")`).
//!
//! No expected value is read out of the answer being checked: every id asserted on is one
//! `CreateNamedQuery` answered with, and every other expected value is what the create sent.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{call, create_work_group, error_code, message, router};

/// The workgroup a request naming none addresses: "if a workgroup is not specified, lists the
/// saved queries for the primary workgroup".
const DEFAULT_WORK_GROUP: &str = "primary";

/// The largest page `ListNamedQueries` may be asked for, which is the maximum of the modelled
/// `MaxNamedQueriesCount` (`range 0..50`); one past it is outside the model and refused.
const MAX_PAGE: i64 = 50;

/// The error this service answers a bad request with (`InvalidRequestException`, HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// One row of the listing table: the case, the workgroup the request names (`None` for a
/// request naming none), the page size it asks for, and the ids the walk must report.
type ListingRow<'a> = (&'a str, Option<&'a str>, Option<i64>, &'a [String]);

/// Save one query and answer with the `NamedQueryId` Athena minted for it.
async fn create_named_query(
    router: &MultiServiceRouter,
    name: &str,
    query_string: &str,
    work_group: Option<&str>,
) -> String {
    let mut request = json!({
        "Name": name,
        "Database": "default",
        "QueryString": query_string,
    });
    if let Some(work_group) = work_group {
        request["WorkGroup"] = json!(work_group);
    }
    let (status, created) = call(router, "CreateNamedQuery", &request).await;
    assert_eq!(
        status, 200,
        "creating {name:?}: expected HTTP 200, got {status} with {created:?}"
    );
    created["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("creating {name:?}: expected a minted id, got {created:?}"))
        .to_string()
}

/// One page of `ListNamedQueries`, as `(the ids it carried, the token it handed back)`.
async fn page(
    router: &MultiServiceRouter,
    request: &Value,
    case: &str,
) -> (Vec<String>, Option<String>) {
    let (status, listed) = call(router, "ListNamedQueries", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListNamedQueries({request}), got {status} with \
         {listed:?}"
    );
    let ids: Vec<String> = listed["NamedQueryIds"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the id list on every answer, got {listed:?}")
        })
        .iter()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect();
    let token = listed["NextToken"].as_str().map(str::to_string);
    (ids, token)
}

/// Every id the listing reports for `work_group` when it is walked `page_size` at a time,
/// following the tokens it hands back.
async fn walked(
    router: &MultiServiceRouter,
    work_group: Option<&str>,
    page_size: Option<i64>,
    case: &str,
) -> Vec<String> {
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One look per page the workgroup can hold, plus one: a walk that has not ended by then is
    // not making progress, which is itself a failure of the rule.
    for _ in 0..=(MAX_PAGE * 2) {
        let mut request = json!({});
        if let Some(work_group) = work_group {
            request["WorkGroup"] = json!(work_group);
        }
        if let Some(page_size) = page_size {
            request["MaxResults"] = json!(page_size);
        }
        if let Some(token) = &token {
            request["NextToken"] = json!(token);
        }
        let (ids, next) = page(router, &request, case).await;
        if let Some(size) = page_size {
            assert!(
                i64::try_from(ids.len()).unwrap_or(i64::MAX) <= size,
                "case {case:?}: expected a page of at most {size} ids, got {ids:?}"
            );
        }
        collected.extend(ids);
        match next {
            None => return collected,
            Some(next) => token = Some(next),
        }
    }
    panic!("case {case:?}: the walk never reached the last page, collected {collected:?}");
}

/// The listing reports the ids of the workgroup's own saved queries, whatever page size it is
/// walked at — and a request asking for none gets none.
#[tokio::test]
async fn the_listing_reports_the_saved_queries_of_one_work_group() {
    let router = router();
    create_work_group(&router, "analytics").await;

    // The queries of the two workgroups, created through the service's own call: `(the name, the
    // SQL, the workgroup)`.
    let declared: &[(&str, &str, Option<&str>)] = &[
        ("default-one", "SELECT 1", None),
        ("default-two", "SELECT 2", Some(DEFAULT_WORK_GROUP)),
        ("default-three", "SELECT 3", None),
        ("default-four", "SELECT 4", None),
        ("default-five", "SELECT 5", None),
        ("analytics-one", "SELECT 6", Some("analytics")),
        ("analytics-two", "SELECT 7", Some("analytics")),
    ];
    let mut in_default: Vec<String> = Vec::new();
    let mut in_analytics: Vec<String> = Vec::new();
    for (name, query_string, work_group) in declared {
        let id = create_named_query(&router, name, query_string, *work_group).await;
        if work_group.unwrap_or(DEFAULT_WORK_GROUP) == DEFAULT_WORK_GROUP {
            in_default.push(id);
        } else {
            in_analytics.push(id);
        }
    }

    // `(the case, the workgroup the request names, the page size it asks for, the ids the walk
    // must report)`. The page size rows cover a request naming none, the modelled minimum, one,
    // several, and the modelled maximum; the workgroup rows cover one a create registered and
    // the default workgroup a request naming none addresses.
    let rows: &[ListingRow<'_>] = &[
        (
            "no page size, the default workgroup",
            None,
            None,
            &in_default,
        ),
        (
            "no page size, the workgroup named",
            Some(DEFAULT_WORK_GROUP),
            None,
            &in_default,
        ),
        (
            "one id a page",
            Some(DEFAULT_WORK_GROUP),
            Some(1),
            &in_default,
        ),
        (
            "three ids a page",
            Some(DEFAULT_WORK_GROUP),
            Some(3),
            &in_default,
        ),
        (
            "the modelled maximum page",
            Some(DEFAULT_WORK_GROUP),
            Some(MAX_PAGE),
            &in_default,
        ),
        (
            "another workgroup's own queries",
            Some("analytics"),
            Some(2),
            &in_analytics,
        ),
        (
            "another workgroup, one id a page",
            Some("analytics"),
            Some(1),
            &in_analytics,
        ),
    ];
    for (case, work_group, page_size, expected) in rows {
        let mut actual = walked(&router, *work_group, *page_size, case).await;
        actual.sort();
        let mut wanted = (*expected).to_vec();
        wanted.sort();
        assert_eq!(
            actual, wanted,
            "case {case:?}: expected the workgroup's own saved queries, got {actual:?}"
        );
    }

    // The modelled MINIMUM page is a page of no ids, and it hands back no token: a caller that
    // asked for none gets none rather than a page it did not ask for.
    let (ids, token) = page(
        &router,
        &json!({ "WorkGroup": DEFAULT_WORK_GROUP, "MaxResults": 0 }),
        "the modelled minimum page",
    )
    .await;
    assert!(
        ids.is_empty() && token.is_none(),
        "case \"the modelled minimum page\": expected no ids and no token, got {ids:?} and \
         {token:?}"
    );
}

/// The refusals of the listing: a page size past the modelled maximum, a workgroup this account
/// does not hold, and a token this service did not mint.
#[tokio::test]
async fn the_listing_refuses_what_it_cannot_answer() {
    let router = router();
    create_named_query(&router, "the-one", "SELECT 1", None).await;

    // `(the case, the request, the error the service answers)`. A page size one past the
    // modelled maximum is outside `MaxNamedQueriesCount`, which the framework refuses before the
    // handler runs; the other two are the handler's own.
    let rows: &[(&str, Value, Option<&str>)] = &[
        (
            "a page size one past the modelled maximum",
            json!({ "MaxResults": MAX_PAGE + 1 }),
            None,
        ),
        (
            "a workgroup this account does not hold",
            json!({ "WorkGroup": "no-such-workgroup" }),
            Some(INVALID_REQUEST),
        ),
        (
            "a token this service did not mint",
            json!({ "NextToken": "not-a-token-this-service-minted" }),
            Some(INVALID_REQUEST),
        ),
    ];
    for (case, request, expected) in rows {
        let (status, refused) = call(&router, "ListNamedQueries", request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        if let Some(expected) = expected {
            assert_eq!(
                error_code(&refused),
                *expected,
                "case {case:?}: expected the error {expected}, got {refused:?}"
            );
        }
    }
}

/// A read of an id this account holds no query under is refused in Amazon Athena's own words:
/// the fixed text of that sentence with the id the CALLER supplied as its one variable part, so
/// a client that parses the refusal reads the id it asked about.
#[tokio::test]
async fn a_read_of_an_unminted_id_is_refused_in_athenas_own_words() {
    let router = router();
    let held = create_named_query(&router, "the-one", "SELECT 1", None).await;

    // The ids differ along what the wording generalises over — its one variable part — so the
    // row that was deleted and the row that was never minted are the same sentence about
    // different ids.
    let (status, answered) = call(
        &router,
        "DeleteNamedQuery",
        &json!({ "NamedQueryId": held }),
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting the query: expected HTTP 200, got {status} with {answered:?}"
    );
    let rows: &[(&str, &str)] = &[
        ("an id whose query was deleted", held.as_str()),
        ("an id no create minted", "00000000-0000-4000-8000-000000000000"),
        ("an id of another form", "not-a-minted-id"),
    ];
    for (case, id) in rows {
        let (status, refused) = call(&router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        let expected = format!("NamedQuery: {id} does not exist");
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected the refusal {expected:?}, got {refused:?}"
        );
    }
}

/// A delete takes the query it names out of the read that resolves its id and out of the
/// workgroup's listing, and leaves every other query exactly where it is — and an id this
/// account holds no query under is refused in the same words a read of that id is refused
/// with, taking nothing away.
#[tokio::test]
async fn a_deleted_named_query_is_gone_from_every_read() {
    let router = router();
    let first = create_named_query(&router, "keep-one", "SELECT 1", None).await;
    let removed = create_named_query(&router, "remove-me", "SELECT 2", None).await;
    let last = create_named_query(&router, "keep-two", "SELECT 3", None).await;

    // `(the case, the id the delete names, whether the registry holds a query under it)`: the
    // query this account holds, the same id again — which has nothing left to remove — and an
    // id no create ever minted.
    let deletes: &[(&str, &str, bool)] = &[
        ("the query this account holds", removed.as_str(), true),
        ("the same id again", removed.as_str(), false),
        (
            "an id no create minted",
            "00000000-0000-4000-8000-000000000000",
            false,
        ),
    ];
    for (case, id, holds_a_query) in deletes {
        let (status, answered) = call(&router, "DeleteNamedQuery", &json!({ "NamedQueryId": id })).await;
        if *holds_a_query {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DeleteNamedQuery, got {status} with {answered:?}"
            );
        } else {
            // A delete has a query to take only while the registry holds one under that id.
            // An id it holds none under is refused rather than answered as a success — the
            // same refusal, in the same words, a read of that id states.
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from DeleteNamedQuery for an id naming no \
                 query, got {status} with {answered:?}"
            );
            assert_eq!(
                error_code(&answered),
                INVALID_REQUEST,
                "case {case:?}: expected the error {INVALID_REQUEST}, got {answered:?}"
            );
            let expected = format!("NamedQuery: {id} does not exist");
            assert_eq!(
                message(&answered),
                expected,
                "case {case:?}: expected the refusal {expected:?}, got {answered:?}"
            );
        }

        // The deleted query resolves to nothing afterwards.
        let (status, gone) = call(&router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from GetNamedQuery once the id names no query, got \
             {status} with {gone:?}"
        );
        assert_eq!(
            error_code(&gone),
            INVALID_REQUEST,
            "case {case:?}: expected the error {INVALID_REQUEST}, got {gone:?}"
        );

        // And the queries the delete did not name are untouched: still readable, still listed.
        let mut listed = walked(&router, None, None, case).await;
        listed.sort();
        let mut wanted = vec![first.clone(), last.clone()];
        wanted.sort();
        assert_eq!(
            listed, wanted,
            "case {case:?}: expected the two queries the delete did not name, got {listed:?}"
        );
        for kept in [&first, &last] {
            let (status, held) =
                call(&router, "GetNamedQuery", &json!({ "NamedQueryId": kept })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the query {kept} to be readable still, got {status} with \
                 {held:?}"
            );
        }
    }
}
